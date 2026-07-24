# Public Authentication

Public routes accept:

- `X-Moira-System-Key`
- `X-Consumer-Key`
- `Authorization: Bearer <trusted JWT>`
- `X-Consumer-Key` plus `Authorization: Bearer <trusted JWT>` for delegated application calls

System keys cannot be combined with other credential mechanisms. Consumer plus JWT authentication uses the consumer key application binding as authoritative, rejects application conflicts, and intersects consumer/JWT scopes. Consumer principals never inherit `moira:admin`.

Standalone trusted JWTs used on public routes must resolve to a valid internal
application UUID through the configured application claim. Trusted JWTs without
an application binding may still authenticate for explicitly authorized admin
surfaces, but they cannot create, discover, or read public application resources.
When a consumer key and JWT are combined, the consumer key supplies the required
application binding and a JWT application claim, when present, must match it.

Raw credentials are never logged, persisted in idempotency records, returned in OpenAPI examples, or replayed in responses.
