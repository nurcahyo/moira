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
| `text.format.type = "json_schema"`, `strict` omitted or `true` | `response_format: {"type":"json_schema", …}`, carrying the caller's `name`, `schema` and `strict` |
| `text.format.type = "json_schema"`, `strict: false` | **422 `unsupported_request_option`** |
| `text.format.type = "json_object"` | **422 `unsupported_request_option`** |
| any other `text` key, including `verbosity` and `format.description` | **422**, from `deny_unknown_fields` |

`json_object` is refused rather than translated, and **since ledger F46 the native path refuses it too** — the two endpoints agree. `rig-core` 0.40 has no representation of free-form JSON (the string `"json_object"` does not occur anywhere in the crate), so `CompletionRequest::output_schema` is the only structured-output seam and every encoder reads it as a constraint: the OpenAI family completes an object schema to `{"type":"object","properties":{},"additionalProperties":false,"required":[]}` and sends it under a hardcoded `strict: true` — satisfied by exactly one document, `{}` — while Anthropic's API has no free-form JSON mode at all. Translating `json_object` would therefore answer a request for free-form JSON with the empty object. Both refusals are pinned on the wire by `tests/openai_compat_text_format.rs`.

Two limits are inherited from the native path rather than introduced here, and apply identically to `POST /api/v1/responses` (ledger F45). **Both were silent until F45 closed; one is now a refusal and the other is documented.**

- **`strict: false` is refused, on both endpoints.** `rig-core` 0.40 hardcodes `strict: true` in the OpenAI encoder and cannot be overridden through `additional_params` (with an `output_schema` present the encoder's `response_format` wins the merge); Anthropic and Gemini have no strict/non-strict distinction at all. So non-strict structured output is not expressible anywhere, and accepting `false` meant delivering the opposite — which is not merely stricter: `sanitize_schema` promotes **every declared property to `required`**, so a caller's optional fields become mandatory, and OpenAI's strict mode rejects schemas outside its supported subset, so a caller who asked for best-effort can receive a provider error. Omitting `strict` is unchanged and is still the common case: the field is nullable, so "I did not say" stays distinguishable from "I said no". (F35 declined this refusal when the native field was a defaulting `bool`, where the two were the same value; that is what changed.)
- **`name` is not forwarded to any provider, and is documented rather than refused** — it is a required field of the variant, so refusing it would refuse every request. `rig-core` offers no response-format name on any typed seam: the OpenAI family derives `json_schema.name` from the *schema's own* `title` (falling back to `response_schema`), Anthropic's `OutputConfig` carries a schema and nothing else, and Gemini sets only `response_json_schema`. Moira does not smuggle `name` in by rewriting the schema's `title`: that mutates caller data to pass a value through a field meaning something else, and it would work on one provider family only, making the contract's truthfulness depend on routing. **If you want a name on the wire, put it in the schema's `title`** — that already works, wherever a name exists at all.

Both facts are pinned on the provider socket by `rig_0_40_still_hardcodes_strict_true_and_promotes_optional_properties_to_required` in `tests/openai_compat_text_format.rs`, which is also the reversal trigger: it reds when `rig-core` changes either one.

Any request whose `text.format` is honoured is a structured-output request, so it is subject to the application's `structured_output_enabled` policy and requires a model advertising the `structured_output` capability. A request that previously returned prose because `text.format` was discarded may therefore now return `422 structured_output_unsupported` or `model_capability_mismatch` instead. That is the intended outcome: the caller asked for a schema and Moira is now telling them whether it can deliver one.

Advertising the capability is **necessary but not sufficient** (ledger F39). The capability is a value on the provider-model row, and for some provider types `rig-core` will not put the schema on the wire whatever that row says — `DeepSeek` sets `SUPPORTS_RESPONSE_FORMAT = false` and its schema is discarded before the request is built. Routing therefore reconciles the advertised capability against the provider type and skips candidates that cannot receive a schema, so a structured request falls through to a provider that can, or fails with `no_eligible_model` if none is configured — exactly as it already behaves for a row that honestly declares `structured_output: false`. A row that claims the capability on such a provider is not merely ignored: it is treated as though it had never claimed it.

The converse case is not decidable here. `openai_compatible` and `local` providers do receive `response_format.json_schema` on the wire, but whether a self-hosted backend honours it is a property of that backend, which Moira has no way to check at admission time. Those provider types are therefore admitted, and a non-conforming reply surfaces the same way it does anywhere else — **since issue #80, as `422 structured_output_invalid` rather than as a `200` with `structured_output` absent.** A backend that ignores the schema and answers prose now fails the request instead of returning an empty-looking success. See `docs/release-notes.md`.

`POST /v1/responses` carries no route field, so every compat request resolves through the default route (`general`, seeded by migration `0005`). A provider must be bound to that route for the endpoint to serve anything.

No `/v1/chat/completions` route is registered in Phase 4.

