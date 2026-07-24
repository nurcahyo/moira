# Public API

Phase 4 adds the public execution surface on top of the Phase 3 execution kernel. Public routes authenticate callers, validate inputs, resolve application execution policy, route through the existing runtime service, emit audit records, and expose metadata-only response/execution resources.

## Routes

- `POST /api/v1/responses`
- `POST /api/v1/responses/stream`
- `GET /api/v1/responses/{response_id}`
- `GET /api/v1/executions/{execution_id}`
- `GET /api/v1/executions`
- `GET /api/v1/usage`
- `GET /api/v1/models`
- `GET /api/v1/routes`
- `GET /api/v1/capabilities`

The only optional compatibility route is `POST /v1/responses`, gated by `public_api.openai_responses_compat_enabled`. `/v1/chat/completions` is intentionally not registered.

All JSON responses include `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and propagated `X-Request-Id`.

User-visible API messages follow the i18n contract documented in [docs/i18n-response-contract.md](i18n-response-contract.md): the backend emits `message_key` plus an English fallback `message`, and callers should translate by key only when a localized string is available.

Do not send real provider secrets, API keys, authorization headers, JWTs, or private documents in prompts while developing locally. Moira does not return provider credentials or raw key material from these routes.
