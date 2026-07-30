# pgvector

Moira enables the `vector` extension in the first migration.

Phase 5 adds vector columns for:

- `memory_embeddings.embedding`
- `rag_chunk_embeddings.embedding`

Indexes use HNSW with cosine operators for active, non-null embeddings. Embedding model and
dimension are recorded alongside vectors to prevent silent incompatible reuse.

## Rust binding — decision

**Moira adds no `pgvector` crate.** Vectors are bound as `text` in pgvector's documented input
format (`[0.5,-1.25,…]`) and cast in SQL with `$n::vector`. The encoder is
`crate::orchestration::encode_vector_literal`.

Reasons, in order of weight:

1. A new dependency on this repository is a reviewable supply-chain decision — it moves
   `Cargo.lock`, `deny.toml` and `tests/supply_chain_policy.rs` — and at this stage it would buy
   only the binary wire format.
2. Nothing writes *and* reads a vector in Rust. Vectors are written once and compared inside
   PostgreSQL by the `<=>` operator; the value never crosses back over the boundary, so no
   decoder is needed.
3. `f32::to_string` prints the shortest representation that parses back to the same value, so the
   text encoding is lossless. `vector_literals_round_trip_through_text` asserts this.

Non-finite components have no pgvector representation and would be rejected by the server with an
opaque error, so they are normalised to `0` at the encoder rather than at the database.

**Reversal condition:** adopt the `pgvector` crate if either (a) retrieval benchmarking shows
text encoding is a material share of write or query latency, or (b) any code path needs to read a
stored vector back into Rust, at which point hand-rolling a parser would be strictly worse than
taking the dependency.

## Query plans

Not yet measured. Nothing in the ingestion pipeline issues a vector *query* — the `<=>` operator
appears in no SQL Moira runs today. The `EXPLAIN ANALYZE` work on whether the planner selects
`memory_embeddings_active_hnsw_idx` / `rag_chunk_embeddings_active_hnsw_idx` under the added
`application_id`/tenant/user equality filters belongs with the retrieval service that issues
those queries, and is recorded here when it exists rather than guessed now.
