# Idempotency

`POST /api/v1/responses` supports `Idempotency-Key`. The key is hashed before storage and scoped by actor fingerprint plus operation name.

Moira stores:

- hashed idempotency key
- actor fingerprint
- operation
- normalized request hash
- sanitized response body
- resource id
- expiration timestamp

A replay with the same normalized request returns the stored sanitized response. Reusing the same key with a different request returns `409 idempotency_conflict`. A replay while the first execution is still running returns `409 execution_in_progress`.

Streaming does not support idempotency because replaying a partially delivered SSE stream is unsafe.

