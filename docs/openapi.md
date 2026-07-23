# OpenAPI

Moira generates an OpenAPI 3.1 document from its annotated Axum handlers and serves it at `/openapi.json`. Scalar renders the same document at `/docs`.

Route registration uses `utoipa_axum::OpenApiRouter`, so the HTTP method, path, handler, generated operation, and referenced schemas are registered together. The contract covers health, readiness, Prometheus metrics, documentation delivery, native responses and SSE, execution and usage history, discovery, OpenAI compatibility, conversations, messages, memory, policies, RAG, and every admin operation.

When `MOIRA_DOCS__EXPOSE_ADMIN=false`, `/api/v1/admin/**` paths are removed from the returned document. When enabled, the complete document is returned only after the caller authenticates and is authorized for `moira:admin`.

The document defines bearer JWT, `X-Moira-System-Key`, and `X-Consumer-Key` security schemes. Operations describe their actual request bodies, path and query parameters, success statuses, typed errors, content types, and supported `ETag`, `If-Match`, `Idempotency-Key`, and `X-Request-Id` headers.

Plaintext provider secrets, API-key hashes, ciphertext, nonces, peppers, decrypted JWT material, embedding vectors, extraction prompts, protected instructions, and parser internals remain excluded. API-key create and rotate responses document the once-only `secret` field. `/v1/chat/completions` remains intentionally unregistered.

When changing an endpoint, follow `.agents/skills/moira-openapi/SKILL.md` and run the standard Rust checks. Contract tests verify path and method coverage, unique operation IDs, schema references, public admin filtering, security schemes, parameters, statuses, and streaming/metrics content types.
