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

Event types include `response.created`, `response.in_progress`, mapped runtime events, and exactly one terminal event:

- `response.completed` after successful persistence
- `response.failed` after an execution or persistence failure
- `response.cancelled` when execution is cancelled

The transport sends heartbeat keep-alives using `public_api.heartbeat_seconds`. Runtime deltas are forwarded through a bounded channel as they arrive; they are not replayed after provider completion.

Dropping the client connection cancels the supervised execution. Moira waits for the execution attempt to become terminal, persists the public response as `cancelled`, and records the cancellation audit event. Partial streamed output is not appended to conversation history.
