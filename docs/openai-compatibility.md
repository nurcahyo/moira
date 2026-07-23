# OpenAI Responses Compatibility

`POST /v1/responses` is optional and disabled by default. Enable it with:

```toml
[public_api]
openai_responses_compat_enabled = true
```

The adapter maps a small OpenAI Responses-like request into Moira's native `PublicResponseRequest`:

- `model`
- `input`
- `stream`
- `temperature`
- `max_output_tokens`
- `metadata`

String `input` becomes one user `input_text` message. Structured `input` is parsed as Moira public input. Unsupported options are rejected by `deny_unknown_fields` or public validation.

No `/v1/chat/completions` route is registered in Phase 4.

