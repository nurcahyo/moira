# Public Authentication

Public routes accept:

- `X-Moira-System-Key`
- `X-Consumer-Key`
- `Authorization: Bearer <trusted JWT>`
- `X-Consumer-Key` plus `Authorization: Bearer <trusted JWT>` for delegated application calls

System keys cannot be combined with other credential mechanisms. Consumer plus JWT authentication uses the consumer key application binding as authoritative, rejects application conflicts, and intersects consumer/JWT scopes. Consumer principals never inherit `moira:admin`.

Raw credentials are never logged, persisted in idempotency records, returned in OpenAPI examples, or replayed in responses.

