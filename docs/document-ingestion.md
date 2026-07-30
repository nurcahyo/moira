# Document Ingestion

Phase 5 supports bounded synchronous direct-text document metadata and version creation.

Supported source types:

- `direct_text`
- `metadata_only`

Supported MIME types:

- `text/plain`
- `text/markdown`
- `application/json`

Remote URL ingestion, PDF parsing, crawling, OCR, and distributed ingestion queues remain out of scope for this implementation pass.

## Ingestion status

Each RAG document response includes `ingestion_status`, reporting the status of the document's
current version. Since plan 11 wave 1 there is a real pipeline behind it, and the status is
**derived from what the pipeline produced** — never bound as a literal:

| Situation | `ingestion_status` |
|---|---|
| No version row exists yet | `null` |
| Content stored but it chunks to nothing (empty or whitespace) | `pending` |
| Chunks written, `rag_embeddings_enabled = false` (the default) | `indexed` |
| Chunks written and every chunk embedded | `indexed` |
| Chunks written, embeddings requested but unconfigured or failing | `failed` |
| Superseded by a later version that had reached `indexed` | `superseded` |

Documents ingested before the 02a release recorded a false `indexed` status; migration
`0009_backfill_false_indexed_ingestion_status.sql` reset those rows to `pending`, so no stored
value claims indexing that never happened. Those rows are re-ingestible and pick up the pipeline
on their next `/ingest` or `/reindex`.

## Ingestion entry points

There are **three**, and all three run the same pipeline. A pipeline wired into only one of them
would leave the others writing chunk-less versions forever:

- `POST /api/v1/admin/rag-collections/{collection_id}/documents` with inline `content`
- `POST /api/v1/admin/rag-documents/{id}/ingest`
- `POST /api/v1/admin/rag-documents/{id}/reindex` (a literal alias of `/ingest`, sharing one
  idempotency operation identity)

## Ordering, and what it costs

Chunking, hashing and embedding all happen **before** the command transaction opens. The
transaction only writes. This is deliberate: `ingest_rag_document_with_connection` runs inside
the `AdminCommandRunner` transaction holding `select … for update` on the document row plus the
idempotency advisory lock, and embedding is a network call — doing it there would pin a pooled
connection and both locks across an unbounded await, once per batch.

Two accepted consequences:

- Two concurrent requests carrying the same `Idempotency-Key` both embed, and only one wins the
  envelope. That wastes provider spend; it corrupts nothing, because the loser's chunks are never
  written.
- `422 rag_document_too_large` is raised before the key is claimed, matching every other
  validation on this surface — a rejected request must never occupy an idempotency key.

The `'chunking'` and `'embedding'` values of the check constraint are **not written** on this
path. Everything happens in one transaction, so no other session could observe them, and a
sequence of updates nobody can read is the appearance of honesty rather than honesty. They become
real, and must be written, when ingestion spans more than one transaction — which is what
remote-URL ingestion (`'downloading'`/`'parsing'`) and worker-resumed ingestion will need.

## Failure handling

An embedding failure does **not** fail the HTTP request. The version is persisted at `'failed'`
with the reason in `rag_ingestion_runs.failure_class` (`embedding_not_configured`,
`embedding_dimension_unsupported`, or `embedding_failed`), and the chunks that were successfully
produced are kept — they are real rows describing real content, and discarding them would lose
the only work that completed. The document remains re-ingestible.

## Observability

One `rag_ingestion_runs` row per version that ran the pipeline, carrying `chunk_count`,
`embedded_chunk_count`, `status` and `failure_class`. A version with no content records no run.

Metrics (label-free or closed-label; per-application diagnostics live in `rag_ingestion_runs`
rows, never in metric labels): `moira_rag_chunks_written_total`,
`moira_rag_embeddings_written_total`, `moira_rag_ingestion_runs_total{outcome}`, and the
histogram `moira_embedding_batch_latency_seconds`.

