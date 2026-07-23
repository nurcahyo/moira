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

