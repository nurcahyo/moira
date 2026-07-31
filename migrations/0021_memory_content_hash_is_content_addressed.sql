-- Finding F14 — `memory_records.content_hash` becomes a content address.
--
-- # What changed above this file
--
-- `src/application/conversation.rs` used to write this column with
-- `IdempotencyHasher::hash`, producing `"{pepper_version}:{base64url(hmac_sha256(pepper, …))}"`.
-- That hasher verifies only against the *currently active* pepper, and justifies that in its
-- own module doc with a retention argument — every `idempotency_records` row expires within 24
-- hours, so old-pepper rows age out on their own. `memory_records` has no retention: nullable
-- `valid_until`, a `status` that stays `'active'` indefinitely. So a pepper rotation did not
-- open a bounded window here, it permanently orphaned every stored hash, and exact-match memory
-- dedupe would stop matching with no error and no log line.
--
-- It is now `crate::security::request_hash` — a plain, unkeyed SHA-256 (an alias for
-- `secret_fingerprint`) rendered as base64url with no padding, i.e. exactly
-- `rtrim(translate(encode(sha256(bytes), 'base64'), '+/', '-_'), '=')`. 43 characters, no `:`.
--
-- `conversation_messages.content_hash` is deliberately NOT touched by this migration and stays
-- peppered: it is returned on `ConversationMessageRecord`, so an unkeyed digest of message
-- content would hand every holder an offline verifier for content the schema expects to be able
-- to hold encrypted. The admitting rule is applied per table, not to "content_hash" as one thing.
--
-- # Why re-hash rather than accept a one-time dedupe reset
--
-- The reset would be nearly free today — plan 11 Sub-Phase F is deferred, so no read path
-- compares this column yet, and a stale value costs nothing while nothing reads it. It is still
-- the wrong choice, for one reason: leaving the old values in place makes the column hold two
-- incomparable formats forever, and the first dedupe reader to ship would silently miss every
-- pre-F14 row. That is F14's exact failure mode, re-created from a format split instead of a
-- pepper rotation, and arriving later — when there is more data and no one remembers this file.
--
-- The recompute is exact: the digest is unkeyed, so the database has everything it needs.
--
-- # What this migration cannot recompute, and why that is safe
--
-- Only rows whose plaintext is in `content_plain`. `memory_records.content_encrypted bytea`
-- exists in `migrations/0007` for a future content-persistence policy and has **no writer
-- anywhere in the tree today** (verified by grep: the identifier appears only in this schema,
-- in plan/skill prose, and never in a Rust statement) — so every existing row has
-- `content_plain`. If that ever changes, the ciphertext is not the content and the key is not
-- in the database, so no SQL migration could recompute those rows at all; the recompute is
-- restricted rather than allowed to silently hash the wrong bytes.
--
-- Rows left behind keep their `"v1:"`-prefixed value, and that is self-describing rather than
-- merely stale: a content address produced by `request_hash` is base64url and can never contain
-- `:`. So a dedupe reader comparing `request_hash(content) = content_hash` misses such a row —
-- a miss, never a false match — and the marker is inherent to the encoding rather than a
-- convention someone has to remember.
--
-- # Reversal condition
--
-- If `memory_records.content_hash` ever becomes reachable across a trust boundary — returned on
-- a caller-visible response, accepted as a caller-supplied lookup key, or compared without an
-- `application_id` predicate — this decision reverses: the column goes back to a keyed hash and
-- needs a re-hash-on-rotation procedure, because the lifetime problem that motivated this file
-- does not go away.

update memory_records
set content_hash = rtrim(
        translate(encode(sha256(convert_to(content_plain, 'UTF8')), 'base64'), '+/', '-_'),
        '='
    )
where content_plain is not null
  and content_hash like '%:%';
