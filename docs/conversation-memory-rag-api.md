# Conversation Memory RAG API

Public APIs:

- conversations and messages under `/api/v1/conversations`
- explicit memories under `/api/v1/memories`
- response conversation attachment under `/api/v1/responses`

Admin APIs:

- conversation, memory, retrieval, and embedding policy endpoints under application resources
- RAG collection and document endpoints under `/api/v1/admin`

OpenAPI includes schemas for these resources and omits embeddings, extraction prompts, protected instructions, and parser internals.

## MVP boundary

These are persistence and configuration primitives only. See
[`docs/public-api.md`](./public-api.md#mvp-boundary-conversations-memory-and-rag) for
the full statement of what does not run yet (retrieval, chunking, embeddings, context
injection, and summarization).

| Route group | Does today | Does not do today |
| --- | --- | --- |
| `/api/v1/conversations`, `/api/v1/conversations/{id}/messages` | Store conversation and message rows; attach a conversation to a response | Load history into the prompt sent to a provider |
| `/api/v1/memories` | Store explicit memory records | Extract memories automatically; embed or retrieve memories; inject memories into a prompt |
| Conversation and memory policy endpoints | Store policy configuration, and genuinely gate the conversation and memory routes with it | Enforce summarization or extraction behavior that does not exist yet |
| Retrieval and embedding policy endpoints | Store and return policy configuration | Affect any behavior at all — nothing reads these policies yet, because the retrieval and embedding pipelines they configure do not exist |
| `/api/v1/admin/rag-collections`, `/api/v1/admin/rag-documents` (create/ingest/reindex) | Store and version document content; set `ingestion_status`; replay `Idempotency-Key` | Chunk, embed, or index content for retrieval |

Idempotency behavior for these routes is documented in
[`docs/idempotency.md`](./idempotency.md).

