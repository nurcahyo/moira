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

## Conversations, memory, and RAG: what runs

`/api/v1/conversations`, `/api/v1/memories`, and the admin RAG endpoints under
`/api/v1/admin/rag-collections` and `/api/v1/admin/rag-documents` store and version
content, enforce policy, **and feed the model**:

- Ingested RAG documents are chunked, embedded, and indexed. `ingestion_status` reports
  that pipeline's real progress; a version that produced no chunks does not reach
  `indexed`.
- Conversation history, explicit memories, and retrieved RAG chunks are injected into the
  prompt on `POST /api/v1/responses` (and the compatibility route `POST /v1/responses`)
  when the request attaches a conversation. Memories and chunks that reach the prompt come
  back in `citations`; replayed history and the summary do not.
- Memories are extracted automatically from completed turns under the application's
  consent and extraction policy.
- Conversation summarization runs as of plan 11 Sub-Phase E, behind
  `application_conversation_policies.summarization_enabled`, which defaults to `false`.
  `POST /api/v1/conversations/{id}/summarize` produces a version on demand, and an
  application with the flag on summarizes automatically once a conversation's backlog
  crosses both `summary_trigger_tokens` and `minimum_messages_since_summary`. See
  `docs/conversation-summarization.md`.

**Everything above is opt-in, and that is the point of this section.**
`application_retrieval_policies.enabled`, `.memory_retrieval_enabled` and
`.rag_retrieval_enabled` default to `false`, retrieval also needs an embedding model
configured for the application, and summarization has its own default-`false` flag. Until
an operator turns them on, `citations` is `[]` and no retrieval runs — which is why an
empty array must be read as "nothing was retrieved" rather than as a contract guarantee.

Two capabilities are genuinely still absent: no OAuth/OIDC *client* runs inside Moira, and
the durable worker queue claims summarization and extraction jobs through a stub
dispatcher, so both run inline rather than being retried out of band.

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

Retrieval and memory intelligence landed in plan 11; see
[`docs/conversation-memory-rag-api.md`](./conversation-memory-rag-api.md) for the per-route
breakdown and [`docs/retrieval-citations.md`](./retrieval-citations.md) for what a citation
means.

Do not send real provider secrets, API keys, authorization headers, JWTs, or private documents in prompts while developing locally. Moira does not return provider credentials or raw key material from these routes.

