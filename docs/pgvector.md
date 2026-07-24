# pgvector

Moira enables the `vector` extension in the first migration.

Phase 5 adds vector columns for:

- `memory_embeddings.embedding`
- `rag_chunk_embeddings.embedding`

Indexes use HNSW with cosine operators for active, non-null embeddings. Embedding model and dimension are recorded alongside vectors to prevent silent incompatible reuse.

