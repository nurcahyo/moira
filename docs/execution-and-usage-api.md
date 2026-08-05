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
- `cursor`
- `limit`

Lists use the standard `{ "data": [], "pagination": { ... } }` envelope. The default limit is `50`; max is `200`. Records are ordered newest first.

## Pagination

Both lists page by opaque keyset cursor, the same mechanism the admin lists use.

- `GET /api/v1/executions` orders by `created_at desc, id desc` over `responses`.
- `GET /api/v1/usage` orders by `occurred_at desc, id desc` over `usage_records`.

Send `pagination.next_cursor` back as the `cursor` query parameter to fetch the next page.
`pagination.has_more` is `false` and `next_cursor` is `null` on the last page, including when
that page is exactly `limit` rows long.

A cursor is validated. A malformed, tampered-with or foreign cursor — one minted by a
different list, including the other list on this page — is refused with
`400 invalid_cursor`. It is never a `500` and never a silent reset to page one.

The `id` half of the key is not optional. `usage_records.occurred_at` defaults to the
transaction timestamp, so rows written together share it exactly; without the tiebreak a page
boundary inside such a group would skip one row and repeat another.

Usage records contain normalized token counts and estimated cost when pricing is available. They never include prompt text or provider response bodies.

