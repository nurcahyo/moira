# Request And Response Mapping

Public request mapping turns `PublicResponseRequest` into `ExecutionCommand`.

- `X-Request-Id` becomes `ExecutionCommand.request_id`.
- `resp_<uuid>` and `exec_<uuid>` are generated with UUIDv7.
- Caller identity maps into `CallerRuntimeIdentity`.
- Consumer-bound applications become `application_id`.
- JWT subject and mapped external user/tenant IDs are propagated without storing raw claims.
- `route`, `provider`, `model`, and `credential_id` are treated as overrides and require both policy allowance and scope.
- `response_format: json_object` maps to a generic object schema.
- `response_format: json_schema` maps the provided schema into execution options.

Responses expose route/model references, usage summaries, status, request ID, timestamps, and safe failure classes. They do not expose provider request headers, raw upstream errors, prompts, plaintext credentials, API key hashes, or ciphertext.

When a response includes human-readable text, it must use the i18n contract: `message_key` for translation lookup, `message` for the English fallback, and `message_args` for interpolation values.
