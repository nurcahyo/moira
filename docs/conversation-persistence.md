# Conversation Persistence

`conversation_content_persistence` accepts `none`, `metadata_only`, `plain_content`, and
`encrypted_content`. The column and its check constraint come from migration
`0007_conversations_memory_rag.sql`, which defaults an application to `plain_content`.

All four values are enforced end to end. `encrypted_content` was refused at the API until issue
[#139](https://github.com/nurcahyo/moira/issues/139) wired a cipher to the `*_encrypted` columns;
the refusal narrowed rather than disappeared — see below.

## What each value does

| value | message body | body length / token estimate | accepted by `PUT` policy |
|---|---|---|---|
| `plain_content` (default) | stored in `content_plain`, bounded | stored | yes |
| `metadata_only` | withheld | stored | yes |
| `none` | withheld | zeroed | yes |
| `encrypted_content` | **sealed** into `content_encrypted` | stored | yes, unless this deployment cannot seal |

`metadata_only` and `none` differ in exactly the length metadata; that is the only thing
separating them.

## Where the policy is enforced

Enforcement is at two writes, not at the callers, and both route the body through the
`ContentWrite` enum (`src/domain/conversation.rs`), which **replaces** the old
`content_plain: Option<String>` field rather than sitting beside it. Three consequences: a caller
cannot supply a plaintext and a ciphertext for one row; `Omitted` is a named state rather than a
`None` with a comment; and a fifth persistence value is a compile error at every write site
instead of a silently defaulted branch.

- **`add_message` in `src/infra/repositories/conversation.rs`.** This is the only path into
  `conversation_messages`, so a new writer inherits the policy rather than having to remember it.
  It reads the persistence value off the conversation row under the same `for update` lock that
  assigns the sequence number, re-derives the storage form with `ContentWrite::under_policy`, and
  zeroes the size and token columns unless `persists_content_metadata()`. Under
  `encrypted_content` the body is sealed **inside that transaction**.
- **`run_summarization` in `src/application/conversation.rs`, writing through
  `insert_conversation_summary`.** A summary is derived content but still a body of caller text,
  so it follows the same policy. `covers_through_sequence` and `summary_hash` are still written
  under every value: the run genuinely happened and genuinely covers that backlog. This second
  point is reachable through a policy tightened mid-conversation, where a readable plaintext
  backlog still exists.

Before this was wired, both sites carried comments asserting the policy was honoured while
`content_plain` was written unconditionally (finding F32).

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

None of this applies to `encrypted_content`: that value stores a body, so summarization,
extraction and history all keep working on content that is opened transparently on read.

Tightening the policy does not rewrite history. Rows written earlier under `plain_content`
keep their bodies; only writes after the change are withheld.

## `encrypted_content`

The body is sealed by `ContentCipher` (`src/security/content_envelope.rs`) under the content
keyring's **active** data key and written to `content_encrypted` /
`conversation_summaries.summary_text_encrypted`; the `*_plain` column is left null, and the CHECK
constraints from `migrations/0027_content_encryption_keyring.sql` make a row holding both a
database refusal. The AAD binds the row's own identity — message id, conversation id and sequence
number for a message; summary id, conversation id and coverage boundary for a summary — so a
ciphertext lifted into another tenant's conversation does not open.

Reads apply one precedence, defined once in
`pg_rows::conversation_content_from_row` and used by every reader:

```
(_,          Some(bytes)) => open(...)     // encrypted wins
(Some(text), None)        => plaintext
(None,       None)        => content absent
```

Four things that are easy to misread:

- **Counters are computed on the plaintext, before sealing.** `content_size_bytes`, `token_count`
  and the 262,144-byte content cap all measure what the caller sent. A ciphertext length never
  reaches a counter something else does arithmetic on, so flipping the policy does not move a
  limit or shift a metric.
- **Sealing and opening do no I/O.** Both take an already-unwrapped DEK out of the keyring
  snapshot, which is what makes it safe to call the cipher inside a write transaction and what
  keeps a future KMS custody from turning a 24-message history read into 24 network round trips.
  The cost is stated rather than hidden: a row sealed by another replica under a key minted after
  this process's last keyring refresh is unreadable here until the next refresh, and surfaces as
  `503 content_key_unavailable`.
- **Refusal, never fallback.** A write under this value with no usable content key returns
  `503 content_key_unavailable` and stores **nothing**. Writing plaintext under a policy named for
  encryption would be F32 with extra steps.
- **It is not retroactive in either direction.** Switching *to* `encrypted_content` does not
  encrypt existing history, and switching away does not decrypt it.

### The 422 narrowed; it did not disappear

`PUT` of the conversation policy with `conversation_content_persistence: "encrypted_content"` no
longer returns **422 `conversation_content_persistence_unsupported`** for the *value*. It returns
it when encryption is configured but **unusable at write time** — a deployment whose content
keyring is not loaded, where accepting the setting would mean refusing every subsequent message.

It is deliberately not removed: that would leave no write-time refusal for a key-custody failure,
which is a real and permanent condition. It is deliberately not made conditional on "is the
feature built" either, because a permanently-true branch is the never-taken code this project has
been bitten by. The refusals for `none` and `metadata_only` are storage policies and are
unaffected.

### Read-side errors

| code | status | condition |
|---|---|---|
| `content_key_unavailable` | 503 | no usable active key on write; or the envelope names a key this replica's snapshot does not carry |
| `content_key_abandoned` | 500 | the envelope names a key an operator abandoned — permanently unreadable |
| `content_envelope_unsupported` | 500 | the stored bytes are not a v1 envelope this build can read (bad magic, unknown version/algorithm/key mode, non-zero reserved, unknown profile, short blob, `body_len` mismatch) |
| `content_decryption_failed` | 500 | the AEAD tag did not verify |

The last two are split on purpose. Every framing failure is decided **before any key is touched**,
from bytes anyone holding the ciphertext can already read, so the log names the specific
discriminant. An AEAD failure gets one opaque code and one opaque log line (`aead_open_failed`),
because saying why a tag did not verify is an oracle. Both reach the caller with a constant
message carrying no key id, no discriminant and no fragment of the row.

## What else this policy governs, and what it does not

This setting is the application's **single** answer to "what do you keep of caller content", so it
reaches past conversation messages. All five `*_encrypted` columns are wired to it:

- **Memories obey it in full, since issue #140.** All four values apply — a memory written under
  `none` or `metadata_only` stores no body at all. `memory_records.content_hash` is retained under
  every value, and since issue #168 its form no longer follows this setting: it is a digest keyed
  by the keyring's `memory_dedupe` key under all four values. A memory body is short enough to
  guess, so an unkeyed digest of one was an oracle even on a row that stores no body. The cost —
  every deployment now depends on that key for dedupe — is stated in `docs/security.md`.
- **RAG bodies obey it on the sealing axis only, since issue #141.**
  `rag_document_versions.content_plain` and `rag_chunks.chunk_text_plain` move to their
  `*_encrypted` columns under `encrypted_content`; `none` and `metadata_only` store plaintext
  rather than omitting the body. That divergence is deliberate and is argued at
  `ContentWrite::under_policy_for_rag` — a chunk row with its body removed would still carry the
  document's verbatim section heading, the chunk offsets, an unkeyed hash and an embedding of the
  text, so suppressing the body alone would be a privacy claim that is not true.

Not covered at all:

- **Protected internal instructions** are never persisted in conversation messages.
- **Embeddings.** `memory_embeddings` and `rag_chunk_embeddings` are computed from the plaintext
  and stored unencrypted under every value of this setting. `docs/security.md` states the
  consequence plainly.
- **Rows written before the encryption release train.** Switching to `encrypted_content` does not
  seal existing history, and switching away does not unseal it.

## Stored metadata and deletion

Stored message metadata includes role, message type, response/execution link, sequence number,
content hash, size, token estimate, and safe metadata. Under `none` the size and token
estimate are zeroed; the hash and the structural fields remain. `content_hash` is retained under
every value, `encrypted_content` included: it is an HMAC under a deployment-held pepper, so it is
a fingerprint of content rather than content.

Deletion is soft by default and preserves audit metadata without message text in audit records.
