# Embeddings

Embedding policy is application scoped (`application_embedding_policies`):

- provider (`embedding_provider_id`)
- model (`embedding_model_id`)
- dimension (`embedding_dimension`)
- batch size (`batch_size`, default 32)
- timeout (`timeout_ms`, default 60000)
- memory/RAG enablement (`memory_embeddings_enabled`, `rag_embeddings_enabled`, both default
  `false`)
- failure behavior

The schema records embedding model and dimension for memory and chunk embeddings. Embedding
vectors are never returned publicly.

## Rig integration (plan 11 Sub-Phase B)

Implemented in `src/orchestration/embedding.rs`, the embedding twin of
`src/orchestration/runtime_factory.rs`. See [`rig-integration.md`](./rig-integration.md#embeddings)
for the verified rig-core 0.40 surface and which providers expose an embedding model.

The wrapper:

- builds a Rig embedding client from the resolved provider, model and credential, reusing
  `resolve_runtime_credential` so embeddings and completions cannot end up with different
  credential precedence for the same provider;
- batches inputs by `batch_size`, clamped to the provider's own `MAX_DOCUMENTS`;
- bounds the **whole** run — not each batch — by `timeout_ms`, so a caller-visible timeout is
  not multiplied by the batch count;
- refuses a response with the wrong number of vectors or the wrong width rather than padding or
  zip-truncating it, because a mis-aligned embedding is a corrupt index entry that degrades
  retrieval invisibly;
- sanitises provider failures. The provider's response body is never propagated: it can echo the
  request, and an embedding request body is document content.

## Dimension

Exactly one width is supported: **1536**, matching the `vector(1536)` columns. A policy that
declares a different `embedding_dimension` is not silently coerced — its documents ingest to
`ingestion_status = 'failed'` with `rag_ingestion_runs.failure_class =
'embedding_dimension_unsupported'`.

## Mock coverage, stated honestly

There is no live embedding credential in this repository. The end-to-end tests run against
`tests/support/mock_openai.rs`, which serves `/v1/embeddings` with deterministic vectors derived
from the input text. What that proves is the **pipeline**: policy resolution, provider and
credential resolution, batching, per-chunk vector alignment, persistence, and status derivation.

What it does not and cannot prove, and would need a live provider: real vector semantics (that
semantically similar text yields nearby vectors), real provider rate limiting and quota
behaviour, and any model dimension other than 1536.

### Wave 2 addition: hand-chosen vectors

`EmbeddingBehaviour::Fixed` maps an exact input string to a caller-supplied vector, and
`planar_vector(angle)` builds unit vectors in the plane of the first two basis axes.

This exists because the default `mock_embedding_for` is deterministic but *pseudo-random*: two
different texts are near-orthogonal by construction and their cosine similarity is an accident of
the hash. That is fine for "was a vector stored" and useless for "does this row outrank that
one" — which is exactly the question the cross-tenant isolation suite has to ask. With
`planar_vector`, two candidates have cosine similarity `cos(a - b)` exactly, so an assertion like
"the other tenant's row is a strictly closer match and is still not returned" is a fact about the
SQL rather than a hope about a hash.

It does not change what the mock can prove about semantics. It makes the *distances* arithmetic,
not the *meanings* real.
