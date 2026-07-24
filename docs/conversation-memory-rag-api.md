# Conversation Memory RAG API

Public APIs:

- conversations and messages under `/api/v1/conversations`
- explicit memories under `/api/v1/memories`
- response conversation attachment under `/api/v1/responses`

Admin APIs:

- conversation, memory, retrieval, and embedding policy endpoints under application resources
- RAG collection and document endpoints under `/api/v1/admin`

OpenAPI includes schemas for these resources and omits embeddings, extraction prompts, protected instructions, and parser internals.

