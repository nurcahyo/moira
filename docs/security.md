# Security

Production security expectations:

- no plaintext provider secrets in HTTP responses, audit metadata, traces, metrics, or logs
- no raw API keys after create or rotate responses
- no prompt, memory, document, embedding, or retrieved-context content in metrics or logs
- deny-by-default authorization scopes
- HTTPS-only provider/JWKS/image URLs unless explicit local development opt-in is enabled
- outbound URLs supplied by callers or admins pass the shared SSRF guard in
  `src/security/ssrf.rs` before use (see "Outbound URL guard" below)
- non-root container runtime
- read-only root filesystem
- dependency, container, secret, SAST, DAST, and ASVS checks in CI/CD

## Outbound URL guard

One module, `src/security/ssrf.rs`, decides whether Moira may use an outbound URL. It
classifies the address space — loopback, RFC1918, link-local (which covers every cloud
metadata endpoint), CGNAT, unique-local, and the IPv4-in-IPv6 encodings of all of them —
resolves hostnames through the OS resolver under an explicit budget, and requires **every**
resolved address to be permitted, not merely the first.

Two callers share it, and they differ in one way that matters:

| | JWKS URL | Public image URL |
|---|---|---|
| Configured by | an admin | any caller, in a request body |
| Who performs the fetch | Moira | the provider |
| Response controls (content type, byte cap, redirect refusal) | enforced by Moira | **not available** |
| Egress allow-list | not needed | `public_api.image_urls.allowed_hosts` |

Because Moira hands the image URL to the provider rather than fetching it, the guard on
that path is *admission control*, not a fetch policy. The controls that police a response
cannot apply, and the provider resolves DNS again when it connects — so a hostname whose
answer changes between validation and use is not fully closed by validation alone. The
egress allow-list is the control that does not depend on resolution timing, and a
deployment that knows which origins its images come from should set it.

The dev escape hatches (`auth.jwks.allow_insecure_dev_urls`,
`public_api.image_urls.allow_insecure_dev_urls`) are deliberately separate, so loosening
one trust surface for local development does not silently loosen the other. Production
start-up refuses to come up while either is true.

## Content encryption at rest

Five columns hold caller content sealed with AES-256-GCM. The key hierarchy, the format and the
reasoning are recorded in `docs/decision-encryption-at-rest.md`; this section is what an operator
needs to run it and what a reader needs in order not to over-read it.

Sealing is selected per application by `conversation_content_persistence = 'encrypted_content'`
on `application_conversation_policies`. Nothing is sealed by default.

### Which columns are sealed, and what stays in the clear

All five are wired for new writes. **All five also carry permanent plaintext history**, and that
is a steady state rather than a migration in progress.

| table | sealed column | AAD binds | plaintext history | read path |
|---|---|---|---|---|
| `conversation_messages` | `content_encrypted` | message id, conversation id, sequence number | every row written before the release train, and every row written under any other policy | opened on read |
| `conversation_summaries` | `summary_text_encrypted` | summary id, conversation id, covered sequence | same | opened on read |
| `memory_records` | `content_encrypted` | memory id, application id, memory scope | same | opened on read |
| `rag_document_versions` | `content_encrypted` | version id, document id, version number | same | **no reader exists** |
| `rag_chunks` | `chunk_text_encrypted` | chunk id, document version id, chunk index | same | opened on retrieval |

Three things follow that are easy to misread:

- **Switching an application to `encrypted_content` does not encrypt its history.** Existing rows
  keep their storage form. There is no backfill in this release; `seal-existing` is designed and
  deferred. Nullness is the discriminator, and a CHECK constraint per table
  (`migrations/0027_content_encryption_keyring.sql`) guarantees a row never holds both forms, so
  a partially-sealed table stays unambiguous forever.
- **Switching away does not decrypt it either.** The policy governs subsequent writes only.
- **`rag_document_versions.content_encrypted` has no reader today.** The chunker consumes the
  in-memory ingestion plan and `rag_document_record_from_row` does not project the column, so
  sealing it costs nothing on the read path right now. That column can reach roughly 2 MB — 2,000
  chunks × 1,000 characters, bounded by the 2 MiB admin body limit — so **if** a read path is ever
  added it will decrypt up to 2 MB per call. Cheap now; not cheap forever.

RAG bodies obey the policy on the **sealing axis only**: `encrypted_content` seals them, and
`none` and `metadata_only` store plaintext rather than omitting the body. That asymmetry is
deliberate and is argued at `ContentWrite::under_policy_for_rag` — a `rag_chunks` row omitting its
body would still carry the document's verbatim section heading, the chunk's exact offsets and
token count, an unkeyed content hash, and an embedding of the text. Suppressing the body alone
would be a privacy claim that is not true, which is worse than not making one.

### The three rotation verbs, and the fourth thing that is not one

They all sound like "rotation" and they are not interchangeable. An operator picking the wrong one
under pressure is the main risk this design carries, so each is named, and what it touches is
stated rather than implied.

| verb | what it changes | user rows read | user rows rewritten | downtime |
|---|---|---|---|---|
| **`add` + `promote`** (R1, data-key rotation) | mints a new DEK and makes it the one new writes seal under | none | **none** | none |
| **`rewrap`** (R2, master-key rotation) | re-wraps the handful of `content_data_keys` rows under a new master key | none | **none** | none, but four ordered steps |
| **`reseal`** (R4) | re-encrypts existing rows onto a newer DEK | **all of them** | **all of them** | none, but expensive |
| **custody swap** (R3) | changes *where the master key comes from* | none | **none** | a rolling restart |

- **`add` then `promote`** is the routine one. Rows written under the old key stay under it and
  stay readable forever — `retiring` means "not writable, still loaded". Nothing is re-encrypted,
  no config changes, no restart. It runs automatically on the cluster lease once the active key is
  older than `content_encryption.data_key_rotation_days` (default 30).
- **`rewrap`** is master-key rotation and touches **no user data at all**. Its four steps are
  ordered and the ordering is enforced, not documented-and-hoped: add the new master key to
  `MOIRA_CONTENT_ENCRYPTION__KEYS` and restart; run `moira keyring rewrap --to <id>`; promote it
  by setting `ACTIVE_KEY_ID` and restart; drop the old key and restart. `rewrap` refuses if any
  keyring row names a master key the process does not hold, and **boot refuses on the same
  condition** — so skipping step 2 fails loudly at step 4 instead of losing data quietly.
- **`reseal`** is the only verb that rewrites user rows, and it is the only way to move a data key
  from `retiring` to genuinely `retired`. It is resumable and idempotent by construction
  (compare-and-swap per row, so a row a concurrent writer rewrote is skipped rather than
  clobbered). It is **not** part of the rollout and nothing requires it until a data key must
  genuinely retire — which may be never: `memory_records` has no retention, so its rows can
  reference the first key indefinitely. Master keys retire in minutes; data keys retire only when
  their rows die or are resealed.
- **The custody swap is not a rotation.** If the new backend serves the same bytes under the same
  key id — environment → Vault KV, an external-secrets operator, a CSI driver — change
  `MOIRA_CONTENT_ENCRYPTION__CUSTODY` and restart. **Zero writes, zero re-encryption**, proven at
  boot by unwrapping every key before the listener binds. It works only because the backend name
  is deliberately absent from the wrapped-key AAD. If the new backend will not release key bytes
  at all (AWS KMS), it is not a swap — it is `rewrap` with a different target.

`moira keyring status` prints the current state, per-key seal counts, and each key's check value
(a truncated HMAC of a fixed label, safe to print and safe to compare between operators). Read it
before choosing a verb.

One verb is a confession rather than an operation: `moira keyring abandon <id> --confirm --reason
"<text>"` marks a key abandoned so a service whose master key is permanently lost can start again.
Rows under that key then return a distinct typed refusal; every other key reads normally. It is a
status, never a delete — the ciphertext stays in case the key resurfaces — and it makes those rows
permanently unreadable on purpose.

### The nonce budget, as arithmetic rather than folklore

Nonces are **random 96-bit values from a CSPRNG**, fresh per row, never derived and never a
counter. Randomness rather than a counter is what makes a per-generation DEK safe across
concurrent writers on multiple replicas, and it is what puts a ceiling on how many rows one DEK
may seal.

NIST SP 800-38D §8.3 caps a single key at **2³² invocations** when the IV is chosen at random,
which keeps the probability of a repeated nonce below 2⁻³². A repeated nonce under one key is a
catastrophic GCM failure, not a degraded one, so the cap is a hard budget and not a guideline.

Written out:

```
2^32 invocations                     = 4,294,967,296 sealed rows per data key
data_key_rotation_days = 30          =     2,592,000 seconds
4,294,967,296 / 2,592,000            ≈         1,657 sealed rows per second
```

So the default time-bounded rotation reaches the cap only at roughly **1,657 sealed rows per
second, sustained for thirty days without pause**. That is far above what this system writes, and
the number is written down here so that a future deployment which does approach it can see that it
has, rather than rediscover the bound.

Two operational notes, because time-bounded rotation is only a proxy for a count:

- `moira keyring status` prints **per-key seal counts**, which is the quantity the cap is actually
  about. If a deployment ever ran hot enough to matter, that is where it would show.
- The guard against a rotation lease-holder that has wedged is the **boot WARN on key age**: an
  instance whose active data key is older than `data_key_rotation_days` logs
  `"the active content data key is older than the configured rotation interval; rotation may be
  wedged"` with the key id and its age in days. A wedged scheduler is therefore visible rather
  than silent.

### Format versions and rolling deploys — read support ships one release ahead of write support

Every envelope carries a `format_version`, and a build that does not recognise one **refuses the
row**. That refusal is correct: guessing at an unknown layout is how a downgrade oracle gets
built. But during a rolling deploy, old and new pods serve the same database, so a new pod writing
a `v2` blob that an old pod correctly refuses is a **read outage for newly written rows** — not a
corruption, and not something a rollback fixes, because the rows are already there.

Therefore, without exception:

> **Read support for a new `format_version` ships one full release before write support.**
> Release *N* teaches every pod to read `v2` and writes only `v1`. Release *N+1* starts writing
> `v2`, by which time no pod is left that cannot read it.

The same rule applies to `algorithm_id` and `key_mode`, which are validated in the same
pre-decryption header check. It is written down now, while the reason is fresh, because the day
this is needed is the day it is easiest to skip.

### What this does **not** protect

The protected surface is stated above. The unprotected surface is stated here at the same volume,
because a release note that says "content is encrypted at rest" without the following sentences is
a false claim.

**Vectors derived from the content are not encrypted.** `memory_embeddings` and
`rag_chunk_embeddings` hold dense vectors computed from the **plaintext** and stored in the clear,
in the same database, under no key at all. Embedding-inversion attacks recover substantial source
text from such vectors. Sealing `memory_records.content_encrypted` and
`rag_chunks.chunk_text_encrypted` while leaving their embeddings in the clear therefore raises the
cost of a database dump; it does not reduce it to nothing. Fixing this is out of scope; disclosing
it is not.

> **"Content is encrypted at rest" must never be said without "vectors derived from it are not."**

**Every row written before this release train stays plaintext, permanently.** There is no
backfill. An application switched to `encrypted_content` today has its entire history in the clear
and its future writes sealed, and both states are visible in the same table at the same time.
`seal-existing` is designed and deferred; until it exists, the sentence above is the honest
description of what an operator gets.

Also not protected, and each for its own recorded reason:

- **Length and shape.** `content_size_bytes`, `token_count`, chunk offsets and `section_title`
  are computed on the plaintext and stored in the clear. `section_title` in particular is a
  verbatim substring of the document — the nearest preceding Markdown heading.
- **`rag_document_versions.content_hash` and `rag_chunks.chunk_hash`** are unkeyed digests. A
  whole document or a thousand-character chunk is not a guessable-plaintext oracle, so this is a
  decision rather than an oversight; see the next section for the per-table reasoning, including
  the two columns where it goes the other way.
- **Caller-supplied `metadata`** is the caller's own JSON, not something derived from the body,
  and is retained under every policy value.
- **Provider credentials** are not on this key hierarchy. They use `LocalSecretCipher` and
  `EncryptedSecret`; whether they move onto `MasterKeyCustody` is an open decision, not a
  completed one.
- **Full-text search over sealed rows is foreclosed.** Nothing indexes `content_plain` today so
  nothing is lost now, but any future keyword search over sealed history is impossible, with no
  backfill that can undo it.

Finally, the two operational risks that are not cryptographic:

- **A rollback past the write-path release is a data-visibility rollback.** An older build selects
  `content_plain`, sees NULL, and renders the content as *absent* — silently, for every row
  written under `encrypted_content` while the newer build was live. There is no feature flag that
  softens this; the mitigation is knowing the blast radius before upgrading.
- **The master key is required in production unconditionally**, even where no application uses
  `encrypted_content` today, because the policy is flippable at runtime. A deployment that
  upgrades without setting `MOIRA_CONTENT_ENCRYPTION__KEYS` does not boot.

## Why the four `content_hash` columns are not hashed the same way

Four tables carry a digest of caller content, and they use three different constructions. The
asymmetry is deliberate, it is decided **per table**, and it is written down here because an
asymmetry with no recorded reason gets "fixed" by the next reader.

The question each column has to answer is not "is this content secret" but **who holds the
digest, and how guessable is the thing it digests**. A digest is an offline verifier: whoever
holds it can test a guess for free. That is harmless when guessing is hopeless and fatal when the
plaintext comes from a short list.

| Column | Construction | Why |
|---|---|---|
| `conversation_messages.content_hash` | **peppered** — `IdempotencyHasher`, `"{pepper_version}:{base64url}"` | It is **returned to callers** on `ConversationMessageRecord`. An unkeyed digest handed out over the API is an offline verifier for content the schema expects to be able to hold encrypted, and every holder gets one. Migration `0021` deliberately left it alone for exactly this reason. |
| `memory_records.content_hash`, rows stored **encrypted** | **keyed** — `"d1:" + base64url(HMAC-SHA256(K_dedupe, content))` | Memory bodies are short, low-entropy and highly guessable ("user prefers dark mode", "user's timezone is Asia/Jakarta"). Sealing the body while leaving an unkeyed digest of that same body **in the same row** defeats the encryption completely: a database dump plus a wordlist recovers it without touching a key. |
| `memory_records.content_hash`, rows stored **plain** or **omitted** | **unkeyed** — `request_hash`, a bare SHA-256 as base64url | Nothing is protected by hiding the digest of a body that is already in the clear in the adjacent column. And keying it would re-create finding F14: `memory_records` has no retention, so a key change orphans every stored digest permanently and exact-match dedupe stops matching with no error and no log line. |
| `rag_document_versions.content_hash`, `rag_chunks.chunk_hash` | **unkeyed** | A whole document, or a thousand-character chunk, is not a guessable-plaintext oracle. There is no wordlist for it. These are also write-only fingerprints today — nothing recomputes and compares them. They now sit **beside a sealed body** (issue #141), which is the condition that flipped the memory column to a keyed digest; the entropy argument is what makes the answer different here, and it is the thing to re-check if chunking ever produces short, formulaic chunks. |

Two consequences worth stating plainly:

- **The two memory forms can only miss, never falsely match.** `d1:` contains a `:`, and a
  `request_hash` value is unpadded base64url over a fixed 32-byte digest — alphabet `A-Za-z0-9-_`,
  43 characters, no `:`, which is migration `0021`'s own rule. An application that switches its
  persistence policy therefore accumulates at most one duplicate per re-stated memory across the
  boundary; it never dedupes a sealed memory onto an unsealed one or the reverse.
- **The dedupe key rides master-key rotation.** It is a `content_data_keys` row with
  `purpose = 'memory_dedupe'`, wrapped by the master key like every content key. Rotating the
  master key re-wraps the envelope; the 32 bytes inside are unchanged, so every stored
  `content_hash` stays byte-identical and every dedupe lookup keeps working. That is precisely
  the property a deployment pepper cannot offer, and it is why F14's objection to a keyed hash
  here does not apply. **The dedupe key itself is not rotated.** If it ever must be, the
  consequence is a documented one-time loss of cross-era dedupe — duplicates, not errors — and
  nothing will report it.

One residual is recorded rather than hidden: under `none` and `metadata_only` no body is stored
but the unkeyed digest still is, so those rows remain a guessing oracle for the memory content
they no longer hold. Closing it means keying those arms too, which costs the F14 property above;
it is a separate decision and has not been taken.

## How long a verification key survives its issuer

A JWKS document is cached for five minutes. When a refresh fails, the last-known-good copy
keeps answering, so a transient outage at the identity provider does not break
authentication — but only up to `auth.jwks.max_stale_seconds` (default 24 hours) measured
from the last *successful* fetch. Past that ceiling the entry is evicted and authentication
against that issuer fails closed.

That bound is what keeps key rotation and key revocation meaningful. Without it, an issuer
that stays unreachable leaves its keys verifying tokens for as long as the process lives, so
retiring a signing key at the provider would have no effect here. Two operational
consequences worth planning for:

- an identity provider that is unreachable for longer than the ceiling will start failing
  authentication, by design — raise `max_stale_seconds` only with that trade in view;
- a successful fetch restarts the clock, so an issuer that merely refreshes slowly never
  approaches the ceiling.

The ceiling is validated at startup, in every environment. It must be greater than zero and
at least the five-minute freshness window: below that it would be a bound the cache never
consults, because a still-fresh entry is served without reference to it, and a setting that
silently means less than it says is worse than no setting. Both cases refuse to start rather
than run on a ceiling that does not hold.

Environment variables remain supported for development. Production should use Vault, AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, Kubernetes Secrets, or an external-secrets operator.
