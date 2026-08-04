# Request And Response Mapping

Public request mapping turns `PublicResponseRequest` into `ExecutionCommand`.

- `X-Request-Id` becomes `ExecutionCommand.request_id`.
- `resp_<uuid>` and `exec_<uuid>` are generated with UUIDv7.
- Caller identity maps into `CallerRuntimeIdentity`.
- Consumer-bound applications become `application_id`.
- JWT subject and mapped external user/tenant IDs are propagated without storing raw claims.
- `route`, `provider`, `model`, and `credential_id` are treated as overrides and require both policy allowance and scope.
- `response_format: json_object` is refused with **422 `unsupported_request_option`** (ledger F46). It used to map to a generic object schema, which `rig-core` completes to `{"type":"object","properties":{},"additionalProperties":false,"required":[]}` under `strict: true` — a schema satisfied only by `{}`, so a caller asking for free-form JSON received the empty object with a `200` and a `succeeded` status. `rig-core` 0.40 has no free-form JSON mode on any provider (`"json_object"` does not occur in the crate), and Anthropic's API has none at all, so there is nothing to translate it into. Send `json_schema` instead. The variant remains in the contract so the refusal can name it.
- `response_format: json_schema` maps the provided schema into execution options.

Responses expose route/model references, usage summaries, status, request ID, timestamps, and safe failure classes. They do not expose provider request headers, raw upstream errors, prompts, plaintext credentials, API key hashes, or ciphertext.

