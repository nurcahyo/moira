# Execution And Usage API

`GET /api/v1/executions/{execution_id}` and `GET /api/v1/executions` return execution summaries derived from `responses` plus `execution_attempts`.

`GET /api/v1/usage` returns usage records with optional filters:

- `application_id`
- `external_tenant_id`
- `external_user_id`
- `provider_id`
- `provider_model_id`
- `route_id`
- `occurred_after`
- `occurred_before`
- `limit`

Lists use the standard `{ "data": [], "pagination": { ... } }` envelope. The default limit is `50`; max is `200`. Records are ordered newest first.

Usage records contain normalized token counts and estimated cost when pricing is available. They never include prompt text or provider response bodies.

