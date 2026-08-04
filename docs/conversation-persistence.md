# Conversation Persistence

`conversation_content_persistence` accepts `none`, `metadata_only`, `plain_content`, and
`encrypted_content`. The column and its check constraint come from migration
`0007_conversations_memory_rag.sql`, which defaults an application to `plain_content`.

Three of those four values are enforced end to end. The fourth is refused at the API.

## What each value does

| value | message body | body length / token estimate | accepted by `PUT` policy |
|---|---|---|---|
| `plain_content` (default) | stored, bounded | stored | yes |
| `metadata_only` | withheld | stored | yes |
| `none` | withheld | zeroed | yes |
| `encrypted_content` | withheld (fails closed) | withheld | **no — 422** |

`metadata_only` and `none` differ in exactly the length metadata; that is the only thing
separating them.

## Where the policy is enforced

Enforcement is at two writes, not at the callers:

- **`insert_conversation_message` in `src/infra/repositories/conversation.rs`.** This is the
  only path into `conversation_messages`, so a new writer inherits the policy rather than
  having to remember it. It reads the persistence value off the conversation row under the
  same `for update` lock that assigns the sequence number, then nulls `content_plain` unless
  `persists_plaintext()`, and zeroes the size and token columns unless
  `persists_content_metadata()`.
- **`run_summarization` in `src/application/conversation.rs`.** A summary is derived content
  but still a body of caller text, so the summary body is written only when the policy admits
  plaintext. `covers_through_sequence` and `summary_hash` are still written: the run genuinely
  happened and genuinely covers that backlog. This second point is reachable through a policy
  tightened mid-conversation, where a readable plaintext backlog still exists.

Before this was wired, both sites carried comments asserting the policy was honoured while
`content_plain` was written unconditionally.

## The consequences under `none` and `metadata_only`

Withholding message content is not a storage detail; it makes the conversation stateless by
design. Everything downstream reads the same null:

- **Message reads return `null` content.** There is no body to return.
- **Summarization refuses with `409 summarization_not_needed`**, `details.reason` =
  `no_persisted_content`. Advancing the coverage boundary over messages that were never
  summarised would be worse than refusing.
- **Memory extraction stops** before any model call: the transcript it would build is empty,
  so there is nothing to extract from. This is a consequence of the null content, not a second
  guard — if extraction ever needs to be gated on the policy directly, read the policy there
  and say so.
- **Cross-turn history is not rebuilt.** The context planner skips any history message whose
  content is null, so a later turn is planned as if the earlier ones carried no text.

Tightening the policy does not rewrite history. Rows written earlier under `plain_content`
keep their bodies; only writes after the change are withheld.

## `encrypted_content` is refused

`PUT` of the conversation policy with `conversation_content_persistence: "encrypted_content"`
returns **422 `conversation_content_persistence_unsupported`**. The `*_encrypted` content
columns exist on three tables and have no writer anywhere in `src/`, so accepting the value
would tell an operator with a PII or data-residency obligation that their content is encrypted
at rest when it is not.

It fails closed on both sides: a row that already holds `encrypted_content` keeps parsing and
stores no plaintext, so an existing deployment is made safer rather than broken, while no new
deployment can select the value.

**Reversal condition:** delete this refusal — the `is_enforceable()` check in
`put_conversation_policy` — the moment a cipher is wired to the `content_encrypted` columns.
At that point `persists_plaintext()` must stay false for `encrypted_content` and a
`persists_ciphertext()` arm joins it. Nothing else has to change.

## Not covered by this policy

- **RAG document versions.** Direct text supplied to a RAG document is stored in
  `rag_document_versions.content_plain` regardless of `conversation_content_persistence`;
  that policy governs conversation content only.
- **Protected internal instructions** are never persisted in conversation messages.

## Stored metadata and deletion

Stored message metadata includes role, message type, response/execution link, sequence number,
content hash, size, token estimate, and safe metadata. Under `none` the size and token
estimate are zeroed; the hash and the structural fields remain.

Deletion is soft by default and preserves audit metadata without message text in audit records.
