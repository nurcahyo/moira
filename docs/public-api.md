# Public API

Phase 4 adds the public execution surface on top of the Phase 3 execution kernel. Public routes authenticate callers, validate inputs, resolve application execution policy, route through the existing runtime service, emit audit records, and expose metadata-only response/execution resources.

## Routes

- `POST /api/v1/responses`
- `POST /api/v1/responses/stream`
- `GET /api/v1/responses/{response_id}`
- `GET /api/v1/executions/{execution_id}`
- `GET /api/v1/executions`
- `GET /api/v1/usage`
- `GET /api/v1/models`
- `GET /api/v1/routes`
- `GET /api/v1/capabilities`

The only optional compatibility route is `POST /v1/responses`, gated by `public_api.openai_responses_compat_enabled`. `/v1/chat/completions` is intentionally not registered.

All JSON responses include `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and propagated `X-Request-Id`.

## MVP boundary: conversations, memory, and RAG

`/api/v1/conversations`, `/api/v1/memories`, and the admin RAG endpoints under
`/api/v1/admin/rag-collections` and `/api/v1/admin/rag-documents` are **persistence
and configuration primitives only** in this release. They store and version content
durably and enforce policy, but:

- No retrieval, chunking, or embedding pipeline runs. `ingestion_status` on a RAG
  document reflects storage, not indexing for retrieval.
- Conversation history, explicit memories, and RAG documents are not loaded into the
  prompt sent to a provider. `POST /v1/responses` always returns `citations: []`.
- No summarization runs; `conversation_summaries` is never populated.

The RAG create/ingest/reindex routes under `/api/v1/admin/rag-collections` and
`/api/v1/admin/rag-documents` now replay under `Idempotency-Key`, on the same
atomic admin-command machinery as the admin command routes (claim, mutation-in-savepoint,
finalize, and audit committing in one PostgreSQL transaction, serialized by an advisory
lock); see `docs/idempotency.md`. `POST /v1/responses` also supports `Idempotency-Key`,
but through a separate, non-transactional claim/replay scheme keyed on a distinct
actor-fingerprint formula, with no advisory lock and a different in-progress error code
(`execution_in_progress` rather than `idempotency_in_progress`) — see `docs/idempotency.md`
for the distinction. Conversation and memory create routes do not declare
`Idempotency-Key` and do not replay.

Full retrieval/memory intelligence is tracked separately and is not part of this MVP.

Do not send real provider secrets, API keys, authorization headers, JWTs, or private documents in prompts while developing locally. Moira does not return provider credentials or raw key material from these routes.

