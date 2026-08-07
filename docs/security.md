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
| `rag_document_versions.content_hash`, `rag_chunks.chunk_hash` | **unkeyed** | A whole document, or a thousand-character chunk, is not a guessable-plaintext oracle. There is no wordlist for it. These are also write-only fingerprints today — nothing recomputes and compares them. |

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
