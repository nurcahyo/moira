# Idempotency

Moira supports `Idempotency-Key` on non-streaming public response creation, selected admin create and rotate commands, and the RAG collection/document create and ingest routes. Keys are hashed before storage and scoped by actor fingerprint plus operation name.

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

The RAG collection/document create and ingest routes are also routed through the same
atomic admin-command machinery, so their replay guarantees are identical to the core
admin operations above:

| Operation | Endpoint |
| --- | --- |
| `rag.collection.create` | `POST /api/v1/admin/rag-collections` |
| `rag.document.create` | `POST /api/v1/admin/rag-collections/{collection_id}/documents` |
| `rag.document.ingest` | `POST /api/v1/admin/rag-documents/{id}/ingest` |

`POST /api/v1/admin/rag-documents/{id}/reindex` is a direct call-through to the same
handler as `.../ingest` and therefore shares the `rag.document.ingest` operation
identity and request-hash envelope. A consequence worth calling out explicitly: an
`Idempotency-Key` already used on `/ingest` for a document, sent again with the same
body to `/reindex` for that same document, replays the original `/ingest` response
instead of creating a new version. Using a different key on `/reindex` creates a new
version as usual. Retention is the same 24-hour window as the core admin operations.

Conversation and memory create routes (`POST /api/v1/conversations`,
`POST /api/v1/conversations/{id}/messages`, `POST /api/v1/memories`) do **not**
declare or accept `Idempotency-Key` today and do not replay. Retrying one of those
requests can duplicate side effects. Extending replay to those routes is tracked in
`docs/todo.md` as a follow-up, since it is new API surface rather than a fix to an
already-advertised header.
