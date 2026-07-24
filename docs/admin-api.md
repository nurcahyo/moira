# Moira Admin API

Phase 2 and Phase 3 admin APIs live under `/api/v1/admin`. Public non-admin routes are limited to `/health/live`, `/health/ready`, `/openapi.json`, and `/docs`.

Admin requests authenticate with exactly one of:

- `Authorization: Bearer <trusted-jwt>`
- `X-Moira-System-Key: <raw-system-key>`
- `X-Consumer-Key: <raw-consumer-key>`

Conflicting credentials are rejected. All state-changing versioned resources use `If-Match: "<version>"` and return `ETag: "<version>"`. Create, rotate, and provider runtime policy upsert operations that support `Idempotency-Key` store only hashed idempotency keys and sanitized responses.

List endpoints use `limit` with default `50` and max `200`. Responses are shaped as `{ "data": [], "pagination": { "next_cursor": null, "has_more": false } }`.

## Endpoint Groups

- Applications: `/api/v1/admin/applications`
- Providers: `/api/v1/admin/providers`
- Provider models: `/api/v1/admin/providers/{provider_id}/models` and `/api/v1/admin/provider-models/{id}`
- Provider credentials: `/api/v1/admin/provider-credentials`
- User credential convenience: `/api/v1/admin/users/{external_user_id}/provider-credentials`
- Trusted JWT issuers: `/api/v1/admin/jwt-issuers`
- System keys: `/api/v1/admin/system-keys`
- Consumer keys: `/api/v1/admin/consumer-keys`
- Audit events: `/api/v1/admin/audit-events`
- Route definitions: `/api/v1/admin/routes`
- Routing policies: `/api/v1/admin/routing-policies`
- Agent profiles: `/api/v1/admin/agent-profiles`
- Provider runtime policies: `/api/v1/admin/providers/{provider_id}/runtime-policy`
- Application execution policies: `/api/v1/admin/applications/{id}/execution-policy`
- Runtime diagnostics: `/api/v1/admin/runtime/diagnose`, disabled by default and requiring `moira:runtime:diagnose`

OpenAPI is served at `/openapi.json`; admin paths are exposed only when `MOIRA_DOCS__EXPOSE_ADMIN=true`.
