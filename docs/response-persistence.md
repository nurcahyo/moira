# Response Persistence

Phase 4 creates a metadata-only public response record.

Stored:

- response id
- execution id
- request id
- application and external identity bindings
- route, provider, and model references
- status and safe failure fields
- usage summary
- caller metadata after validation
- retention timestamp

Not stored:

- prompt body
- assistant output body
- provider request/response headers
- raw provider response body
- raw authorization headers
- provider credentials or API keys

The policy enum includes `none`, `metadata_only`, `encrypted_content`, and `plain_content`, but the implemented Phase 4 path always returns `output_persisted:false` and does not persist content bodies. Plain content persistence should not be enabled for production.

## What `GET /api/v1/responses/{response_id}` returns instead of the output

A **completed** response read back later never carries an empty `output` array. An empty array
there would be indistinguishable from a model that returned nothing, which is a legitimate and
very different result — an empty completion is served as `output_text` with `text: ""`. Instead
the read returns one `output_unavailable` part whose `reason` names the persistence mode
recorded on the response row at the moment it completed, not the application's policy today:

| recorded `persistence_mode` | `reason` |
|---|---|
| `metadata_only` | `metadata_only_persistence` |
| `none` | `persistence_disabled` |
| `plain_content`, `encrypted_content` | `content_persistence_not_implemented` |

`reason` is an unconstrained string in the OpenAPI schema and these values may grow. Do not
branch on it for control flow; it exists to explain a missing body to a human.

### Asymmetry with conversation content persistence — deliberate, and tracked

The two policies now answer an unimplementable mode differently, and the difference is worth
naming so neither side is mistaken for an oversight:

- The **conversation** policy now *implements* `encrypted_content` (issues #139, #140, #141): the
  message, summary, memory, RAG document and RAG chunk bodies are sealed into the five
  `*_encrypted` columns. Its **422 `conversation_content_persistence_unsupported`** narrowed to
  the one case that is still unhonourable — encryption configured but unusable at write time. See
  `docs/conversation-persistence.md`.
- The **execution** policy's `ResponsePersistenceMode` still *accepts* both
  `encrypted_content` and `plain_content` even though neither is implemented. F40 chose to
  explain the gap at read time instead — the `content_persistence_not_implemented` string in
  the table above — rather than reject the value at write time.

**The remaining gap on the response side is a schema gap, not a cipher gap — and it is wider than
the encrypted half.** The reasoning this section used to give, "no cipher is wired to the
`*_encrypted` columns", was true when F40 was written and is false now: the
envelope-encryption release train wired all five of them.

What is true, and what actually blocks `ResponsePersistenceMode`, is that
`migrations/0006_public_execution_api.sql` creates `responses` **with no output body column of
either form** — no `output_text_plain`, no `output_text_encrypted`. What the table stores of a
result is `output_summary jsonb`, `usage_summary jsonb`, `failure_class`, `failure_message` and
the `output_persisted` boolean; the only later migrations to touch it add `conversation_id`
(`0007`) and `updated_at` (`0008`). So **`plain_content` has nowhere to write either**, and the
five sealed columns all belong to `conversation_messages`, `conversation_summaries`,
`memory_records`, `rag_document_versions` and `rag_chunks` — none of them to `responses`.

Implementing either mode therefore needs new DDL, not a cipher, which is why
`docs/decision-encryption-at-rest.md` §14 resolves
[#103](https://github.com/nurcahyo/moira/issues/103) toward *refuse with a 422, symmetric with the
conversation side* rather than *implement*. The arm in `src/application/public.rs` stays untouched
until that decision is taken.

The explanatory string is honest about the outcome, but it is a weaker guarantee than a
refusal: an operator can still set a response persistence mode whose name promises a body that
will never be stored.

Responses that are queued, in progress, failed or cancelled return `output: []`, because they
genuinely produced no output and `status` already says which.

