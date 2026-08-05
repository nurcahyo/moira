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

An `input_image` URL is checked against the shared outbound-URL guard before the request is
accepted: it must be `https`, must carry no embedded credentials, must not point into
loopback, private, link-local, or other reserved address space — by literal or by DNS
resolution — and must sit on the egress allow-list if the deployment configures one. A URL
that fails returns `422 image_url_not_allowed`. Address-space refusals deliberately share a
single message, so the response cannot be used to probe the deployment's internal network;
the specific reason is recorded server-side. See `docs/security.md`.

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

