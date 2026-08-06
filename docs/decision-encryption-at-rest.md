# Decision — envelope encryption at rest for the five content columns (F33 / issue #86)

**Status:** decided by the maintainer, 2026-08-06. Binding. Not yet implemented — the
implementation is issued as a numbered sequence of pull requests, listed in §14.

**Decided by:** the repository maintainer, on a written comparison of three options. This document
is the record of that decision and of the reasoning behind it. It is not a proposal and it is not
awaiting sign-off.

**Supersedes nothing. Retires two standing claims:** the reversal condition recorded at
`src/domain/conversation.rs` ("no cipher is wired to the `content_encrypted` columns"), and
migration `0021`'s statement that `memory_records.content_encrypted` has "no writer anywhere in the
tree today". Both become false when PR 5 and PR 6 land, and both are named in the PRs that falsify
them.

---

## Why this file, and not `docs/decisions-taken.md` or `plans/CONVENTIONS.md` §0

This repository has two existing decision homes and this decision belongs to neither, so it gets a
sibling file rather than a new directory or a new format.

- [`docs/decisions-taken.md`](./decisions-taken.md) records decisions **plan runners took
  unilaterally**, each awaiting a human signature that has not arrived. Its title says so and its
  header insists on it. Filing a maintainer-made, already-approved decision in that table would
  contradict the one property that file exists to preserve.
- [`plans/CONVENTIONS.md` §0](../plans/CONVENTIONS.md) records **product-owner decisions
  (RESOLVED — do not reopen)** as a table of one-row entries, D1–D7, binding on the iteration plans
  `02a`–`11`. This decision is maintainer-made and resolved, which is the right class — but it is
  not scoped to those plans, and it does not fit in a table row. D7 already shows the strain: it
  needed a prose section under the table to carry its consequences.

So: the *provenance* is CONVENTIONS §0's (a maintainer decided; do not reopen), the *shape* is
`decisions-taken.md`'s (question, decision, reasoning, evidence, reversal condition), and the file
lives in `docs/` next to it. A pointer from `decisions-taken.md` closes the loop for a reader who
starts there.

There is no `docs/adr/` directory in this repository and this file does not create one. If a second
architecture decision is ever written, moving both into a series is a `git mv` plus two link
updates; inventing the series for a population of one is not worth the churn.

**Verification.** Every claim about the tree below was checked against `181ed3b`
(`origin/develop`, 2026-08-06) before it was written here. Line numbers drift; the identifiers and
file paths are the durable part, and that is what is cited wherever a choice existed.

---

## 1. The problem, stated once

`migrations/0007_conversations_memory_rag.sql` creates five columns:

| table | column |
|---|---|
| `conversation_messages` | `content_encrypted` |
| `conversation_summaries` | `summary_text_encrypted` |
| `memory_records` | `content_encrypted` |
| `rag_document_versions` | `content_encrypted` |
| `rag_chunks` | `chunk_text_encrypted` |

**Nothing in `src/` writes or reads any of them.** The only occurrences of those identifiers in the
crate are comments explaining that they are unused. There is no cipher for content, no key store, no
rotation path. The schema advertises a capability the binary does not have — finding **F33**, split
out of F32 on 2026-08-02 and escalated to a human as issue
[#86](https://github.com/nurcahyo/moira/issues/86) precisely because it is a scoping question about
key custody and rotation rather than an implementation gap.

The acute half of the harm was already removed. `put_conversation_policy` refuses
`encrypted_content` with `422 conversation_content_persistence_unsupported`, so no operator can be
told their content is encrypted while it is stored in the clear. What remains is the schema itself,
which will read to the next person as a partially-built feature, and the fact that Moira has no
answer for an operator with a PII or data-residency obligation beyond "use disk encryption".

---

## 2. The three options, and why two lost

### Option 1 — drop the five columns (**rejected**)

Delete them in a migration and state plainly that Moira relies on storage-layer encryption
(disk/volume) rather than application-layer. This was the cheapest honest answer and F33's own text
offered it as one of two.

**Why it lost.** Storage-layer encryption defends exactly one threat: someone walks off with the
disk. It defends nothing against a database dump taken by anyone with `select` — a backup pipeline,
a read replica, a support engineer's `\copy`, a compromised application-level credential, a logical
replication subscriber. That is the threat an operator asking for content encryption is asking
about, and answering it with full-disk encryption is answering a different question. Dropping the
columns would also make the answer permanent-by-default: re-adding five `bytea` columns to five
populated tables later is a bigger migration than filling the ones that already exist.

The option is not absurd, and it would have been the right call if nobody needed the capability.
The maintainer decided somebody does.

### Option 2 — build it, bound to AWS KMS (or with Vault as a hard runtime dependency) (**rejected**)

Use KMS as the key manager directly: a KMS-encrypted data key per row or per generation, `Encrypt`
and `Decrypt` calls made inline, the AWS SDK as a required dependency. Or the same shape with
HashiCorp Vault Transit.

**Why it lost, and this is the reason the whole design has the shape it has.** Binding to one key
manager makes the key manager a property of the *stored data*, not of the *deployment*. Moira is
self-hostable; a deployment that is not on AWS should not carry the AWS SDK, and a deployment that
has no Vault should not be required to stand one up before it can boot. More importantly, the
maintainer's stated requirement was:

> KMS or Vault must be swappable later **without re-encrypting stored data** — only the way the
> master key is obtained should change.

That sentence is the non-negotiable, and it is a constraint on the *data format*, not on the code.
It is satisfied only if the number of things the master key directly protects is small and fixed,
and if none of the five `*_encrypted` columns records anything about which backend was in use. Any
design that fails it fails it invisibly: it still "works", it just makes the migration you were
promised would be free cost a full-table rewrite the day you need it.

### Option 3 — envelope encryption, key custody behind a pluggable interface, first implementation reading the master key from the environment (**CHOSEN**)

A small keyring of **data-encryption keys (DEKs)** lives in a new database table, each DEK stored
**wrapped** by a **master key** that is obtained through a `MasterKeyCustody` trait. The first and
only implementation of that trait reads keys from the environment. AWS KMS and Vault Transit are
later implementations of the same trait, added without touching the format, the tables, or the
call sites.

Rotating the master key, or swapping environment → KMS → Vault, rewraps a handful of keyring rows in
one transaction and **touches zero bytes of any `*_encrypted` column**. That is the non-negotiable,
executed literally, and §12 asserts it as a test rather than arguing it in prose.

---

## 3. What was decided *inside* option 3, and what was rejected there

Three concrete designs were written against option 3 and compared. The chosen one is a
key-hierarchy design (a database-held keyring of versioned data keys wrapped by a master key from a
pluggable custody trait), with deliberate grafts from the other two. Recording the losing arguments
matters more than recording the winner, because these are the choices a future reader will be
tempted to undo.

**Rejected: a data key per row.** Each row gets its own DEK, wrapped by the master key and stored
beside the ciphertext. This is the shape most textbooks describe, and it has the right custody
surface (`wrap`/`unwrap`). It was rejected because it makes the number of wrapped keys equal to the
number of rows: swapping environment → KMS then means unwrapping and re-wrapping one key **per
message, per memory, per chunk**, with one KMS `Encrypt` call each. It is true that no content
ciphertext is re-encrypted; it is also irrelevant, because it is still a full rewrite of every
encrypted row plus one network call per row. That is exactly the migration the pluggable shape was
chosen to avoid. It also puts a network round-trip on the read path — a turn that loads 24 messages,
a summary, 10 memories and 8 chunks becomes 43 unwraps — and costs ~60 bytes of wrapped-key material
per row.

**Rejected: shipping only two of the five columns now and the rest later.** A design that sealed
`conversation_messages` and `conversation_summaries` and left `memory_records`, `rag_chunks` and
`rag_document_versions` in plaintext, secured by a documented promise and a drift test. It was
honest about the gap — memories are distilled *from* the very messages it sealed, and are often more
sensitive per byte — and it was still rejected, for the reason it stated itself: promises rot. A
partially-encrypted schema left behind by design is the same defect as F33 wearing a different hat.
**All five columns ship in one release train.** The slicing survives as PR *ordering*, not as
release scope.

**Rejected: a small integer key version in the ciphertext header.** A `u16` version is 14 bytes
cheaper per row than a UUID and caps rotation at 65,535 keys — a cap correctable only by a format
version bump, i.e. by the one change this design is built to make expensive. On a ~1 KB row, 14
bytes is not a trade worth making, and a UUID means a blob restored from `pg_dump` into another
deployment still names its own key.

**Grafted in, deliberately:** the `abandon` escape hatch (§10), the omission of the custody backend
name from the wrapped-key AAD (§4), the magic prefix and explicit body length (§6), the
compare-and-swap reseal loop (§9), and the wrapped HMAC key that resolves the `content_hash` oracle
(§11). Each is justified where it appears.

---

## 4. Key custody — the pluggable seam

New module, `src/security/key_custody.rs`. Sketch, not the final signature:

```rust
#[async_trait::async_trait]
pub trait MasterKeyCustody: Send + Sync + std::fmt::Debug {
    fn backend_name(&self) -> &'static str;      // "environment" | "aws_kms" | "vault_transit"
    fn active_master_key_id(&self) -> &str;
    fn can_unwrap(&self, master_key_id: &str) -> bool;

    async fn wrap(&self, dek: &Zeroizing<[u8; 32]>, aad: &[u8])
        -> Result<WrappedKey, KeyCustodyError>;
    async fn unwrap(&self, wrapped: &WrappedKey, aad: &[u8])
        -> Result<Zeroizing<[u8; 32]>, KeyCustodyError>;

    /// Boot probe. Must prove the backend is *usable*, not merely configured.
    async fn preflight(&self) -> Result<(), KeyCustodyError>;
}
```

`WrappedKey` carries `master_key_id`, `wrap_algorithm`, a nonce (empty for backends that carry their
own framing), and the wrapped bytes.

**Why `wrap`/`unwrap` and not `get_master_key_bytes()`.** AWS KMS never releases key material; its
API is `Encrypt`/`Decrypt` with an `EncryptionContext`. These two methods map one-to-one onto that,
onto Vault Transit's `encrypt`/`decrypt`, and onto a local AES key. A byte-fetching seam would have
admitted only environment variables and Vault KV, and would have needed redesigning the day KMS
arrived — which is the failure the requirement forbids. The seam is chosen for the backend that
does *not* exist yet, on purpose.

**Why `async` when the environment implementation never awaits.** Making it synchronous now and
asynchronous later is a breaking change to every caller at exactly the moment the swap is supposed
to be cheap. It is called single-digit times per process lifetime; the cost of the `async` is
nothing and the cost of adding it later is the whole point of the seam.

**Errors are three, and their shapes are deliberate.** `UnknownMasterKey { master_key_id }` **names
the id** — a key id is not a secret, and it is the one thing an operator needs mid-rotation.
`Unavailable { backend }` is retryable. `UnwrapFailed` deliberately does **not** say why: "wrong
key" versus "tampered blob" is an oracle.

### The environment implementation, and the bug it is built not to have

`EnvironmentMasterKeyCustody` holds a **`HashMap<String, Zeroizing<[u8; 32]>>`** and selects on
`wrapped.master_key_id`. That map is the entire structural difference from the existing
`LocalSecretCipher` in `src/security/crypto.rs`, which stamps `key_id` onto `EncryptedSecret` on
encrypt and **never reads it on decrypt** — `SecretCipher::decrypt` checks `algorithm` and `version`
and then decrypts with the one key it holds. `AppState` holds exactly one cipher, so rotating
`MOIRA_SECRETS__MASTER_KEY_BASE64` today silently orphans every `provider_credentials` row.

This work **does not fix that**, and does not claim to. `SecretCipher` and `LocalSecretCipher` are
left untouched. What it does is make the new subsystem structurally incapable of reproducing the
defect, and leave the door open: `EncryptedSecret` already carries `key_id` and
`encrypted_data_key`, so a later change can re-back provider credentials on `MasterKeyCustody`
without a column change. Old master keys stay loaded during a rotation, which is what makes
previously-wrapped DEKs unwrappable with no re-encryption.

Key material is held as `Zeroizing<[u8; 32]>` — the stronger of the two patterns already in the
tree, chosen on purpose. `Debug` prints ids only.

### The one omission that is load-bearing

The AAD binding a wrapped DEK is:

```
moira/data-key/v1;data_key_id=<uuid>;master_key_id=<id>;wrap_algorithm=<alg>
```

semicolon-joined, matching the `credential_aad` house style in `src/security/crypto.rs`.

**The custody backend name is deliberately absent.** If a future Vault KV deployment, a CSI driver,
or an external-secrets operator serves the *same 32 bytes* under the *same key id* — the common
environment → Vault case — then swapping custody requires **zero writes to any table**. Binding the
backend name would force a rewrap for no cryptographic gain whatsoever. This is the strongest
available form of the non-negotiable and it is bought by one omission, which is why it is written
down here: an omission with no comment gets "fixed" by the next reader.

---

## 5. Configuration

A **new settings block**, deliberately not reusing `secrets.master_key_base64`. Provider credentials
and user content have different retention and different blast radius, the existing `SecretSettings`
cannot express a key *list*, and changing it would change `provider_credentials` semantics.

```toml
[content_encryption]
custody = "environment"      # enum; Kms / Vault are later variants
keys = ""                    # "id:base64,id:base64"
active_key_id = "dev-local"
allow_insecure_dev_key = true
refresh_seconds = 300
min_refresh_seconds = 5
data_key_rotation_days = 30
```

```
MOIRA_CONTENT_ENCRYPTION__CUSTODY=environment
MOIRA_CONTENT_ENCRYPTION__KEYS=<id>:<base64>,<id>:<base64>
MOIRA_CONTENT_ENCRYPTION__ACTIVE_KEY_ID=<id>
MOIRA_CONTENT_ENCRYPTION__ALLOW_INSECURE_DEV_KEY=false
```

The `config` crate already handles the `MOIRA_` prefix, `__` nesting, and `,` list splitting.
Neither `,` nor `:` is in the base64 alphabet, so `split_once(':')` is unambiguous. Key ids are
constrained to `[A-Za-z0-9._-]{1,128}` because they reach a `varchar(128)` column and log lines.

**An implicit fallback — "if `content_encryption.keys` is unset, use `secrets.master_key_base64`" —
is rejected outright.** That is the silently-inert-configuration class this whole release exists to
avoid; see §13.

---

## 6. The ciphertext format, and why it is self-describing

The five columns are bare `bytea` with **no sibling algorithm, nonce, or key-id columns**.
`provider_credentials` has six such siblings; these have none. So one blob must be entirely
self-describing — and entirely **authenticated**, because "self-describing but unauthenticated" is
just a downgrade oracle with extra steps.

```
off  len  field           notes
---  ---  --------------  ------------------------------------------------------------
  0    4  magic           "MOE1" — Moira Object Envelope, version-1 family
  4    1  format_version
  5    1  algorithm_id    AES-256-GCM, 12-byte nonce, 16-byte tag
  6    1  key_mode        generation DEK wrapped by a custody master key
  7    1  reserved        MUST be zero; non-zero is a hard refusal, never ignored
  8   16  data_key_id     raw UUID bytes = content_data_keys.id
 24   12  nonce           CSPRNG, fresh per row, never derived, never a counter
 36    2  aad_profile     u16 big-endian
 38    4  body_len        u32 big-endian, length of the body
 42    N  body            ciphertext || 16-byte tag
```

Header 42 bytes; the shortest legal blob is 58, because an empty plaintext still has a tag.

```
AAD = header_bytes[0..42] || 0x00 || profile_identity_string
```

Each field earns its place:

- **The full header is inside the AAD**, so `format_version`, `algorithm_id`, `key_mode`,
  `data_key_id`, `reserved` and `body_len` cannot be edited without breaking the tag. This is what
  makes "self-describing" safe.
- **Magic *and* version is deliberate redundancy.** Magic answers "is this a Moira envelope at all"
  for bytes of unknown origin; version answers "which layout".
- **`body_len` exists although `octet_length` implies it.** A mismatch proves truncation, so damaged
  storage surfaces as a *framing* error ("your storage is damaged") rather than as a GCM tag failure
  ("wrong key, or someone tampered"). The check is key-independent, so it is not an oracle, and it
  sends the operator down the right path at 3 a.m.
- **A 16-byte UUID key id, not a small integer** — no rotation cap, and a blob restored elsewhere
  still names its key. See §3.
- **The header is validated before any key lookup and before any crypto call.** This mirrors the
  console's practice of putting `envelope_version` in a sibling column specifically so it is checked
  first; a fixed-offset magic-tagged prefix gets the same property without twenty new columns across
  five tables, and it travels with the bytes through `pg_dump`, logical replication, and a support
  engineer's `\copy`.
- **`data_key_id` is SQL-reachable without any key.** A small `immutable parallel safe` SQL function
  reads the UUID at a fixed offset after checking magic and length, which is what makes key
  retirement auditable — an operator can count rows still referencing a key without holding it. Two
  parsers for one format is a drift risk, so a property test asserts the SQL parser and the Rust
  parser agree over generated envelopes.

### The AAD profile registry

One profile per column, **all five declared from day one**, each binding the row's identity:

| profile | column | binds |
|---|---|---|
| 1 | `conversation_messages.content_encrypted` | message id, conversation id, sequence number |
| 2 | `conversation_summaries.summary_text_encrypted` | summary id, conversation id, covered sequence |
| 3 | `memory_records.content_encrypted` | memory id, application id, memory scope |
| 4 | `rag_document_versions.content_encrypted` | version id, document id, version number |
| 5 | `rag_chunks.chunk_text_encrypted` | chunk id, document version id, chunk index |

Binding row identity prevents an attacker with database *write* access from lifting tenant A's
ciphertext into tenant B's conversation — the same property `credential_aad` already buys for
provider credentials. `AadProfile::ALL` plus an exhaustive `match` means adding a profile without a
test breaks the build, and each identity string is pinned against a literal in a test, so a rename
in a future refactor cannot silently change the AAD and orphan every existing row.

**Only values that are final at encrypt time may be bound.** Verified per table before this was
written: message ids, RAG version ids, chunk ids and memory ids are all generated in the repository
immediately before their insert. `conversation_summaries` binds `Uuid::now_v7()` **inline** in its
insert statement and must be refactored to a named variable before its AAD can be built — that is
a named task in PR 5, not an assumption. Anything that can change after write is deliberately not
bound, because binding a mutable value creates a re-encryption requirement, which is the thing being
refused.

---

## 7. The keyring

A new table, `content_data_keys`, holding single digits of rows. Each row is one wrapped DEK with:
its id, a monotonic version for humans, a `purpose` (`content` or `memory_dedupe`, see §11), a
`state`, the custody backend and master key id that wrapped it, the wrap algorithm, nonce and
wrapped bytes, and an 8-byte **key check value**.

States, and what each one means for data:

| state | writable | loaded at boot | rows under it readable |
|---|---|---|---|
| `pending` | no | yes | n/a |
| `active` | **yes** | yes | yes |
| `retiring` | no | yes | **yes, forever** |
| `retired` | no | no | no — clean typed failure |
| `abandoned` | no | marked | no — distinct typed failure (§10) |

**Exactly one `active` key per purpose is a database invariant**, enforced by a partial unique
index, not a convention. Two pods racing to rotate produce one winner and one unique violation,
which is the correct outcome.

The **key check value** is a truncated HMAC of a fixed label under the DEK. It proves that a
successful unwrap produced the *right* key, and it is safe to print in logs and in
`keyring status`, so two operators can compare keyrings without exchanging key material.

Each of the five tables also gains an exclusivity CHECK — `plain is null or encrypted is null` —
added `NOT VALID` and then validated, which takes `SHARE UPDATE EXCLUSIVE` rather than a long
`ACCESS EXCLUSIVE`, so there is no write outage. Validation cannot fail, because every existing row
has `*_encrypted is null` structurally: no code in `src/` binds those identifiers. That constraint
is what makes "which column is non-null" a trustworthy discriminator forever.

**In memory**, the process holds a snapshot of **cipher objects, not key bytes** — after load, no
Moira-owned type contains a printable DEK. A read clones an `Arc`. Refresh is a background tick plus
an on-demand single-flight reload when a decrypt meets an unknown key id; a *failed* refresh keeps
the previous snapshot and keeps serving, because the old snapshot is still correct, merely stale.
Readiness reads the cached snapshot and never the database.

**Custody is never called on the request path.** The keyring is unwrapped once at boot. That is the
property that makes a future KMS swap operationally possible, and §12 defends it with a counting
test rather than a comment.

---

## 8. Write path and read path

**Write.** The insert structs replace `content_plain: Option<String>` with an enum —
`Omitted` / `Plain` / `Encrypt` — **replacing the field rather than adding one beside it**. A caller
then physically cannot supply both, `Omitted` is a named state rather than a `None` with a comment,
and a fourth persistence mode becomes a compile error at every write site instead of a silently
defaulted branch. Given that the defect this feature grew out of (F32) was precisely a write path
that ignored a policy, making the policy unrepresentable-as-forgotten is worth the wider diff.

Two invariants, cheap to state now and expensive to discover later:

- **`content_size_bytes`, `token_count` and the 262,144-byte content cap are computed on plaintext,
  before sealing.** Ciphertext length must never reach a counter something else does arithmetic on,
  or limits and metrics shift under an operator the moment they flip the policy.
- **Seal and open never perform I/O.** They take an already-unwrapped DEK from the snapshot. This is
  what makes it safe to call the cipher inside a transaction, and what keeps a future KMS custody
  from turning every INSERT into a network call.

**Refusal, never fallback.** If the policy says `encrypted_content` and no usable active key exists,
the write returns `503` with a coded error. It does **not** write plaintext. Writing plaintext under
an encrypted policy is F32 with extra steps, and a test asserts the row count did not increase.

**Read.** Precedence is defined once and tested exhaustively: encrypted wins if present; otherwise
plaintext; otherwise the content is absent (policy `none` or `metadata_only`). Both-non-null cannot
arise because of the CHECK constraint; if it somehow does, encrypted wins and one WARN naming the
row id is logged.

Two row mappers in `src/infra/pg_rows.rs` — `conversation_message_record_from_row` and
`memory_record_from_row` — are pure functions with no access to `AppState`, so they cannot decrypt.
Each gains an opener parameter, and the compiler then finds every call site. That is the intended
mechanism: not a grep, a type error.

**Latency.** Worst case per turn is roughly 24 messages, a summary, 10 memories and 8 chunks — about
43 opens over ~48 KB. With AES-NI that is tens of microseconds against a model call measured in
hundreds of milliseconds. The number that actually matters is the other one: **zero custody calls on
the request path**.

---

## 9. Rotation, as an operator runs it

Three verbs, deliberately named apart, because conflating them is how this goes wrong. They ship as
a process mode on the existing binary — they need the database and custody but no HTTP surface, no
new authorization design, and no OpenAPI change.

### R1 — data-key rotation (`add`, then `promote`)

No config change, no restart, no downtime, **nothing re-encrypted**. Mint a DEK, wrap it under the
active master key, insert it `pending`; then one transaction promotes it to `active` and demotes the
previous one to `retiring`. New writes pick up the new key at each instance's next refresh. **Rows
written under the old key stay under it forever and stay readable forever** — `retiring` means "not
writable, still loaded". Runs automatically on the existing cluster lease once the active key
exceeds `data_key_rotation_days`.

### R2 — master-key rotation (`rewrap`)

Four steps, each independently reversible:

1. **Add** the new master key alongside the old in `MOIRA_CONTENT_ENCRYPTION__KEYS`; leave
   `ACTIVE_KEY_ID` alone. Rolling restart.
2. `moira keyring rewrap --to <new-master-key-id>` — one transaction over a handful of rows.
   Running instances are unaffected: they already hold unwrapped DEKs, and only a *booting* instance
   reads these rows. **No row of user data is read or written.**
3. **Promote** the new master key by setting `ACTIVE_KEY_ID`. Rolling restart; verify the startup
   line names it.
4. **Drop** the old key from `KEYS`. Rolling restart. The old key may now be destroyed.

Ordering is enforced rather than documented-and-hoped: `rewrap` refuses if any keyring row names a
master key the process does not hold, and **boot refuses** on the same condition. Skipping step 2
therefore produces a startup failure at step 4 — loudly, immediately — instead of silent data loss
discovered weeks later.

### R3 — custody backend swap (the non-negotiable, executed literally)

- **The new backend serves the same bytes under the same key id** (environment → Vault KV, an
  external-secrets operator, a CSI driver): change `CUSTODY` and its address, rolling restart.
  **Zero writes. Zero re-encryption.** Boot preflight proves it by unwrapping every key before the
  listener binds. This works *only* because the backend name is absent from the wrapped-key AAD
  (§4).
- **The new backend will not release key bytes** (AWS KMS): this is R2 with a different target.
  `rewrap` unwraps with the environment custody and wraps with the KMS custody — single digits of
  rows, one transaction, no user data touched.

### R4 — reseal (`reseal`), expensive, optional, deliberately not in the first release train

Re-encrypting existing rows onto a newer data key is the only way to move a key from `retiring` to
genuinely `retired`. It is resumable and idempotent by construction: select rows whose header names
the old key, then per row a compare-and-swap update, so a row a concurrent writer rewrote matches
zero rows, is skipped rather than clobbered, and is already under the new key anyway. Each pass
shrinks the set it selects, so killing and rerunning converges.

`retire` refuses unless the key is not active and all five per-table reference counts are zero.

**Say this out loud rather than let "rotation" imply it:** retiring a *data* key may never actually
be reachable. `memory_records` has no retention, so its rows may reference the first key
indefinitely. Master keys retire in minutes; data keys retire only when their rows die or are
resealed.

---

## 10. Boot validation, and failing closed

All of it runs in `AppState::new`, **before the listener binds**. `AppState::new` becomes `async` as
a result (it is `pub fn new` today at `src/app/state.rs:85`) — a mechanical but wide diff through
`main.rs` and the test lifecycle fixture, and one more thing between the process and readiness. That
is accepted; migrations and the cluster lease already made serving depend on the database.

1. **Settings validation**, accumulating **every** violation into the existing single joined list —
   never first-violation-wins. Reuses `validate_32_byte_secret` with a new development sentinel, so
   pasting the well-known dev constant into a real environment variable is rejected exactly as it is
   for the three secrets that already have one. Production additionally requires
   `allow_insecure_dev_key = false` and an `active_key_id` that is not the development id.
2. **`custody.preflight()`** — wrap and unwrap a throwaway key under the active master key. Proving
   the backend is *usable*, not merely *configured*, is the difference between failing here and
   failing on some user's first read.
3. **Load and unwrap every non-retired keyring row, including `retiring` ones, and verify each key
   check value. Any failure aborts boot.** A keyring you can only partly unwrap means some stored
   rows are unreadable, and learning that lazily, on a request, is the failure mode this design
   exists to refuse.

   The failure message names the data key id, the master key id it wants, the ids that *are*
   configured, the environment variable to put it in, and **both remedies** — restore the key, or
   acknowledge the loss explicitly (below). A test asserts that message text, because the remedy is
   the deliverable.
4. **Bootstrap only if `content_data_keys` is strictly empty**, under an advisory lock: mint, wrap,
   insert `active`. Because it fires only on a strictly empty table, an existing deployment booted
   against the wrong master key gets the refusal in step 3, never a silent second key.
5. **One structured startup line** naming backend, master key id, active data key, key age, number
   of keys loaded, the active key check value, and the count of sealed columns — and a WARN if the
   active key is older than `data_key_rotation_days`, so a wedged rotation scheduler is visible
   rather than silent.

### `abandon` — the one command that is a confession

Without an escape hatch, a permanently lost master key means a service that can never boot again,
with no path back up. That is strictly worse than an explicit, audited, logged acknowledgement of
data loss. So: `moira keyring abandon <id> --confirm --reason "<text>"` marks a key `abandoned`.
The service starts; rows under that key return a distinct typed refusal; rows under every other key
read normally. It is a **status, never a delete** — the ciphertext stays, in case the key resurfaces.

It is refused unless both `--confirm` and a non-empty `--reason` are given. It is still a button that
makes rows permanently unreadable on purpose, and its user is a tired operator at 3 a.m., so its
guard text deserves as much review as its code.

### The break this creates, owned rather than softened

**The master key is required in production unconditionally**, even on deployments where no
application uses `encrypted_content` today, because the policy is flippable at runtime through the
admin API without a restart. **Every existing production deployment therefore fails to boot after
upgrading until `MOIRA_CONTENT_ENCRYPTION__KEYS` is set.**

The softer alternative — require the key only if some policy row selects `encrypted_content` — was
considered and rejected: it makes a boot invariant depend on mutable database state, so a replica
that booted yesterday fails today because someone changed a policy on another replica. That is the
same silently-inert failure wearing a different hat.

Mitigation is scheduling, not softening: the settings block and boot validation ship in **PR 1, one
full release ahead of any behaviour change**, together with `.env.example`, the dev-env script, the
Helm values, and a bolded release note.

---

## 11. Two things that are easy to miss

### The `content_hash` oracle

Since migration `0021`, `memory_records.content_hash` is an **unkeyed SHA-256 content address**,
used for exact dedupe. Memory records are short, low-entropy and highly guessable ("user prefers
dark mode", "user's timezone is …"). Sealing the content while leaving an unkeyed digest of that
same content in the same row **defeats the encryption for memories**: a database dump plus a
wordlist recovers them. Migration `0021` anticipated exactly this and refuses to recompute hashes
for encrypted rows — but the reversal it anticipated was never built.

**Resolution.** Mint one HMAC key as a keyring row with `purpose = 'memory_dedupe'`, wrapped by the
master key exactly like every content key. Encrypted rows store a prefixed, keyed digest instead of
the bare content address. The prefix is unambiguous against the existing base64url content address,
which can never contain the prefix separator — migration `0021`'s own rule, reused — so a
plaintext-era hash and an encrypted-era hash produce a **miss, never a false match**.

Because the dedupe key is itself a wrapped envelope, **master-key rotation re-wraps it and the
stored hashes never change**. That dissolves the objection that killed the peppered approach in
F14: a pepper rotation permanently orphaned every stored hash in a table with no retention. The
dedupe key is not rotated by design; if it ever must be, the consequence is a documented one-time
loss of cross-era dedupe, not an error.

`conversation_messages.content_hash` stays peppered — `0021` kept it that way *because* the schema
expects encrypted content. `rag_document_versions.content_hash` and `rag_chunks.chunk_hash` stay
unkeyed: a whole document, or a thousand-character chunk, is not a guessable-plaintext oracle. That
asymmetry is a threat-model statement and gets a paragraph in `docs/security.md` rather than being
left to be rediscovered.

### Encrypting content while leaving embeddings in the clear

Memory retrieval and chunk retrieval rank by vector distance over embeddings computed from the
plaintext and stored unencrypted. Embedding-inversion attacks recover substantial source text.
Fixing that is out of scope here. **Disclosing it is not:** the release notes must not say "content
is encrypted at rest" without saying "vectors derived from it are not".

---

## 12. How this is proven, and the one risk that would make the proof fake

The full test plan lives with the implementation issues. Five tests carry the argument:

1. **The plaintext must not appear as a byte substring of the raw `bytea`.** This is the one
   assertion that catches "we forgot to actually call the cipher", a bug every other test passes
   straight through.
2. **Rewrap byte-identity.** After a master-key rewrap, every `master_key_id` and every wrapped key
   changed, and **the digest of every value in all five `*_encrypted` columns is byte-identical**.
   Then rebuild the keyring under a custody holding *only* the new master key and re-read
   pre-rotation rows. This is the non-negotiable asserted directly instead of argued.
3. **Rewrap-skipped boot refusal.** With only the new master key configured and no rewrap performed,
   boot must refuse, and the message must contain the missing master key id, the environment
   variable name, and both remedies. The absence of this test is why key rotation bricks systems.
4. **Zero unwrap calls on read.** A counting custody wrapper asserts that reading a turn's history
   performs **zero** unwraps. Under environment custody a regression here costs microseconds and
   nobody notices; under KMS it costs a network round-trip per row and makes the swap impossible.
   It must land before there is a KMS implementation to break.
5. **A committed backward-compatibility vector per format version**, kept forever, and a golden
   vector whose failure message says: this is a v2, not an edit. (The vectors live in the test
   suite. They are generated from fixed test-only material and are not a key anyone deploys.)

**The largest risk to this plan is not a missing test — it is a skipped one.** This repository's
recorded landmine is that a wrong `MOIRA_TEST_DATABASE_URL` makes database-backed tests **silently
skip**, and the rotation tests are exactly the ones that would then never run. A check never wired
to the thing it checks is the `accept_legacy_hashes` incident (issue #125) that motivated the
no-flag decision in the first place. **Mandatory mitigation:** the rotation tests belong to a named
gate that **fails rather than skips** when the database is absent, CI asserts a non-zero count of
executed rotation tests, and the dev bootstrap grows a target that performs a real R1 and R2 against
the local database — so the rotation path is exercised by humans routinely, which is the actual
reason rotation code is usually broken the first time it is needed.

---

## 13. No feature flag — and the reason is concrete

**This ships immediately, with release notes, and with no feature flag.** The reason is not
stylistic.

On the same day this decision was taken, `idempotency.accept_legacy_hashes` — a pre-existing switch
in this repository — was found never to have been wired to the code that reads it (issue
[#125](https://github.com/nurcahyo/moira/issues/125)). A flag is a second code path plus a chance to
be silently inert, and this repository has now demonstrated twice that it produces exactly that
failure.

The cost is real and is named rather than minimised:

**Rolling the binary back past the write-path release is a data-visibility rollback.** An older build
selects `content_plain`, sees NULL, and renders content as *absent* rather than erroring — silently,
for every row written under `encrypted_content` while the new build was live. There is no flag to
soften this. The mitigation is disclosure: the release notes lead with the query that names every
application whose policy is `encrypted_content`, so an operator knows the blast radius **before**
upgrading. Those applications are, today, silently receiving plaintext (F32); on upgrade day they
begin receiving ciphertext with no further action, and that must be the first line of the notes.

---

## 14. What happens to the existing refusals

### `conversation_content_persistence_unsupported` (422) — **narrows, does not disappear**

Today `put_conversation_policy` refuses `encrypted_content` outright. Once the cipher is wired, it
stops firing for `encrypted_content` as a *value* and fires only when encryption is configured but
unusable at write time.

It is **not removed**: removing it would leave no write-time refusal for a key-custody failure, which
is a real and permanent condition. It is **not made conditional on "is the feature built"**: a
permanently-true branch is the never-taken code this project has been bitten by. The refusals for
`none` and `metadata_only` are storage policies and are unaffected.

The prose in `src/domain/conversation.rs`, `src/application/conversation.rs` and
`docs/conversation-persistence.md` currently reasons from "no cipher is wired to the
`content_encrypted` columns". That becomes false for all five columns and must be rewritten in the
same PR that falsifies it.

### F32 — the prerequisite, and why it must land alone and first

On `develop` today, `add_message` binds `content_plain: Some(...)` unconditionally; the write path
never reads the policy. The policy-write refusal blocks new deployments from *selecting*
`encrypted_content`, but the enforcement at the choke point is on the unmerged branch
`fix/f32-content-persistence` (head `a817c54` at the time of writing; the implementation brief cites
an older head, and the branch is what matters, not the SHA).

**That branch merges first, unchanged, on its own review.** Landing a cipher on a write path that
ignores the policy would ship a cipher no operator can select — inert code, which is the
`accept_legacy_hashes` failure the no-flag decision exists to avoid. Its enforcement of `none` and
`metadata_only` is independently valuable, and its 422 break deserves human sight rather than
arriving buried inside an encryption release.

### Issue #103 — this decision resolves it toward *refuse*, not *implement*

`ResponsePersistenceMode` still accepts `encrypted_content` and `plain_content` on the execution
policy and maps both to `content_persistence_not_implemented`. That arm **stays untouched**, and
that is a finding rather than an omission.

`ResponsePersistenceMode` governs the `responses` table, and **`responses` has no `*_encrypted`
column at all** — the five listed in §1 are the complete set, verified in
`migrations/0006_public_execution_api.sql`. So `EncryptedContent` for responses cannot be
implemented by this design without new DDL. That is concrete evidence for resolving #103 toward
*refuse with a 422, symmetric with the conversation side*, rather than *implement*.

What **must** change is the arm's *justification*. Its catalog description and
`docs/response-persistence.md` currently reason from "no cipher is wired to the `*_encrypted`
columns", which becomes false the day this ships. Both are rewritten to name the actual remaining
gap — the response output columns do not exist — and #103 gets a comment recording it. The
docs-mirror gate forces the two files to move together; nothing forces the prose to be *true*, so
this is a named review item, not an automated one.

---

## 15. Local development, without tempting anyone to ship a development key

The rule this repository already follows for its three existing 32-byte secrets is reused exactly,
because a fourth secret with a fourth story is how one of them ends up in production:

- **A development sentinel value that production refuses by name.** The existing validator rejects
  the well-known development constant for each secret with a message naming the field. Content
  encryption gets its own sentinel and the same treatment. Pasting the dev constant into a real
  environment variable is a startup failure, not a warning.
- **`allow_insecure_dev_key` is a flag, and production requires it false**, checked in the same
  production-crypto validation pass as the others, with all violations accumulated rather than
  reported one at a time.
- **The flag's name is registered in `unsafe_development_features`**, so leaving it on produces a
  greppable startup WARN naming the feature. This is precisely the mechanism whose absence produced
  the `accept_legacy_hashes` incident: a switch nobody could see was inert.
- **The dev bootstrap script generates real random key material**, carries it across regeneration so
  `make env-force` does not silently invalidate a developer's existing database, and mints new
  material only on an explicit rotate. A developer's local key is real and random; it is never the
  sentinel, and the sentinel exists only so that *shipping* it is impossible.
- **`.env.example` is production-shaped** — `ALLOW_INSECURE_DEV_KEY=false`, no key value — so
  copying it and filling it in yields a safe deployment, and copying it *without* filling it in
  yields a refusal.
- **The Helm values carry the non-secret settings in the ConfigMap and the key bytes in the
  operator-supplied Secret**, exactly like the three existing secrets. No new pattern.
- **`make rotate-keys` performs a real R1 and R2 against the local database**, so the rotation path
  is something developers run, not something CI alone claims to cover.

---

## 16. What this deliberately does not do, and what is still open

**Not decided, and deliberately left open:**

- **Whether provider credentials move onto `MasterKeyCustody`.** The `LocalSecretCipher` defect
  described in §4 is real and is not fixed here. `EncryptedSecret` already has the `key_id` and
  `encrypted_data_key` slots, so the later change is additive, but it is a separate decision with a
  separate blast radius (`provider_credentials`), and bundling it would make this release
  unreviewable.
- **Whether `responses` gets encrypted output columns at all.** §14 argues #103 resolves toward
  *refuse*. That is evidence, not a decision; the maintainer decides #103 separately.
- **Whether any future backend other than the environment one is built, and when.** The seam exists
  so the answer can be "later, cheaply". Nothing here commits to writing a KMS or Vault
  implementation.
- **What keyword or full-text retrieval over sealed rows would look like.** Nothing indexes
  `content_plain` today, so nothing is lost now — but any future full-text search over conversation
  history is foreclosed for sealed rows, permanently, with no backfill that can undo it. Said out
  loud before shipping rather than discovered later.

**In the design but deliberately not in the first release train:**

- **`reseal`** ships as a verb (PR 4) but is not part of the rollout; nothing requires it until a
  data key must genuinely retire.
- **`seal-existing`** — a backfill that seals *existing* plaintext rows — is designed and deferred to
  a follow-up PR. It cannot be a SQL migration: migrations run in a process mode that skips runtime
  settings, and SQL cannot do AES. Until it exists, **switching an application to
  `encrypted_content` does not encrypt its history** — that sentence, in those words, goes in the
  release notes and in `docs/security.md`, because it is the single biggest thing an operator can
  misread.

**Accepted, named trade-offs:**

- **Per-generation DEKs are cryptographically weaker than per-row DEKs.** One recovered DEK exposes
  every row of that generation. Chosen because per-row DEKs make the custody swap a full-table
  rewrite (§3) — true on paper, false in practice, which is the worst kind of promise. AAD
  row-binding prevents ciphertext relocation but not disclosure; rotation bounds each generation's
  span.
- **A partially-encrypted table is the permanent steady state.** Existing rows stay plaintext; new
  rows are sealed. This is honest because nullness is the discriminator and the CHECK constraint
  guarantees the discriminator.
- **Ciphertext is incompressible.** TOAST compression does real work today on long messages and RAG
  documents; sealed rows lose it and gain 42 bytes of header. On document versions that is the
  dominant storage effect. A capacity note in the release notes, not a blocker.
- **Bootstrap-on-empty is the one place convenience beat refusal.** It is bounded to a strictly
  empty table under an advisory lock, and an empty database is a louder signal than the keyring —
  migrations ran against it too. It is nonetheless the single judgement call in this document most
  worth challenging in review.
- **Three verbs that all sound like "rotation"**, plus a fourth thing that is not one. If the
  documentation is bad, this design is *worse* than a simple one, because the operator picks the
  wrong verb under pressure. The naming is defended in `docs/security.md` by name, and
  `keyring status` must make the current state obvious without reading prose.

**A rule written down now, while someone remembers:** a v1 build correctly refusing a v2 blob is a
read outage for newly written rows during a rolling deploy. Therefore **read support for a new
format version ships one release before write support, always.** This belongs in `docs/security.md`
before it is ever needed.

---

## 17. Implementation sequence

Each item is a separate pull request, in this order.

| # | Issue | Scope | Depends on |
|---|---|---|---|
| 0 | [#134](https://github.com/nurcahyo/moira/issues/134) | Merge `fix/f32-content-persistence` unchanged, on its own review | — |
| 1 | [#135](https://github.com/nurcahyo/moira/issues/135) | Settings block, `MasterKeyCustody`, environment custody, boot validation — **no call sites** | 0 |
| 2 | [#136](https://github.com/nurcahyo/moira/issues/136) | The envelope format: codec, AAD profile registry, golden vectors — pure, no I/O | 1 |
| 3 | [#137](https://github.com/nurcahyo/moira/issues/137) | The keyring: migration, snapshot, boot load, refresh — still no call sites | 1, 2 |
| 4 | [#138](https://github.com/nurcahyo/moira/issues/138) | The keyring CLI and the rotation test suite, including the CI gate that fails rather than skips | 3 |
| 5 | [#139](https://github.com/nurcahyo/moira/issues/139) | Wire `conversation_messages` and `conversation_summaries`; narrow the 422; release notes | 4 |
| 6 | [#140](https://github.com/nurcahyo/moira/issues/140) | Wire `memory_records`; resolve the `content_hash` oracle | 5 |
| 7 | [#141](https://github.com/nurcahyo/moira/issues/141) | Wire `rag_chunks` and `rag_document_versions`; finalise docs; reframe #103 | 6 |
| 8 | [#142](https://github.com/nurcahyo/moira/issues/142) | `seal-existing` backfill — **follow-up, not part of the release train** | 7 |

PRs 1–4 change no behaviour. PR 5 is the first one a rollback cannot fully undo (§13).

---

## Reversal condition

This decision is reversed if the operator requirement it was taken for goes away — that is, if no
deployment needs application-layer content encryption and storage-layer encryption is accepted as
sufficient. In that case the reversal is Option 1 from §2: drop the five columns, delete
`content_data_keys`, and say plainly in `docs/security.md` that Moira relies on disk-level
encryption.

**The reversal gets more expensive with every row written under `encrypted_content`**, and after PR 5
ships it is no longer free at all: sealed rows must be unsealed before the columns can be dropped,
which requires the master key that a reversal is likely to be trying to get rid of. The cheap window
is between now and PR 5.

Reversing only *part* of it — keeping the columns but binding directly to one key manager — is not a
reversal, it is Option 2, and it is rejected for the reasons in §2 regardless of when it is
proposed.
