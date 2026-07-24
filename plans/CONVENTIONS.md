# Cross-Cutting Conventions (binding on every plan)

Authoritative rules that **every** iteration plan (`02a`–`11`) must comply with. Where a plan's own text conflicts with this file, **this file wins** and the plan must be corrected.

All version facts below were verified by web research on **2026-07-25** and must not be changed without re-verification.

---

## 0. Product-owner decisions (RESOLVED — do not reopen)

Recorded **2026-07-25**. These were open questions across the plans; they are now decided and binding. A plan still presenting one of these as "product input required" is out of date and must be corrected.

| # | Decision | Consequence |
|---|----------|-------------|
| **D1** | **P0-2 is fixed by implementing real idempotency replay**, not by removing the `Idempotency-Key` parameter and rejecting with `501`. | The parameter **stays** in the OpenAPI spec on conversation/memory/RAG routes because it is about to become true. |
| **D2** | **The work is split into two branches/PRs**: **02a** (honesty — no migrations, ships fast, closes P0-1/P0-3) and **02b** (replay — closes P0-2, stacked on 02a). | The truthful-API fix is not delayed by the replay implementation and its concurrency tests. |
| **D3** | **Email/domain allow-list is deny-by-default.** An unconfigured list denies every claim. **No first-claim exemption and no bootstrap bypass** — do not add one. | The operator must configure allowed domains before the first admin claim succeeds; this is expected behaviour, not a bug. Error: coded `403 admin_claim_domain_not_allowed`. |
| **D4** | **`GET /api/v1/admin/setup/auth-methods` stays authenticated** (SystemKey \| TrustedJwt + `moira:setup:read`). | The console calls it **server-side** with its system key, never from the browser. Prevents anonymous reconnaissance of the identity configuration. Deliberately contrasts with the anonymous `GET .../setup/claim-status`, which returns only `{"claimed": bool}`. |
| **D5** | **`email` + `email_verified` are required on BOTH claim paths** — system-key and setup-token alike. | `ClaimAdminIdentityRequest.email` is **non-optional** in the DTO and OpenAPI schema (plans 08/09 bind to this). The deny-by-default domain policy is therefore enforceable on every path with no bypass, and every grant carries a human-identifiable audit attribute. |
| **D6** | **Prometheus histograms use the `metrics` facade + `metrics-exporter-prometheus`**, not hand-rolled buckets. | Correct cumulative-bucket semantics, `le="+Inf"` handling, label escaping, and exposition formatting come from the library. Accepted cost: two new dependencies; the hand-rolled `render_prometheus` (`src/infra/metrics.rs:114`) is replaced. The `/metrics` route's `prometheus_enabled` gating and `moira.error.metrics_disabled` contract are preserved unchanged. **`metrics-exporter-prometheus` MUST be declared `default-features = false`** — its default features start an independent HTTP listener that would bypass the `prometheus_enabled` gate. |
| **D7** | **The OAuth client secret is owned by the console, stored in the console's own database — Moira never stores it and never returns it.** | Resolves a real design gap: Better Auth needs the plaintext secret in process to run the code exchange, but Moira's secret envelope is write-only by design. **Moira's load-bearing invariant is preserved: a decrypted secret never crosses a network boundary.** Consequences below. |

### D7 consequences (binding)

**Moira side — the client secret is removed from `auth_provider_settings` entirely.** Delete from plan 07's spec: the encrypted-secret envelope columns (`encrypted_payload`, `encryption_algorithm`, `encryption_version`, `encrypted_data_key`, `nonce`, `secret_fingerprint`, `masked_secret`), the `POST /api/v1/admin/auth/providers/{id}/rotate-secret` endpoint, the `auth_provider_secret_aad` / `AuthProviderSecretAadParts` addition to `src/security/crypto.rs`, and the `auth_provider_secret_rebind_required` (409) error and its i18n key. `auth_provider_settings` keeps **non-secret config only**: issuer, discovery/authorization/token/userinfo/JWKS URLs, client id, requested scopes, `allowed_email_domains`, allowed algorithms, audiences, redirect URIs, `trusted_jwt_issuer_id`, `enabled`, `version`. The frozen contract drops from 11 operations to 10.

**Console side — the console owns the secret.** It is stored in the console's own `console_auth` database (which Better Auth already requires), encrypted at rest, written by the setup wizard, never sent to Moira, never exposed to the browser, never in `NEXT_PUBLIC_*`.

**Drift protection is mandatory.** Two config stores means they can diverge — a `client_id` changed in Moira while the console still holds the old client's secret would fail the code exchange with an opaque provider error. Required mitigations: (1) the wizard writes Moira's provider config and the console's secret **in the same step**, and treats partial success as a failure the operator must resolve; (2) the console stores a **fingerprint of the `client_id`** alongside the secret and compares it against Moira's `client_id` on load, surfacing a specific, actionable keyed error on mismatch rather than letting the OAuth flow fail obscurely; (3) an e2e test asserts the mismatch path produces that actionable error.

---

## 1. Branch & pull-request workflow (one plan = one branch = one PR)

Each iteration plan is executed on its **own branch** and lands via **its own pull request**. No plan may be implemented directly on `main`, and no two plans may share a branch.

| Plan | Branch |
|------|--------|
| 02a | `plan/02a-mvp-boundary-honesty` |
| 02b | `plan/02b-idempotency-replay` (stacked on 02a) |
| 03 | `plan/03-security-hardening` |
| 04 | `plan/04-durability-correctness` |
| 05 | `plan/05-observability-ci-gates` |
| 06 | `plan/06-architecture-test-hygiene` |
| 07 | `plan/07-identity-foundation` |
| 08 | `plan/08-nextjs-console-google-oauth` |
| 09 | `plan/09-generic-oidc-github-invitations` |
| 10 | `plan/10-multi-replica-readiness` |
| 11 | `plan/11-rag-memory-intelligence` |

**Rules**
1. Branch from the **current `main`** (not from another plan branch) unless the dependency graph in `01-roadmap-and-dependencies.md` requires stacking; if stacked, the PR description must name the base PR and the branch must be rebased once the base merges.
2. **Conventional Commits** (`feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `chore:`) — matching the existing history style (`feat: make admin commands atomic`).
3. The PR **must not** be opened until every gate in §2 passes locally.
4. PR description template (required sections): **Plan link** (`plans/NN-*.md`) · **Findings addressed** (P-IDs from `00-audit-report.md`) · **Migrations included** (filenames, or "none") · **Breaking API/OpenAPI changes** · **Test evidence** (unit + e2e output summary) · **Rollback procedure** · **Deferred follow-ups**.
5. A plan is **not done** when the PR opens — it is done when the PR is merged with all gates green and the plan's Definition of Done objectively verified.
6. Plans that change the OpenAPI surface must land **before** plan 05's OpenAPI-drift gate freezes the spec (see `01` §3 ordering).
7. Never force-push a branch another plan is stacked on.

---

## 2. Required gates (every PR, no exceptions)

**Rust (all plans touching `src/`, `migrations/`, `tests/`)**
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release --locked
```
plus clean PostgreSQL migration validation (migrations apply from an empty database).

**Frontend (plans 08, 09)**
```bash
bun install --frozen-lockfile
bun run lint
bun run typecheck
bun test                # unit
bunx playwright test    # e2e
bun run build
```

---

## 3. Testing: unit **and** e2e are mandatory

**Every plan MUST deliver both a unit-test layer and an end-to-end layer.** A plan with only one layer is incomplete and must not be merged. "e2e" means the behavior is exercised through its real external surface, not through an internal function call.

### Rust
- **Unit** — `#[cfg(test)] mod tests` beside the code (pure logic, mappers, validators, hashing, cursor encode/decode, policy decisions). No database required.
- **E2E / integration** — a file under `tests/` driving the real HTTP surface against a **real PostgreSQL 16 + pgvector** (the audit environment), following the existing harness in `tests/support/mod.rs`. Existing exemplars to imitate: `tests/admin_idempotency.rs` (9 tests), `tests/execution_lifecycle.rs` (14 tests), `tests/public_authorization.rs`, `tests/http_error_contract.rs`.
- **Concurrency tests must use acknowledgement gates, not `sleep()`** (see finding P2-12). New sleep-based interleaving is rejected in review.
- DB-dependent tests must fail closed in CI (`panic!` when `CI` is set and `MOIRA_TEST_DATABASE_URL` is absent) — the existing pattern.

### Frontend (plans 08, 09)
- **Unit** — `bun test` for pure modules (Moira client, JWT/claim helpers, policy guards, atoms/molecules rendering).
- **E2E** — **Playwright** against a running console + a real (test-fixture) Moira instance: setup wizard, sign-in, sign-out, a config round-trip, and an authorization-denial path. OAuth must be driven by a **local mock OIDC provider**, never real Google, in CI.
- **Accessibility** — automated a11y assertions (axe) on every page-level route.
- **Secret-leak test** — assert no Moira system key, admin key, or decrypted credential appears in any client bundle, HTML payload, or browser-visible response.

### Definition of Done addition (all plans)
A finding or `docs/todo.md` item may only be marked complete when a **named, passing test** proves the behavior. "Implemented" is not "done."

---

## 4. i18n: every response carries a message key **and** a default English message

**Requirement: every user-visible response — error *and* success/notice — MUST carry a stable i18n message key plus a default English message.** No handler may return a hardcoded human string that has no catalog entry.

### Existing machinery (reuse it; do not invent a parallel system)
- Catalog entries are `I18nEntry { key, default_message, description }` in **`src/i18n/catalog/errors.rs`** (`moira.error.*`) and **`src/i18n/catalog/notices.rs`** (`moira.notice.*`).
- The wire envelope is `ErrorResponse { error: ErrorDetail { code, message_key, message, message_args, request_id, details } }` (`src/error.rs:52-65`).
- `message_key` is derived as `format!("moira.error.{}", code())` (`src/error.rs:146-148`) — so **every new error `code` requires a matching catalog entry with the same suffix**.
- `docs/i18n-response-catalog.json` is the documentation mirror of the Rust catalog.

### Rules
1. Every new error code added by a plan → a new `moira.error.<code>` entry in `errors.rs` with an English `default_message` and a `description`.
2. Every new success/notice string → a `moira.notice.*` entry in `notices.rs`. Never inline an English literal in a handler response.
3. `message_args` carries interpolation values as structured data — never pre-formatted English prose.
4. Update `docs/i18n-response-catalog.json` in the same PR (it is hand-synced today; plan 06 adds the drift test — until then, sync manually and treat drift as a review failure).
5. **Test requirement:** each plan adds an assertion that its new keys exist in the catalog and that responses carry a non-empty `message_key` + `message`. `tests/http_error_contract.rs` is the exemplar.
6. **Frontend:** the console renders `message_key` through its own i18n layer and falls back to the server-supplied `message`. The console must never hardcode English copy for a server-originated condition.

---

## 5. Frontend toolchain (plans 08, 09) — verified 2026-07-25

| Tool | Pinned choice | Note |
|------|---------------|------|
| **Next.js** | **16.2.11** (latest stable, released 2026-07-21) | App Router. 16.3 is canary/preview only — do not use. |
| **Node.js** | **24.x — Active LTS** (EOL 2028-04-30) | Node 26 is *Current*, not LTS until Oct 2026 — do not use. Node 22 is Maintenance-only. |
| **Bun** | **1.3.14** (released 2026-05-13) | Package manager, script runner, and unit-test runner. |
| **React** | as bundled with Next.js 16.2.11 | Do not pin independently. |

**Rules**
- `bun install --frozen-lockfile` in CI; `bun.lock` is committed.
- Pin exact versions in `package.json` (`"next": "16.2.11"`), and pin Node via `.nvmrc` / `engines` (`"node": ">=24 <25"`).
- Bun is the package manager and test runner; **Playwright** remains the e2e runner (`bunx playwright test`).
- The console lives in its own directory (`console/`) with its own `Dockerfile` and Helm chart additions — it is a **separate deployable** from the Rust service.

---

## 6. Frontend architecture: Atomic Design (mandatory)

The console's UI **must** follow Atomic Design with this exact mapping:

| Layer | Meaning in this project | Location |
|-------|-------------------------|----------|
| **Pages** | Next.js routes/pages (App Router route segments; server components; data fetching, auth guards, redirects) | `console/app/**/page.tsx`, `layout.tsx`, `route.ts` |
| **Organisms** | UI **modules** — composed, feature-aware sections that own a slice of a page (e.g. `SetupWizard`, `ProviderTable`, `CredentialForm`, `AuditLogPanel`) | `console/modules/<feature>/` |
| **Molecules** | Composite UI components built from atoms (e.g. `FormField`, `TableRow`, `ConfirmDialog`, `StatusBadgeGroup`) | `console/components/molecules/` |
| **Atoms** | Primitive UI components (e.g. `Button`, `Input`, `Label`, `Badge`, `Spinner`, `Icon`) | `console/components/atoms/` |

**Rules**
1. **Dependency direction is one-way:** pages → organisms → molecules → atoms. An atom must never import a molecule/organism; a molecule must never import an organism.
2. **Atoms and molecules are presentational and feature-agnostic** — no Moira API calls, no `next/navigation` side effects, no auth logic. They receive data and callbacks via props.
3. **Organisms (modules) own feature logic** — they may call server actions and the Moira client, and compose molecules/atoms.
4. **Pages own routing, auth gating, and server-side data fetching**, then delegate rendering to organisms. Keep page files thin.
5. **Secrets never descend past the page/server boundary** — a system key or decrypted credential must never be passed as a prop into an organism/molecule/atom, since those render client-side.
6. Every atom and molecule ships a **unit test**; every organism is covered by at least a unit test, and by an **e2e** test through the page that hosts it.
7. Shared, non-UI logic lives in `console/lib/` (e.g. `lib/moira-client.ts`, `lib/auth.ts`) — never in `components/`.

---

## 7. Authentication & authorization architecture (binding)

### 7.1 The split (do not blur it)
- **Authentication (who is the human)** happens in the **console BFF**, never in Moira.
- **Authorization (what may this identity do in Moira)** happens in **Moira**, which is the **system of record**: `trusted_jwt_issuers` + `admin_identities` `(issuer, subject)` grants + scopes. Moira never runs an OAuth flow and never stores passwords or sessions.

### 7.2 Auth is configured **in settings at runtime**, not baked into build-time env
Auth provider configuration is **runtime configuration owned by Moira's database** (consistent with how providers, models, routing, and credentials already work — `docs/project-structure.md`: "Runtime provider config belongs in PostgreSQL"). The setup wizard writes it; the console reads it at boot and on invalidation.

Consequences that plans 07/08/09 must honor:
- A migration-backed table stores enabled auth methods and their non-secret config (issuer URL, discovery URL, client id, allowed email domains, allowed algorithms, JWKS URL).
- **Client secrets are owned by the console, not Moira** (decision **D7** above). Moira's `auth_provider_settings` stores **non-secret config only**; the OAuth client secret lives encrypted at rest in the console's own `console_auth` database, written by the setup wizard, never sent to Moira, never returned to the browser. This preserves Moira's invariant that a decrypted secret never crosses a network boundary. *(Provider credentials — the AI-provider API keys — are unaffected and remain encrypted in Moira with `SecretCipher` + AAD as today.)*
- Changing auth settings must invalidate the runtime cache through the existing Postgres `LISTEN/NOTIFY` path (`src/infra/db.rs:43-80`).
- Bootstrap remains the existing out-of-band `bootstrap-system-key` CLI; the first admin is **claimed explicitly** (never "first login wins").

### 7.3 The three supported modes (all three must be reachable from settings)
1. **Google OAuth** — the default first-party option (verified email + hosted-domain policy).
2. **Custom OAuth / generic OIDC** — any provider via OIDC discovery (`discoveryUrl`/`issuer`), so self-hosted and enterprise IdPs work without code changes.
3. **Bring-your-own JWT via JWKS** — the operator registers a `trusted_jwt_issuer` (JWKS URL + allowed algorithms + audience) and Moira accepts that IdP's JWTs directly. **This path needs no console and no OAuth at all**, which is what keeps air-gapped and machine-to-machine deployments working.

### 7.4 Recommended plug-and-play stack: **Better Auth** in the console BFF
**Verified 2026-07-25:** the Auth.js/NextAuth team joined Better Auth in September 2025; Auth.js now receives security patches only, and **Better Auth is the recommended choice for new projects**. Better Auth covers all three modes above natively, which removes the hand-rolled JWT-minting code earlier drafts of plan 08 proposed:

| Requirement | Better Auth mechanism |
|-------------|----------------------|
| Google sign-in | built-in social provider |
| Custom OAuth / generic OIDC | **`genericOAuth` plugin** — OAuth 2.0 + OIDC with `discoveryUrl` auto-discovery and `issuer` validation |
| BFF→Moira short-lived JWT (Mode A) | **`jwt` plugin** — asymmetric signing and a **published JWKS endpoint** (custom path supported, e.g. `/.well-known/jwks.json`) that Moira registers as a `trusted_jwt_issuer` |
| Sessions, CSRF, rate limiting, MFA | built in |

**Why this fits Moira specifically:** the `jwt` plugin's JWKS endpoint is precisely the trust primitive Moira's existing `trusted_jwt_issuers` machinery already consumes — so the console becomes "just another trusted issuer," with **no new trust mechanism invented on the Moira side**.

**Known limitation to record honestly:** Better Auth does not provide enterprise SSO (SAML, or acting as an SP against external enterprise IdPs) out of the box. For SAML-based enterprise SSO, mode 3 (bring-your-own JWT/JWKS, fronted by the customer's own IdP or an SSO gateway) is the supported path. Plans must not claim SAML support.

### 7.5 Non-negotiable security rules
- The BFF-minted JWT **must not carry a `scope` claim** — Moira copies scopes from the JWT verbatim (`actor_from_trusted_claims`), so a self-asserted scope would bypass the `admin_identities` grant. Authorization must come from Moira's grant table alone.
- PKCE, `state`, `nonce`, and an exact redirect-URI allow-list are mandatory on every OAuth flow.
- Verified email required; email/domain allow-list is **deny-by-default**.
- Identity binds to stable **`(issuer, subject)`** — never to email alone.
- System keys, admin keys, and decrypted provider credentials must never reach the browser, `NEXT_PUBLIC_*`, or any client bundle.
- Sessions: httpOnly + Secure + SameSite cookies; logout clears the BFF session; Moira-facing JWTs are short-lived.

---

## 8. Compliance checklist (add to every plan's Definition of Done)

- [ ] Work performed on the plan's own branch; PR opened with the required description sections.
- [ ] All gates in §2 pass (Rust and/or frontend as applicable).
- [ ] **Unit tests** delivered and passing.
- [ ] **E2E tests** delivered and passing (HTTP-level for Rust; Playwright for console).
- [ ] Every new error/notice string has an i18n **key + English default** in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence.
- [ ] (Frontend) Next.js 16.2.11 · Node 24 LTS · Bun 1.3.14 pinned; Atomic Design layering respected with the one-way dependency rule.
- [ ] (Auth-touching) Config is runtime/DB-backed, secrets encrypted, no scope claim in minted JWTs, deny-by-default domain policy.
- [ ] No secret-leak: verified by test.
