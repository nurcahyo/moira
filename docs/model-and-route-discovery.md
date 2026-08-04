# Model And Route Discovery

`GET /api/v1/models` lists active provider models visible through active routing policies for the caller.

`GET /api/v1/routes` lists active route definitions visible to the caller and summarizes capabilities observed from attached active models.

Both take `cursor` and `limit` and return the standard
`{ "data": [], "pagination": { "next_cursor": …, "has_more": … } }` envelope, paging by the
same opaque keyset cursor as every other list. The default limit is `50`; max is `200`. A
malformed, tampered-with or foreign cursor is refused with `400 invalid_cursor`.

Both order by `created_at desc, id desc`. That is a change: `/api/v1/models` previously
returned rows in ascending `provider_models.id` order and `/api/v1/routes` in ascending
`route_key` order, each capped at a hard-coded 200 rows with no way to reach the rest. Neither
old order was documented or reachable past the cap; a caller that wants alphabetical routes
should sort the page it received.

`GET /api/v1/capabilities` returns the caller application's execution policy booleans and limits:

- streaming
- vision
- tools
- structured output
- response persistence mode
- maximum input items
- maximum request bytes
- maximum output tokens

Discovery is authorization-filtered, but it is still not a provider health check and does not prove a live credential can execute successfully at that moment.

