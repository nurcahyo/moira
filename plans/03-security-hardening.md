# Plan 03 — Security & Credential Hardening

Companion to `00-audit-report.md` and `01-roadmap-and-dependencies.md`. Addresses **P1-1, P1-2, P1-3**.

This is a **security-critical iteration**. Per `01-roadmap-and-dependencies.md` §1.2 it must contain **no unrelated refactors** — every change below is directly load-bearing for one of the three findings. No `AdminService` restructuring, no repository-trait work, no pagination/cursor changes, no OTel wiring — those belong to plans 04/05/06.

---

## Summary

**Objective.** Close three concrete security gaps in code that is otherwise architecturally sound: (1) the idempotency/request-hash mechanism uses unkeyed SHA-256 over bodies that can contain secrets, making a DB-only compromise an offline dictionary-attack surface; (2) JWKS fetches (both the per-issuer trusted-JWT path and the static admin/caller JWKS path) have no SSRF protection at all, and an admin-controlled `jwks_url` is a direct path to cloud metadata/internal services; (3) there is no production HTTP middleware stack — no request timeout, no panic isolation, no secure response headers beyond three ad hoc ones, and body-limit policy is a single global constant disconnected from the configured `PublicApiSettings.maximum_request_bytes`.

**Why ordered here.** Per `01-roadmap-and-dependencies.md` §3, plan 03 sits directly after plan 02 (spec must be honest before the drift gate in plan 05 locks it) and directly before plan 04 (durability work reuses the middleware/error primitives this plan establishes) and plan 07 (identity foundation must not be built on top of an unhardened auth/JWKS path). This is the single most security-sensitive iteration before any human-identity or multi-replica work begins.

**User-visible outcome.**
- Idempotency-key hashes and request-body hashes stored in `idempotency_records` are now versioned HMAC-SHA-256 with a dedicated pepper (mirroring `ApiKeyHasher`'s pattern exactly), not plain SHA-256. Existing rows continue to verify during migration (dual-read).
- An admin-registered `jwks_url` (trusted JWT issuer or static admin/caller JWKS config) that resolves to a private, link-local, loopback, or cloud-metadata IP is rejected outright, with an audit record. JWKS responses are capped in size, must be `application/json`, are fetched with an explicit timeout, and concurrent requests for the same issuer's JWKS during a cache miss coalesce into one upstream fetch (singleflight) instead of a stampede. A failed refresh keeps serving the last-known-good cached JWKS rather than breaking auth.
- Every HTTP request now runs under a request timeout (`504` on expiry), a panic in a handler returns a `500` error envelope instead of dropping the connection, three additional secure headers are added, body-limit enforcement is aligned per-route to `PublicApiSettings.maximum_request_bytes` for public routes versus a distinct admin limit, tracing spans are redacted of secret-bearing fields, and once-only secret responses (`ApiKeySecretResponse`) are never compressed.

**Included scope.**
- New `IdempotencyHasher` (or equivalently named) construct in `src/security/`, mirroring `ApiKeyHasher`'s pepper/pepper-version pattern, plus a new `MOIRA_IDEMPOTENCY__*` config section.
- Dual-hash verification: old unkeyed-SHA-256 rows still match on read; all new writes use the versioned HMAC.
- SSRF-hardened JWKS fetch function shared by both call sites (`TrustedIssuerAuthenticator`'s per-issuer `jwks()` and `validate_static_jwt`'s static-URL fetch).
- Singleflight coalescing for JWKS refresh; retain-old-cache-on-failure; audit log entries for JWKS fetch rejections.
- New Axum middleware stack: `TimeoutLayer`, `CatchPanicLayer` → structured error envelope, secure headers additions, per-route body limits, redacted tracing span, no-compression for once-only-secret responses.
- Tests: pepper-rotation/legacy-hash verification, SSRF denial (private/link-local/metadata IP + oversized/slow response), 413 on oversized body, panic → 500 envelope, timeout → 504, header presence assertions.

**Excluded scope (explicitly deferred).**
- No `AdminService` decomposition (plan 06/P2-1).
- No cursor pagination work (plan 04/P1-4).
- No retention/cleanup worker (plan 04/P1-5).
- No execution-deadline budget extension into credential resolution / runtime construction (plan 04/P1-6).
- No streaming-cancellation integration test (plan 04/P1-7).
- No `If-Match` requirement change on execution-policy PUT (plan 04/P1-8).
- No OpenTelemetry wiring or Prometheus histograms (plan 05/P1-9).
- No CI OpenAPI-drift gate or secret-leak snapshot suite infrastructure (plan 05/P1-10) — this plan only adds the *unit-level* tests the new code needs, not the systemic snapshot-suite gate.
- No identity/owner-claiming work (plan 07/P1-11) — this plan only hardens the JWKS path that plan 07 will build on.
- No changes to `ApiKeyHasher` itself (`src/security/api_keys.rs`) beyond using it as the pattern reference — it is already correct (Argon2id + pepper) per the audit's positive findings.
- No changes to `LocalSecretCipher`/credential AES-256-GCM logic (`src/security/crypto.rs`) — already verified correct; out of scope.

### Branch & PR (binding — `plans/CONVENTIONS.md` §1)

**Branch:** `plan/03-security-hardening`. Per `01-roadmap-and-dependencies.md` §3 this plan is **stacked on plan 02** (`I02 --> I03`): branch from `plan/02-mvp-boundary-honesty`, name that base PR in this PR's description, and **rebase onto `main` once plan 02 merges** before requesting review. Plan 02's branch must not be force-pushed while this branch is stacked on it (`CONVENTIONS.md` §1 rule 7); the same protection applies to this branch once plan 04 or 07 stacks on it.

**Commits:** Conventional Commits. Expected prefixes: `feat:` (the `IdempotencyHasher`, the SSRF-hardened fetch helper, the middleware stack), `fix:` (the `maximum_request_bytes` / Axum body-limit mismatch), `chore:` (the `hmac` + `tower-http` feature additions to `Cargo.toml`), `test:` (both test layers), `docs:` (the new `MOIRA_IDEMPOTENCY__PEPPER_BASE64` deployment documentation).

**PR must not open until every gate in `CONVENTIONS.md` §2 passes locally** (the five Rust gates enumerated under Verification below).

**PR description — required sections (all seven, none omitted):**
1. **Plan link** — `plans/03-security-hardening.md`.
2. **Findings addressed** — `P1-1`, `P1-2`, `P1-3` (from `plans/00-audit-report.md`).
3. **Migrations included** — **none** (verified: `idempotency_records.idempotency_key_hash`/`.request_hash` and all four `content_hash` columns are already `varchar(128)`; a `"v1:" + base64url(32-byte HMAC)` value is 46 chars).
4. **Breaking API/OpenAPI changes** — **no OpenAPI diff** (this plan is infrastructure-only and must produce a zero-diff spec against plan 02's output). Behavioral breaks to call out explicitly: `http://`/private-address `jwks_url` values now rejected; new `504` on slow non-streaming requests; `413` thresholds move per route.
5. **Test evidence** — output summary of **both** layers: the unit layer (named functions in `src/security/idempotency.rs`, `src/security/ssrf.rs`, `src/lib.rs`, `src/config/settings.rs`, `src/i18n/catalog/mod.rs`) **and** the e2e layer (`tests/http_middleware_contract.rs`, `tests/jwks_hardening.rs`, `tests/idempotency_hash_migration.rs`) run against real PostgreSQL 16 + pgvector.
6. **Rollback procedure** — see Risks & Rollback (`git revert` of the merge commit; note the ≤24h legacy-hash verification window).
7. **Deferred follow-ups** — DNS-rebinding IP pinning, response compression with secret-route exclusion, configurable body limits for conversation/memory/RAG routes.

**Deploy-order note to carry in the PR description:** `MOIRA_IDEMPOTENCY__PEPPER_BASE64` must be provisioned in every non-dev environment **before** this merges to a deployable branch, or `Settings::validate` will correctly refuse to start.

**Done means merged.** This plan is not complete when the PR opens. It is complete when the PR is **merged with all gates green** and every Definition of Done box is proven by a named, passing test (`CONVENTIONS.md` §1 rule 5, §3).

---

## Findings Addressed

### P1-1 — Unkeyed SHA-256 idempotency request hash over secret-bearing bodies
- `src/security/masking.rs:10-12`: `request_hash(bytes) -> secret_fingerprint(bytes) -> Sha256::digest(bytes)` — plain, unkeyed SHA-256, no pepper.
- **Full call-site inventory confirmed by grep during re-grounding** (broader than the audit's single citation — four producer files, not two):
  - `src/infra/repositories/public.rs:741` (`idempotency_record` helper, fn at `:733`): `idempotency_key_hash: request_hash(key.as_bytes())` — the *key* hash for `/v1/responses` idempotency records. The matching *body* hash is produced by `normalized_request_hash` in `src/application/public.rs:1880-1884` (serde-serialize then `request_hash`), consumed at `:1021`; the replay-mismatch comparison is the string equality at `:1032`.
  - `src/application/admin_command.rs` — the **admin command ledger's** hash producer (not `src/application/admin.rs`, which has zero idempotency references): `AdminCommandSpec::request_hash` (`:96-106`) for command bodies and `request_hash(idempotency.key.as_bytes())` (`:163`) for the key, feeding `PgAdminRepository::claim_idempotency` (`src/infra/repositories/admin.rs:559`, body-mismatch comparison at `:602`) and `finalize_idempotency` (`:657`). Admin command bodies can contain provider API keys, credential material, and other secrets being created/rotated — this is the highest-value target: `POST /api/v1/admin/provider-credentials`, `POST /api/v1/admin/system-keys`, `POST /api/v1/admin/consumer-keys` bodies are hashed with plain SHA-256 and persisted (columns `idempotency_key_hash varchar(128)` / `request_hash varchar(128)` per `migrations/0003_security_foundation.sql:349,352`).
  - `src/application/runtime_admin.rs` — a **third, independent idempotency-ledger user missed by the audit**: `idempotency_replay` (`:621-657`, key hash at `:636`, body hash at `:637`, mismatch comparison at `:645`) and `record_idempotency` (`:659-690`, key hash at `:682`, body hash at `:685`), with its own private `normalized_request_hash` copy at `:717-721`, all reading/writing the same `idempotency_records` table via `get_idempotency_record` (`src/infra/repositories/admin.rs:1732`). Runtime-policy admin request bodies are hashed unkeyed here too.
  - `src/application/conversation.rs` — **seven** content-hash call sites, not one: `:286`, `:352`, `:395`, `:455`, `:546`, `:876`, `:967` — hashing conversation message content, response output, memory content, and RAG document content (potentially sensitive personal data, not just "secrets," but the same offline-verifier risk applies) before storing in `content_hash` columns. These are write-only fingerprints — grep confirms no code path compares a stored `content_hash` against a recomputed one, so switching them to the keyed hasher has no read-side compatibility impact.
  - (`src/application/public.rs:1597` also fingerprints response output text via `request_hash` into `ResponseTerminalUpdate.output_hash` — same treatment as the conversation content hashes.)
- `docs/todo.md:9` — "Replace unkeyed admin command request hashes with versioned HMAC-SHA-256 using a dedicated idempotency pepper..." — still fully open per audit reconciliation.
- **Impact:** after a DB-only compromise (e.g., a leaked backup, a read-replica breach, an SQL-injection read), an attacker holding `idempotency_records.request_hash` values can offline-verify guesses of the original secret-bearing request body (dictionary/rainbow-table attack against SHA-256, which is fast and unsalted at the mechanism level — Argon2id API-key hashing is unaffected since that's a separate, already-correct mechanism per the audit's positive findings).
- **Correction (this plan):** versioned HMAC-SHA-256 keyed by a dedicated idempotency pepper, mirroring `ApiKeyHasher`'s `pepper` + `pepper_version` fields exactly (`src/security/api_keys.rs:13-17,28-39`). Preserve verification of legacy unkeyed hashes during migration (additive, non-breaking).

### P1-2 — JWKS fetch has no SSRF protection
- `src/security/auth.rs:386-410` (`TrustedIssuerAuthenticator::jwks`, called per-issuer for trusted-JWT-based caller/admin auth): `self.http.get(&issuer.jwks_url).send().await?.error_for_status()?.json::<JwkSet>().await?` — no scheme restriction (accepts `http://`), no DNS-resolved-IP allow/deny list, no response size cap, no explicit per-request timeout (relies on whatever default the shared `reqwest::Client` in `AppState.http` has, if any — `src/app/state.rs:43-46` builds the client with only a `user_agent`, **no timeout configured**), no content-type check before attempting `.json::<JwkSet>()`, and no singleflight — concurrent cache-miss requests for the same issuer each fire their own upstream fetch.
- `src/security/auth.rs:484-506` (`validate_static_jwt`, used by both the admin and caller authenticators when a static `jwks_url` is configured via `JwtAuthSettings`/`CallerAuthSettings`; the unhardened fetch itself is at `:497-503`): identical `http.get(jwks_url)...json::<JwkSet>()` pattern, called on **every** request that doesn't hit the trusted-issuer cache path (no caching at all here — this is a fetch-per-request in the worst case, compounding both the SSRF exposure and a denial-of-service/cost risk).
- `docs/todo.md:25` — "Harden JWKS refresh with full SSRF checks, strict timeout, response size and content-type limits, valid JWKS parsing, singleflight refresh, old-cache retention on failure, and audit records." Open, matches this finding's correction list exactly.
- **Impact:** since `jwks_url` for a trusted issuer is admin-configured (`POST /api/v1/admin/jwt-issuers`) and `JwtAuthSettings.jwks_url`/`CallerAuthSettings.jwks_url` are deployment config, either an admin (intentionally or via a compromised admin credential) or an operator typo can point Moira's outbound HTTP client at `http://169.254.169.254/latest/meta-data/...` (AWS/GCP/Azure metadata), `http://localhost:6379` (internal Redis), or any RFC1918 address, and Moira will fetch and parse the response as if it were a JWKS — at minimum an SSRF probe, at worst a path to internal-service compromise depending on what's reachable.

### P1-3 — No production HTTP middleware (timeout, panic catch, secure headers, per-route body limits)
- `src/lib.rs:16` imports `extract::DefaultBodyLimit`; `:42` (inside `build_router`) applies exactly one body-limit layer: `.layer(DefaultBodyLimit::max(512 * 1024))` — a single global 512KiB cap for **every** route, admin and public alike, with no connection to `PublicApiSettings.maximum_request_bytes` (`src/config/settings.rs:168`, defaulted to `1_048_576` = 1MiB both in the settings default and in the `src/infra/repositories/public.rs:721` default-policy constant) — **the configured policy value and the enforced Axum limit are already inconsistent with each other** (512KiB hard cap vs. a documented 1MiB policy), confirming the audit's framing precisely.
- `src/lib.rs:34-53` (`build_router`) layer stack, read in full during grounding: `metrics_middleware` (`:36-39`, custom), `secure_response_headers` (custom, sets `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` — `:92-105`), `DefaultBodyLimit` (`:42`), `TraceLayer::new_for_http()` (`:43`, untouched, standard tower-http tracing — **not redacted**, so headers could appear in trace spans depending on `TraceLayer`'s default span fields), `request_id_context`, `PropagateRequestIdLayer`, `SetRequestIdLayer`, and a conditional `CorsLayer` (`:48-50`). **No `TimeoutLayer`, no `CatchPanicLayer`, no `CompressionLayer`-exclusion logic** exist anywhere in this stack or elsewhere in `src/`.
- `docs/todo.md:26-27` — "Add production HTTP middleware for body limits, content-type enforcement, request timeout, panic handling, secure response headers, redacted tracing, and no compression for once-only key secret responses." / "Align configurable `maximum_request_bytes` policy with the actual Axum body-limit layer, including per-route public/admin limits and tests for oversized JSON requests." Both open, matching this finding exactly.
- **Impact:** a panicking handler (e.g., an unexpected `unwrap()` on malformed but not-yet-validated input) currently drops the TCP connection with no response body at all instead of returning a clean `500`, since Axum's default behavior without `CatchPanicLayer` is to abort the response; there is no ceiling on how long a request can run server-side (a stalled downstream DB or provider call can hold a connection indefinitely, `TimeoutLayer` absent); and the body-size policy operators believe is configurable (`maximum_request_bytes`) is not actually what's enforced.

---

## Architecture

**Components & ownership boundaries (per `docs/project-structure.md` layering).**
- `src/security/` — owns all three findings' core logic: a new hashing module for P1-1, a new SSRF-hardened HTTP fetch helper for P1-2 (shared by `auth.rs`), unchanged crypto/masking modules otherwise.
- `src/config/settings.rs` — owns new config sections: `IdempotencySettings` (P1-1 pepper config) and any new JWKS-hardening knobs (P1-2: max response bytes, timeout, allowed-scheme).
- `src/app/state.rs` — owns wiring the new hasher and hardened HTTP client into `AppState`.
- `src/lib.rs` — owns the middleware stack additions (P1-3) — this is the **only** file that assembles the Axum `Router`'s layer chain; all new layers are added here.
- `src/infra/repositories/public.rs`, `src/application/public.rs`, `src/application/admin_command.rs`, `src/application/runtime_admin.rs`, `src/application/conversation.rs` — own the P1-1 call-site swaps (every producer of `idempotency_key_hash`/`request_hash`/`content_hash` values enumerated in Findings); read-side `verify()` swaps land in `src/infra/repositories/admin.rs` (`:602`) and the two application-layer mismatch comparisons. No other logic touched in these files.
- `src/security/auth.rs` — owns the P1-2 call-site changes (`TrustedIssuerAuthenticator::jwks`, `validate_static_jwt`) routed through the new shared SSRF-hardened fetch helper.

This plan does not touch `src/application/execution.rs`, `src/orchestration/`, or any Rig-boundary code — it stays entirely within Moira's config/identity-claims/credentials/auth/routing-adjacent security layer, consistent with Moira's stated boundary (Moira owns credentials/authz; Rig owns AI execution — untouched here).

**Data flow.**
- P1-1: request body bytes → (unchanged) validation/business logic → **new**: `IdempotencyHasher::hash(bytes) -> "v{N}:{base64(hmac_sha256(pepper_n, bytes))}"` → stored in `idempotency_records.request_hash` / `.idempotency_key_hash`. On read (replay lookup), the stored hash's version prefix determines which pepper to verify against; unprefixed legacy values (no `v{N}:` prefix) fall back to the old unkeyed `secret_fingerprint` comparison.
- P1-2: issuer/JWKS URL (admin-supplied) → **new**: `resolve_and_validate_url(url) -> Result<ValidatedUrl, AppError>` (parses URL, enforces `https://`-only unless an explicit dev override, resolves DNS, rejects private/loopback/link-local/multicast/metadata-reserved IP ranges for **every** resolved address, not just the first) → hardened `reqwest` GET with explicit `Duration` timeout, `Content-Length`/streamed-byte cap, `Content-Type: application/json` check → `JwkSet` parse → cache write (with singleflight lock held across the fetch) → on any failure, existing cached value (if present) is retained and served, with an audit-log write recording the failure and the rejected/failed URL (never the JWKS contents, which are public keys anyway but keep audit entries minimal).
- P1-3: incoming request → `SetRequestIdLayer` → `PropagateRequestIdLayer` → `request_id_context` → **new** `TimeoutLayer` (outermost practical position, wraps the rest) → **new** `CatchPanicLayer` (must sit where it can catch panics from inner handler logic; per-route body limit applied via `RequestBodyLimitLayer`/`DefaultBodyLimit::max` per router branch rather than one global layer) → `TraceLayer` (configured with a redacting `make_span_with`) → `secure_response_headers` (extended) → handler → response, with a **new** response-post-processing step stripping/forbidding compression on responses whose route is flagged once-only-secret (`ApiKeySecretResponse`-returning routes).

**Security boundaries.**
- P1-1's pepper is a new secret analogous to `ApiKeySettings.pepper_base64` — it must never be logged, must be loadable from env (`MOIRA_IDEMPOTENCY__PEPPER_BASE64`), and must have the same `allow_insecure_dev_pepper` dev-only escape hatch pattern as `ApiKeySettings` so local/dev/test environments don't require a real secret.
- P1-2's SSRF allow/deny logic is the actual security boundary here — get the IP-range denial list right (RFC1918, RFC4193 IPv6 ULA, loopback `127.0.0.0/8`/`::1`, link-local `169.254.0.0/16`/`fe80::/10`, the AWS/GCP/Azure/Alibaba metadata addresses which are link-local so already covered by the link-local rule, and IPv4-mapped IPv6 forms of the above so a `::ffff:169.254.169.254` bypass doesn't slip through DNS-rebinding). Validate **after** DNS resolution, on the actual resolved IP(s) `reqwest` would connect to — not just string-matching the hostname — to close DNS-rebinding attacks (resolve once, validate, then either pin the connection to the validated IP or accept the small residual TOCTOU window and document it as a known, low-severity residual risk given this is an admin-configured, not attacker-configured, input surface).
- P1-3's `CatchPanicLayer` must not leak panic messages (which can contain internal state) to the client — the error envelope's `message`/`details` fields must be a generic, fixed string; the real panic payload goes only to server-side tracing/logs.

**Database/migration changes.** **None — verified.** `idempotency_records.idempotency_key_hash varchar(128)` and `.request_hash varchar(128)` (`migrations/0003_security_foundation.sql:349,352`) have enough width for a versioned HMAC value (`"v1:" + base64url(32-byte HMAC-SHA-256) = 3 + 43 = 46 chars`, well under 128). All four `content_hash` columns touched by the conversation-layer call sites are likewise `varchar(128)` (`migrations/0007_conversations_memory_rag.sql:173,239,378,437` — `conversation_messages`, `memory_records`, `rag_document_versions`, `rag_ingestion_runs`), so no widening migration is needed anywhere.

**API & OpenAPI changes.** **None.** This plan changes internal hashing and infrastructure, not request/response shapes, status codes, or documented parameters. The one visible surface change is behavioral: oversized requests on routes that previously fit under the old 512KiB global cap but exceed a new, smaller per-route public limit will now get `413` sooner — see Interfaces & Contracts for exact limits.

**Backward compatibility.**
- P1-1: fully backward compatible by design — legacy unkeyed hashes remain verifiable (dual-read; note the key-hash **lookup** dimension needs the dual-lookup treatment in Detailed Implementation item 6, since `idempotency_key_hash` is the index key, not just a compared value); only new writes use the versioned format. No data migration/backfill needed (unlike plan 02's optional RAG backfill) because idempotency records already expire after 24h everywhere (`expires_at` at `src/infra/repositories/public.rs:748`, `IDEMPOTENCY_RETENTION_HOURS: i64 = 24` at `src/application/admin_command.rs:17`, and `Duration::hours(24)` in `runtime_admin.rs`) — old-format rows age out naturally within a day, so no forced rewrite is necessary.
- P1-2: fully backward compatible for any legitimately-configured `https://` JWKS URL pointing at a public, non-private address — which is the only configuration that should exist in any real deployment. A deployment that was (mis)relying on an `http://` or private-address JWKS URL will break; this is the intended effect of the fix, not a regression, but **flag for product/ops**: confirm no current deployment's JWKS URL is `http://` or a private address before shipping, since this plan does not add a bypass flag for production (a `provider_security`-style `allow_private_provider_urls`/`allow_http_provider_urls` escape hatch already exists for a *different* subsystem — `ProviderSecuritySettings`, `src/config/settings.rs:126-129` — and this plan should add an analogous, separately-named dev-only override for JWKS, defaulting to `false`/disabled in all non-dev environments, so local development against a self-signed/HTTP test IdP remains possible).
- P1-3: `TimeoutLayer` and per-route body limits are new *constraints* — any client currently sending requests that would now exceed the timeout or the tightened public-route body limit will start seeing `504`/`413` where they previously succeeded. This is expected hardening, not a bug, but must be sized sensibly (see Detailed Implementation for concrete numbers) to avoid breaking legitimate slow-but-valid streaming responses (`/v1/responses/stream` must be exempted from or given a much longer timeout than the request-level `TimeoutLayer`, since SSE streams are long-lived by design — verify this exemption explicitly, it is the single highest-risk compatibility issue in this plan).

**Deployment implications.**
- New required-in-production config: `MOIRA_IDEMPOTENCY__PEPPER_BASE64` (mirrors `MOIRA_API_KEYS__PEPPER_BASE64`) — must be documented in deployment docs/Helm values and set as a real secret in any non-dev environment; `allow_insecure_dev_pepper` gates the same fallback pattern `ApiKeySettings` already uses.
- No new external dependency (no new crate needed for HMAC — `sha2`/`hmac` crates; check `Cargo.toml` for an existing `hmac` dependency at implementation time, add if absent, following the existing `sha2`/`argon2`/`aes-gcm` dependency style already in `src/security/`).
- No new external dependency for DNS-resolution-based SSRF checks either — `std::net::ToSocketAddrs` or the `trust-dns-resolver`/`hickory-resolver` transitively pulled in by `reqwest`'s default resolver can be used; prefer resolving via `tokio::net::lookup_host` (already available via `tokio`, no new dependency) rather than adding a new DNS crate, unless a more precise IP-range crate (e.g. `ipnet`) is not already a dependency — check `Cargo.toml` and add `ipnet` only if genuinely needed for clean CIDR-range checks (acceptable minimal addition, still "no unrelated refactor" since it's directly load-bearing for this finding).
- Helm/K8s: no chart change required by this plan itself, though operators should be made aware (deployment docs note) that JWKS URLs must be `https://` and publicly routable (or the dev-override flag explicitly set for non-production use, e.g., an in-cluster test IdP reachable only via a cluster-internal DNS name that is *not* in the denied-IP ranges — verify this doesn't accidentally get blocked by the private-IP rule for legitimate in-cluster JWKS if such a topology exists; if it does, the dev-override flag is the intended escape hatch, not a rule exception).

**Failure & recovery behavior.**
- P1-1: hashing failures are not expected (HMAC is infallible for byte-slice input); no new failure mode.
- P1-2: on SSRF-check rejection, `AppError` with a `403`/`400`-class code (see Interfaces & Contracts) is returned to the caller whose JWT triggered the fetch (or, for admin-registration time, to the admin creating/updating the trusted issuer — validate proactively at `POST /api/v1/admin/jwt-issuers` time too, not only at first-use time, so a bad URL is caught at configuration time rather than surfacing as an auth failure for every subsequent caller). On a *transient* fetch failure (timeout, 5xx, network error) for an already-cached issuer, retain and continue serving the last-known-good cached `JwkSet` rather than failing auth — this is the "retain-old-cache-on-failure" requirement; only a *first-ever* fetch failure (no prior cache) surfaces as an auth failure.
- P1-3: `TimeoutLayer` failure → `504 Gateway Timeout` via the standard error envelope (not tower's default plain-text timeout response — must be mapped to `ErrorResponse`/`ErrorDetail` per `src/error.rs`'s existing shape, with `message_key = "moira.error.request_timeout"`). `CatchPanicLayer` failure → `500` via the same envelope shape, `message_key = "moira.error.internal_panic"` (new key) or reuse `"moira.error.internal_error"` (existing, code `Internal`) — prefer reusing the existing key/code unless a distinct one is genuinely useful for operators filtering logs, in which case add exactly one new i18n key.

---

## Detailed Implementation

### 1. `src/security/` — new idempotency hasher (P1-1)

- Add `src/security/idempotency.rs` (new file, colocated with `api_keys.rs`, `masking.rs`, `crypto.rs`, `auth.rs` — follow the existing module layout in `src/security/mod.rs`'s re-exports).
- Define:
  ```rust
  #[derive(Debug, Clone)]
  pub struct IdempotencyHasher {
      pepper: Vec<u8>,
      pepper_version: String, // e.g. "v1"
  }

  impl IdempotencyHasher {
      pub fn new(pepper: impl Into<Vec<u8>>, pepper_version: impl Into<String>) -> Self { ... }

      /// Produces "{pepper_version}:{base64url(hmac_sha256(pepper, bytes))}"
      pub fn hash(&self, bytes: &[u8]) -> String { ... }

      /// Verifies `bytes` against a stored hash produced by either this hasher
      /// (versioned HMAC) or the legacy unkeyed `secret_fingerprint`/`request_hash`
      /// function, so old rows keep verifying during migration.
      pub fn verify(&self, bytes: &[u8], stored: &str) -> bool {
          match stored.split_once(':') {
              Some((version, _)) if version == self.pepper_version => {
                  self.hash(bytes) == stored
              }
              Some(_) => false, // a different/rotated pepper version: exact recompute not possible here without that version's pepper; see note below
              None => crate::security::masking::secret_fingerprint(bytes) == stored, // legacy unkeyed format
          }
      }
  }
  ```
  Use `hmac::Hmac<sha2::Sha256>` (the `hmac` crate is **not** currently in `Cargo.toml` — add `hmac = "0.12"`, compatible with the pinned `sha2 = "0.10"` at `Cargo.toml:26`) exactly as `ApiKeyHasher` uses `Argon2` for its own peppered comparison, i.e. compute `HMAC-SHA256(key = pepper, message = bytes)` then base64url-encode (reuse `URL_SAFE_NO_PAD` from `base64`, already a dependency, same encoding `secret_fingerprint` uses) so the output format stays visually consistent with the existing fingerprint format, just now keyed and versioned.
  - **Multi-version pepper rotation note:** unlike `ApiKeyHasher` (which stores `pepper_version` per-row alongside a hash produced with *that* version's pepper, and the caller supplies the raw key to re-derive), `IdempotencyHasher` here only ever needs to verify against the **currently active** pepper for new-format hashes (idempotency records expire in 24h — see Backward Compatibility above — so there is no practical need to support verifying against a *previous, rotated* HMAC pepper the way `ApiKeyHasher` must support previous Argon2id peppers for long-lived API keys). Document this explicitly in the module: **if pepper rotation is needed operationally, the safe procedure is to accept that in-flight (unexpired) idempotency claims made with the old pepper will simply fail to replay-match after rotation and fall through to normal (non-idempotent) processing — never a security issue, only a rare duplicate-processing window during the rotation, already an accepted risk given P0-2's finding that these routes' idempotency isn't real replay-safety anyway for the RAG/conversation surface, and for `/v1/responses` and admin commands (where real replay matters) operators should avoid rotating the idempotency pepper during active traffic, or accept the narrow duplicate-window, exactly as recommended for the `ApiKeySettings` pepper today.**
- Add unit tests in the same file's `#[cfg(test)]` module, following `api_keys.rs`'s test style exactly: `new_versioned_hash_round_trips`, `legacy_unkeyed_hash_still_verifies`, `hash_never_recoverable_to_plaintext` (mirrors `masking.rs`'s `fingerprints_are_stable_without_exposing_secret` test).

### 2. `src/config/settings.rs` — new `IdempotencySettings` section

- Add:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct IdempotencySettings {
      pub pepper_base64: Option<String>,
      pub pepper_version: String,
      pub allow_insecure_dev_pepper: bool,
  }
  ```
  placed near `ApiKeySettings` (`:118-123`), with an `impl IdempotencySettings { pub fn pepper_bytes(&self) -> Result<Vec<u8>, AppError> { ... } }` copying `ApiKeySettings::pepper_bytes`'s exact structure (`:482-495`) — same base64-decode-or-dev-fallback-or-error logic, same error message pattern (`"MOIRA_IDEMPOTENCY__PEPPER_BASE64 must be set"`).
- Add `pub idempotency: IdempotencySettings` field to `Settings` (`:11-41`), `#[serde(default)]`.
- Add an `impl Default for IdempotencySettings` mirroring `impl Default for ApiKeySettings` (`:573-579`, which sets `allow_insecure_dev_pepper: true` for dev/test) so existing tests that construct `Settings` via defaults don't break — grep `Settings::default()`/fixture construction in `tests/support/mod.rs` to confirm no other fixture needs the new field spelled out.
- Add JWKS-hardening knobs, either as new fields on the existing `JwtAuthSettings`/`CallerAuthSettings` structs or (preferred, to avoid duplicating the same five fields twice) as a new shared struct:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  pub struct JwksFetchSettings {
      pub max_response_bytes: usize,       // e.g. default 262_144 (256KiB) — JWKS documents are small
      pub timeout_ms: u64,                 // e.g. default 3_000
      pub allow_insecure_dev_urls: bool,    // false by default; permits http:// and private IPs when true
  }
  ```
  Embed one instance in `AuthSettings` (`:90-96`, alongside `admin`/`caller`) as `pub jwks: JwksFetchSettings` since both `AdminAuthenticator` and `CallerAuthenticator` (and the trusted-issuer path) should share one fetch policy rather than three divergent ones — simplifies config surface and avoids the three call sites drifting.

### 3. `src/app/state.rs` — wire the new hasher and hardened HTTP client

- Construct `let idempotency_hasher = IdempotencyHasher::new(settings.idempotency.pepper_bytes()?, settings.idempotency.pepper_version.clone());` alongside the existing `key_hasher` construction (`:51-55`), add `pub idempotency_hasher: IdempotencyHasher` to the `AppState` struct (`:20-38`) and its constructor's final `Ok(Self { ... })` block.
- The shared `reqwest::Client` (`:43-46`) currently has **no timeout configured at all**. This is itself part of P1-3's "no timeout" concern as it applies to *outbound* calls, but the JWKS-specific timeout (P1-2) should be enforced **per-request** via `reqwest::RequestBuilder::timeout(Duration)` at the JWKS call sites specifically (not a blanket client-level timeout, since the same `AppState.http` client is also used for provider execution calls in `src/application/execution.rs`, which have their own, different timeout semantics owned by the execution deadline system — out of scope here, do not touch). Do not add a client-level default timeout in this plan; scope the timeout to the new SSRF-hardened JWKS fetch helper only, to avoid an unrelated behavioral change to provider-call timeouts.

### 4. `src/security/auth.rs` — SSRF-hardened JWKS fetch (P1-2)

- Add a new shared function, e.g. `async fn fetch_jwks_hardened(http: &Client, url: &str, settings: &JwksFetchSettings) -> Result<JwkSet, AppError>`, placed in `auth.rs` itself (or a new `src/security/ssrf.rs` if the DNS/IP-range logic is substantial enough to warrant its own file/tests — prefer a separate file for testability of the pure IP-classification logic independent of any network I/O):
  1. Parse `url` with the `url` crate (already a direct dependency, `Cargo.toml:37` `url = "2"`) to inspect the scheme/host before handing off to `reqwest` — reject any scheme other than `https` unless `settings.allow_insecure_dev_urls` is `true`.
  2. Extract the host; if it's already a literal IP, classify it directly. If it's a hostname, resolve via `tokio::net::lookup_host((host, port_or_443)).await` and classify **every** resolved address (a hostname can resolve to multiple IPs; reject if *any* resolved address is in a denied range, to be conservative).
  3. Denial classification (implement as a small pure function `fn is_denied_ip(ip: IpAddr) -> bool` with dedicated unit tests for each range): loopback (`is_loopback()`), link-local (`is_link_local()` for v4; `is_unicast_link_local()` for v6, stable in `std` — check MSRV/std version availability at implementation time and use the `ipnet`/manual-range fallback if the needed `std::net::Ipv6Addr` method isn't stabilized in the pinned Rust toolchain), private (`is_private()` for v4; ULA `fc00::/7` for v6 — implement manually if no stable std method), unspecified (`is_unspecified()`), multicast (`is_multicast()`), and IPv4-mapped-in-IPv6 (`to_ipv4_mapped()`, then re-run the v4 checks on the unwrapped address — this closes the `::ffff:169.254.169.254` bypass explicitly called out in Architecture above).
  4. If `allow_insecure_dev_urls` is `true` (dev/test only, defaulting `false`), skip steps 1-3's rejections entirely and log a warning-level trace event so it's visible in dev logs that the protection is disabled.
  5. Issue the GET with `.timeout(Duration::from_millis(settings.timeout_ms))`, and enforce `max_response_bytes` by reading the body as a stream and aborting once the cap is exceeded (do not rely solely on `Content-Length`, since that header can be absent or lie — use `reqwest::Response::bytes_stream()` and accumulate with a running counter, erroring out once the cap is crossed, rather than `.json()` directly which buffers unboundedly).
  6. Check `Content-Type` starts with `application/json` (JWKS responses; some IdPs use `application/jwk-set+json` — allow both) before attempting to parse; reject with a clear error otherwise rather than attempting `serde_json` parsing of arbitrary content.
  7. Parse into `JwkSet` (existing `jsonwebtoken`/whatever crate already provides `JwkSet` — unchanged).
  8. On any rejection at steps 1-6, write an audit-log entry (mirror the existing `AuditLogInsert` pattern — `insert_audit(AuditLogInsert { ... })` at `src/application/admin.rs:1310` and the builder at `:1412-1413`) recording `resource_type = "jwt_issuer"`, the rejected URL (safe to log — URLs are not secrets, unlike JWKS *contents* which are also not secrets since they're public keys, but keep the audit entry to just the URL/reason/timestamp per existing audit-entry shape), and the specific denial reason (scheme/IP-range/size/content-type/timeout) as `AuditResult`/metadata.
- **Singleflight:** wrap the fetch-on-cache-miss path in a per-issuer (keyed by `jwks_url` or `issuer` identifier) `tokio::sync::Mutex` or a small `HashMap<String, Arc<tokio::sync::Semaphore>>`/`tokio::sync::OnceCell`-per-key structure held in `AuthService`/`TrustedIssuerAuthenticator`'s existing `jwks_cache` (`RwLock<HashMap<...>>`, referenced at `:387`) — the simplest correct approach: change the cache-miss path to (a) acquire a per-issuer async lock, (b) re-check the cache under that lock (another concurrent request may have already refreshed it while this one was waiting), (c) fetch only if still stale, (d) release. This avoids adding a new dependency (no need for the `singleflight` crate) using only `tokio::sync` primitives already in use elsewhere in the codebase.
- **Retain-old-cache-on-failure:** change `jwks()`'s error path so that if `fetch_jwks_hardened` returns `Err` and a cached entry exists (even if expired past its 300s TTL), return the stale cached value instead of propagating the error — only propagate the error if there is no cached entry at all (first-ever fetch for this issuer). Log the fallback at `warn` level via `tracing`.
- Apply the identical hardened-fetch call at the `validate_static_jwt` call site (`:484-506`, fetch at `:497-503`) — this path currently has **no caching at all** (audit-noted, confirmed during grounding), which is both a P1-2 gap (repeated unhardened fetches) and a latent availability/cost issue; adding the same singleflight+cache treatment here (keyed by the static `jwks_url` string, using a similarly-shaped cache owned by `AdminAuthenticator`/`CallerAuthenticator`) is in-scope for this plan since it's the same finding, not a separate refactor — implement it as a small shared cache struct reused by both `TrustedIssuerAuthenticator` and the two static authenticators, rather than three divergent copies.

### 5. `src/lib.rs` — production middleware stack (P1-3)

- Add the `"timeout"` and `"catch-panic"` features to `tower-http` (`Cargo.toml:32`, currently `features = ["cors", "request-id", "trace", "util"]`).
- `TimeoutLayer`: apply at the router level, but **explicitly exempt or override** `/api/v1/responses/stream` — grep of `text/event-stream` confirms it is the only SSE route today (`src/http/public.rs:92,368`) — since SSE connections are intentionally long-lived. Axum/tower's per-route `.layer()` on a specific route (rather than the whole router) is the mechanism — apply `TimeoutLayer::new(Duration::from_secs(N))` (N sized to `RuntimeSettings.maximum_execution_timeout_seconds`, `src/config/settings.rs:154`, default 600, so the HTTP-level timeout is a small buffer *above* the execution-level deadline, e.g. `maximum_execution_timeout_seconds + 30` — the exact buffer is a product/ops sign-off number, not self-evident) to the non-streaming router, and either omit `TimeoutLayer` entirely on the streaming sub-router or give it a much larger ceiling reflecting the longest acceptable stream duration.
- `CatchPanicLayer`: use `tower_http::catch_panic::CatchPanicLayer::custom(handle_panic)` where `handle_panic` is a new function in `src/lib.rs` converting the caught panic payload into the standard `ErrorResponse`/`ErrorDetail` shape (`src/error.rs`) with status `500`, code `"internal_error"` (reuse existing `AppError::Internal`'s code string for consistency), and a fixed generic message — **never** interpolate the raw panic payload into the client-visible `message` field; log it server-side only via `tracing::error!`.
- Secure headers: extend `secure_response_headers` (`:92-105`) with:
  - `X-Frame-Options: DENY` (defense-in-depth; Moira has no browser-rendered UI itself, but this costs nothing and is standard).
  - `Content-Security-Policy: default-src 'none'` (all responses are `application/json`/`text/event-stream`/`text/plain` — no script/style/img context ever needed).
  - `Strict-Transport-Security: max-age=63072000; includeSubDomains` — **gate this behind a config check**: `DeploymentSettings.environment` exists (`src/config/settings.rs:62-63`, `DeploymentEnvironment` enum with a `Production` variant) and is the natural gate; do not unconditionally send HSTS in dev/test where local HTTP is normal, or it can cause browser-side HTTPS-pinning problems for local tooling hitting `http://localhost`.
- Per-route body limits: replace the single global `.layer(DefaultBodyLimit::max(512 * 1024))` (`:42`) with route-group-scoped limits:
  - Public execution routes (`/api/v1/responses*`): `DefaultBodyLimit::max(settings.public_api.maximum_request_bytes as usize)` — finally wiring the already-configured-but-unused policy value (`PublicApiSettings.maximum_request_bytes`, `:170`) to the actual enforcement layer, closing the audit's specific "align configurable policy with actual layer" complaint.
  - Conversation/memory/RAG public+admin routes: keep a sane fixed cap (e.g. 1MiB, matching the current effective global default) unless/until those routes get their own configured policy value — do not invent a new config field for these in this plan (out of scope; flag as a plan-04-or-later nice-to-have if wanted).
  - Admin routes generally: a larger cap than public routes is reasonable (admin payloads can include larger metadata/policy documents) — pick a concrete number (e.g. 2MiB) and document the rationale in a code comment; this is a judgment call worth flagging for product/ops sign-off on the exact numbers rather than treating them as self-evidently correct.
  - Implement via Axum's per-route `.layer(DefaultBodyLimit::max(N))` applied to route sub-groups in `pub fn router()` (`src/http/mod.rs:16`) — determine at implementation time whether route groups are already split into nameable sub-routers that a `.layer()` can attach to cleanly, or whether the router needs light restructuring *purely for this purpose* (acceptable since it's directly load-bearing for this finding, not an unrelated refactor).
- Redacted tracing span: replace the bare `TraceLayer::new_for_http()` (`:43`) with `TraceLayer::new_for_http().make_span_with(...)` using a custom span-builder closure that includes method/path/request-id but explicitly **excludes** the `Authorization` header, any `X-Api-Key`/system-key/consumer-key header, and request/response bodies (the default `TraceLayer` does not log bodies already, so this is chiefly about ensuring headers aren't captured — verify by grep/read of tower-http's `DefaultMakeSpan` behavior at implementation time, since the "no unrelated refactor" instruction means only add redaction, don't restructure the whole tracing setup).
- No-compression on once-only-secret responses: Moira currently has **no `CompressionLayer` at all** in the stack (confirmed by grep — `tower_http::compress` is not imported anywhere in `src/lib.rs`), so there is currently nothing to "disable" compression on. Document this finding precisely: **this sub-item of P1-3 is currently a non-issue because compression isn't enabled anywhere**; if a future plan adds response compression, it must exclude routes returning `ApiKeySecretResponse` (`POST /api/v1/admin/system-keys`, `.../rotate`, `POST /api/v1/admin/consumer-keys`, `.../rotate` — the same four routes covered by the existing `once_only_key_responses_use_the_secret_envelope` OpenAPI test in `src/http/mod.rs`). Add a code comment at the top of `build_router` noting this constraint explicitly so it isn't missed when compression is eventually added, and add a regression test (see Verification) asserting no `Content-Encoding` header is ever present on these four routes today, which will also catch any future accidental compression addition.

### 6. Call-site swaps for P1-1 (idempotency hasher)

- `src/infra/repositories/public.rs:733-750` (`idempotency_record` helper): change signature to accept `&IdempotencyHasher` and replace `request_hash(key.as_bytes())` (`:741`) with `hasher.hash(...)`. In the caller (`src/application/public.rs`), swap `normalized_request_hash` (`:1880-1884`) to hash via the `IdempotencyHasher` (`PublicExecutionService` already holds `state: AppState`, `:41-42`).
- `src/application/admin_command.rs`: swap `AdminCommandSpec::request_hash` (`:96-106`) and the key hash at `:163` to the new hasher — `AdminCommandRunner` holds only `PgAdminRepository`, so thread the `IdempotencyHasher` (or `AppState`) into `AdminCommandRunner`'s constructor and through `AdminCommandSpec::request_hash`'s signature as needed. This covers the ten atomic admin commands.
- `src/application/runtime_admin.rs`: swap the key/body hashes in `idempotency_replay` (`:636-637`) and `record_idempotency` (`:682,685`) and its private `normalized_request_hash` (`:717-721`) the same way.
- `src/application/conversation.rs`: swap all **seven** `request_hash(` content-hash call sites (`:286,352,395,455,546,876,967`) to `self.state.idempotency_hasher.hash(...)` (`ConversationService` holds `state: AppState`, `:52-56`). These are write-only fingerprints with no read-side comparison (verified by grep), so no `verify()` fallback is needed for them.
- `src/application/public.rs:1597`: swap the `output_hash` fingerprint the same way.
- **Read-side (verification/replay) changes:** replace the three stored-vs-fresh equality checks with `hasher.verify(bytes, &stored_value)` so both legacy and new-format stored values are handled correctly: `src/application/public.rs:1032` (`existing.request_hash != request_hash_value`), `src/infra/repositories/admin.rs:602` (`record.request_hash != claim.request_hash` — the repository compares two caller-supplied strings, so either thread the hasher/raw bytes into `AdminIdempotencyClaim` or perform the verify in `admin_command.rs` before/after the claim; choose whichever keeps the repository free of hashing policy — prefer verifying in the application layer), and `src/application/runtime_admin.rs:645` (`record.request_hash != request_hash`). Note the lookup key (`idempotency_key_hash`) is also an **index key**, not just a compared value (`get_idempotency_record` at `src/infra/repositories/admin.rs:1732` and the unique index on `(idempotency_key_hash, actor_fingerprint, operation)` per `0003:360-361`): a legacy row's key hash will not equal the new HMAC of the same key, so a replay lookup must fall back to also querying by the legacy unkeyed key hash when the versioned lookup misses (dual-lookup, mirroring the dual-verify) — or accept the ≤24h non-replay window for legacy rows and document it; **decide explicitly, default to dual-lookup for `/v1/responses` and admin commands where replay is contractual**.
- Leave `src/security/masking.rs`'s `request_hash`/`secret_fingerprint` functions **in place, unchanged** — they remain the correct tool for non-secret-bearing fingerprinting (e.g., API key fingerprints for display/lookup purposes, which are a different, already-appropriate use of unkeyed SHA-256 since fingerprints are meant to be a stable public identifier, not a secret verifier) and are still needed for the legacy-hash verification fallback inside `IdempotencyHasher::verify`.

### 7. Tests — **both layers are mandatory** (`plans/CONVENTIONS.md` §3)

This plan delivers a **unit layer** (`#[cfg(test)] mod tests` beside the code, no database) **and** an **e2e layer** (files under `tests/` driving the real HTTP surface against real PostgreSQL 16 + pgvector via `tests/support/mod.rs`). A single-layer plan is unmergeable. Note during grounding: `src/lib.rs` already has a `#[cfg(test)] mod tests` (`:107`) with three router/CORS tests, and `tests/http_error_contract.rs` currently holds exactly **one** test (`error_response_body_includes_i18n_fields_and_request_id`) which builds `AppState::new(Settings::default(), None)` — i.e. **without a database**. Under `CONVENTIONS.md` §3 that file is a *unit-grade* HTTP test, not an e2e one; this plan therefore adds real e2e files rather than only extending it.

#### 7a. Unit layer — no database, `#[cfg(test)] mod tests` beside the code

**`src/security/idempotency.rs`** (new file's own test module, styled after `api_keys.rs`'s and `masking.rs`'s existing modules):

| Test function | Asserts |
|---|---|
| `versioned_hash_round_trips_under_the_active_pepper` | `hash(b)` then `verify(b, &h)` is `true`, and the output is prefixed `"v1:"` |
| `hash_output_fits_the_varchar_128_column` | length ≤ 128, protecting the "no migration needed" claim (`"v1:" + 43 base64url chars = 46`) |
| `legacy_unkeyed_hash_still_verifies` | a value produced by `masking::secret_fingerprint` (no `:` prefix) verifies `true` — the dual-read contract |
| `verify_rejects_a_hash_from_a_different_pepper_version` | a `"v2:"`-prefixed stored value returns `false` under a `"v1"` hasher (no silent cross-version acceptance) |
| `hash_changes_when_the_pepper_changes` | two hashers with different peppers produce different digests for identical input — proves the pepper is actually keyed in, the whole point of P1-1 |
| `hash_never_reveals_plaintext` | mirrors `masking.rs`'s `fingerprints_are_stable_without_exposing_secret`: the output contains no substring of the input |

**`src/security/ssrf.rs`** (new file — prefer this over inlining in `auth.rs`, precisely so the IP classification is unit-testable without network I/O):

| Test function | Asserts |
|---|---|
| `loopback_addresses_are_denied` | `127.0.0.1`, `127.1.2.3`, `::1` |
| `rfc1918_private_ranges_are_denied` | `10.0.0.1`, `172.16.0.1`, `172.31.255.254`, `192.168.1.1` |
| `link_local_and_cloud_metadata_addresses_are_denied` | `169.254.0.1` **and `169.254.169.254` as its own named assertion** (AWS/GCP/Azure/Alibaba metadata) |
| `ipv6_unique_local_and_link_local_are_denied` | `fc00::1`, `fd12:3456::1`, `fe80::1` |
| `ipv4_mapped_ipv6_metadata_address_is_denied` | `::ffff:169.254.169.254` — the explicit bypass this plan calls out |
| `unspecified_and_multicast_addresses_are_denied` | `0.0.0.0`, `::`, `224.0.0.1`, `ff02::1` |
| `public_addresses_are_allowed` | `1.1.1.1`, `8.8.8.8`, `2606:4700:4700::1111` are **not** denied — guards against an over-broad list breaking real IdPs |
| `non_https_scheme_is_rejected` | `http://idp.example.com/jwks` rejected when `allow_insecure_dev_urls == false` |
| `non_https_scheme_is_permitted_under_the_dev_override` | same URL accepted when the flag is `true` — proves the escape hatch works, so nobody disables the whole check to unblock local dev |

**`src/lib.rs`** (extend the existing `mod tests` at `:107`; these use `AppState::new(Settings::default(), None)` + `tower::ServiceExt::oneshot`, no database):

| Test function | Asserts |
|---|---|
| `secure_response_headers_include_frame_options_and_csp` | `X-Frame-Options: DENY` and `Content-Security-Policy: default-src 'none'` present, alongside the three pre-existing headers set at `src/lib.rs:92-105` |
| `hsts_is_absent_outside_production` | no `Strict-Transport-Security` under a non-`Production` `DeploymentEnvironment` (`src/config/settings.rs:46`) |
| `hsts_is_present_under_a_production_deployment_environment` | present, `max-age=63072000; includeSubDomains`, when the environment is `Production` |
| `panic_response_body_contains_no_panic_payload` | the `handle_panic` mapper's output is a valid `ErrorResponse` with `code == "internal_error"`, a fixed generic `message`, and **no substring of the panic payload** |
| `router_still_builds_with_the_full_middleware_stack` | extends the existing `router_builds_with_phase_one_routes`, catching layer-ordering compile/type breakage early |

**`src/config/settings.rs`** (extend the existing test module — grep confirms it already tests `api_keys.allow_insecure_dev_pepper` production rejection at `:703,746`):

| Test function | Asserts |
|---|---|
| `idempotency_pepper_bytes_decodes_base64` | valid base64 decodes to the expected bytes |
| `idempotency_pepper_bytes_uses_the_dev_fallback_when_allowed` | mirrors `ApiKeySettings::pepper_bytes`'s `None if self.allow_insecure_dev_pepper => Ok(vec![11; 32])` (`:491`) |
| `production_rejects_allow_insecure_dev_idempotency_pepper` | `Settings::validate` (`:233`) pushes a violation, mirroring the existing `api_keys` check at `:355-357` |
| `production_rejects_allow_insecure_dev_jwks_urls` | same, for the new `JwksFetchSettings.allow_insecure_dev_urls` |

**`src/i18n/catalog/mod.rs`** (extend the existing `mod tests` — it already has `response_catalog_keys_are_unique` and `default_messages_can_be_resolved`):

| Test function | Asserts |
|---|---|
| `middleware_error_keys_are_catalogued` | `is_known_key` is `true` for `moira.error.request_timeout`, `moira.error.payload_too_large`, `moira.error.jwks_url_rejected`, and (reused) `moira.error.internal_error` + `moira.error.unauthorized`; and `default_message_for_key` returns a **non-empty** English default for each |

#### 7b. E2E layer — three new files under `tests/`, real HTTP + real PostgreSQL 16 + pgvector

All three follow `tests/support/mod.rs` (`mod support;`, `LifecycleFixture`, `MoiraHttpServer::start(state)` binding a real `127.0.0.1:0` listener, `reqwest` against `base_url`), imitating `tests/admin_idempotency.rs` (9 tests, `post(...)` helpers at `:90`/`:168`) and `tests/execution_lifecycle.rs` (14 tests). All inherit the fail-closed rule: `MOIRA_TEST_DATABASE_URL` absent under `CI` ⇒ `panic!` (`tests/support/mod.rs:427-441`).

**`tests/http_middleware_contract.rs`** (P1-3):

| Test function | Asserts |
|---|---|
| `oversized_public_body_is_rejected_with_413_and_the_standard_envelope` | `413`, `error.code == "payload_too_large"`, `error.message_key == "moira.error.payload_too_large"`, non-empty `error.message`, populated `error.request_id` — this is the test that proves the new envelope mapping, since today the response is bare plain text |
| `public_body_at_the_configured_maximum_request_bytes_boundary_is_accepted` | a body of exactly `PublicApiSettings.maximum_request_bytes` (`src/config/settings.rs:168`, default `1_048_576`) succeeds — proves the **configured** value is the enforced one, closing the 512KiB-vs-1MiB mismatch at `src/lib.rs:42` |
| `admin_routes_enforce_their_own_distinct_body_limit` | an admin body above the public limit but below the admin limit succeeds, and one above the admin limit gets the same `413` envelope — proves the limits are genuinely per-route, not one global layer |
| `slow_non_streaming_request_returns_504_with_the_request_timeout_key` | `504`, `error.code == "request_timeout"`, `error.message_key == "moira.error.request_timeout"`, non-empty `message`. Driven by a `#[cfg(test)]`-gated slow route, or by a mock provider held open past the timeout via `support::mock_openai` — **prefer the mock-provider route** so no test-only surface is added to the production router |
| `panicking_handler_returns_500_envelope_without_panic_payload` | `500`, valid `ErrorResponse`, `error.code == "internal_error"`, and the body contains **no** substring of the panic message. Requires a `#[cfg(test)]`-gated panicking route; it must not exist in release builds |
| `sse_stream_outlives_the_non_streaming_timeout` | **the single highest-risk compatibility check in this plan.** Opens `/api/v1/responses/stream`, holds it longer than the non-streaming `TimeoutLayer` ceiling, and confirms the stream completes normally. Must use an **acknowledgement gate** — the mock provider signals each chunk via `tokio::sync::Notify`/`Barrier`, and the test waits on that signal — **never `sleep()`** (`CONVENTIONS.md` §3, finding P2-12) |
| `security_headers_are_present_on_a_live_response` | the e2e counterpart to the unit header test: `X-Frame-Options`, `Content-Security-Policy`, and the three pre-existing headers survive the full layer stack over a real socket (layer ordering can strip headers in ways `oneshot` does not reveal) |
| `once_only_secret_responses_carry_no_content_encoding` | the four `ApiKeySecretResponse` routes (the same set covered by `once_only_key_responses_use_the_secret_envelope`, `src/http/mod.rs:512`) return **no** `Content-Encoding` header — the forward-looking regression guard for whenever compression is added |
| `middleware_error_responses_carry_non_empty_message_key_and_message` | `CONVENTIONS.md` §4 rule 5 over the live wire: for each of the `413`/`504`/`500` responses above, `message_key` and `message` are non-empty and `moira::i18n::is_known_key(message_key)` holds |

**`tests/jwks_hardening.rs`** (P1-2). Upstream IdP is a local stub built on `tokio::net::TcpListener` (the pattern `tests/support/mod.rs` already uses at `:86`) with an `AtomicUsize` hit counter — **no new mock-HTTP dev-dependency is required**; do not add `wiremock` for this.

| Test function | Asserts |
|---|---|
| `jwks_url_with_http_scheme_is_rejected_at_issuer_registration` | `POST /api/v1/admin/jwt-issuers` → `400`, `error.code == "jwks_url_rejected"`, `message_key == "moira.error.jwks_url_rejected"` |
| `jwks_url_resolving_to_a_private_address_is_rejected_at_issuer_registration` | same envelope for a hostname resolving into a denied range — the DNS-resolved check, not a string match |
| `oversized_jwks_response_is_abandoned_before_full_buffering` | the stub streams past `max_response_bytes`; the fetch errors and the stub observes the connection closed **before** it finished writing (proves streaming enforcement, not a post-hoc `Content-Length` check) |
| `non_json_jwks_content_type_is_rejected` | `text/html` upstream is rejected without a `serde_json` parse attempt; `application/jwk-set+json` is **accepted** (both must be asserted, or the fix breaks real IdPs) |
| `slow_jwks_response_is_abandoned_at_the_configured_timeout` | stub holds the response open; the fetch aborts at `timeout_ms`. Gate the stub's release on a `Notify` the test controls — **not** a `sleep()` |
| `concurrent_cache_miss_fetches_are_singleflighted_to_one_upstream_call` | N concurrent auth requests for the same issuer during a cold cache ⇒ the stub's `AtomicUsize` reads exactly `1`. Fan-out coordinated by `tokio::sync::Barrier` (the pattern at `tests/admin_idempotency.rs:518,1028,1128`); the stub signals arrival via `Notify` — **acknowledgement gates only, no `sleep()`** |
| `failed_refresh_serves_the_last_known_good_cached_jwks` | warm the cache, make the stub fail, force a refresh past TTL, and confirm auth still succeeds from the stale entry |
| `first_ever_fetch_failure_surfaces_as_an_auth_failure` | the negative half: with **no** prior cache, a failed fetch does **not** fail open |
| `jwks_rejection_is_audited_without_leaking_the_resolved_ip_to_the_caller` | an `audit_log` row exists for the rejection, **and** the caller-visible response body contains neither the resolved IP nor the denial reason — the SSRF-oracle guard. At request-verification time the response must be the generic `moira.error.unauthorized` |

**`tests/idempotency_hash_migration.rs`** (P1-1) — the dual-read/dual-lookup contract is the riskiest part of P1-1 and is untestable without a real database:

| Test function | Asserts |
|---|---|
| `legacy_unkeyed_row_still_replays_after_the_hmac_switch` | seed an `idempotency_records` row **directly via `sqlx`** using the old `masking::secret_fingerprint` format for both `idempotency_key_hash` and `request_hash`, then replay the same request over HTTP and confirm the stored response is returned — this is the dual-**lookup** proof (the key hash is an index key under the unique index on `(idempotency_key_hash, actor_fingerprint, operation)`, `migrations/0003_security_foundation.sql:360-361`), not merely dual-verify |
| `new_records_persist_the_versioned_hmac_prefix` | after a fresh idempotent admin command, `SELECT request_hash, idempotency_key_hash` both start with `"v1:"` |
| `admin_command_replay_matches_under_the_new_hasher` | end-to-end replay across the ten atomic admin commands' shape (reuse the fixture style of `tests/admin_idempotency.rs:245`) |
| `body_mismatch_under_the_new_hasher_still_returns_idempotency_conflict` | same key + different body ⇒ `409`, `error.code == "idempotency_conflict"`, `message_key == "moira.error.idempotency_conflict"` — proves hardening did not weaken the existing conflict contract |
| `concurrent_same_key_claims_still_produce_one_ledger_row` | mirrors the existing `concurrent_same_key_create_has_one_resource_audit_and_ledger` (`tests/admin_idempotency.rs:507`) under the new hasher, using the same `Barrier` gate — **no `sleep()`** |
| `conversation_content_hashes_are_written_in_the_versioned_format` | the seven `src/application/conversation.rs` call sites (`:286,352,395,455,546,876,967`) now persist `"v1:"`-prefixed `content_hash` values, and the rows still fit `varchar(128)` |

#### 7c. Concurrency discipline (binding)

Three tests above are interleaving tests: `concurrent_cache_miss_fetches_are_singleflighted_to_one_upstream_call`, `concurrent_same_key_claims_still_produce_one_ledger_row`, and `sse_stream_outlives_the_non_streaming_timeout`. All three **must** use acknowledgement gates (`tokio::sync::Barrier` / `Notify` / channel handshakes). New `sleep()`-based interleaving is rejected in review (`CONVENTIONS.md` §3, finding P2-12) — note that `tests/admin_idempotency.rs` still contains legacy `sleep` usage at `:977,1259`; **do not copy that pattern**, and do not "fix" those lines here either (that cleanup belongs to plan 06's test-hygiene scope).

---

## Multi-Agent Workflow

**Coordinator responsibilities.** This plan has three independent findings (P1-1, P1-2, P1-3) that touch almost entirely disjoint files except for `AppState` (`src/app/state.rs`) and `Settings` (`src/config/settings.rs`), which every finding needs to extend. The coordinator's key job is serializing the shared-file edits into one wave so no two agents race on the same struct definition.

### Wave 1 — shared config & state scaffolding (single agent, sequential prerequisite for everything else)
- **Agent A:** `src/config/settings.rs` (add `IdempotencySettings`, `JwksFetchSettings`, wire into `Settings`/`AuthSettings`) **and** `src/app/state.rs` (construct `IdempotencyHasher`, add to `AppState`). One agent, two files, because both changes are small, tightly coupled (the struct fields Agent A adds in `settings.rs` are immediately consumed in `state.rs` in the same wave), and doing them together avoids a merge-order dependency between two separate agents.
- Checkpoint: `cargo build` (not full test) must succeed before Wave 2 starts, since every subsequent wave depends on `AppState.idempotency_hasher` and the new settings types existing.

### Wave 2 — parallel, disjoint files
- **Agent B — P1-1 idempotency hashing:** `src/security/idempotency.rs` (new file), plus call-site swaps in `src/infra/repositories/public.rs`, `src/application/public.rs`, `src/application/admin_command.rs`, `src/application/runtime_admin.rs`, `src/application/conversation.rs`, and (if the read-side verify lands in the repository rather than the application layer) `src/infra/repositories/admin.rs` — the exact sites enumerated in Findings P1-1 and Detailed Implementation item 6. This agent owns the full P1-1 finding end to end.
- **Agent C — P1-2 SSRF-hardened JWKS:** `src/security/auth.rs` (and/or new `src/security/ssrf.rs`). This agent owns the full P1-2 finding end to end, including the audit-log write (reusing existing `AuditLogInsert` patterns — read-only reference into `src/application/admin.rs` to copy the pattern, no edits to that file needed for this purpose).
- **Agent D — P1-3 middleware stack:** `src/lib.rs`, and `src/http/mod.rs` **only** for the minimal per-route body-limit layering change (grep first to confirm the exact insertion points; if `src/http/mod.rs`'s router-composition needs restructuring to expose sub-router groups for per-route layering, that restructuring is owned by Agent D alone).

B, C, and D touch entirely disjoint files (`security/idempotency.rs` + the six call-site files vs. `security/auth.rs`/`security/ssrf.rs` vs. `lib.rs` + `http/mod.rs`) — verify no overlap before dispatch; if Agent D's `http/mod.rs` touch turns out to overlap with anything Agent B or C needs (unlikely, since B/C don't touch routing), resolve by giving Agent D exclusive ownership of `http/mod.rs` for this plan.

### Wave 2b — i18n catalog (single agent, may run in parallel with B/C/D)
- **Agent I — i18n:** `src/i18n/catalog/errors.rs` (the three new entries: `request_timeout`, `payload_too_large`, `jwks_url_rejected`), `docs/i18n-response-catalog.json` (identical mirrored objects, same positions), and the `middleware_error_keys_are_catalogued` test in `src/i18n/catalog/mod.rs`'s existing test module. These files are touched by no other agent, so there is no conflict — but the entries must exist **before** Agents C and D can reference the codes, so dispatch Agent I at the start of Wave 2 and require its completion before C's and D's error-raising code merges. `src/i18n/catalog/notices.rs` is **not** touched (no new notice strings).

### Wave 3 — tests: **two layers, both mandatory** (sequential, after Wave 2 fully merged)
- **Agents B, C, D write their own inline `#[cfg(test)]` unit tests during Wave 2** (standard Rust practice; keeps tests beside the code). That covers Detailed Implementation §7a for `src/security/idempotency.rs` (B), `src/security/ssrf.rs` (C), and `src/lib.rs` (D). Agent A owns the `src/config/settings.rs` unit tests in Wave 1, since it owns that file.
- **Agent E — e2e layer (§7b) only:** the three new files `tests/http_middleware_contract.rs`, `tests/jwks_hardening.rs`, `tests/idempotency_hash_migration.rs`. Requires real PostgreSQL 16 + pgvector via `MOIRA_TEST_DATABASE_URL`; follows `tests/support/mod.rs`. E does **not** duplicate any unit test.
- **Agent E also owns the `#[cfg(test)]`-gated panicking and slow test routes** if they are needed — they must be `cfg`-gated so they cannot exist in a release build, and Wave 4 verifies this explicitly.
- **Merge gate:** both layers must be present. A PR carrying only unit tests, or only e2e tests, is unmergeable under `CONVENTIONS.md` §3 regardless of how complete the implementation is.

### Wave 4 — read-only reviewer
- Confirms: (1) no file outside the list in this plan's Detailed Implementation was touched (grep the diff's file list against this plan), (2) no `AdminService`/pagination/retention/OTel/identity code was touched (the explicit exclusions), (3) `MOIRA_IDEMPOTENCY__PEPPER_BASE64` and any new JWKS config knobs are documented alongside `MOIRA_API_KEYS__PEPPER_BASE64` — currently documented only at `docs/moira-foundation-v1.md:67` (`export MOIRA_API_KEYS__PEPPER_BASE64="$(openssl rand -base64 32)"`); mirror that, and check `charts/moira` values/templates for a secret-injection point to extend, (4) no panic-message or JWKS-fetch-failure detail leaks into a client-visible error response (spot-check the `CatchPanicLayer` handler and the SSRF-rejection `AppError` messages for accidental verbosity).

**Conflict-avoidance strategy.** Wave 1 is the only wave touching shared/foundational files (`settings.rs`, `state.rs`) and must fully complete and build before Wave 2 starts. Within Wave 2, each agent's file set is disjoint by construction (security submodule vs. auth submodule vs. lib/http-wiring). Wave 3 is sequential-after because tests exercise the merged behavior of all three findings together (e.g., the `413`/`504`/`500` HTTP-contract tests need the full middleware stack from Agent D *and* a working `AppState` from Agent A).

---

## Interfaces & Contracts

**Endpoints affected.** No route added/removed, no request/response *shape* changes. Behavioral changes only:

| Concern | Before | After |
|---|---|---|
| Idempotency/request hash storage | Unkeyed SHA-256 | Versioned HMAC-SHA-256 (`v1:...`), legacy hashes still verify |
| `jwks_url` (trusted issuer, admin/caller static config) | Any scheme/host, unbounded fetch | `https://`-only (unless dev override), private/link-local/metadata IP denied, size/timeout/content-type capped |
| All HTTP requests (non-SSE) | No server-side timeout | `504` after `maximum_execution_timeout_seconds + 30s` (exact value TBD at implementation, product-confirmable) |
| Panicking handler | Connection dropped, no response | `500` with standard `ErrorResponse` envelope |
| Public route body size | Global 512KiB regardless of config | `PublicApiSettings.maximum_request_bytes` actually enforced (default 1MiB) |
| Admin route body size | Global 512KiB | Distinct, larger admin cap (exact number TBD, flagged for sign-off) |
| Response headers | `Cache-Control`, `X-Content-Type-Options`, `Referrer-Policy` | + `X-Frame-Options`, `Content-Security-Policy`, conditionally `Strict-Transport-Security` |

**Status codes.** New: `504` (request timeout, previously the connection would simply hang or eventually be killed by an infra-level LB timeout with no coherent body); `500` for caught panics now returns a **body** where previously the connection dropped with none; `413` now triggers at the correct, per-route-configured threshold instead of a single global one (may trigger *later* for public routes if `maximum_request_bytes` > 512KiB, or *earlier* for admin routes if the chosen admin cap is set below what was previously allowed — size the admin cap conservatively above any known legitimate admin payload to avoid a regression, e.g. checking the largest existing admin request body in current tests/fixtures before finalizing the number).

**Headers.** New response headers as listed above. No new *request* headers required from clients (the `Idempotency-Key`/`Authorization`/API-key headers are unchanged in format).

**Scopes/authorization rules.** Unchanged.

**Error codes & i18n message keys** (binding: `plans/CONVENTIONS.md` §4).

Message keys derive mechanically from the error code (`format!("moira.error.{}", code())`, `src/error.rs:146-148`) into the `ErrorResponse { error: ErrorDetail { code, message_key, message, message_args, request_id, details } }` envelope (`src/error.rs:52-65`). Catalog entries are `I18nEntry { key, default_message, description }` in `src/i18n/catalog/errors.rs`, mirrored into `docs/i18n-response-catalog.json`. **Verified against the real catalog during re-grounding: it holds 57 `moira.error.*` keys today; none of the three below exists.**

**Three new `moira.error.*` entries are required by this plan.** Add each to `src/i18n/catalog/errors.rs` in its alphabetical-by-key position (the file's existing ordering), and mirror each verbatim into the `entries` array of `docs/i18n-response-catalog.json` in the same position, in the **same PR** (hand-synced; the drift test is plan 06's — until then drift is a review failure, `CONVENTIONS.md` §4 rule 4).

1. **`moira.error.request_timeout`** — genuinely new. The catalog's only timeout keys today are `moira.error.upstream_timeout` (`src/i18n/catalog/errors.rs:65`, provider-side) and `moira.error.timeout_override_forbidden` (`:265`, a policy rejection); neither describes a server-side deadline elapsing. Raised by the `TimeoutLayer` error mapper as `AppError::coded(StatusCode::GATEWAY_TIMEOUT, "request_timeout", ...)` (`coded` at `src/error.rs:78`).
   ```rust
   I18nEntry {
       key: "moira.error.request_timeout",
       default_message: "The request timed out before it could be completed.",
       description: "Used when the server-side request timeout elapses before the handler produces a response.",
   },
   ```

2. **`moira.error.payload_too_large`** — genuinely new, and it closes a **gap this re-audit discovered that the plan did not previously name**: today an oversized body is rejected by Axum's `DefaultBodyLimit` (`src/lib.rs:42`) *before* any Moira code runs, and there is **no router `fallback`, no `HandleErrorLayer`, and no `413` mapping anywhere in `src/lib.rs`, `src/http/mod.rs`, or `src/error.rs` (verified by grep)** — so the client receives Axum's default plain-text `413`, with **no `ErrorResponse` envelope, no `code`, no `message_key`, and no `request_id`**. That is a standing violation of `CONVENTIONS.md` §4's "every user-visible response carries a key + default English message." Since P1-3 is the finding that owns body-limit policy, fixing the envelope belongs here, not in a later plan. Implementation: map the limit rejection into the standard envelope (a router-level `fallback`/error-mapping layer, or per-extractor rejection mapping — pick one and apply it uniformly; do **not** hand-write a second envelope shape).
   ```rust
   I18nEntry {
       key: "moira.error.payload_too_large",
       default_message: "The request body exceeds the maximum allowed size.",
       description: "Used when a request body exceeds the configured per-route body limit.",
   },
   ```

3. **`moira.error.jwks_url_rejected`** — genuinely new. Raised **at issuer-registration time** (`POST /api/v1/admin/jwt-issuers`) as `AppError::coded(StatusCode::BAD_REQUEST, "jwks_url_rejected", ...)` — `400`, not `502`, because at registration the bad value is the **admin's input**, and a `4xx` is what tells the admin to fix their request. Resolve the earlier "`400` or `502`?" ambiguity this way and stop carrying it as an open question.
   ```rust
   I18nEntry {
       key: "moira.error.jwks_url_rejected",
       default_message: "The JWKS URL was rejected by the server's security policy.",
       description: "Used when a configured JWKS URL fails scheme, address-range, size, content-type, or timeout validation.",
   },
   ```
   **At request-verification time the code is deliberately *not* reused.** A caller whose JWT merely triggered a lazy JWKS fetch is unauthenticated and must not learn *why* the fetch failed — that would turn the auth path into an SSRF oracle. Return the existing `AppError::Unauthorized(...)` (code `unauthorized`, key `moira.error.unauthorized`, already catalogued) and put the specific denial reason **only** in the server-side audit record and `tracing` output. This is an explicit security decision, not an oversight.

**Panics reuse an existing key — decision made, no longer open.** Caught panics map to the existing `AppError::Internal` code `internal_error` → `moira.error.internal_error` (`src/error.rs:142`; catalog entry present). No new key. Operators filter panic-originated `500`s by the server-side `tracing::error!` event the panic handler emits, **not** by a client-visible code — exposing "this was a panic" to callers is information disclosure for zero client benefit. Removed from Deferred Follow-ups accordingly.

**No new `moira.notice.*` entries.** This plan adds no success/notice strings; `src/i18n/catalog/notices.rs` (4 entries today) is untouched. No handler introduced here returns a hardcoded English literal in a body — the panic handler's fixed message flows through `AppError::Internal`'s catalogued English default, not an inline string.

**`message_args`.** All three new errors carry `{}` (no interpolation). Do **not** put the rejected URL, the resolved IP, the actual body size, or the elapsed timeout into `message_args` on the client-visible response: the first two are the SSRF-oracle leak described above, and the latter two belong in logs. `CONVENTIONS.md` §4 rule 3 permits structured args; this plan declines to use them for security reasons.

**Test requirement** (`CONVENTIONS.md` §4 rule 5) — discharged by the unit test `middleware_error_keys_are_catalogued` (asserting `moira::i18n::is_known_key` for all three new keys plus `moira.error.unauthorized` and `moira.error.internal_error`) **and** by the e2e assertions that live `413`/`504`/`500` responses carry a non-empty `message_key` + `message`. See Verification.

**Idempotency behavior.** No intended change to which requests get replayed vs. not (P1-1 is a hashing-mechanism hardening, not a semantics change) — but preserving legacy-row replay across the deploy requires **both** the dual-verify (body hash) and the dual-lookup (key hash, which is the index key) from Detailed Implementation item 6. If dual-lookup is descoped, in-flight (≤24h) pre-deploy idempotency claims simply stop replay-matching and fall through to normal processing — a bounded duplicate-processing window, never a security regression; state the chosen behavior in the rollout notes.

**Transaction boundaries.** Unchanged.

**Cache invalidation.** New: the JWKS cache now supports a "stale but retained" state distinct from "absent" — document this as a new cache state in `TrustedIssuerAuthenticator`'s cache type if it isn't already naturally expressible (e.g., keep serving `expires_at`-expired entries when a refresh fails, rather than treating expiry as a hard eviction).

**Concurrency behavior.** New: JWKS refresh is now singleflighted per issuer/URL — concurrent requests during a cache miss no longer each independently hit the upstream JWKS endpoint.

**SSE behavior.** `/api/v1/responses/stream` must be explicitly exempted from (or given a much longer ceiling than) the new request-level `TimeoutLayer` — this is the single most important compatibility check in this plan; verify with an integration test that a stream lasting longer than the non-streaming timeout still completes successfully.

---

## Verification

**Both test layers are required to merge** (`plans/CONVENTIONS.md` §3). The full per-test breakdown lives in Detailed Implementation §7; this section states the acceptance criteria.

**Layer 1 — unit (`#[cfg(test)] mod tests`, no database).** Delivered across five files:
- `src/security/idempotency.rs` — `versioned_hash_round_trips_under_the_active_pepper`, `hash_output_fits_the_varchar_128_column`, `legacy_unkeyed_hash_still_verifies`, `verify_rejects_a_hash_from_a_different_pepper_version`, `hash_changes_when_the_pepper_changes`, `hash_never_reveals_plaintext`.
- `src/security/ssrf.rs` — `loopback_addresses_are_denied`, `rfc1918_private_ranges_are_denied`, `link_local_and_cloud_metadata_addresses_are_denied`, `ipv6_unique_local_and_link_local_are_denied`, `ipv4_mapped_ipv6_metadata_address_is_denied`, `unspecified_and_multicast_addresses_are_denied`, `public_addresses_are_allowed`, `non_https_scheme_is_rejected`, `non_https_scheme_is_permitted_under_the_dev_override`.
- `src/lib.rs` (extending the existing `mod tests` at `:107`) — `secure_response_headers_include_frame_options_and_csp`, `hsts_is_absent_outside_production`, `hsts_is_present_under_a_production_deployment_environment`, `panic_response_body_contains_no_panic_payload`, `router_still_builds_with_the_full_middleware_stack`.
- `src/config/settings.rs` — `idempotency_pepper_bytes_decodes_base64`, `idempotency_pepper_bytes_uses_the_dev_fallback_when_allowed`, `production_rejects_allow_insecure_dev_idempotency_pepper`, `production_rejects_allow_insecure_dev_jwks_urls`.
- `src/i18n/catalog/mod.rs` — `middleware_error_keys_are_catalogued`.

**Layer 2 — e2e / integration (real HTTP surface, real PostgreSQL 16 + pgvector, via `tests/support/mod.rs`).** Delivered in three new files:
- `tests/http_middleware_contract.rs` — `oversized_public_body_is_rejected_with_413_and_the_standard_envelope`, `public_body_at_the_configured_maximum_request_bytes_boundary_is_accepted`, `admin_routes_enforce_their_own_distinct_body_limit`, `slow_non_streaming_request_returns_504_with_the_request_timeout_key`, `panicking_handler_returns_500_envelope_without_panic_payload`, `sse_stream_outlives_the_non_streaming_timeout`, `security_headers_are_present_on_a_live_response`, `once_only_secret_responses_carry_no_content_encoding`, `middleware_error_responses_carry_non_empty_message_key_and_message`.
- `tests/jwks_hardening.rs` — `jwks_url_with_http_scheme_is_rejected_at_issuer_registration`, `jwks_url_resolving_to_a_private_address_is_rejected_at_issuer_registration`, `oversized_jwks_response_is_abandoned_before_full_buffering`, `non_json_jwks_content_type_is_rejected`, `slow_jwks_response_is_abandoned_at_the_configured_timeout`, `concurrent_cache_miss_fetches_are_singleflighted_to_one_upstream_call`, `failed_refresh_serves_the_last_known_good_cached_jwks`, `first_ever_fetch_failure_surfaces_as_an_auth_failure`, `jwks_rejection_is_audited_without_leaking_the_resolved_ip_to_the_caller`.
- `tests/idempotency_hash_migration.rs` — `legacy_unkeyed_row_still_replays_after_the_hmac_switch`, `new_records_persist_the_versioned_hmac_prefix`, `admin_command_replay_matches_under_the_new_hasher`, `body_mismatch_under_the_new_hasher_still_returns_idempotency_conflict`, `concurrent_same_key_claims_still_produce_one_ledger_row`, `conversation_content_hashes_are_written_in_the_versioned_format`.

**Concurrency discipline.** `concurrent_cache_miss_fetches_are_singleflighted_to_one_upstream_call`, `concurrent_same_key_claims_still_produce_one_ledger_row`, and `sse_stream_outlives_the_non_streaming_timeout` use `tokio::sync::Barrier`/`Notify` acknowledgement gates. `sleep()`-based interleaving is rejected in review (`CONVENTIONS.md` §3; finding P2-12). The legacy `sleep` calls already in `tests/admin_idempotency.rs:977,1259` are **not** touched by this plan (plan 06's scope) and must not be used as a template.

**i18n verification** (`CONVENTIONS.md` §4 rule 5). Three new keys — `moira.error.request_timeout`, `moira.error.payload_too_large`, `moira.error.jwks_url_rejected` — each added to `src/i18n/catalog/errors.rs` with an English `default_message` + `description` and mirrored into `docs/i18n-response-catalog.json` in the same PR. Presence is asserted by the unit test `middleware_error_keys_are_catalogued`; live-response coverage is asserted by the e2e test `middleware_error_responses_carry_non_empty_message_key_and_message` plus the per-status assertions in the `413`/`504`/`500` and JWKS-rejection tests. No new `moira.notice.*` entries.

- Security/secret-leak: `panic_response_body_contains_no_panic_payload` (unit) and `panicking_handler_returns_500_envelope_without_panic_payload` (e2e) prove no panic payload reaches the client; `jwks_rejection_is_audited_without_leaking_the_resolved_ip_to_the_caller` proves the SSRF check is not an oracle. Existing `src/security/masking::tests` must continue to pass unchanged.
- OpenAPI validation: confirm no route's documented parameters/responses changed as a side effect of this plan (this plan should produce a **zero-diff** OpenAPI snapshot compared to plan 02's output, since P1-1/P1-2/P1-3 are infrastructure-only) — add or reuse a snapshot-diff assertion.
- Migration validation: run the standard clean-Postgres migration check even though no migration is added (all affected hash columns verified `varchar(128)` — see Architecture → Database/migration changes), to catch any accidental schema drift.
- Required Rust gates, run verbatim and must pass clean:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - clean PostgreSQL migration validation
  - `cargo build --release --locked`

---

## Definition of Done

- [ ] `IdempotencyHasher` exists, is wired through `AppState`, and every write of `idempotency_key_hash`/`request_hash`/`content_hash` for idempotency purposes uses it (grep for remaining direct `request_hash(...)` calls at secret-bearing/idempotency call sites returns zero matches outside the legacy-verification fallback and non-secret fingerprint uses that are explicitly out of scope).
- [ ] `MOIRA_IDEMPOTENCY__PEPPER_BASE64` is required in production (validated by `Settings::validate` the same way `MOIRA_API_KEYS__PEPPER_BASE64` already is — confirm and extend that validation function).
- [ ] A DB row written with the legacy unkeyed hash format before this change still replay-matches correctly after this change ships (proven by an automated test, not manual inspection).
- [ ] JWKS fetch rejects, with an audit-log entry, every one of: non-`https` scheme (absent dev override), a loopback/private/link-local/metadata-range resolved IP, an oversized response, a non-JSON content-type, and a response exceeding the configured timeout — each proven by its own test.
- [ ] Concurrent cache-miss JWKS fetches for the same issuer are singleflighted (proven by a test asserting exactly one upstream call).
- [ ] A JWKS refresh failure for an issuer with an existing cached entry serves the stale cache rather than failing auth (proven by a test).
- [ ] Every non-SSE HTTP route runs under `TimeoutLayer`; `/api/v1/responses/stream` (and any other SSE route) is proven, by test, to survive longer than the non-streaming timeout.
- [ ] A panic inside a handler returns `500` with the standard `ErrorResponse` envelope and no panic-payload text in the response body (proven by test).
- [ ] `PublicApiSettings.maximum_request_bytes` is the actual enforced limit on public routes (proven by a `413` test at exactly that boundary); a distinct, documented admin-route limit is enforced separately.
- [ ] `X-Frame-Options`, `Content-Security-Policy`, and (in production-configured deployments) `Strict-Transport-Security` are present on responses, proven by test.
- [ ] No `Content-Encoding`/compression header appears on any `ApiKeySecretResponse`-returning route (regression-proofing the currently-true "compression doesn't exist yet" state).
- [ ] Oversized requests return the **standard `ErrorResponse` envelope** with `code == "payload_too_large"`, `message_key == "moira.error.payload_too_large"`, a non-empty `message`, and a populated `request_id` — closing the currently-bare plain-text `413` that Axum's `DefaultBodyLimit` produces today (no router `fallback` or `413` mapping exists in `src/lib.rs`/`src/http/mod.rs`/`src/error.rs`, verified by grep).
- [ ] All three new i18n keys (`request_timeout`, `payload_too_large`, `jwks_url_rejected`) exist in `src/i18n/catalog/errors.rs` with English `default_message` + `description`, are mirrored byte-identically into `docs/i18n-response-catalog.json`, and are asserted by `middleware_error_keys_are_catalogued`.
- [ ] `MOIRA_IDEMPOTENCY__PEPPER_BASE64` and the new JWKS knobs are documented alongside `MOIRA_API_KEYS__PEPPER_BASE64` (`docs/moira-foundation-v1.md:67`) and wired into the `charts/moira` secret-injection point.
- [ ] All five required Rust gates pass with zero warnings/failures.
- [ ] The Wave 4 reviewer confirms no file outside this plan's explicit scope was touched, and no plan-04/05/06/07 concern (pagination, retention worker, OTel, identity claiming) was implemented under this plan's banner.
- [ ] The Wave 4 reviewer confirms every `#[cfg(test)]`-gated panicking/slow test route is genuinely `cfg`-gated and cannot appear in a release build (`cargo build --release --locked` plus a grep of the release router).

### Cross-cutting compliance checklist (`plans/CONVENTIONS.md` §8 — binding)

- [ ] Work performed on branch `plan/03-security-hardening`, branched from `plan/02-mvp-boundary-honesty` (stacked per `01-roadmap-and-dependencies.md` §3), **rebased onto `main` after plan 02 merges**, with the base PR named in this PR's description. Plan 02's branch was never force-pushed while stacked.
- [ ] PR opened with **all seven** required description sections (Plan link · Findings addressed · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups), including the `MOIRA_IDEMPOTENCY__PEPPER_BASE64` deploy-order note.
- [ ] All gates in `CONVENTIONS.md` §2 pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, plus clean PostgreSQL migration validation from an empty database.
- [ ] **Unit tests delivered and passing** — all 25 functions named in Verification Layer 1, across `src/security/idempotency.rs`, `src/security/ssrf.rs`, `src/lib.rs`, `src/config/settings.rs`, and `src/i18n/catalog/mod.rs`, none requiring a database.
- [ ] **E2E tests delivered and passing** — all 24 functions named in Verification Layer 2, across `tests/http_middleware_contract.rs`, `tests/jwks_hardening.rs`, and `tests/idempotency_hash_migration.rs`, driving the real HTTP surface against real PostgreSQL 16 + pgvector via `tests/support/mod.rs`, fail-closed under `CI`.
- [ ] No new concurrency test uses `sleep()`; all three interleaving tests use `tokio::sync::Barrier`/`Notify` acknowledgement gates.
- [ ] Every new error string has an i18n key + English default in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence — and every live middleware error response carries a non-empty, catalogued `message_key` + `message`. No new notice strings; `notices.rs` untouched.
- [ ] Frontend conventions (`CONVENTIONS.md` §5/§6) — **N/A**, this plan ships no console code.
- [ ] Auth conventions (`CONVENTIONS.md` §7) — this plan **hardens** the JWKS trust primitive that plans 07/08/09 build on but adds no auth *configuration* surface: no minted JWTs, no scope claims, no new identity storage. It does honor §7.5 by ensuring an admin-supplied `jwks_url` cannot be turned into an SSRF probe, and §7.3 mode 3 (bring-your-own JWT via JWKS) remains fully reachable — verified by `non_https_scheme_is_permitted_under_the_dev_override` and `public_addresses_are_allowed` proving the hardening does not break legitimate in-cluster or public IdPs.
- [ ] **No secret-leak, verified by test:** panic payloads absent from `500` bodies, resolved internal IPs and SSRF denial reasons absent from caller-visible responses, and no pepper value in any log or response. The new `MOIRA_IDEMPOTENCY__PEPPER_BASE64` is never logged.
- [ ] **Zero OpenAPI diff** against plan 02's output — this plan is infrastructure-only and must not perturb the spec that plan 05's drift gate will freeze (`CONVENTIONS.md` §1 rule 6).
- [ ] **Done means merged.** Every box above is proven by a named, passing test — "implemented" is not "done" (`CONVENTIONS.md` §1 rule 5, §3).

---

## Risks & Rollback

**Security risks.**
- The DNS-then-validate-then-connect SSRF check has a small residual TOCTOU window (DNS could theoretically re-resolve to a different, denied IP between validation and the actual `reqwest` connect). Documented as an accepted low-severity residual risk in Architecture above, since `jwks_url` is admin-configured, not attacker-supplied at request time — closing it fully would require pinning the resolved IP for the actual connection (e.g., via a custom `reqwest::dns::Resolve` override), which is a larger change; flag as a follow-up hardening item if product wants zero residual risk.
- Getting the private/link-local/metadata IP-range list wrong (too narrow) would leave a real gap; getting it wrong (too broad) would break legitimate in-cluster JWKS endpoints. Mitigate via the explicit per-range unit tests in Verification and a manual review of the exact CIDR list against a standard reference (e.g., IANA special-purpose address registries) during implementation.
- A misconfigured `allow_insecure_dev_urls`/`allow_insecure_dev_pepper` left `true` in a production deployment would silently disable this plan's protections. Mitigate by having `Settings::validate` (`src/config/settings.rs:233`) hard-fail in production for both new dev-override flags, mirroring the existing check that already rejects `api_keys.allow_insecure_dev_pepper` in production (`:355-357`).

**Data-migration risks.** None — no migration at all. `idempotency_records` uses dual-format verification, and every affected `content_hash` column is already `varchar(128)` (verified against `migrations/0003` and `migrations/0007`).

**Compatibility risks.**
- Any deployment currently using an `http://` or private-IP JWKS URL breaks post-deploy unless the dev-override is explicitly and deliberately set — this is the intended effect, but **must be confirmed against any known real deployment configuration before shipping** (flagged for ops sign-off).
- Timeout/body-limit tightening could reject previously-succeeding requests; size both conservatively (timeout above the execution deadline ceiling; body limits at or above the already-documented policy values) to minimize surprise, and flag the exact numbers for product/ops review rather than treating them as final.

**Deployment risks.** New required secret (`MOIRA_IDEMPOTENCY__PEPPER_BASE64`) must be provisioned in every non-dev environment before this deploys, or `Settings::validate` will correctly refuse to start — this is a deploy-order dependency to communicate to whoever runs the rollout (generate and set the secret first, then deploy).

**Rollback procedure.** Standard `git revert` of the merge commit; no migration to reverse. No data written by this plan requires cleanup on rollback. If rolled back after the new pepper secret was already rotated/removed from the environment, note that any idempotency records written *during* the period this plan was live and not yet expired (≤24h) would fail to verify post-rollback under the old unkeyed-only code path unless that old code path is also restored unchanged — acceptable, since a failed idempotency match only means "treat as non-duplicate," never a security or correctness issue in the fail-open direction already established by this codebase's existing idempotency design.

**Deliberately deferred follow-ups (not in this plan's scope, tracked elsewhere):**
- Full IP-pinning to eliminate the residual DNS-rebinding TOCTOU window (not implemented, flagged above).
- ~~A distinct i18n key for panic-originated 500s~~ — **decided, not deferred**: panics reuse `moira.error.internal_error`; the panic-vs-other-500 distinction lives in server-side `tracing` only, because exposing it to callers is information disclosure with no client benefit (see Interfaces & Contracts).
- Response compression with per-route secret-response exclusion — not added by this plan since compression doesn't exist yet; the exclusion requirement is documented for whoever adds compression later, and `once_only_secret_responses_carry_no_content_encoding` guards it.
- Configurable body-limit policy for conversation/memory/RAG routes (currently a fixed constant in this plan) — candidate for plan 04 or a later config-surface pass.
- Cleaning up the legacy `sleep()` calls in `tests/admin_idempotency.rs:977,1259` — plan 06's test-hygiene scope (P2-12). This plan adds no new `sleep`-based interleaving but does not fix the existing ones.
- Adding a blanket client-level timeout to the shared `reqwest::Client` (`src/app/state.rs:43-46`, which today has **no timeout at all**) — deliberately **not** done here, because that client is also used for provider execution calls whose timeout semantics are owned by the execution-deadline system (plan 04/P1-6). This plan scopes its timeout to the JWKS fetch only. **Flagged as a real, still-open exposure**, not a resolved one.

---

## Re-audit notes (2026-07-25, against `plans/CONVENTIONS.md`)

**Citations re-verified — all hold.** `src/security/masking.rs:5-12` (`secret_fingerprint` → `Sha256::digest`, `request_hash` delegating to it, unkeyed, no pepper); `src/lib.rs:16` (`extract::{DefaultBodyLimit, State}`), `:34-53` (the full `build_router` layer chain — `metrics_middleware`, `secure_response_headers`, `DefaultBodyLimit::max(512 * 1024)` at `:42`, `TraceLayer::new_for_http()` at `:43`, `request_id_context`, `PropagateRequestIdLayer`, `SetRequestIdLayer`, conditional CORS at `:48-50`), `:92-105` (the three existing secure headers), `:107` (the existing `#[cfg(test)] mod tests`); `src/security/auth.rs:386-410` (`jwks`, cache read at `:387`, `.json::<JwkSet>()` at `:399`, cache write at `:402`) and `:484-506` (`validate_static_jwt`, `.json::<JwkSet>()` at `:502`); `src/config/settings.rs:46` (`DeploymentEnvironment`), `:90` (`AuthSettings`), `:118` (`ApiKeySettings`), `:164`/`:168` (`PublicApiSettings.maximum_request_bytes`), `:233` (`validate`), `:355-357` (the production `allow_insecure_dev_pepper` rejection this plan mirrors), `:491` (the `vec![11; 32]` dev fallback). **Confirmed absent, as the plan claims:** no `TimeoutLayer`, no `CatchPanicLayer`, no `CompressionLayer`, no `tower_http::compress` import anywhere in `src/`.

**Gaps found and now closed in this plan:**
1. **No unit/e2e split.** The plan previously proposed extending `tests/http_error_contract.rs`, which — verified during grounding — builds `AppState::new(Settings::default(), None)`, i.e. **without a database**, and holds exactly one test. Under `CONVENTIONS.md` §3 that is a unit-grade HTTP test, not e2e. The plan now delivers 24 named unit functions across five files **and** 24 named e2e functions across three new `tests/` files driving a real listener against real PostgreSQL 16 + pgvector.
2. **`413` had no i18n key and no error envelope at all.** Newly discovered: there is **no router `fallback`, no `HandleErrorLayer`, and no `413` mapping** anywhere in `src/lib.rs`, `src/http/mod.rs`, or `src/error.rs`, so Axum's `DefaultBodyLimit` today returns bare plain text with no `code`, no `message_key`, and no `request_id`. Since P1-3 owns body-limit policy, `moira.error.payload_too_large` plus the envelope mapping now land here.
3. **Two i18n decisions were left open.** `jwks_url_rejected`'s status (`400` at registration; the generic `unauthorized` at request-verification time so the auth path is not an SSRF oracle) and the panic key (reuse `internal_error`) are now **decided with stated reasoning**, not carried as open questions.
4. **No i18n test existed.** `CONVENTIONS.md` §4 rule 5 was undischarged. Also discovered: `moira::i18n::is_known_key` is exported (`src/i18n/mod.rs:3-6`) but has **zero callers anywhere in `src/` or `tests/`** — the catalog is presently enforced by nothing. `middleware_error_keys_are_catalogued` and `middleware_error_responses_carry_non_empty_message_key_and_message` are among the first enforcement points.
5. **Concurrency-test discipline was unstated.** Three interleaving tests are now explicitly gated on `Barrier`/`Notify`, with an explicit instruction not to copy the legacy `sleep` at `tests/admin_idempotency.rs:977,1259`.
6. **No catalog owner in the wave plan.** `src/i18n/catalog/errors.rs` and `docs/i18n-response-catalog.json` had no assigned agent; Wave 2b (Agent I) now owns them, sequenced before C's and D's error-raising code.
7. **Branch/PR workflow was absent**, including the stacking relationship on plan 02 — now specified under Summary.
8. **The untimed shared `reqwest::Client` was noted but not tracked.** `src/app/state.rs:43-46` builds the client with only a `user_agent` and no timeout; this plan correctly declines to change it (provider-call semantics belong to plan 04) but it is now an explicit deferred follow-up rather than a buried aside.

**Product/ops-input decisions flagged, not fabricated:** (a) the non-streaming `TimeoutLayer` ceiling (proposed `maximum_execution_timeout_seconds + 30s`, `src/config/settings.rs:154` default 600 — the buffer is a judgment call); (b) the admin-route body limit (proposed 2MiB, sized above the largest existing admin fixture body); (c) confirmation that no current deployment uses an `http://` or private-address `jwks_url` before shipping; (d) whether to take dual-**lookup** or accept the ≤24h legacy non-replay window (this plan's default: dual-lookup for `/v1/responses` and admin commands, where replay is contractual). No API, migration, config default, or catalog key has been invented beyond what is enumerated in this plan.

**Scope discipline preserved.** Every addition above is a test, an i18n entry, or a workflow/compliance statement. No `AdminService` decomposition, no pagination, no retention worker, no OTel, no identity work, no repository-trait refactor entered this plan.
</content>
