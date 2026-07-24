-- Plan 02a (finding P0-1): both RAG version write paths hardcoded
-- ingestion_status = 'indexed' regardless of what actually happened, and nothing ever
-- happened -- rag_chunks and rag_chunk_embeddings have no writer anywhere in the
-- codebase. Every existing 'indexed' row therefore records chunking, embedding and
-- indexing work that was never performed.
--
-- The write paths are fixed to record 'pending'. This backfill corrects the rows
-- already on disk, because leaving them would keep the API reporting the exact false
-- value this plan exists to remove, with no way for a caller to tell a legacy row from
-- a genuinely indexed one. Per the audit, 'indexed' was never true here, so there is no
-- legitimate prior state to preserve.
--
-- Rows already marked 'superseded' are deliberately left alone: that value describes
-- being replaced by a newer version, which did genuinely happen, and superseded_at
-- remains the authoritative signal either way.
--
-- Rollback: this migration is data-only and reversible only approximately.
--   update rag_document_versions set ingestion_status = 'indexed'
--   where ingestion_status = 'pending';
-- That inverse is lossy -- after this ships, honest 'pending' rows are indistinguishable
-- from rows this migration touched, so running it would re-assert the false claim on new
-- data too. Prefer reverting the code and leaving the data honest.

update rag_document_versions
set ingestion_status = 'pending'
where ingestion_status = 'indexed';
