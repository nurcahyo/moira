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

## Query plans (plan 11 Sub-Phase C) — NOT YET MEASURED

The two candidate queries shipped by plan 11 wave 2 are in
`src/infra/repositories/conversation.rs` (`find_memory_candidates`,
`find_rag_chunk_candidates`). Both combine equality filters on application/tenant/user (and, for
chunks, joins through `rag_collections` / `rag_document_versions` / `rag_documents`) with an
`order by embedding <=> $1::vector` and a `limit`.

**No `EXPLAIN ANALYZE` at realistic scale has been run.** The plan lists that as a deliverable;
it is not done, and this section exists to say so rather than to imply otherwise. What is known:

- The HNSW indexes are partial (`where superseded_at is null and embedding is not null`), and
  both queries carry those predicates, so the indexes are at least applicable.
- pgvector's HNSW does not pre-filter. Under a selective equality filter the planner may prefer a
  sequential scan, or may over-fetch from the index and discard. Which one it picks at 100k+ rows
  across multiple applications is exactly what has not been measured.
- The over-fetch multiplier (`CANDIDATE_OVERFETCH = 4`) makes the `limit` larger than the policy
  cap, which pushes in the direction of more index work per query.

Until this is measured, treat retrieval latency at scale as unknown. `moira_retrieval_latency_seconds`
is the histogram to watch; its buckets are sized for a response-path operation (5 ms to 30 s).
