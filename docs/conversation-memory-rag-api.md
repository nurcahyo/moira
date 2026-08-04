# Conversation Memory RAG API

Public APIs:

- conversations and messages under `/api/v1/conversations`
- explicit memories under `/api/v1/memories`
- response conversation attachment under `/api/v1/responses`

Admin APIs:

- conversation, memory, retrieval, and embedding policy endpoints under application resources
- RAG collection and document endpoints under `/api/v1/admin`

OpenAPI includes schemas for these resources and omits embeddings, extraction prompts, protected instructions, and parser internals.

## What these routes feed

Plan 11 wired the pipeline these routes configure. Stored content now reaches the model:
`POST /api/v1/responses` plans context for the attached conversation, retrieves memories and
RAG chunks, injects them into the prompt, and returns the provenance in `citations`. See
[`docs/retrieval-citations.md`](./retrieval-citations.md) and
[`docs/context-planning.md`](./context-planning.md).

**Retrieval is off until an operator turns it on.**
`application_retrieval_policies.enabled`, `.memory_retrieval_enabled` and
`.rag_retrieval_enabled` all default to `false`, and an application with no embedding model
configured cannot retrieve at all. An application that has changed none of these behaves
exactly as it did before plan 11.

| Route group | Does today | Does not do today |
| --- | --- | --- |
| `/api/v1/conversations`, `/api/v1/conversations/{id}/messages` | Store conversation and message rows; attach a conversation to a response; replay bounded history into the prompt; summarize the conversation on demand via `POST /api/v1/conversations/{id}/summarize` | — |
| `/api/v1/memories` | Store explicit memory records; embed them; retrieve and inject them under policy; cite them | — |
| Conversation and memory policy endpoints | Store policy configuration **and gate live behavior**: history replay, memory retrieval, automatic memory extraction, and summarization (`summarization_enabled`, the trigger thresholds, and `history_strategy`, which decides whether the active summary is injected) | — |
| Retrieval and embedding policy endpoints | Store policy configuration **and gate live behavior**: `enabled` switches retrieval on for the application, and the embedding policy selects the model that embeds both the corpus and the query | — |
| `/api/v1/admin/rag-collections`, `/api/v1/admin/rag-documents` (create/ingest/reindex) | Store and version document content; chunk, embed, and index it; report real progress in `ingestion_status`; replay `Idempotency-Key` | — |

## Summarization

Plan 11 Sub-Phase E gave `conversation_summaries` its writer. Summarization is off by default
(`summarization_enabled` defaults to `false`); with it on, a new immutable summary version is
produced automatically after an assistant turn crosses both configured thresholds, or on demand
through `POST /api/v1/conversations/{id}/summarize`. The context planner injects the active summary
unless `history_strategy` is `recent_messages`. See
[`docs/conversation-summarization.md`](./conversation-summarization.md).

Idempotency behavior for these routes is documented in
[`docs/idempotency.md`](./idempotency.md).

