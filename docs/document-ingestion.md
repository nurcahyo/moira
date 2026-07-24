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

Each RAG document response includes `ingestion_status`, reporting the status of the document's current version: `null` when no version row exists yet, and `pending` once content has been stored, since no chunking/embedding/indexing pipeline has run against it (see [`docs/public-api.md`](./public-api.md#mvp-boundary-conversations-memory-and-rag)). Documents ingested before this release recorded a false `indexed` status; migration `0009_backfill_false_indexed_ingestion_status.sql` resets those rows to `pending`, so no stored value claims indexing that never happened.

