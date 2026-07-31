# Document Chunking

Implemented in `src/orchestration/chunking.rs` (plan 11 Sub-Phase A). Pure text transformation:
no database access, no provider calls, no clock.

## Strategies

| Strategy | Behaviour |
|---|---|
| `paragraph` | Splits on blank lines. A paragraph longer than the chunk ceiling is windowed. |
| `markdown` | As `paragraph`, plus ATX headings (`#` … `######`) start a new chunk and name the `section_title` of every chunk beneath them. |
| `fixed_window` | A sliding character window with overlap, ignoring structure. |

The strategy is selected from the document's declared MIME type: `text/markdown`,
`text/x-markdown` and `application/markdown` get `markdown`; everything else gets `paragraph`.
Content with no blank line at all is still windowed, never returned as one chunk.

The strategies are deliberately **character**-based, not token-based. Moira has no tokenizer —
`estimate_tokens` is a divide-by-four approximation — and calling a character window a token
window would make the limits read as a guarantee they are not. `rag_chunks.token_count` is
stored as that same estimate and is labelled as one.

## UTF-8 safety

Every boundary is a `char` boundary, produced by `str::char_indices`; there is no raw byte
slicing anywhere in the module. `start_offset`/`end_offset` are byte offsets into the original
content, and `content[start..end] == chunk.text` is asserted directly by
`every_chunk_offset_pair_slices_back_to_its_own_text` for all three strategies over content
containing emoji and accented characters.

## Determinism and `chunk_hash`

The same `(content, strategy, limits)` triple always produces the same chunks, byte for byte.
`rag_chunks.chunk_hash` is `crate::security::request_hash` — a plain, unkeyed SHA-256 — over the
exact chunk text bytes, so identical text re-ingested produces an identical hash indefinitely.

It is deliberately **not** `IdempotencyHasher::hash`, which most of the neighbouring
`content_hash` columns still use. That hasher is peppered and version-prefixed and verifies only
against the active pepper, so a pepper rotation would invalidate every stored `chunk_hash` and
force every document to re-chunk and re-embed. A chunk hash must be content-addressable
indefinitely; that is its whole job. The reversal condition is recorded on
`PreparedChunk::chunk_hash` in `src/orchestration/ingestion.rs`: move to a keyed hash if
`chunk_hash` ever becomes reachable across a trust boundary.

Finding F14 applied that same admitting rule per *table* rather than to `content_hash` as one
thing, and `memory_records.content_hash` moved to `request_hash` for the same reason (it is not
caller-visible, is never a caller-supplied lookup key, and is only ever compared within one
application — see `memory_content_hash` in `src/application/conversation.rs`).
`conversation_messages.content_hash` stays peppered because it fails the first clause outright:
it is returned on `ConversationMessageRecord`.

## Limits

Deployment-wide, in `Settings.rag`, because they are resource ceilings protecting the database
and the embedding provider rather than a product policy an application should raise for itself:

- `rag.max_chunk_chars` — default `1000`.
- `rag.max_chunks_per_document` — default `2000`. Validated below `i32::MAX` at startup, because
  `rag_chunks.chunk_index` is an `integer`.

Exceeding the document ceiling returns `422 rag_document_too_large`. It is **refused, never
truncated**: a truncated document produces a retrieval index that is quietly incomplete.
