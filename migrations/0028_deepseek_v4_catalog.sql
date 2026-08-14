-- Issue #209 — DeepSeek v4 model catalog.
--
-- DeepSeek already builds at the Rig boundary (`ProviderType::DeepSeek` in
-- `src/orchestration/runtime_factory.rs`); this migration is catalog data only, no runtime code.
-- `provider_models.model_key` is passed straight through to `rig_core`'s
-- `Client::completion_model(model_key)` with no validation against a fixed list
-- (`runtime_factory.rs`), so widening the catalog needs nothing beyond these rows.
--
-- `providers` rows are tenant-configured through the admin API
-- (`POST /api/v1/admin/providers`), never fabricated by a migration — the only existing
-- precedent for a migration writing `provider_models` (0003's legacy-import seed) also drives its
-- inserts from whatever `providers`/`legacy_providers` rows already exist rather than inventing
-- one. This migration follows the same shape: every statement below is scoped to
-- `providers.provider_type = 'deepseek'` and is a no-op wherever no such provider is configured,
-- including a fresh, empty database.
--
-- What it does, for every existing (non-deleted) `deepseek` provider:
--   1. Registers the current-generation model ids, `deepseek-v4-flash` and `deepseek-v4-pro`, as
--      `active` if not already present.
--   2. Registers the legacy aliases `deepseek-chat` and `deepseek-reasoner` as `deprecated` if not
--      already present — inserted deprecated rather than silently absent, so discovery still
--      lists them with an honest status instead of a caller hitting an unknown-model error.
--   3. Flips any pre-existing `deepseek-chat` / `deepseek-reasoner` row that is not already
--      `deprecated` to `deprecated`, reflecting DeepSeek's own per-vendor deprecation dated
--      2026-07-24. `provider_models.status = 'deprecated'` already excludes a row from routing
--      and discovery (`pm.status = 'active'` in every routing/candidate query in
--      `src/infra/repositories/public.rs`) — no new column or status value is needed.
--
-- Idempotent and append-only: every insert uses `on conflict do nothing` against the existing
-- partial unique index `provider_models_provider_model_key_active_unique
-- (provider_id, model_key) where deleted_at is null`, matching the bare `on conflict do nothing`
-- form 0003 already uses against the same index. Re-running this migration, or running it after
-- an admin has already created these rows by hand, changes nothing further.

insert into provider_models (
    id,
    provider_id,
    model_key,
    display_name,
    capabilities,
    status,
    created_at,
    updated_at
)
select
    gen_random_uuid(),
    p.id,
    v.model_key,
    v.display_name,
    '{}'::jsonb,
    'active',
    now(),
    now()
from providers p
cross join (
    values
        ('deepseek-v4-flash', 'DeepSeek V4 Flash'),
        ('deepseek-v4-pro', 'DeepSeek V4 Pro')
) as v(model_key, display_name)
where p.provider_type = 'deepseek'
  and p.deleted_at is null
on conflict do nothing;

insert into provider_models (
    id,
    provider_id,
    model_key,
    display_name,
    capabilities,
    status,
    created_at,
    updated_at
)
select
    gen_random_uuid(),
    p.id,
    v.model_key,
    v.display_name,
    '{}'::jsonb,
    'deprecated',
    now(),
    now()
from providers p
cross join (
    values
        ('deepseek-chat', 'DeepSeek Chat (legacy alias, deprecated 2026-07-24)'),
        ('deepseek-reasoner', 'DeepSeek Reasoner (legacy alias, deprecated 2026-07-24)')
) as v(model_key, display_name)
where p.provider_type = 'deepseek'
  and p.deleted_at is null
on conflict do nothing;

update provider_models pm
set status = 'deprecated',
    updated_at = now()
from providers p
where pm.provider_id = p.id
  and p.provider_type = 'deepseek'
  and p.deleted_at is null
  and pm.deleted_at is null
  and pm.model_key in ('deepseek-chat', 'deepseek-reasoner')
  and pm.status <> 'deprecated';
