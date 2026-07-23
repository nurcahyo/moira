# Document Chunking

The schema includes `rag_chunks` with stable public chunk IDs, chunk hashes, offsets, section titles, and metadata.

Chunking strategy abstraction is documented but not yet wired to ingestion. Direct-text ingestion currently stores immutable document versions without generated chunks.

Future chunking must preserve UTF-8 boundaries and enforce configured chunk count and size limits.

