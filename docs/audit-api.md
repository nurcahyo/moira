# Audit API

Audit events are append-only and readable through:

- `GET /api/v1/admin/audit-events`
- `GET /api/v1/admin/audit-events/{id}`

Required scope: `moira:audit:read`.

Audit serializers must not expose raw credentials, API-key hashes, JWT tokens, encrypted payloads, nonces, or secret-bearing provider errors. Consumer principals are constrained to events for their application.
