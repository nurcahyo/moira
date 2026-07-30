# Retrieval Ranking

Implemented in `src/orchestration/retrieval.rs` (scoring, pure) and
`src/infra/repositories/conversation.rs` (candidate SQL). Plan 11 Sub-Phase C.

## Recall is semantic; the weights are a re-rank

**This is the most important thing to know about the current implementation, and it is a real
limitation rather than a detail.**

The candidate set is whatever the vector query returns for the query embedding, over-fetched by
`CANDIDATE_OVERFETCH` (4x the policy's result cap, hard-capped at 512 rows).
`semantic_weight` / `keyword_weight` / `recency_weight` / `importance_weight` from
`application_retrieval_policies` are then applied as a **re-rank over that candidate set**.

Consequence: **a chunk or memory that matches the query lexically but not semantically is not
retrieved at all.** There is no independent keyword recall arm.

Why not a real `plainto_tsquery` union: `rag_chunks.chunk_text_plain` and
`memory_records.content_plain` are both nullable — the content-persistence policy can store only
the `*_encrypted` variant — and neither has a full-text index. A `to_tsvector(...)` term in the
retrieval query would be an unindexed expression over the whole table, evaluated on every
response, for a column that is frequently null.

Add a GIN index and a genuine keyword arm when either (a) retrieval-quality measurement shows
lexical-only matches are being missed in practice, or (b) the persistence policy guarantees
plaintext. At that point `lexical_overlap_score` should be deleted, not kept alongside it.

## The score

Each component is on `[0, 1]`:

| Component | Source | Definition |
|---|---|---|
| `semantic` | pgvector `<=>` cosine distance | `(2 - distance) / 2` — distance 0 gives 1.0, orthogonal gives 0.5, opposite gives 0.0 |
| `keyword` | `lexical_overlap_score` | fraction of the query's **distinct** terms present in the candidate text; 0.0 when the text is not readable |
| `recency` | row age | `1 / (1 + age_weeks)` — halves each week, never reaches zero |
| `importance` | `memory_records.importance` | as stored. **Absent for RAG chunks**, which carry no importance column |

The blend is the weight-normalised mean of the components that are *present*:
`sum(w_i * s_i) / sum(w_i)`. An absent component drops out of both numerator and denominator, so
a chunk is not penalised against a memory for lacking a signal it cannot have. All weights zero
falls back to the pure semantic score, because "all weights zero" is a misconfiguration and
returning zero for everything would look identical to an empty corpus.

## Thresholds and caps

- `minimum_memory_score` / `minimum_chunk_score` are **exclusive**: a candidate scoring exactly
  the minimum is excluded. With the default 0.5 and pure semantic weighting that means "strictly
  better than orthogonal".
- `maximum_memory_results` / `maximum_chunk_results` cap the returned set.
- `maximum_chunks_per_document` caps per document when `diversity_enabled`. Diversity is applied
  **before** the global cap, so de-duplicating a dominant document still fills the cap.
- Ties break on `public_id`. Without that, two equal scores order by whatever the vector scan
  emitted, and the `context_plans` row — and therefore the response's citations — would be
  non-deterministic for identical input.

## Provenance

Every retrieval writes one `retrieval_runs` row: candidate counts, returned counts, latency,
status and failure class. The candidate counts are **in-scope only**; leaking an out-of-scope
count is an inference channel even when no content is returned, and
`tests/retrieval_cross_tenant_isolation.rs` asserts it.

There are no per-application metric labels. `moira_retrieval_runs_total{outcome}` and
`moira_retrieval_latency_seconds` carry no tenant, application or collection dimension — see the
cardinality rule in `src/infra/metrics.rs`. `retrieval_runs` is the per-application surface.
