# Streaming API

`POST /api/v1/responses/stream` returns `text/event-stream` SSE envelopes. It accepts the same request body as `POST /api/v1/responses`.

Streaming rejects `Idempotency-Key`. A retrying client should open a new stream and use its own client-side request tracking.

Events use the envelope:

```json
{
  "response_id": "resp_...",
  "execution_id": "exec_...",
  "request_id": "req_...",
  "sequence": 1,
  "type": "response.created",
  "payload": {}
}
```

Event types include `response.created`, `response.in_progress`, mapped runtime events, `response.completed`, and `response.failed`. The transport sends heartbeat keep-alives using `public_api.heartbeat_seconds`.

The current transport is wired to the Phase 3 execution event collector. It preserves the public SSE contract, but token events may be emitted after the upstream execution finishes rather than as true first-token live streaming.

