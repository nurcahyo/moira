# DeepSeek Model Catalog

DeepSeek is already functional at the Rig boundary — `ProviderType::DeepSeek` in
`src/orchestration/runtime_factory.rs` builds a native `rig_core::providers::deepseek::Client`,
and `provider_models.model_key` is passed straight through to `Client::completion_model` with no
validation against a fixed list. Adding a model id to the catalog is a data change, never a code
change; see `docs/rig-integration.md` and `docs/provider-management.md` for the provider
integration and admin CRUD surface respectively.

## Current generation

- `deepseek-v4-flash`
- `deepseek-v4-pro`

Registered as `active` `provider_models` rows for every configured `deepseek` provider by
migration `0028_deepseek_v4_catalog.sql`.

## Deprecated legacy aliases

- `deepseek-chat`
- `deepseek-reasoner`

Deprecated per-vendor as of **2026-07-24**. Migration `0028_deepseek_v4_catalog.sql` marks any
existing `provider_models` row for these ids `deprecated` (and registers them deprecated if a
tenant never had them), for every configured `deepseek` provider. A `deprecated`
`provider_models.status` already excludes the row from routing and from
`GET /api/v1/models` / `GET /api/v1/routes` discovery — every candidate query in
`src/infra/repositories/runtime.rs` and `src/infra/repositories/public.rs` filters on
`pm.status = 'active'` — so no new status value or column was needed to retire them.

**A routing policy that still names a deprecated model resolves to nothing.** The row continues
to exist, but `list_model_candidates` joins `provider_models` on `pm.status = 'active'`, so the
policy contributes no candidate and, if it was the route's only one, the route stops producing
candidates at all. `0028` alone would therefore have broken every deployment routing to DeepSeek
at the moment it migrated. Migration
`0035_deepseek_legacy_aliases_do_not_strand_routing_policies.sql` closes that: it repoints every
live policy naming a retired alias onto the current-generation model on the same provider —
`deepseek-chat` to `deepseek-v4-flash`, `deepseek-reasoner` to `deepseek-v4-pro` — and records
what it moved off in `routing_policies.metadata -> 'deepseek_v4_repoint'`, so the substitution is
visible per policy and reversible with a single PATCH. It skips a policy whose scope already has
a live policy for the successor, to avoid putting the same candidate in one fallback chain twice.

New discovery and new routing configuration should name `deepseek-v4-flash` /
`deepseek-v4-pro` directly.

## Capabilities

DeepSeek sets `SUPPORTS_RESPONSE_FORMAT = false` in `rig-core` for the provider as a whole — see
`docs/openai-compatibility.md` — so a `provider_models.capabilities` row claiming
`structured_output` on a `deepseek` provider is reconciled against the provider type and treated
as though it had never claimed it, regardless of model id. This is unchanged by the v4 catalog
addition.
