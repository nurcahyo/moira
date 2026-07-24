# Memory Correction And Deletion

Public memory PATCH can update content, importance, validity, status, and metadata where policy and scopes allow.

Content updates change the content hash. Embedding supersession is modeled in `memory_embeddings`; automatic re-embedding remains a TODO.

DELETE tombstones a memory with `status=deleted` and `deleted_at`.

