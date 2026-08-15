# Provider Management

Providers store non-secret runtime configuration. Secret provider headers, organization keys, and project keys belong in provider credentials.

Endpoints:

- `POST /api/v1/admin/providers`
- `GET /api/v1/admin/providers`
- `GET /api/v1/admin/providers/{id}`
- `PATCH /api/v1/admin/providers/{id}`
- `DELETE /api/v1/admin/providers/{id}`
- `POST /api/v1/admin/providers/{id}/enable`
- `POST /api/v1/admin/providers/{id}/disable`
- `POST /api/v1/admin/providers/{provider_id}/models`
- `GET /api/v1/admin/providers/{provider_id}/models`
- `GET/PATCH/DELETE /api/v1/admin/provider-models/{id}`
- `POST /api/v1/admin/provider-models/{id}/enable`
- `POST /api/v1/admin/provider-models/{id}/disable`

Provider base URLs pass the outbound URL policy: HTTPS by default, no embedded credentials, no loopback/private/link-local/multicast/cloud metadata targets, and DNS resolution checks. HTTP/private URLs require explicit local development opt-ins.

Creating a `chatgpt_oauth` provider (issue #216) additionally requires `provider_security.allow_chatgpt_subscription = true`. It is off by default in every environment, including production: ChatGPT/Codex subscriptions are personal, single-user under OpenAI's terms, and wiring one into a multi-tenant gateway is a deployment operator's own explicit ToS risk acceptance, never a silent default. Without the opt-in, `POST /api/v1/admin/providers` refuses the request with `403 chatgpt_subscription_opt_in_required`; the same gate is re-checked at execution time as defense in depth. See `docs/chatgpt-subscription-spike.md` and `docs/rig-integration.md`.

Required scopes:

- Providers read/write/delete: `moira:providers:*`
- Models read/write/delete: `moira:models:*`
