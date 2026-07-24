# Responses API

`POST /api/v1/responses` executes a single request through Moira routing and returns a response resource.

## Request Shape

`input` is required and contains message objects:

```json
{
  "input": [
    {
      "role": "user",
      "content": [
        { "type": "input_text", "text": "Summarize the deployment state." }
      ]
    }
  ],
  "model": "gpt-4.1-mini",
  "metadata": { "ticket": "MOIRA-123" }
}
```

Supported content parts are `input_text` and HTTPS `input_image` URLs. Client-defined tools, `top_p`, `seed`, file input, and raw provider options are rejected in this phase.

## Response Shape

The immediate non-streaming response contains generated output in memory:

```json
{
  "id": "resp_...",
  "object": "response",
  "status": "completed",
  "execution_id": "exec_...",
  "output": [
    {
      "type": "message",
      "role": "assistant",
      "content": [{ "type": "output_text", "text": "..." }]
    }
  ],
  "output_persisted": false
}
```

`GET /api/v1/responses/{response_id}` returns metadata and an `output_unavailable` marker unless a future persistence mode stores content. Prompt bodies and provider output bodies are not persisted by the Phase 4 implementation.

## Human-Readable Messages

When this API returns a user-visible message, it uses the keyed i18n contract:

- `message_key` is the stable translation identifier.
- `message` is the default English fallback string.
- `message_args` carries interpolation data when needed.

That makes the payload readable in curl or Postman without a frontend translation layer, while still letting Next.js translate by key and fall back to `message` when a locale string is missing.

Example failure payload:

```json
{
  "error": {
    "code": "validation_failed",
    "message_key": "moira.error.validation_failed",
    "message": "The request validation failed.",
    "message_args": { "field": "input" },
    "request_id": "req_..."
  }
}
```

Example success notice:

```json
{
  "message_key": "moira.notice.response_completed",
  "message": "The response completed successfully."
}
```

When a caller needs localization, use `message_key` as the lookup key and `message` as the fallback:

```ts
const text = t(payload.message_key, payload.message_args, {
  defaultValue: payload.message,
});
```
