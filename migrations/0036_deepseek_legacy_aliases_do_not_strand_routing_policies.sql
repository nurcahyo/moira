-- Issue #256 finding 1 — `migrations/0028_deepseek_v4_catalog.sql:93`.
--
-- `0028` flips every live `deepseek-chat` / `deepseek-reasoner` row to `deprecated` and stops
-- there. `deprecated` is not a label: it removes the row from candidate resolution and from
-- discovery, because every one of those queries filters `pm.status = 'active'` —
-- `list_model_candidates` (`src/infra/repositories/runtime.rs:1174`), the embedding target
-- lookup (`:1773`), and the three public queries in `src/infra/repositories/public.rs:799`,
-- `:842`, `:889`.
--
-- So a `routing_policies` row that still names one of the retired ids resolves to **nothing** the
-- moment `0028` commits. `list_model_candidates` returns an empty list, the route stops producing
-- candidates at all, and this happens on migrate — during a rolling deploy, with no admin action,
-- with the policy row still sitting there `active` and looking correct. A deployment routing to
-- DeepSeek breaks on upgrade. `docs/provider-deepseek-catalog.md` asserted that such policies
-- "keep resolving (the row still exists)"; they do not, and that sentence is corrected in the
-- same change as this file.
--
-- ===================================================================================
-- What this does
-- ===================================================================================
--
-- Repoints every live routing policy that still names a retired alias onto the current-generation
-- model `0028` registered **on the same provider**:
--
--     deepseek-chat      ->  deepseek-v4-flash    (the general chat tier)
--     deepseek-reasoner  ->  deepseek-v4-pro      (the reasoning-heavy tier)
--
-- That mapping is a judgement about which successor is the closer replacement, so it is recorded
-- on the row and not only here: every repointed policy carries `metadata -> 'deepseek_v4_repoint'`
-- naming the model it was moved off. An operator who wants a different pairing can see exactly
-- what changed, on which policies, and move it with one PATCH.
--
-- **Repointing rather than un-deprecating**, because the alias is retired at the vendor
-- (2026-07-24, per `0028`), not merely in this catalog. Re-activating the row would keep the
-- policy resolving onto an id DeepSeek is withdrawing — which converts a break visible in
-- configuration on deploy day into an upstream failure at request time, later, on someone else's
-- shift.
--
-- ===================================================================================
-- Scope, deliberately narrow
-- ===================================================================================
--
--  * Live rows only (`deleted_at is null`), `active` or `disabled`. A soft-deleted policy is
--    history and is not rewritten. A `disabled` one is included because it resolves to nothing
--    either way today, and repointing it now is what keeps re-enabling it later from resurrecting
--    a dead reference.
--  * Only where the successor exists as a live `active` row on the same provider. `0028`
--    registers both ids for every configured `deepseek` provider, so this holds wherever `0028`
--    ran; where it somehow does not, the policy is left alone rather than pointed at a row that
--    resolves no better.
--  * Skipped where a live policy in the same scope — same route, application, external tenant and
--    provider — already names that successor. Repointing there would put two identical candidates
--    in one fallback chain (`routing_policies` has no uniqueness over that tuple, so the database
--    would accept it). Nothing is stranded in that case: the route is already served by the
--    successor policy, and the legacy row is redundant rather than load-bearing.
--
-- Idempotent: a second run matches nothing, because the first left no live policy naming a
-- retired alias. A no-op wherever no `deepseek` provider is configured, exactly as `0028` is, and
-- a no-op on a fresh database.
--
-- Runs inside the migrator's transaction — this is a bounded row update on a configuration table,
-- not a scan of a hot one — so a deployment either has every affected policy repointed or none of
-- them, and the `routing_policies` NOTIFY trigger tells every replica once the transaction
-- commits.

update routing_policies as rp
set provider_model_id = successor.id,
    metadata = rp.metadata || jsonb_build_object(
        'deepseek_v4_repoint',
        jsonb_build_object(
            'migration', '0035',
            'from_provider_model_id', legacy.id,
            'from_model_key', legacy.model_key,
            'to_model_key', successor.model_key,
            'at', now()
        )
    )
from provider_models legacy
join providers p on p.id = legacy.provider_id
join (
    values
        ('deepseek-chat', 'deepseek-v4-flash'),
        ('deepseek-reasoner', 'deepseek-v4-pro')
) as retirement(legacy_key, successor_key)
  on retirement.legacy_key = legacy.model_key
join provider_models successor
  on successor.provider_id = legacy.provider_id
 and successor.model_key = retirement.successor_key
 and successor.status = 'active'
 and successor.deleted_at is null
where rp.provider_model_id = legacy.id
  and rp.deleted_at is null
  and rp.status in ('active', 'disabled')
  and legacy.status = 'deprecated'
  and legacy.deleted_at is null
  and p.provider_type = 'deepseek'
  and p.deleted_at is null
  and not exists (
      select 1
      from routing_policies other
      where other.id <> rp.id
        and other.deleted_at is null
        and other.route_id = rp.route_id
        and other.provider_id = rp.provider_id
        and other.provider_model_id = successor.id
        and other.application_id is not distinct from rp.application_id
        and other.external_tenant_id is not distinct from rp.external_tenant_id
  );
