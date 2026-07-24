# Moira MVP Readiness — Audit Report

- **Audited commit:** `ea94eb939fe58864b04fec912daed1a0f0bfcb4b` (branch `codex/atomic-admin-idempotency`).
- **Authoritative branch:** `origin/main`. HEAD `ea94eb9` is **not** an ancestor of `origin/main` by commit id (main squash-merged it as `356eec7`), but the worktrees are **byte-identical**: `git rev-parse HEAD^{tree}` == `git rev-parse origin/main^{tree}` == `9261202…`, and `git diff HEAD origin/main` is empty. The audit therefore reflects the current authoritative code. No branches were modified; the working tree was clean before and after.
- **Method:** eight parallel read-only specialist agents (architecture, security, database, orchestration/streaming, conversations/memory/RAG, HTTP/OpenAPI/i18n, DevOps/observability, tests). Findings below are tagged **Verified** (confirmed by reading code / running a command), **Inference** (reasoned from code but not executed), or **Assumption**.
- **Verification performed live:** `cargo fmt --check` PASS; `cargo clippy --all-targets -- -D warnings` PASS ("No issues found"); `cargo test --workspace --all-features` **120/120 passed, 0 failed, 0 ignored** against a real pgvector Postgres 16 + Redis 7. One test failed only under a host-port collision (local Homebrew Postgres on 5432) and passed clean on a remapped port — environment artifact, not a defect.
- **Environment limitations:** OpenAPI-vs-spec drift, secret/content-leak behavior, and multi-replica behavior could not be exercised end-to-end (no such tests/harness exist yet); those are assessed by code reading and marked Inference.

---

## MVP boundary Moira can safely advertise **today**

> Moira is a **single-replica**, self-hosted AI gateway providing:
> - DB-backed provider / model / routing-policy configuration at runtime.
> - Machine authentication via **system keys**, **consumer keys**, and **trusted JWT issuers** (JWKS-validated, per-issuer algorithm allow-list). Argon2id+pepper key hashing.
> - Encrypted provider credentials (AES-256-GCM with AAD binding) and scope-precedence resolution (user → application → tenant → global, 8 tiers).
> - **Atomic, idempotent admin APIs** with a Postgres-backed idempotency ledger, advisory-lock single-winner concurrency, savepoint-scoped business-failure rollback, and optimistic concurrency (`If-Match`) on almost all mutations.
> - A public execution API `POST /v1/responses` (+ `/v1/responses/stream` SSE) with retry / fallback / in-process circuit breaking, in-process concurrency + rate limiting, and an OpenAI-compatible `/v1/responses` **subset**.
> - Conversation, explicit-memory, and RAG endpoints **as persistence/configuration primitives only.**

**Must NOT be advertised today:** intelligent memory (extraction, embeddings, semantic retrieval), RAG retrieval/chunking/citations, conversation summarization, multi-replica / horizontally-scaled deployment, and any human OAuth/OIDC login or admin console (none exists).

---

## Severity summary

| Sev | Meaning | Count |
|-----|---------|-------|
| **P0** | Release blocker — must fix or descope before any controlled MVP | 5 |
| **P1** | Required before a controlled MVP | 11 |
| **P2** | Important hardening / near-term | 14 |
| **P3** | Later / genuinely post-MVP | 9 |

Blocking legend per finding: **[BE]** backend MVP · **[UI]** setup UI · **[OAuth]** OAuth/OIDC readiness · **[MR]** multi-replica.

---

## P0 — Release blockers

### P0-1 · RAG / memory / summarization endpoints look functional but are no-ops [BE] · *Verified*
- **Evidence:**
  - `src/infra/repositories/conversation.rs:1138-1210` `ingest_rag_document` stores `content_plain` into `rag_document_versions` and hardcodes `ingestion_status = 'indexed'`, but never writes `rag_chunks` or `rag_chunk_embeddings`. **Second write site:** `create_rag_document` (`conversation.rs:1013-1084`, literal at `:1064`) also hardcodes `'indexed'` when the create carries inline `content` — both paths must be corrected.
  - `src/application/public.rs:1657` `citations: Vec::new()` — every response returns empty citations unconditionally.
  - `src/application/conversation.rs:37-47` `ContextPlanner::deterministic_phase_five_order()` returns a hardcoded `[&str; 8]`; `prepare_response_conversation` (`:314-380`) never loads history/summaries/memories/RAG into the prompt.
  - Summarization (`conversation_summaries` never written), memory extraction, memory embeddings, and semantic retrieval: **no implementing code exists** (Verified absence via grep across `src/application/conversation.rs`, `src/infra/repositories/conversation.rs`).
- **Impact:** A caller sees `ingestion_status:"indexed"` and an empty-but-present `citations` field and reasonably concludes retrieval works. It does not. This is a correctness + truth-in-advertising defect, not merely "incomplete."
- **Correction:** Either (a) gate/hide these endpoints behind an explicit "preview/disabled" flag and return an honest status (`ingestion_status:"stored"` / `not_indexed`), and document them as persistence primitives; or (b) implement the pipeline (large, post-MVP). MVP path = (a). See **plans/02**.
- **Expertise:** Rust/backend, AI platform. **Dependencies:** none. **Tests:** endpoint-contract tests asserting honest status + OpenAPI description.

### P0-2 · `Idempotency-Key` advertised in OpenAPI but unimplemented on conversation/memory/RAG [BE] · *Verified*
- **Evidence:** `src/http/conversation.rs:665,847,954,984` declare `Idempotency-Key` in `#[utoipa::path]`, but `src/application/conversation.rs` has **zero** idempotency references; the header is never read. Real replay exists only for `/v1/responses` (`src/application/public.rs:125-134,1010-1050`).
- **Impact:** A client retrying a create with the same `Idempotency-Key` gets **duplicate side effects**, not a replay — silent data duplication under normal retry behavior.
- **Correction (DECIDED — `CONVENTIONS.md` §0 D1/D2):** **Implement real replay**, reusing the existing `AdminCommandRunner` / `claim_idempotency` / `finalize_idempotency` ledger machinery. The parameter **stays** in the OpenAPI spec because it is about to become true — it is *not* removed and *not* rejected with `501`. Delivered in **plans/02b** (stacked on **plans/02a**, which ships the honesty fix first without waiting for the replay implementation).
- **Expertise:** Rust/backend, API. **Dependencies:** none. **Tests:** contract test that spec advertises only implemented headers.

### P0-3 · Conversation/memory/RAG surface must be explicitly scoped in API docs & OpenAPI before public exposure [BE] · *Verified/Inference*
- **Evidence:** Aggregation of P0-1/P0-2 plus `docs/todo.md:77` (Moira's own instruction to advertise these as primitives "until retrieval, chunking, embeddings, context injection, and citations are wired end to end").
- **Impact:** Marketing/consumer OpenAPI overclaims capability.
- **Correction:** OpenAPI descriptions + `docs/public-api.md` state the preview boundary explicitly. See **plans/02**.
- **Expertise:** API/OpenAPI. **Dependencies:** P0-1, P0-2. **Tests:** OpenAPI snapshot.

### P0-5 · The entire i18n catalog is orphaned — never compiled into the crate [BE] · *Verified (coordinator-confirmed)*
- **Evidence (independently re-verified 2026-07-25):**
  - `src/lib.rs:3-11` declares `app, application, config, domain, error, http, infra, orchestration, security` — there is **no `pub mod i18n;`**.
  - `src/domain/mod.rs:3` `mod i18n;` resolves to **`src/domain/i18n.rs`** (which exists, exporting `ResponseText`/`ResponseTextArgs` at `:36`) — it does **not** reach `src/i18n/`.
  - Every reference to `RESPONSE_ERROR_CATALOG`, `RESPONSE_NOTICE_CATALOG`, and `is_known_key` is **internal to `src/i18n/` itself** (`src/i18n/mod.rs:4-5`, `catalog/mod.rs:21-30`); zero callers in `src/` or `tests/`. The directory is orphaned exactly like `src/http/chat.rs` (P3-9) — not compiled, so `clippy -D warnings` never sees it.
  - **At least nine emitted error codes have no catalog entry at all.** Six from the `AppError` tail (`src/error.rs:128-142`): `database_unavailable`, `upstream_error`, `configuration_error`, `database_error`, `http_client_error`, `redis_error` (only `internal_error` of that tail is catalogued). Plus two on live, frequently-hit paths: **`capacity_exhausted`** (`src/application/public.rs:1971`, returned as `429` on the public execution path) and **`idempotency_in_progress`** (`src/infra/repositories/admin.rs:576,610`, asserted by `tests/admin_idempotency.rs:854` and declared in ten `#[utoipa::path]` 409 descriptions in `src/http/admin.rs`). The catalog has only `idempotency_conflict` and `idempotency_not_supported_for_stream`.
  - `docs/i18n-response-catalog.json` has **63 entries / 61 unique** — `moira.error.idempotency_conflict` and `moira.error.rate_limited` are duplicated.
- **Impact:** Responses *do* still carry a `message_key` (derived at `src/error.rs:146-148` as `format!("moira.error.{}", code())`, independent of the catalog), so the wire contract is not broken. But the catalog that is supposed to define the key vocabulary and its English default messages **is dead code**: nothing validates that an emitted key exists, nothing can look up a default message, and six real codes resolve to keys with no definition anywhere. Any i18n guarantee is currently unenforced by construction. `tests/http_error_contract.rs:36-41` already asserts a live response carrying an uncatalogued key without noticing.
- **Correction:** Add `pub mod i18n;` to `src/lib.rs` (resolving the `src/domain/i18n.rs` name collision), add the six missing catalog entries, dedupe the JSON mirror, and make `is_known_key` load-bearing via a test asserting every emitted code resolves to a catalog entry. Wiring is assigned to **plans/04** (Wave 0); the missing entries and the enforcement test to **plans/05**.
- **Expertise:** Rust/backend, i18n. **Dependencies:** none (blocks the i18n requirement in `plans/CONVENTIONS.md` §4). **Tests:** every-emitted-code-is-catalogued; every error response carries non-empty `message_key` + `message`.

### P0-4 · Broken/toothless supply-chain CI gate (`cargo deny` has no config) [BE] · *Verified*
- **Evidence:** `.github/workflows/ci.yml:60-68` runs `cargo deny check`, but **no `deny.toml` exists** anywhere in the repo (confirmed: `ls deny.toml` → absent).
- **Impact:** The supply-chain gate either errors or enforces nothing; the team believes license/advisory/ban policy is enforced when it is not. A single broken gate that reads as green is a release-integrity blocker.
- **Correction:** Add a `deny.toml` (advisories, licenses allow-list, bans, sources) and confirm the job fails on a seeded violation. See **plans/05**.
- **Expertise:** DevOps/Rust. **Dependencies:** none. **Tests:** CI job passes with policy present; negative test with a banned crate.

---

## P1 — Required before a controlled MVP

### P1-1 · Unkeyed SHA-256 idempotency request hash over secret-bearing bodies [BE][OAuth] · *Verified*
- **Evidence:** `src/security/masking.rs:10-12` `request_hash()` is plain SHA-256; used at `src/infra/repositories/public.rs:741` to hash raw request bytes including credential/key-create payloads. `docs/todo.md:9` still open. **Full producer set (verified during the Fable re-audit — broader than a single call site):** `src/application/admin_command.rs:96-106,163`, `src/application/runtime_admin.rs:717-721`, seven sites in `src/application/conversation.rs` (`:286,352,395,455,546,876,967`), and `src/application/public.rs:1597,1880-1884`. Note `idempotency_key_hash` is also the ledger **lookup index key**, so preserving legacy replay needs dual-*lookup*, not just dual-verify (see plan 03). (API-key hashing itself is fine: Argon2id+pepper, `src/security/api_keys.rs`.)
- **Impact:** After a DB-only compromise, stored `request_hash` values act as offline verifiers for guessed plaintext secrets (dictionary attack).
- **Correction:** Versioned HMAC-SHA-256 with a dedicated idempotency pepper (mirror the API-key pepper-version pattern). See **plans/03**.
- **Expertise:** Security. **Dependencies:** none (additive; keep verifying legacy hashes during migration). **Tests:** old-hash-still-verifies + new-hash-uses-active-pepper; secret never recoverable from hash.

### P1-2 · JWKS fetch has no SSRF protection [BE][OAuth] · *Verified*
- **Evidence:** `src/security/auth.rs:386-410,484-506` fetch JWKS via `reqwest::get`/`http.get(jwks_url)` — no scheme/host allow-list, no localhost/link-local/metadata-IP denial, no size/content-type limit, no explicit timeout, no singleflight. `docs/todo.md:25` open.
- **Impact:** Admin-controlled issuer URL becomes an SSRF vector (cloud metadata, internal services). Directly relevant to OAuth/OIDC where issuer/JWKS URLs proliferate.
- **Correction:** SSRF-hardened fetcher: HTTPS-only, DNS-resolution + private/link-local/metadata denial, size/content-type/timeout caps, singleflight, retain old cache on failure, audit. See **plans/03**.
- **Expertise:** Security/networking. **Dependencies:** none. **Tests:** denial of private/metadata hosts; oversized/slow response rejection.

### P1-3 · Incomplete production HTTP middleware (no timeout, no panic catch, no per-route body limits) [BE] · *Verified*
- **Evidence:** Only one global `DefaultBodyLimit::max(512*1024)` in `src/lib.rs:42`. No `TimeoutLayer`, `CatchPanicLayer`, per-route public/admin limits, or compression-disable for once-only secret responses. `docs/todo.md:26-27` open. **Verified nuance:** a minimal secure-headers middleware *does* exist — `secure_response_headers` (`src/lib.rs:41,92-105`) sets `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` on every response — so the gap is timeout/panic/per-route limits plus the remaining headers (e.g. HSTS behind TLS), not headers from zero.
- **Impact:** A panic in a handler can drop the connection ungracefully; no request timeout ceiling; oversized-request policy is not aligned to config (`maximum_request_bytes` config vs the hardcoded 512 KiB layer).
- **Correction:** Add a middleware stack (timeout, panic → 500 envelope, complete secure headers, redacted tracing span, per-route body limits, no compression on secret responses). See **plans/03**.
- **Expertise:** Rust/Axum, Security. **Dependencies:** none. **Tests:** oversized-JSON 413; panic → error envelope; timeout → 504; headers present.

### P1-4 · List `cursor` param accepted but silently ignored (no real pagination) [BE] · *Verified*
- **Evidence:** `src/domain/admin.rs:8-17,34-46` define `PageQuery.cursor` + `Pagination{next_cursor,has_more}`, but `src/http/admin.rs:136-146` → `src/application/admin.rs:93-103` never read the cursor; admin repository SQL is `order by created_at desc limit $1` with no cursor predicate or `id` tiebreaker. `next_cursor` is effectively always `None`. **Verified scope:** this spans **all ~18 `list_*` service methods** (9 in `src/application/admin.rs`, 5 in `src/application/conversation.rs`, 4 in `src/application/public.rs`) — `cursor` has **zero** references in any application or repository file. The conversation surface additionally declares `cursor` on 4 filter DTOs (`src/domain/conversation.rs:188,204,510,567`); its repo SQL already has `(…, id desc)` tiebreakers (e.g. `conversation.rs:538,811,921,1096`) but still no cursor predicate.
- **Impact:** Clients cannot page anywhere; supplying `cursor` is silently a no-op (an API-contract bug). Large result sets are truncated at a fixed limit.
- **Correction:** Real keyset pagination on `(created_at DESC, id DESC)` with opaque base64 cursor and `has_more`/`next_cursor`. See **plans/04**.
- **Expertise:** DB/backend, API. **Dependencies:** none. **Tests:** page-through determinism, tamper-rejection of opaque cursor.

### P1-5 · No retention/cleanup for expired idempotency records & metadata-only responses [BE] · *Verified*
- **Evidence:** `src/infra/workers.rs:1-163` `WorkerRegistry` only enumerates `WorkerSpec`s (including a `"retention-cleanup"` name) for `/metrics`; there is **no** execution body, no `DELETE … WHERE expires_at < now()`. Supervisor loop only records ticks (`:120-150`).
- **Impact:** `idempotency_records`, expired `responses`, and related rows grow unbounded — storage exhaustion and slow scans over time.
- **Correction:** Implement a retention worker (bounded batched deletes, configurable TTLs, metrics). Single-replica: plain tokio task; multi-replica later gated by leader election. See **plans/04**.
- **Expertise:** DB/backend. **Dependencies:** none. **Tests:** expired rows deleted, live rows retained, batch bounds respected.

### P1-6 · Execution deadline does not bound credential resolution, runtime construction, or terminal persistence [BE] · *Verified*
- **Evidence:** `src/application/execution.rs:132` sets `execution_deadline`; `:327` only *rejects new attempts* past it. `resolve_credential` (`:249`), `runtime_handle` (`:304`), and terminal persistence (`:509-541`) are plain awaits with no timeout/`select!`. Only the provider call (`:500-503`) is `timeout`-wrapped. `docs/todo.md:39` open.
- **Impact:** A slow credential decrypt or DB stall can extend an execution indefinitely past its advertised deadline, defeating client timeout guarantees.
- **Correction:** Wrap each phase in the remaining-deadline budget (with active-attempt cleanup preserved). See **plans/04**.
- **Expertise:** Rust async, backend. **Dependencies:** none. **Tests:** injected slow cred/runtime → deadline respected, permits released.

### P1-7 · Streaming client-stall cancellation has no DB-backed integration test [BE] · *Verified (code) / Inference (behavior)*
- **Evidence:** `src/application/public.rs:386-475,1791-1802` cancel via bounded `send_timeout` + `public_tx.closed()`; permits `drop`ped unconditionally at `src/application/execution.rs:504`. Code path is plausibly correct, but `docs/todo.md:46` correctly notes there is **no** DB-backed test proving a stalled reader leaves no attempt `started` / response `in_progress`.
- **Impact:** A subtle interleaving (timeout firing mid-attempt vs DB update ordering) could leak a permit or leave a stuck status — unverified.
- **Correction:** Add the integration test with an acknowledgement-gated producer/consumer (not sleep-based). See **plans/04**.
- **Expertise:** Test/reliability, async. **Dependencies:** test harness. **Tests:** the test itself is the deliverable.

### P1-8 · `If-Match` optional on application execution-policy PUT [BE] · *Verified*
- **Evidence:** `src/http/admin.rs:319,339` declare If-Match as `Option<i64>` and use `optional_if_match`, unlike all other versioned mutations (`require_if_match`). `docs/todo.md:21` open.
- **Impact:** Concurrent PUTs to execution policy silently last-write-win (lost update).
- **Correction:** Require `If-Match`; return `409 resource_version_conflict` on stale version. See **plans/04**.
- **Expertise:** API/backend. **Dependencies:** none. **Tests:** stale-version 409; concurrent PUT lost-update prevented.

### P1-9 · OpenTelemetry is a fully dead dependency; Prometheus metrics are aggregate counters only [BE] · *Verified*
- **Evidence:** `opentelemetry = "0.32"` is declared in `Cargo.toml:19` with **zero** references anywhere in `src/` — a fully dead dependency, not "partially wired". `src/config/telemetry.rs:5-26` only inits `tracing_subscriber` fmt/json; the `otel_enabled`/`otel_endpoint` config fields (`src/config/settings.rs:211-212`, defaults `false`/`None` at `:678-679`) have no readers (dead flags). `src/infra/metrics.rs:19-91` is `AtomicU64` counters — no histograms/percentiles/per-route cardinality.
- **Impact:** No latency/TTFT/error distributions, no trace export — operating even a single-replica controlled MVP blind to latency SLOs and root-causing.
- **Correction:** Wire OTel SDK + OTLP exporter behind the existing flag; add Prometheus histograms for HTTP/execution latency, TTFT, provider outcomes, DB pool. See **plans/05**. (Grafana/Alertmanager assets are P2.)
- **Expertise:** DevOps/observability, Rust. **Dependencies:** none. **Tests:** metrics endpoint exposes histograms; trace pipeline smoke test.

### P1-10 · No CI OpenAPI-drift gate; no secret/content-leak snapshot suites [BE] · *Verified/Inference*
- **Evidence:** `.github/workflows/ci.yml` runs fmt/clippy/test only; OpenAPI coverage is asserted in-process (`src/http/mod.rs` tests) but there is no committed-spec-vs-generated diff gate (`docs/todo.md:112`). Secret-leak assertions are scattered unit tests (`src/security/masking::tests`, `src/infra/repositories/setup.rs:165`) with no systematic HTTP/log/audit snapshot suite; no prompt/content-leak suite at all (`docs/todo.md:113-114`).
- **Impact:** Spec can silently drift; a future change could leak a secret/prompt into a response/log/audit without a failing test.
- **Correction:** Commit the generated spec + CI diff check; add snapshot suites over HTTP bodies, OpenAPI, audit metadata, and logs asserting no secret/ciphertext/nonce/prompt material. See **plans/05**.
- **Expertise:** Test/reliability, security. **Dependencies:** P0-2/P0-3 (stabilize spec first). **Tests:** the suites themselves.

### P1-11 · Identity foundation absent — no owner/admin claiming, no user model [BE][UI][OAuth] · *Verified*
- **Evidence:** Exhaustive grep: no OAuth/OIDC/session/login code, no `users` table, no cookie/session store anywhere (`migrations/0001-0008`, `src/`). Only machine credentials exist (system keys, consumer keys, trusted JWT issuers). **Verified nuance:** a `GET /api/v1/admin/setup/status` endpoint already exists (`src/http/admin.rs:32-49` → `SetupService`), but it reports **structural** readiness (DB/config state) only — it knows nothing about admin-identity claiming, and the new claim status must not be confused with it (extend it or add a distinct field; see plans/07).
- **Impact:** There is no way to grant a **human** admin authority, no "setup-required" concept, and thus no safe basis for a Next.js admin console or OAuth login. This is the gating prerequisite for all UI/identity work.
- **Correction:** Add a Moira-native **owner/admin claiming** capability: system-key-gated grant of admin scope to a `(issuer, subject)` from a trusted JWT issuer, plus setup-required detection. See **plans/07**. (This is a backend prerequisite, deliberately separated from the UI iteration.)
- **Expertise:** Security, backend, DB. **Dependencies:** P1-1/P1-2 (harden auth first). **Tests:** claim requires valid system key; first-login-wins impossible; issuer+subject binding enforced.

---

## P2 — Important hardening / near-term

| ID | Finding | Evidence | Blocks | Verified? |
|----|---------|----------|--------|-----------|
| P2-1 | `AdminService` god-object: 1,873 lines, 48 public methods spanning ~9 bounded contexts | `src/application/admin.rs` | — | Verified |
| P2-2 | Rig type leaks into domain layer (`rig_core::completion::Message` in a domain DTO) | `src/domain/runtime.rs:2,249` | — | Verified |
| P2-3 | Repository trait coverage incomplete — only `AdminRepository` has a trait; public/runtime/conversation/setup repos are concrete-only | `src/infra/repositories/{public.rs:22,runtime.rs:25,conversation.rs:33,setup.rs:18}` | — | Verified |
| P2-4 | Dead/legacy `src/orchestration/resolver.rs` provider path not on the live routing route; it also carries a divergent 3-part credential AAD (`resolver.rs:272-281`) incompatible with the live 8-field AAD — it could not decrypt live credentials | `resolver.rs:89-157` vs `src/application/execution.rs:1220-1263` | — | Verified |
| P2-5 | Health/circuit state not an input to candidate ranking (only post-selection gate) | `src/infra/repositories/runtime.rs:701-722`; `execution.rs:278-302` | routing quality | Verified |
| P2-6 | Connection pool dev-scale: `max_connections:10`, no `idle_timeout`/`max_lifetime` | `src/infra/db.rs:26-33`; `src/config/settings.rs:512-514` | load | Verified |
| P2-7 | Embedding dimension policy not tied to the hardcoded `vector(1536)` | `migrations/0007…:103,283-296` | future RAG | Inference |
| P2-8 | i18n JSON catalog is a hand-synced mirror with no drift test | `src/i18n/catalog/mod.rs:9`; `docs/i18n-response-catalog.json` | — | Verified |
| P2-9 | Per-endpoint unknown-query-field rejection is global (`deny_unknown_fields` on one shared `PageQuery`), untested | `src/domain/admin.rs:31-34` | — | Verified |
| P2-10 | Dockerfile base images tag-pinned but not digest-pinned; no SBOM/SAST/DAST/secret-scan in CI | `Dockerfile`; `.github/workflows/ci.yml` | — | Verified |
| P2-11 | Helm broad egress `0.0.0.0/0` (ports 5432/6379/443); PDB disabled by default | `charts/moira/templates/{networkpolicy,pdb}.yaml` | prod hardening | Verified |
| P2-12 | `sleep()`-based interleaving in concurrency tests — latent CI flake | `tests/admin_idempotency.rs:977,1259`; `tests/execution_lifecycle.rs:979,1002` | — | Verified |
| P2-13 | Integration tests share one DB with no truncate/rollback teardown (UUIDv7-name isolation only) | `tests/support/mod.rs:125-127,427-471` | — | Verified |
| P2-14 | Circuit-breaker global `reset_all()` on every unrelated runtime-config NOTIFY (unnecessary churn) | `src/infra/db.rs:43-80` | — | Verified |

---

## P3 — Later / genuinely post-MVP

| ID | Finding | Evidence | Blocks | Note |
|----|---------|----------|--------|------|
| P3-1 | In-memory rate limiting, concurrency permits, and circuit breakers (per-process) | `src/orchestration/controls.rs:149-609`; `src/app/state.rs:35,77-78` | **[MR]** | The hard single-replica constraint. Runtime-cache invalidation already works cross-instance via Postgres `LISTEN/NOTIFY` (`src/infra/db.rs:43-80`), so that is *not* in this bucket. |
| P3-2 | No cluster admission / DB-lease preventing scaling past 1 replica (only a Helm template-time `replicaCount==1` guard; `kubectl scale` bypasses it) | `charts/moira/templates/_helpers.tpl:18-26` | **[MR]** | |
| P3-3 | Redis connected but functionally idle (health check only — no distributed limiter/lock/pubsub) | `src/infra/redis.rs`; `src/http/health.rs:60-61` | **[MR]** | |
| P3-4 | No leader election for singleton workers | `src/infra/workers.rs` | **[MR]** | |
| P3-5 | No durable worker queues (in-process supervisor only) | `src/infra/workers.rs:120-150` | — | |
| P3-6 | Rig `Agent`/`AgentRunner` tool path not wired (direct completion only; tools explicitly disabled) | `src/application/execution.rs:1443-1586` | — | Declared-incomplete, not broken. |
| P3-7 | Custom providers configurable but rejected at runtime | `docs/todo.md:35` | — | |
| P3-8 | Full RAG/memory intelligence (extraction, embeddings, retrieval, chunking, summarization, citations) | Phase 5 of `docs/todo.md` | — | Large; see plans/11. |
| P3-9 | Docs drift: `project-structure.md` omits the largest layer `src/application/`; dead `src/http/chat.rs`; `owner_scope` mislabeled as legacy though load-bearing | `docs/project-structure.md:8-21`; `src/http/chat.rs`; `resolver.rs:274-278` | — | |

---

## Positive findings (verified strengths — do not regress)

- **Atomic admin idempotency is genuinely correct and DB-backed** (not in-memory): single transaction, `pg_try_advisory_xact_lock` single-winner, savepoint-scoped business-failure rollback, `finalize` once-only, actor-fingerprint isolation. Covered by `tests/admin_idempotency.rs` (9 tests). Multi-replica safe. (`src/infra/repositories/admin.rs:560-726`)
- **Credential crypto is correct:** AES-256-GCM; the live AAD (`CredentialAadParts`, `src/security/crypto.rs:84-111`) binds 8 fields; tamper test present (`crypto.rs:118+`). Live call sites: `src/application/admin.rs:506-516,645-655,695-708`, `src/application/execution.rs:814`. (Note: the dead `resolver.rs:272-281` carries a divergent legacy 3-part AAD — see P2-4.)
- **Credential precedence** matches the design doc's 8-tier order exactly in the live SQL (`src/infra/repositories/runtime.rs:818-830`; `resolver.rs:254-268` duplicates it on the dead path).
- **Single coherent error envelope** with i18n `message_key`, enforced by `tests/http_error_contract.rs`. (`src/error.rs:53-65`)
- **OpenAPI is code-generated via `utoipa`** with in-repo route-coverage tests — low structural drift risk. (`src/http/mod.rs:23-165`)
- **Runtime-cache invalidation is cross-instance** via Postgres `LISTEN/NOTIFY`. (`src/infra/db.rs:43-80`)
- **CI already runs** fmt + `clippy --workspace --all-targets --all-features -D warnings` + `test --workspace --all-features` + a migration-contract job + Trivy + `helm lint`/`kubeconform`. (`.github/workflows/ci.yml`)
- **Container hardening baseline:** multi-stage, non-root UID 10001, healthcheck; Helm probes, resource limits, `runAsNonRoot`, `readOnlyRootFilesystem`, dropped capabilities.
- **`/v1/chat/completions` correctly unregistered**; `/v1/responses` subset scoped as documented.
- **`cargo test` green (120/120)**, `fmt`/`clippy` clean.

---

## `docs/todo.md` reconciliation

Verified each relevant TODO against the implementation. Categories:

**Completed — should be removed from todo.md (Verified):**
- Phase 2 "Reject unknown query fields consistently on all admin list/filter endpoints" — implemented globally via `deny_unknown_fields` (P2-9); reword rather than delete (still needs a test + per-endpoint nuance).
- (Implicit) The atomic-admin-idempotency and credential-rotation-version-header items this branch delivered — confirmed done by `tests/admin_idempotency.rs` and commit history; ensure they are marked done.

**Partially completed — should be rewritten (Verified):**
- Phase 1 "unkeyed admin command request hashes → HMAC-SHA-256 pepper" — still fully open (P1-1). Keep.
- Phase 2 "If-Match on every versioned mutation" — done *except* execution-policy PUT (P1-8). Narrow the TODO to that one endpoint.
- Phase 3 "extend execution deadline across routing/cred/runtime/persistence" — partially covered (provider call only); rewrite to name the exact unbounded phases (P1-6).
- Phase 4 "client stops reading without disconnecting" test — code exists, test missing; keep as a test-only item (P1-7).
- Cross-Phase "OpenAPI generation validation in CI" — in-process assertions exist, CI gate missing; keep (P1-10).

**Missing work not recorded (new findings not in todo.md):**
- P0-4 broken `cargo deny` (no `deny.toml`) — **not tracked**; add.
- P1-4 list `cursor` silently ignored (todo says "simplified pagination" but understates: it is *non-functional and silently accepted*) — rewrite.
- P2-2 Rig type leak into `src/domain` — **not tracked**; add.
- P2-14 circuit `reset_all()` over-broad invalidation — **not tracked**; add.
- P3-9 docs drift (`project-structure.md` missing `src/application/`) — **not tracked**; add.

**Duplicated / obsolete framing (Verified):**
- Phase 1 bundles `owner_scope` with "unregistered chat-route types" as legacy to remove — but `OwnerScope` is **live, load-bearing** credential-scope code (`resolver.rs:274-278`). Split: delete-`chat.rs` is valid (P3-9); "remove owner_scope" is wrong.

**Not required for MVP (correctly deferred — keep under "Not TODO"/Phase 6):**
- All of Phase 6 distributed/multi-replica items (P3-1..P3-5), full Phase 5 RAG/memory intelligence (P3-8), Rig Agent tool path (P3-6), custom-provider runtime (P3-7), external secret managers, load/chaos automation. These are correctly post-MVP and must **not** be pulled into security-critical iterations.

---

## Confidence & limitations

- **Verified facts** dominate P0/P1 (code read + `cargo test` executed).
- **Inferences:** streaming-stall behavior (P1-7) and embedding-dimension risk (P2-7, pipeline not yet built). OTel "0% wired" was upgraded to **Verified** (the `opentelemetry` crate has zero `src/` references and the config flags have no readers).
- **Unverified / environment-limited:** end-to-end multi-replica behavior, real OpenAPI-consumer drift, and secret/content-leak at HTTP/log layer — no harness exists to exercise these yet; they are precisely what P1-10 adds.
