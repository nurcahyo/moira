# Moira Admin API

Phase 2 and Phase 3 admin APIs live under `/api/v1/admin`. Public non-admin routes are limited to `/health/live`, `/health/ready`, `/openapi.json`, and `/docs`.

Admin requests authenticate with exactly one of:

- `Authorization: Bearer <trusted-jwt>`
- `X-Moira-System-Key: <raw-system-key>`
- `X-Consumer-Key: <raw-consumer-key>`

Conflicting credentials are rejected. All state-changing versioned resources use `If-Match: "<version>"` and return `ETag: "<version>"`. Credential rotation requires `If-Match`; the expected version participates in the idempotency command hash and is checked inside the mutation transaction.

The ten core application, provider, provider-model, credential, API-key, and JWT-issuer create/rotate operation identities support atomic `Idempotency-Key` execution. Successful creates return and replay `201`; rotations return and replay `200`. A reused key with different command input returns `409 idempotency_conflict`, while a bounded wait for an active winner returns `409 idempotency_in_progress`. Deterministic business failures replay with their original sanitized error and a fresh request ID. Raw API-key secrets are available only to the winning request.

List endpoints use `limit` with default `50` and max `200`. Responses are shaped as `{ "data": [], "pagination": { "next_cursor": null, "has_more": false } }`.

## Endpoint Groups

- Setup readiness: `GET /api/v1/admin/setup/status`
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

Setup readiness is a read-only structural check. It reports coarse component states and whether
the default route has an executable application, provider, model, policy, and compatible global
or application credential. It does not decrypt credentials, contact providers, or return resource
identifiers, names, counts, or secret metadata. Access requires a system key or trusted JWT with
`moira:setup:read`; `moira:admin` implies that scope.
