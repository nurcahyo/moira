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
`src/infra/repositories/public.rs` filters on `pm.status = 'active'` — so no new status value or
column was needed to retire them. Existing routing policies that still point at a now-deprecated
`provider_model_id` keep resolving (the row still exists), but new discovery and new routing
configuration should move to `deepseek-v4-flash` / `deepseek-v4-pro`.

## Capabilities

DeepSeek sets `SUPPORTS_RESPONSE_FORMAT = false` in `rig-core` for the provider as a whole — see
`docs/openai-compatibility.md` — so a `provider_models.capabilities` row claiming
`structured_output` on a `deepseek` provider is reconciled against the provider type and treated
as though it had never claimed it, regardless of model id. This is unchanged by the v4 catalog
addition.
