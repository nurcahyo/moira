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
| `/api/v1/conversations`, `/api/v1/conversations/{id}/messages` | Store conversation and message rows; attach a conversation to a response; replay bounded history into the prompt | Summarize a conversation past its budget |
| `/api/v1/memories` | Store explicit memory records; embed them; retrieve and inject them under policy; cite them | — |
| Conversation and memory policy endpoints | Store policy configuration and gate history replay, memory retrieval, and automatic memory extraction with it | Enforce summarization, which does not exist yet |
| Retrieval and embedding policy endpoints | Store policy configuration **and gate live behavior**: `enabled` switches retrieval on for the application, and the embedding policy selects the model that embeds both the corpus and the query | — |
| `/api/v1/admin/rag-collections`, `/api/v1/admin/rag-documents` (create/ingest/reindex) | Store and version document content; chunk, embed, and index it; report real progress in `ingestion_status`; replay `Idempotency-Key` | — |

## The one thing that still does not run

Conversation summarization (plan 11 Sub-Phase E). `conversation_summaries` has no writer,
so a conversation past its configured budget is truncated rather than summarized. The
context planner already reads a summary when one exists, so this is a missing producer, not
a missing consumer.

Idempotency behavior for these routes is documented in
[`docs/idempotency.md`](./idempotency.md).

