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
- `text.format`

String `input` becomes one user `input_text` message. Structured `input` is parsed as Moira public input. Unsupported options are rejected by `deny_unknown_fields` or public validation.

## `text.format`

| Request | Result |
| --- | --- |
| `text` absent, or `text: {}` | `response_format: {"type":"text"}` — prose |
| `text.format.type = "text"` | `response_format: {"type":"text"}` — prose |
| `text.format.type = "json_schema"` | `response_format: {"type":"json_schema", …}`, carrying the caller's `name`, `schema` and `strict` |
| `text.format.type = "json_object"` | **422 `unsupported_request_option`** |
| any other `text` key, including `verbosity` and `format.description` | **422**, from `deny_unknown_fields` |

`json_object` is refused rather than translated. Moira's native `response_format: {"type":"json_object"}` becomes the output schema `{"type":"object"}`, which `rig-core`'s OpenAI encoder completes to `{"type":"object","properties":{},"additionalProperties":false,"required":[]}` and sends under `strict: true` — a schema satisfied only by `{}`. Accepting `json_object` here would answer a request for free-form JSON with the empty object, which is a silent wrong answer of the same kind this endpoint used to give for `json_schema`. That native-path behaviour is pinned on the wire by `tests/openai_compat_text_format.rs::documents_native_json_object_reaching_the_provider_as_an_empty_object_schema`.

Two limits are inherited from the native path rather than introduced here, and apply identically to `POST /api/v1/responses` (ledger F45):

- `name` is not forwarded to the provider. `rig-core` derives the schema name from the schema's own `title`, falling back to `response_schema`.
- `strict` is not forwarded either. `rig-core` hardcodes `strict: true` and rewrites the schema to suit it — `additionalProperties: false` is inserted and every declared property is made required. A caller sending `strict: false` gets strict enforcement.

Any request whose `text.format` is honoured is a structured-output request, so it is subject to the application's `structured_output_enabled` policy and requires a model advertising the `structured_output` capability. A request that previously returned prose because `text.format` was discarded may therefore now return `422 structured_output_unsupported` or `model_capability_mismatch` instead. That is the intended outcome: the caller asked for a schema and Moira is now telling them whether it can deliver one.

Advertising the capability is **necessary but not sufficient** (ledger F39). The capability is a value on the provider-model row, and for some provider types `rig-core` will not put the schema on the wire whatever that row says — `DeepSeek` sets `SUPPORTS_RESPONSE_FORMAT = false` and its schema is discarded before the request is built. Routing therefore reconciles the advertised capability against the provider type and skips candidates that cannot receive a schema, so a structured request falls through to a provider that can, or fails with `no_eligible_model` if none is configured — exactly as it already behaves for a row that honestly declares `structured_output: false`. A row that claims the capability on such a provider is not merely ignored: it is treated as though it had never claimed it.

The converse case is not decidable here. `openai_compatible` and `local` providers do receive `response_format.json_schema` on the wire, but whether a self-hosted backend honours it is a property of that backend, which Moira has no way to check at admission time. Those provider types are therefore admitted, and a non-conforming reply surfaces the same way it does anywhere else — `structured_output` is left absent rather than fabricated.

`POST /v1/responses` carries no route field, so every compat request resolves through the default route (`general`, seeded by migration `0005`). A provider must be bound to that route for the endpoint to serve anything.

No `/v1/chat/completions` route is registered in Phase 4.

