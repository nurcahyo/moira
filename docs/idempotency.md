# Idempotency

Moira supports `Idempotency-Key` on non-streaming public response creation and selected admin create and rotate commands. Keys are hashed before storage and scoped by actor fingerprint plus operation name.

Moira stores:

- hashed idempotency key
- actor fingerprint
- operation
- normalized request hash
- sanitized response body
- resource id
- expiration timestamp

A replay with the same normalized request returns the stored status and sanitized response. Reusing the same key for a different body, path identifier, key type, or expected version returns `409 idempotency_conflict`. If the command does not finish within the bounded lock wait, Moira returns `409 idempotency_in_progress`.

For atomic admin commands, the mutation, success audit, and replay record commit in one PostgreSQL transaction. Successful creates replay with `201`; rotations replay with `200`. Deterministic `400`, `404`, `409`, and `422` business failures are stored in sanitized form and replayed with a fresh `request_id`. Authentication, authorization, malformed HTTP extraction, lock timeout, cancellation, database errors, and `5xx` failures are not cached.

The core admin operation identities are:

| Operation | Endpoint |
| --- | --- |
| `application.create` | `POST /api/v1/admin/applications` |
| `provider.create` | `POST /api/v1/admin/providers` |
| `provider_model.create` | `POST /api/v1/admin/providers/{provider_id}/models` |
| `credential.create` | `POST /api/v1/admin/provider-credentials` |
| `credential.rotate` | `POST /api/v1/admin/provider-credentials/{id}/rotate` |
| `system_key.create` | `POST /api/v1/admin/system-keys` |
| `system_key.rotate` | `POST /api/v1/admin/system-keys/{id}/rotate` |
| `consumer_key.create` | `POST /api/v1/admin/consumer-keys` |
| `consumer_key.rotate` | `POST /api/v1/admin/consumer-keys/{id}/rotate` |
| `jwt_issuer.create` | `POST /api/v1/admin/jwt-issuers` |

Only the winning system-key or consumer-key command receives the raw secret. Its replay preserves the original status but returns `secret: null` and `secret_retrievable: false`. A lost one-time secret must be recovered by rotating with a new idempotency key.

Streaming does not support idempotency because replaying a partially delivered SSE stream is unsafe.
