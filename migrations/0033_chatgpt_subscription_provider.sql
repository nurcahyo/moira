-- Issue #216 — adopt rig-core 0.40's native `chatgpt` provider (`rig_core::providers::chatgpt`,
-- targeting `chatgpt.com/backend-api/codex`) as `ProviderType::ChatgptOauth`, behind an explicit,
-- off-by-default ToS opt-in.
--
-- This migration only widens what an operator may *configure* — it does not enable execution by
-- itself. `providers.provider_type = 'chatgpt_oauth'` is additionally refused at admin-write time
-- (`POST /api/v1/admin/providers`) and at execution time
-- (`RuntimeFactory::build_completion_model`) unless the deployment sets
-- `provider_security.allow_chatgpt_subscription = true` — see
-- `src/orchestration/runtime_factory.rs::require_chatgpt_subscription_opt_in` and
-- `docs/chatgpt-subscription-spike.md`. ChatGPT/Codex subscriptions are personal, single-user
-- under OpenAI's terms; there is no carve-out for third-party, multi-tenant use. Turning this on
-- is a deployment operator's own explicit acceptance of that risk for their own subscription, not
-- a sanctioned integration path.
--
-- Append-only, same shape 0018 and 0020 already use for widening an inline column check
-- constraint: drop the constraint Postgres generated for `providers.provider_type` in
-- migrations/0003_security_foundation.sql (`<table>_<column>_check`) and re-add it, under that
-- same generated name, with the new value appended. Applies clean from an empty database, and is
-- a no-op on re-run (the constraint always ends up in this same shape).
alter table providers
    drop constraint if exists providers_provider_type_check;

alter table providers
    add constraint providers_provider_type_check
    check (provider_type in (
        'openai_compatible',
        'openai',
        'anthropic',
        'gemini',
        'deepseek',
        'azure_openai',
        'local',
        'custom',
        'chatgpt_oauth'
    ));
