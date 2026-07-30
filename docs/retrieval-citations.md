# Retrieval Citations

`PublicResponse.citations` is populated from the `context_plans` row written during the request.
Plan 11 Sub-Phase G.

## What a citation means

One entry per memory or RAG chunk that **actually reached the assembled context**. Not per
candidate: a candidate the budget dropped is not cited, because a citation for content the model
never saw is a fabricated provenance claim and worse than no citation. `assemble_context` builds
the message list and the citation list in the same pass, so the two cannot disagree.

| Field | RAG chunk | Memory |
|---|---|---|
| `id` | `rag_chunks.public_id` | `memory_records.public_id` |
| `type` | `"rag_chunk"` | `"memory"` |
| `document_id` | `rag_documents.public_id` | `null` |
| `memory_id` | `null` | `memory_records.public_id` |
| `title` | `rag_documents.title` | `memory_records.memory_key` |
| `section` | `rag_chunks.section_title` | the memory type |

## No spans, deliberately

`PublicCitation` has no character or token offset fields and plan 11 does not add any. Moira
tracks provenance at chunk granularity and nothing finer, so a span would be invented.
`citations_carry_no_span_fields_because_none_are_tracked` pins the exact field set, so adding one
becomes a deliberate act.

## Where citations are and are not returned

Returned on the `POST /api/v1/responses` result, from the plan computed earlier in that same
request. Deliberately **not** re-read from `context_plans` later: re-querying would open a window
where a concurrent plan for a different execution could be resolved onto this response.

A later `GET /api/v1/responses/{id}` returns `citations: []`. That is honest rather than a gap —
resolving provenance there would be a second, unauthorised read path over another request's plan.
An operator-facing diagnostic surface over `context_plans` / `retrieval_runs` is specified in the
plan and is **not implemented yet**.

An empty retrieval serialises as `[]`, never `null`.
