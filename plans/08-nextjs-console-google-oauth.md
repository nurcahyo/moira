# Plan 08 — Next.js Admin Console (BFF): Setup Wizard, Better Auth & Google Sign-In

> **Compliance note.** This plan is written against `plans/CONVENTIONS.md` (verified 2026-07-25), which is authoritative and overrides any earlier draft of this file. The four substantive corrections CONVENTIONS forced into this revision are: (1) **Better Auth replaces Auth.js/NextAuth** as the console's identity layer (§7.4) and the hand-rolled JWT-minting/JWKS-exposure code the previous draft specified is **deleted** in favour of the `jwt` plugin; (2) **Atomic Design** (§6) replaces the previous ad-hoc `app/**/components/` layout — every concrete path below has been rewritten; (3) auth is **configured at runtime from Moira's DB-backed settings** (§7.2), not from build-time env, which changes the setup wizard's job and introduces a precisely-stated dependency on plan 07 (see **Dependency D-1**); (4) **product-owner decision D7** (CONVENTIONS §0 and its "D7 consequences (binding)" subsection) settles the client-secret custody question this plan previously carried as a *blocking, undecided Wave 0 coordinator item*: **the console owns the OAuth client secret and stores it in its own database; Moira never stores it and never returns it.** The three candidate resolutions the earlier draft listed — a Moira read-back endpoint, a separately-supplied deployment secret, or proxying the code exchange through Moira — are **closed**; the read-back endpoint option is **rejected and must not be reintroduced**. Every passage in this file that assumed `loadAuthSettings()` could read a secret back from Moira has been rewritten.
>
> **D7 in one paragraph.** Better Auth needs the **plaintext** client secret in console process memory to run the OAuth code exchange. Moira's secret envelope is write-only by design, and its load-bearing invariant is that *a decrypted secret never crosses a network boundary*. Preserving that invariant was judged more important than having a single configuration store. Therefore Moira's `auth_provider_settings` holds **non-secret config only** (issuer, discovery/authorization/token/userinfo/JWKS URLs, `client_id`, scopes, `allowed_email_domains`, algorithms, audiences, redirect URIs, `trusted_jwt_issuer_id`, `enabled`, `version`), the console's own `console_auth` database holds the secret **encrypted at rest**, and `POST /api/v1/admin/auth/providers/{id}/rotate-secret` **no longer exists** — rotation is a console operation. The unavoidable cost is **two configuration stores that can drift**, so the drift protections in *Console-owned client-secret storage* below are **mandatory, not advisory**.

---

## §0 — Wave 0: drift against the tree (audit 2026-07-27, HEAD `27b6e0c`, **plan 07 merged**)

**Read this section before any other.** The body below was written against a pre-07 tree and against
plan 07's *frozen contract table* rather than against what 07 actually shipped. Plan 07 has now merged
(`27b6e0c`), and `docs/openapi.json` — not any plan document — is the ground truth for the contract
this console consumes. A fresh citation-by-citation audit against the real tree found **five
blockers, six contract mismatches, six deployment mismatches, and eight stale Rust-side citations.**

The rule from plan 07's Wave 0 applies again: **where §0 and the body disagree, §0 wins.** The body is
not rewritten wholesale, because its *design* is still sound — it is the ordering, the contract
arithmetic, and the citations that rotted. Two things are corrected inline anyway (migration filename,
operation count), because leaving those wrong in the body is exactly how they ship.

**Line-reference convention.** Every `:NNN` below is the line number in the **pre-§0** file — i.e. a
checkout of `27b6e0c` before this section was inserted. Add **386** to locate the same line in the
current file. Section names are given alongside the numbers wherever the target is not obvious.

**Implementation status: `console/` is 0% implemented.** There is no `console/` directory, no
`package.json` anywhere in the repo, no `charts/moira-console/`, and no frontend job in
`.github/workflows/ci.yml` (its three jobs are `rust`, `supply-chain`, `container-and-helm`). Nothing
in this plan has been started, so every correction below is free to make — none of it is a migration
of existing code.

---

### §0.1 Blockers

#### B1 — the wizard as specified can NEVER produce a successful claim

**This is the one that matters.** An implementer who reads only the body will build the entire console
— wizard, dual write, Better Auth, JWKS, drift protection — and then hit a `403` on the very last step
of the very first run, with nothing in the plan to explain why. The plan's own happy-path e2e tests
`fresh_deployment_completes_when_auth_provider_is_configured_before_claim` (`:831`) and
`setup-wizard.spec.ts` (`:829`) **cannot pass as specified.**

**What Moira actually does.** `AuthSettingsService`/`governing_policy`
(`src/infra/repositories/auth_settings.rs:365-388`) selects the governing policy with:

```sql
select id, allowed_email_domains from auth_provider_settings
 where deleted_at is null and status = 'active' and enabled
   and (issuer = $1 or trusted_jwt_issuer_id = $2)
 order by (issuer is not distinct from $1) desc, created_at asc, id asc
 limit 1
```

`$1` is the **claim body's** `issuer`; `$2` is the `trusted_jwt_issuers.id` resolved *from that same
issuer string* by `resolve_active_issuer` (`src/application/identity.rs:137-146`,
`src/infra/repositories/identity.rs:172-189`). There is no third branch.

**What this plan sends.**

| Thing | Value this plan gives it | Where |
|---|---|---|
| claim body `issuer` | the **console's** `MOIRA_BFF_ISSUER_URL` | `:752` |
| `auth_provider_settings.issuer` | the **IdP's** issuer / discovery URL (Google, or the generic-OIDC IdP) — that is what `AuthSettingsStep` collects, and what `validate_method_shape` requires for `google_oauth`/`generic_oidc` | `:747`, `:174-177` |
| `auth_provider_settings.trusted_jwt_issuer_id` | **never set** — absent from the step's field list (`:747`), absent from the frozen-contract column list (`:122`), absent from the create-request description (`:114`) | — |

So at claim time: `$1` = console URL, `$2` = the console issuer's id. The stored row has
`issuer` = the *IdP* URL and `trusted_jwt_issuer_id` = `NULL`. **Neither branch matches.**
`policy = None` → `evaluate_claim_policy` returns `domain_not_allowed()` at
`src/application/identity.rs:237-240` → **`403 admin_claim_domain_not_allowed`, on every run,
forever.** No amount of correctly populating `allowed_email_domains` changes this: the row is never
selected in the first place.

The failure lands as a `403` and not the `400 unregistered_trusted_issuer` the body anticipates
(`:207`, `:264`) precisely *because* the plan already registers the console's JWT issuer before the
claim (step 10 before step 11) — `resolve_active_issuer` succeeds and then the policy lookup finds
nothing. Do not "fix" this by only reordering the `jwt-issuers` call; that is already correct in the
body and it is not sufficient.

**The shipped correct order** is `docs/admin-identity-claiming.md:7-14`, restated in-code as a doc
comment on `evaluate_claim_policy` (`src/application/identity.rs:224-227`):

> bootstrap the system key → **register the trusted JWT issuer** → create **and enable** an
> `auth_provider_settings` row carrying `allowed_email_domains` → claim.

**Required change (binding).**

1. Move `POST /api/v1/admin/jwt-issuers` (and its `GET` pre-check) **before** step 6a — it becomes the
   *first* Moira write of the wizard, not part of the claim action.
2. Set **`trusted_jwt_issuer_id`** on the `POST /api/v1/admin/auth/providers` body to the id returned
   by step 1. The field exists and is writable on the create request
   (`AuthProviderSettingsCreateRequest.trusted_jwt_issuer_id: Option<Uuid>`,
   `src/domain/auth_settings.rs:112`) and on the patch request (`:139`).
3. Keep `auth_provider_settings.issuer` as the **IdP's** issuer. It is load-bearing for
   `validate_method_shape` and for Better Auth composition; do not repurpose it as the console URL.

**This inverts four parts of the body. Change all of them:**

| Body location | What must change |
|---|---|
| Data-flow diagram `:150-230` | Steps 9–10 (`POST /jwt-issuers`) move up to become the new step 5a, ahead of 6a. The claim action (`:203-220`) shrinks to the claim call alone. |
| Wizard table `:734-741` | Row 4's `POST .../jwt-issuers` moves into row 2 (`AuthSettingsStep`) and runs first; row 4 becomes the claim call only. Row 2's advance gate gains a fifth condition: **`trusted_jwt_issuer_id` is set on the Moira row**. |
| Dual-write ordering `:515-519` | Becomes a **four**-step write: (0) `POST /jwt-issuers` → (1) `POST /auth/providers` **carrying `trusted_jwt_issuer_id`** → (2) `putProviderSecret` → (3) `POST .../{id}/enable` (still the commit point). The partial-state table at `:523-527` gains a row for "step 0 succeeded, step 1 failed" (an orphan trusted issuer — inert, but it must be surfaced and reused rather than re-registered on retry, or the second attempt gets a uniqueness conflict on `issuer`). |
| DoD `:1044` | "issuer self-registration first" is already the words used, but it must now mean *first of all four writes*, and the item must additionally assert `trusted_jwt_issuer_id` is set on the created provider row. |
| `SignInClaimStep` gate `:748`, `AuthSettingsStep` test `:814`, ordering spec `:830-835` | The "enabled provider with a non-empty allow-list" precondition becomes "enabled provider, non-empty allow-list, **and `trusted_jwt_issuer_id` bound to the console's issuer**". Add a named e2e: `provider_without_trusted_jwt_issuer_id_still_denies_the_claim` — it is the exact defect this blocker describes, and without it the regression is invisible. |

**An irony worth recording.** The swap is safe against `console_issuer_must_not_assert_scopes`
(`src/application/auth_settings.rs:333-353`) **only because** this plan already omits `scopes_claim`
from the `jwt-issuers` create body (`:751`). But that check short-circuits to `Ok(())` when
`trusted_jwt_issuer_id` is `None` (`:337-339`) — so **as currently written, plan 08 never exercises
its own most-stated invariant.** Fixing B1 is what finally makes the no-scope-claim rule mechanically
enforced by Moira rather than merely asserted by a console unit test. Add
`console_issuer_with_a_scopes_claim_is_rejected_at_provider_create` to the e2e suite.

**Fallback if provider-first ordering is required for UX reasons.** `trusted_jwt_issuer_id` is also
settable via `PATCH /api/v1/admin/auth/providers/{id}` with `If-Match`
(`src/domain/auth_settings.rs:139`). The sequence create-provider → register-issuer → patch-provider →
enable is equally correct. It is *not* preferred — it adds a fourth Moira round-trip and a fourth
partial state — but it is available and must be documented if chosen, not discovered.

#### B2 — the setup-token path is a hard `400`, not merely unused

The body describes `POST /api/v1/admin/setup/claim` as accepting `X-Moira-System-Key` **or** a
`setup_token` body field. It does not. `#/paths/~1api~1v1~1admin~1setup~1claim/post/security` in
`docs/openapi.json` is **`[{"systemKeyAuth": []}]` and nothing else**, and a populated `setup_token` is
rejected **twice** — at the handler (`src/http/identity.rs:126-132`) and again in the service
(`src/application/identity.rs:112-118`) — both with `400 setup_token_not_supported`. Plan 07 §0.2
decision **D1** deferred the whole path.

Wrong in the body at **`:85`** ("Required on both credential paths (system-key **and** setup-token)"),
**`:112`** ("`X-Moira-System-Key` **or** `setup_token` in body"), **`:264`** (same), and **`:797`**
("on the **system-key path and the setup-token path alike**"). The row at `:923` is already correct and
needs no change.

`:697` — the TypeScript field — **survives**, but must be documented as *reserved and rejected*, not
optional-and-unused, and typed to match the schema:

```ts
setup_token?: string | null;   // RESERVED. Populating it is a hard 400 `setup_token_not_supported`.
```

The schema declares `"type": ["string", "null"]` and omits it from `required`. Add a client unit test
`claim_builder_never_populates_setup_token` and add `setup_token_not_supported` to `moira-keys.ts`.

#### B3 — the claim endpoint's error list is missing eight codes, several on the wizard's own paths

The body enumerates four outcomes for `POST .../setup/claim` (`:264`, `:923`):
`unregistered_trusted_issuer`, `invalid_request`, `admin_claim_domain_not_allowed`,
`admin_identity_already_claimed`. The claim and auth-provider surfaces also emit, all catalogued
already in `src/i18n/catalog/errors.rs` — **the gap is entirely plan-08-side**:

| Code | Status | Emitted from | Console must |
|---|---|---|---|
| `setup_token_not_supported` | **400** | `src/http/identity.rs:129`, `src/application/identity.rs:115` | render keyed (B2) |
| `setup_claim_credential_required` | **401** | `src/http/identity.rs:136` | render keyed — this is what a missing/typo'd system key looks like, and it is *not* a session-expiry 401; it must **not** route into the `console.notice.session_revoked` sign-out flow at `:951` |
| `admin_claim_email_required` | **400** | `src/application/identity.rs:256` | actionable, routes back to sign-in |
| `admin_claim_email_not_verified` | **403** | `src/application/identity.rs:245` | actionable; distinct from `domain_not_allowed`, different remedy |
| `scope_invalid` | **422**, *not* 400 | `AuthorizationService::normalize_scopes`, `src/security/authz.rs:159`/`:165` (`AppError::unprocessable`) | see §0.2 on `scopes: []` |
| `console_issuer_must_not_assert_scopes` | **400** | `src/application/auth_settings.rs:346-350` | becomes reachable once B1 lands |
| `auth_provider_method_config_incomplete` | **400** | `src/application/auth_settings.rs:380-385` | the wizard's own provider write hits this on any incomplete method shape |
| `auth_provider_url_not_allowed` | **400** | `src/application/auth_settings.rs:423-427` | see B5 |

All eight go into `moira-keys.ts`, into the `:948-953` error-handling section, and into
`i18n-catalog-coverage.test.ts`'s mirrored-key assertion.

#### B4 — "enable is the commit point" is a console convention, not a Moira guarantee

The body asserts at `:517` and `:747` that "07 creates rows `enabled: false`", and the whole
"a provider is never enabled without its secret" safety property (`:183`, `:519`, `:1031`) rests on it.

`enabled` is a plain writable `bool` on the create request:

```rust
// src/domain/auth_settings.rs:89-90
#[serde(default)]
pub enabled: bool,
```

`#[serde(default)]` means *omitted* defaults to `false` — it does **not** mean the field is
server-controlled. A create body sending `enabled: true` lands an **enabled** provider with **no
console secret**, which is the exact state the dual-write ordering exists to make unreachable.

**Required:** the safety property must be enforced client-side, mechanically. Add to
`console/tests/unit/lib/moira-client.test.ts`:
`provider_create_never_sends_enabled` — no code path in `lib/moira-client.ts` constructs an
`AuthProviderSettingsCreateRequest` containing an `enabled` key at all (not `enabled: false` — absent),
and only the dedicated `enableProvider(id, version)` method may enable a row. Reword `:517` and `:747`
from "07 creates rows disabled" to "**the console never sends `enabled` on create**, so the row is
created disabled".

#### B5 — the e2e suite cannot run as specified: two different URL gates, two different remedies

The mock OIDC provider at `:828` and the CI `jwks_url` hit **two separate, differently-configured**
validators. The body mentions neither.

**(i) `auth_provider_settings` URLs — https-only, no escape hatch, no private-host check.**
`validate_https_url` (`src/application/auth_settings.rs:421-434`) is applied unconditionally by
`validate_urls` (`:388-411`) to `discovery_url`, `authorization_url`, `token_url`, `userinfo_url`,
`jwks_url` and every entry of `redirect_uris`. There is **no** `allow_http` flag on this path — a
`http://localhost:PORT` mock IdP is rejected `400 auth_provider_url_not_allowed`. It performs *only* a
scheme-and-host check (a fact the source comments on explicitly at `:414-420`), so
**`https://localhost:PORT` passes**. ⇒ the mock IdP needs **TLS**, and the console's e2e runner needs
the mock's CA trusted. No Moira config change is needed for this half.

**(ii) `POST /api/v1/admin/jwt-issuers` `jwks_url` — full SSRF check with a *different* flag.**
This does **not** go through `validate_provider_base_url`/`provider_security.allow_private_provider_urls`.
It goes through `reject_denied_jwks_url` (`src/application/admin/shared.rs:184-220`) →
`validate_jwks_url` (`src/security/ssrf.rs:342-...`), which requires `https` and rejects loopback,
private, and link-local addresses. The escape hatch is
**`auth.jwks.allow_insecure_dev_urls`** (`MOIRA_AUTH__JWKS__ALLOW_INSECURE_DEV_URLS=true`,
`src/config/settings.rs:137-140`) — and `Settings::validate` **hard-fails production** when it is set,
so it is a fixture-only knob and must be documented as such.

Two subtleties that matter for writing the fixture:

- `reject_denied_jwks_url` **soft-accepts** `Resolution` and `Timeout` denials (`shared.rs:194-206`):
  a host that does not resolve is logged and allowed through, and re-checked on every fetch. Only a
  *resolvable* loopback/private host is refused. Do not build the fixture on that behaviour — the
  console's JWKS must actually be fetchable for `authenticate_trusted_jwt` to work.
- The console's own `jwks_url` (the Better Auth `jwt` plugin's published document) is subject to (ii),
  not (i) — so in CI the console must be reachable from Moira over an https URL that is either
  non-loopback or covered by the dev flag.

**Required:** add to the e2e fixture section (`:827-828`) and to the Wave 0 checklist: a **TLS** mock
IdP, a **TLS** console origin, and `MOIRA_AUTH__JWKS__ALLOW_INSECURE_DEV_URLS=true` on the fixture
Moira with an explicit note that it is dev-only and production-invalid.

---

### §0.2 Contract mismatches vs `docs/openapi.json` (ground truth)

- **The auth-provider surface is SEVEN operations, not ten.** `GET`/`POST` on `/auth/providers`,
  `GET`/`PATCH`/`DELETE` on `/auth/providers/{id}`, and `POST` on `{id}/enable` and `{id}/disable`.
  Ten is the **total including the three setup operations** (`claim-status`, `auth-methods`, `claim`),
  which the body's own table (`:110-118`) lists — so `:106`'s heading "**10 auth-provider
  operations**", `:773`'s "**10 ops**" (that cell covers auth-settings endpoints only), and `:926`'s
  "(**10 operations**)" are all mislabelled, and `:120`'s "the four claim/setup rows above plus these
  six" is arithmetic that matches neither reading (there are **three** setup rows and **six table
  rows** covering **seven** operations, because `{enable,disable}` share a row).
  `docs/admin-identity-claiming.md:36` says "Seven operations." **The named test at `:802` —
  `there_is_no_rotate_secret_method`, asserting "the auth-provider surface is exactly **10**
  operations" — would fail as specified.** It must assert **7**.
  `:101`'s D7 before/after count (11 → 10) is a **total** and is correct; leave it.
  These are corrected inline — see §0.5.

- **`Idempotency-Key` is not declared on every mutating call.** `:709` ("on every mutating call"),
  `:928` ("(mutations)") and `:1048` are wrong, and so is the `moira-client.test.ts` assertion at
  `:796` ("`Idempotency-Key` present on every mutation"). Across the whole spec, 23 operations declare
  it, and every one is a POST-to-collection, a `/rotate`, or one of two `PUT`s. **No `PATCH`, no
  `DELETE`, no `enable`/`disable`, and no `refresh-jwks` declares it** — those declare `If-Match` +
  `X-Request-Id` only. Within the ten operations this plan binds to, exactly **two** declare it:
  `POST /api/v1/admin/setup/claim` and `POST /api/v1/admin/auth/providers`.
  Consequence for the dual write: **step 3 (`enable`) cannot be idempotency-keyed** — its retry safety
  comes from `If-Match` and from `enable` being naturally idempotent, and `:519`/`:527` should say so.
  Sending the header anyway is harmless at runtime (unknown headers are ignored), but the contract
  claim and the test assertion are false and must be narrowed to "every operation that declares it".

- **`AdminIdentityRecord` has a required `notice` field that this plan elides.** The schema's
  `required` list is `[id, issuer, subject, email, email_verified, granted_scopes, status, created_at,
  version, notice]`, where `notice` is a `ResponseText` i18n envelope. `:699-702` and `:756` omit it.
  `DoneStep` must render it through `t()` per this plan's own §4.6 rule (`:715-716`), and
  `lib/types.ts` must declare it non-optional. There is also a required `version` the TS shape elides.

- **The cache key at `:605` names a global settings version that does not exist.**
  `` `${moiraSettingsVersion}:${maxConsoleSecretUpdatedAt}` `` — `version` on
  `auth_provider_settings` is **per row**, incremented by that row's own writes. There is no
  deployment-wide settings version to read. Use `max(row.version)` across the fetched rows, or a hash
  of `(id, version)` pairs; a hash is safer, because `max()` cannot see a row **deletion**. Same
  correction applies to `:618`'s description of `getAuth()`'s cache key.

- **`scopes: []` is not the same as omitting `scopes`.** `ClaimAdminIdentityRequest.scopes` carries
  `#[serde(default = "default_admin_grant_scopes")]` (`src/domain/identity.rs:58`), so an **omitted**
  field yields `["moira:admin"]` — but an explicitly sent empty array normalises to an empty vector
  (`normalize_scopes` iterates and returns `Ok(vec![])`) and creates a grant with **zero scopes**: a
  silent, permanent, un-revocable-by-retry no-op admin. `:696` and `:752` correctly say "omitted", but
  nothing enforces it. Add `claim_builder_omits_scopes_entirely_never_sends_an_empty_array` to
  `moira-client.test.ts`. Note also that a *non-empty* bad scope yields **422 `scope_invalid`**, not
  400 (see B3).

- **`GET`/`POST /providers/{id}/models` — the path parameter is `{provider_id}`.** `:766`. The spec
  declares `/api/v1/admin/providers/{provider_id}/models`; `/provider-models/{id}` uses `{id}`. A
  hand-written client that guesses `{id}` for the first will build the wrong URL template.

- **`:124`'s scope claim is true, but by implication, not by grant.** The bootstrap system key does not
  literally carry `moira:auth-settings:{read,write,delete}`. `ActorType::SystemKey` is in
  `ADMIN_IMPLYING_ACTOR_TYPES` (`src/security/authz.rs:138-142`), so `has_scope` (`:148-152`) returns
  true for **any** known scope when the actor holds `moira:admin`. The three scopes must nonetheless
  exist in the known-scope list — they do (`src/security/authz.rs:43-45`) — because
  `AuthorizationService::require` returns a **500** for an unknown scope, not a 403. State the
  mechanism; do not restate it as "the console's system-key actor must hold them".

---

### §0.3 Deployment mismatches — the container baseline moved underneath this plan

- **`:1001` says the console's pod security context must "match the `charts/moira` hardening
  baseline". That baseline is now `runAsUser: 65532` / `fsGroup: 65532`**
  (`charts/moira/templates/deployment.yaml:27-29`), inherited from
  `gcr.io/distroless/cc-debian12:nonroot`. **`node:24-slim` has no uid 65532** — its non-root user is
  `node`, uid **1000**. Copying the baseline literally produces a pod that cannot start. The console
  chart must state **its own** uid (1000, or a uid it explicitly creates in the Dockerfile), and
  `:1001` must be reworded to "matches the *shape* of the `charts/moira` baseline
  (`runAsNonRoot`, dropped capabilities, `readOnlyRootFilesystem`) with a uid appropriate to the
  console's own base image".

- **`:277` and `:859` mandate a Dockerfile `HEALTHCHECK`. Moira's own Dockerfile no longer has one** —
  Kubernetes probes handle liveness/readiness, and a `HEALTHCHECK` is dead weight under an
  orchestrator. Worse, `node:24-slim` ships **neither `curl` nor `wget`**, so the instruction as
  written is unimplementable without adding a package (and attack surface) to the runtime image.
  **Recommendation: drop the `HEALTHCHECK` and specify `livenessProbe`/`readinessProbe` on
  `/api/health` in `charts/moira-console/templates/deployment.yaml` instead.** Keep the
  `app/api/health/route.ts` handler — it is still the probe target.

- **`readOnlyRootFilesystem: true` (`:1001`) needs writable mounts that are unmentioned.** A Next.js
  standalone server writes to `.next/cache` (ISR/fetch cache) and to `/tmp`. Add `emptyDir` volumes for
  both to the deployment template, or the pod crash-loops the first time it renders. This is a
  template requirement, not just an assertion.

- **`:860`'s "mirroring `charts/moira/templates/`" is an incomplete mirror.** `charts/moira/templates/`
  contains `networkpolicy.yaml`, `pdb.yaml`, `priorityclass.yaml` and `servicemonitor.yaml` in addition
  to the eight files `:860` lists. Either mirror them (a `NetworkPolicy` is arguably *more* important
  for the console, which is the internet-facing half) or state explicitly which are deliberately
  omitted and why. Silence reads as an oversight.

- **A second image needs its own Trivy step.** `.github/workflows/ci.yml`'s `container-and-helm` job
  runs `aquasecurity/trivy-action@v0.36.0` with `exit-code: "1"` and `severity: CRITICAL,HIGH` against
  `moira:ci` only, and `helm lint`/`helm template`/`kubeconform` against `charts/moira` only. Plan 08
  mentions neither. Add: `docker build -t moira-console:ci console/`, the same Trivy step against it,
  and the `helm lint`/`template`/`kubeconform` trio for `charts/moira-console` — plus the frontend
  gate job (`bun install --frozen-lockfile`, `lint`, `typecheck`, `test`, `playwright`, `build`) that
  `:976-983` describes but no workflow runs. **Note the practical risk:** `node:24-slim` is a Debian
  base with a far larger CVE surface than Moira's distroless image, and `exit-code: "1"` on
  CRITICAL,HIGH is a *hard* gate. Budget for either a distroless Node runtime or a documented,
  time-boxed `.trivyignore`.

- **`:1005-1010` lists `cargo build --release --locked` as a shipped Rust gate. It is not in
  `ci.yml`.** The `rust` job runs `cargo fmt --check`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `cargo test --workspace --all-features`, and a clean-database
  migration test. The other three lines are accurate. Drop the fourth, or add it deliberately.

---

### §0.4 Citation staleness

23 Rust-side citations were re-checked; **8 are stale**. Assume any unlisted line number is off by a
few and re-check before quoting it in code comments or docs.

| Body cite | Reality |
|---|---|
| `:62` — "`migrations/0001-0008`" | Migrations now run `0001`–`0013`. |
| `:77`, `:122` — "migration `0010_auth_provider_settings.sql`" | Shipped as **`0013_auth_provider_settings.sql`**. `0010` is `list_cursor_indexes`. **Corrected inline** (§0.5). |
| `:228` — "the `admin_identities` grant union in `src/security/auth.rs` (07 module 4)" | It is **module 7a / decision D2**: `apply_admin_identity_grant` (`src/security/auth.rs:901`), called from `authenticate_admin` (`:309`) at **`:334`**. Also at `:661` ("07 module 4's grant union") and `:939`. |
| `:661` — "`actor_from_trusted_claims` (`src/security/auth.rs:555-628`)" | **`:940`**. |
| `:667`, `:937` — "`validation.validate_aud = false`, `src/security/auth.rs:327-328`" | **`:556`** (admin path) and `:836` (caller path). |
| `:751` — "`TrustedJwtIssuerCreateRequest`, `src/domain/admin.rs:564`" | **`src/domain/admin.rs:588`**. |
| `:209`, `:751` — "07 module 3 `resolve_issuer_id`" | The function is **`resolve_active_issuer`** (`src/infra/repositories/identity.rs:172`, called at `src/application/identity.rs:139`). No `resolve_issuer_id` exists. |
| `:263` — `src/http/admin.rs:33-48`, `:75` — `src/infra/db.rs:43-80`, `:950` — `src/error.rs:52-65` | Within a line or two. **True enough**; no change needed. |

**Additionally: decision D2 must be stated in this plan's authz section (`:944-946`), not just
implied.** `authenticate_admin` and `authenticate_caller` both delegate to the same
`authenticate_trusted_jwt`, but **only `authenticate_admin` applies the grant** (`auth.rs:334`);
`authenticate_caller` returns the trusted-JWT actor verbatim (`:394`). So the console's token carries
`moira:admin` **on `/api/v1/admin/*` only** — the same token presented to the public execution API
resolves to exactly whatever the JWT independently claims, which for this console is *nothing*. Plan 08
makes **zero** non-admin API calls, so there is no functional violation, but `:228` and `:238`
misdescribe the mechanism ("Moira … resolves `moira:admin` via the grant union" reads as
unconditional). State the plane restriction explicitly; it is a security property the console depends
on and should not silently inherit.

**And close a stale open question.** Product decision 2 at `:54` asks Wave 0 to "verify whether Moira
accepts `EdDSA`". **It does** — `src/security/auth.rs:1146`, `:1160`, `:1177` map `EdDSA` through to
`jsonwebtoken::Algorithm::EdDSA`. **ES256 nonetheless remains the correct pin**, because
`allowed_algorithms` defaults to `["RS256"]` per issuer and the console registers its own issuer with
an explicit `allowed_algorithms: ["ES256"]` anyway — either would work, and ES256 is the more widely
interoperable of the two. Mark the question **answered**, keep the ES256 pin, and delete the "Wave 0
must verify" instruction.

---

### §0.5 Corrections applied inline to the body

Everything else above is left for the implementer to apply. Two items are patched directly into the
body, because they are single tokens that would otherwise be copied verbatim into shipped code:

1. **`0010_auth_provider_settings.sql` → `0013_auth_provider_settings.sql`** at `:77` and `:122`, and
   `migrations/0001-0008` → `migrations/0001-0013` at `:62`.
2. **The auth-provider operation count `10` → `7`** at `:106`, `:120`, `:773`, `:802`, `:926` and
   `:1035`, with the total-of-ten restated where the count covers setup operations too. `:101`'s
   `11 → 10` D7 delta is a total and is left alone.

---

## Summary

**Objective.** Ship the first Moira admin console: a Next.js application acting as a Backend-For-Frontend (BFF) in front of Moira. It gives a human operator a browser-based way to (1) complete first-run setup by configuring the auth provider and claiming the initial admin identity, and (2) sign in thereafter to manage providers, credentials, models, routing, and view the audit log — all by calling Moira's *existing* (and plan-07-added) admin APIs. No Moira source in this repository is modified by this plan; it adds a **new**, separately deployed Next.js project plus deployment assets.

**Why ordered here.** Plan 01 §3/§4 makes this dependency explicit: "no Next.js line is written until the identity foundation (07) exists in Moira, because 'first login becomes admin' is unsafe." Iteration 07 adds the unauthenticated `GET /api/v1/admin/setup/claim-status` endpoint, the `admin_identities (issuer, subject)` grant table, and `POST /api/v1/admin/setup/claim`. Iteration 08 is the **first and only** iteration that creates the Next.js project — everything UI/OAuth-shaped before this plan is backend-only (02a, 02b, 03, 05, 07 — plan 02 is split per CONVENTIONS §0 D2 into **02a** honesty and **02b** idempotency replay). This plan binds to plan 07's **Frozen contract** table (`plans/07-identity-foundation.md` § Interfaces & Contracts) verbatim, not to the `plans/01` §4.5 sketch.

**Branch & PR (CONVENTIONS §1).** Branch `plan/08-nextjs-console-google-oauth`, cut from current `main`, stacked on `plan/07-identity-foundation` only if 07 has not merged (PR description must then name the base PR and the branch is rebased once 07 lands). Conventional Commits. One plan = one branch = one PR.

**Identity stack decision (CONVENTIONS §7.4, verified 2026-07-25).** The console uses **Better Auth**, not Auth.js/NextAuth. The Auth.js/NextAuth team joined Better Auth in September 2025; Auth.js is security-patch-only and Better Auth is the recommended choice for new projects. Concretely:

| Requirement | Better Auth mechanism (verified against better-auth.com docs, 2026-07-25) |
|---|---|
| Google sign-in | built-in social provider `socialProviders.google` (supports `clientId`, `clientSecret`, `prompt`, `accessType`, and an `hd` hosted-domain restriction) |
| Custom OAuth / generic OIDC | **`genericOAuth` plugin** (`import { genericOAuth } from "better-auth/plugins"`) — `config: [{ providerId, clientId, clientSecret, discoveryUrl, issuer, requireIssuerValidation, scopes, pkce, ... }]` |
| BFF→Moira short-lived JWT + JWKS | **`jwt` plugin** (`import { jwt } from "better-auth/plugins"`) — asymmetric signing plus a **published JWKS endpoint** whose path is customisable via `jwks.jwksPath` (documented example: `"/.well-known/jwks.json"`). Moira registers that URL as a `trusted_jwt_issuer`. |
| Sessions, CSRF, rate limiting | built in — `session.expiresIn`/`updateAge`, `trustedOrigins` origin validation (CSRF), `rateLimit.{enabled,window,max,storage,customRules}`, `advanced.{useSecureCookies,defaultCookieAttributes,cookies}` |
| Next.js App Router wiring | `toNextJsHandler(auth)` in `app/api/auth/[...all]/route.ts`; `auth.api.getSession({ headers: await headers() })` server-side; `nextCookies()` plugin last in the plugin array |

**What this deletes from the previous draft.** `console/lib/moira-jwt.ts` (hand-rolled RS256 minting) and `console/app/.well-known/jwks.json/route.ts` (hand-rolled JWKS document) are **removed as hand-written code**. Key generation, key storage, key rotation with `kid` overlap, and JWKS publication are library features of the `jwt` plugin (`jwks.keyPairConfig`, `jwks.rotationInterval`, `jwks.gracePeriod`, private key AES-256-GCM-encrypted at rest by default, `disablePrivateKeyEncryption` deliberately left at its secure default). What remains hand-written is a **thin, well-tested claims policy** (`console/lib/moira-token.ts`) that configures `jwt.definePayload` / `jwt.getSubject` / `jwt.issuer` / `jwt.audience` / `jwt.expirationTime` and is asserted by unit tests.

**Honest limitation (CONVENTIONS §7.4).** Better Auth does **not** provide enterprise SAML SSO, and does not act as a SAML SP against an external enterprise IdP. This plan makes **no SAML claim**. Operators who need SAML use **mode 3** — bring-your-own JWT via JWKS: they register their own IdP (or an SSO gateway that fronts SAML and emits OIDC/JWT) directly as a Moira `trusted_jwt_issuer` and skip the console's OAuth path entirely. That path needs no console and is unchanged by this plan.

**Architecture change forced by Better Auth: the console gains a small database.** Better Auth persists `user`, `session`, `account`, and `verification` records, and the `jwt` plugin persists its key material in a `jwks` table. The previous draft's "the console holds zero durable state" claim is therefore **no longer true and is corrected here**. The console gets a dedicated PostgreSQL schema (`console_auth`) — either in the existing Postgres instance or a separate one — managed by Better Auth's own CLI (`npx @better-auth/cli generate` / `migrate`), **never** mixed into Moira's `migrations/` directory. Moira remains the sole system of record for **authorization**; the console DB holds human-session/account/JWKS rows **and, per D7, the OAuth client secret encrypted at rest** (see *Console-owned client-secret storage*). It never holds a Moira system key, a Moira admin key, or a decrypted **AI-provider** credential — those remain Moira's, encrypted with `SecretCipher` + AAD, untouched by D7.

**User-visible outcome.** An operator deploys the Moira console container, opens it, is taken to a `/setup` wizard (because `claim-status` reports `claimed: false`), configures the auth provider **in the wizard — the non-secret config written into Moira's settings and the client secret written into the console's own encrypted store, never into a `.env` file**, signs in with Google (or a generic-OIDC provider), claims their identity as the first Moira admin, and is thereafter redirected to `/login` → sign-in → an authenticated admin console that reads/writes Moira's provider, credential, model, routing, and audit configuration through server actions calling the real Moira admin HTTP API. The browser never sees a Moira system key, the console's JWT-signing private key, an OAuth client secret, or a decrypted provider credential — and **no request the console sends to Moira ever carries the OAuth client secret** (D7).

**Included scope.**
- New Next.js **16.2.11** App Router project (`console/`) on **Node 24 LTS** with **Bun 1.3.14** as package manager/script runner/unit-test runner (CONVENTIONS §5).
- **Atomic Design** layering (CONVENTIONS §6): `console/app/**` (pages) → `console/modules/<feature>/**` (organisms) → `console/components/molecules/**` → `console/components/atoms/**`, with a one-way dependency rule enforced by a test.
- Better Auth identity layer: Google social provider, `genericOAuth` plugin for OIDC, `jwt` plugin for the Moira-facing token + JWKS, `nextCookies()` for server-action cookie handling.
- Runtime auth configuration composed from **two stores behind one interface** (D7): **non-secret config read from Moira's DB-backed auth settings** + **the client secret read from the console's own encrypted store**, applied to a lazily-constructed Better Auth instance, with cache invalidation (CONVENTIONS §7.2).
- **Console-owned client-secret storage** (D7): a Better-Auth-CLI-managed `authProviderSecret` table in `console_auth`, AES-256-GCM encrypted at rest under a dedicated key, plus the **mandatory drift protections** (same-step dual write, `client_id` fingerprint comparison, actionable keyed mismatch error) and **console-side secret rotation**, since Moira's `rotate-secret` endpoint is deleted by D7.
- First-run setup wizard driving the dual write (Moira auth-settings + console secret) + `claim-status`/`claim` endpoints.
- MVP admin screens (see the flagged product decision below).
- i18n layer rendering Moira `message_key` with fallback to the server-supplied `message`, plus a console-originated key catalog (CONVENTIONS §4).
- `bun test` unit coverage for every lib module, atom, molecule, and organism; Playwright e2e for wizard, sign-in via **local mock OIDC**, sign-out, config round-trip, and an authorization-denial path; axe a11y on every page route; a secret-leak test (CONVENTIONS §3).
- New Dockerfile + Helm chart additions for the console as a second deployable.

**Excluded scope (explicitly deferred to plan 09 or later).**
- Generic-OIDC *hardening* beyond the `genericOAuth` baseline shipped here (multi-provider management UI, strict `requireIssuerValidation` policy surface), GitHub provider — all 09.
- Invitation flows / additional-admin self-service; ownership transfer; recovery beyond system-key break-glass — all 09.
- Enterprise SAML SSO — **not on any roadmap for the console**; mode 3 (bring-your-own JWKS) is the supported path, permanently.
- Conversation/memory/RAG configuration screens (P0-1/P0-3 — persistence primitives, not honest MVP features).
- Multi-replica session affinity (sessions are DB-backed in the console store, so horizontal scaling is already safe; no affinity work needed).

**Product-input decisions this plan flags (confirm before Wave 1):**
1. **MVP admin-screen list.** Proposed: setup wizard, dashboard/readiness, providers, provider models, provider credentials, routes, routing policies, applications, trusted JWT issuers, audit log. Deliberately excluded: system-keys, consumer-keys, agent-profiles, RAG/memory/conversation. **Needs product sign-off** — the exclusion of system-keys/consumer-keys management is a deliberate blast-radius decision, not an oversight.
2. **Signing algorithm.** Better Auth's `jwt` plugin defaults to `EdDSA`/`Ed25519`. This plan pins `jwks.keyPairConfig: { alg: "ES256" }` because Moira's per-issuer `allowed_algorithms` allow-list must accept it and ES256 is unambiguously supported by Moira's JWT verifier. **Wave 0 must verify** whether Moira accepts `EdDSA`; if it does, EdDSA is the better default and this pin should be revisited.
3. **Console database placement.** Same Postgres instance as Moira under a separate `console_auth` schema (simpler ops) vs. a separate database (stronger isolation). Recommend **separate schema, separate DB role with no grants on Moira's tables**; needs ops confirmation. **D7 raises the stakes on this one**: the console DB now stores the encrypted OAuth client secret as well as the `jwt` plugin's private key, so its backup, restore, access-control, and TLS story is a security decision, not merely an ops-convenience one. It is *not* a blocker — either placement is compatible with D7 — but the recommendation stands more firmly, and the confirmation must be recorded at Wave 0.
4. **Mode B (server-held system key) as an ongoing admin path.** Recommended: setup-time only. Running Mode B permanently is an explicit product/ops decision with coarser audit attribution, never a silent default.

---

## Findings Addressed

- **P1-11** (`plans/00-audit-report.md` — "Identity foundation absent — no owner/admin claiming, no user model... no safe basis for a Next.js admin console or OAuth login"): this plan is the console half of the fix; plan 07 is the backend half. Current behavior referenced by P1-11: **no UI/identity exists** — no `users` table, no session store, no OAuth client anywhere in `src/`. Verified by exhaustive grep in the audit (`migrations/0001-0013` — the audit read `0001-0008`; see §0.4, `src/`).
- **P0-3** (conversation/memory/RAG surface must be explicitly scoped before public exposure): the console MVP screen list deliberately **excludes** RAG/memory/conversation configuration UI so the console does not visually imply capabilities plan **02a** (the honesty half of the split — CONVENTIONS §0 D2) is simultaneously marking as preview/non-functional (`ingestion_status`, empty `citations`, no summarization). If plan **02a** has not yet landed the honest-status change when 08 starts, the console must still not build screens for these surfaces. Note also that per **D1** the `Idempotency-Key` parameter **stays** on Moira's conversation/memory/RAG routes (real replay lands in **02b**); no console text may describe it as removed or `501`-rejected.
- **P1-10** (no committed OpenAPI spec): the console's Moira client is hand-typed for MVP and switches to generated types once plan 05's committed-spec gate exists. Recorded as a dependency, not silently assumed.
- **P1-4** (audit-log cursor pagination correctness): the audit-log screen honestly renders "showing latest N" with no "next" control until plan 04 lands the cursor fix, rather than shipping a broken pager.
- `docs/todo.md` — no direct line items reference a Next.js console (confirmed absent); this plan and 07 are additive roadmap items, not corrections to an existing TODO.
- Referenced in `plans/01-roadmap-and-dependencies.md` §4 (identity architecture decision) and §4.5 — this plan implements rows: "Session management & logout" (08), "CSRF, PKCE, state, nonce, redirect validation" (08), "Secure server-side custody of Moira credentials" (08), "Browser vs BFF trust boundary" (08), "Verified email + allowed-email/domain policy... BFF enforcement" (08).

---

## Architecture

### Dependency D-1 — Moira-side auth settings (**RESOLVED: frozen in plan 07**)

CONVENTIONS §7.2 requires auth provider **non-secret** configuration to be **runtime configuration owned by Moira's database**, written by the setup wizard, read by the console at boot and on invalidation, with cache invalidation over the existing Postgres `LISTEN/NOTIFY` path (`src/infra/db.rs:43-80`). **Per D7, client secrets are explicitly excluded from that store**: §7.2 now reads "Client secrets are owned by the console, not Moira… the OAuth client secret lives encrypted at rest in the console's own `console_auth` database, written by the setup wizard, never sent to Moira, never returned to the browser." *(Moira's `SecretCipher` + AAD remains the mechanism for **AI-provider credentials**, which D7 does not touch.)*

**Status: plan 07 now provides this.** An earlier revision of this plan declared D-1 as a *blocking, unspecified* prerequisite because 07 placed the domain policy in static env config, defined no auth-settings resource, and stated "cache invalidation: none needed." **All three conflicts were resolved in 07's compliance pass**: the env-var domain allow-list (`MOIRA_AUTH__ADMIN_CLAIM_ALLOWED_EMAIL_DOMAINS`) was **withdrawn** in favour of DB-backed policy, migration `0013_auth_provider_settings.sql` (**not `0010`** — see §0.4) adds the `auth_provider_settings` table, and invalidation reuses the existing `LISTEN/NOTIFY` channel. This plan now binds to 07's **frozen** names below — it no longer guesses them.

#### Frozen-contract change adopted (product-owner decisions D3/D4/D5, 2026-07-25)

Plan 07's Interfaces & Contracts section carries a **frozen-contract change callout** that this plan is bound to. Paths, methods (`google_oauth` | `generic_oidc` | `jwks`), and scopes (`moira:auth-settings:{read,write,delete}`) are **unchanged**. Exactly one DTO shape moved, and two policy facts are now load-bearing for this plan's wizard:

| # | Change (07, frozen) | What plan 08 does about it |
|---|---|---|
| **D5** | `ClaimAdminIdentityRequest.email`: `Option<String>` → **`String` (required)**; `email_verified` loses `#[serde(default)]` → **required**; `AdminIdentityRecord.email`: `Option<String>` → **`String`**. Required on **both** credential paths (system-key **and** setup-token). | The wizard sends **`email` and `email_verified` on every claim, including the system-key path**. `lib/types.ts` types both fields as non-optional; when the generated client from P1-10 replaces the hand-written types it **must be regenerated** against the new schema. **There is no optional-email path any more** — any text describing email as optional or omittable is deleted from this plan. |
| **D3** | Email/domain allow-list is **deny-by-default with no exemption and no bootstrap bypass** — 07 explicitly *removed* the system-key carve-out that earlier drafts assumed. Unconfigured or empty ⇒ deny. | **Wizard step order becomes load-bearing**: configure-and-**enable** an auth provider carrying a non-empty `allowed_email_domains` **before** the claim step. On a fresh deployment a claim attempted first **always** returns `403 admin_claim_domain_not_allowed`. The console renders that code as an **actionable setup instruction**, never a generic failure. |
| **D4** | `GET /api/v1/admin/setup/auth-methods` is **authenticated** (`SystemKey` \| `TrustedJwt` + `moira:setup:read`); unauthenticated calls get **401**; **there is no anonymous variant**. | Called **server-side from the BFF with the system key only**. No browser-side fetch, no client component, no route proxying it to the browser. **`GET /api/v1/admin/setup/claim-status` remains the ONLY anonymous Moira call this console makes** — that contrast is stated explicitly wherever either endpoint appears. |

Everything else 08 already bound to is unchanged and remains binding.

#### Frozen-contract change adopted (product-owner decision D7, 2026-07-25)

D7 removes the client secret from Moira's `auth_provider_settings` **entirely**. What this plan is bound to, and what it deletes:

| Aspect | Before D7 (what earlier drafts of this plan assumed) | After D7 (binding) |
|---|---|---|
| Where the client secret lives | Moira, encrypted with `SecretCipher` + AAD, write-only | **The console's own `console_auth` database, encrypted at rest.** Moira never stores it and never receives it. |
| Moira read endpoints | returned `secret_fingerprint` + `masked_secret` alongside config | return **no secret material of any kind** — not a fingerprint, not a mask, not a length, not a "configured" boolean derived from secret state |
| `POST .../{id}/rotate-secret` | an operation on the frozen contract | **deleted.** Rotation is a console operation against the console's own store. No text in this plan may reference it. |
| `409 auth_provider_secret_rebind_required` | a coded error the console had to surface | **deleted** along with the `auth_provider_secret_aad` / `AuthProviderSecretAadParts` addition to `src/security/crypto.rs`. The console must not render, mirror, or catalogue this key. |
| Frozen-contract operation count | 11 | **10** |
| Console reading the secret back | an open question with three candidate answers | **closed. There is no read-back path and none will be added.** `loadAuthSettings()` composes non-secret config from Moira with the secret from the console DB. |

The cost D7 accepts is **two configuration stores that can diverge**, which is why the drift protections below are mandatory.

**Frozen contract this plan consumes** — **seven auth-provider operations plus three setup operations = ten in total** (§0.2: "10 auth-provider operations" was wrong; the auth-provider surface is **7**) (source of truth: `plans/07-identity-foundation.md` § Interfaces & Contracts as amended by D7 — re-verify at Wave 0, do not re-derive):

| Endpoint | Auth | Notes |
|---|---|---|
| `GET /api/v1/admin/setup/claim-status` | **none — the ONLY anonymous Moira call in this console** | 200 `{ "claimed": bool }`, shape frozen, no fields ever added |
| `GET /api/v1/admin/setup/auth-methods` | SystemKey \| TrustedJwt + `moira:setup:read` | `SetupAuthMethodsResponse { methods: [PublicAuthMethod] }`. **Authenticated** (anti-reconnaissance, D4) — the BFF calls it server-side with the system key, **never** from the browser; unauthenticated ⇒ 401; **no anonymous variant exists**. |
| `POST /api/v1/admin/setup/claim` | `X-Moira-System-Key` **or** `setup_token` in body | `ClaimAdminIdentityRequest { issuer, subject, email, email_verified, scopes?, setup_token? }` — **`email` and `email_verified` required on both paths (D5)**, `deny_unknown_fields`. 201 fresh / 200 replay → `AdminIdentityRecord` with `email: String`. |
| `GET /api/v1/admin/auth/providers` | `moira:auth-settings:read` | list, `params(PageQuery)`. **Carries no secret material (D7).** |
| `POST /api/v1/admin/auth/providers` | `moira:auth-settings:write` | opt. `Idempotency-Key` → 201 + `ETag`. **Body carries non-secret config only — no `client_secret` field exists.** |
| `GET /api/v1/admin/auth/providers/{id}` | `moira:auth-settings:read` | 200 + `ETag`. **Carries no secret material (D7).** |
| `PATCH /api/v1/admin/auth/providers/{id}` | `moira:auth-settings:write` | **`If-Match` required.** Non-secret config only. |
| `DELETE /api/v1/admin/auth/providers/{id}` | `moira:auth-settings:delete` | **`If-Match` required** → 204 |
| `POST /api/v1/admin/auth/providers/{id}/{enable,disable}` | `moira:auth-settings:write` | **`If-Match` required** |

**Ten operations in total, not eleven** — **three** setup rows (`claim-status`, `auth-methods`, `claim`) plus the **seven** auth-provider operations the six rows below cover (`{enable,disable}` is one row, two operations). The auth-provider surface on its own is **7**, matching `docs/admin-identity-claiming.md:36`. `POST /api/v1/admin/auth/providers/{id}/rotate-secret` **does not exist** (deleted by D7); the console must never call, reference, or document it, and no client-generation step may resurrect it.

- **Table:** `auth_provider_settings` (migration `0013_auth_provider_settings.sql` — **`0010` is `list_cursor_indexes`**; see §0.4) — **non-secret config only** after D7: issuer, discovery/authorization/token/userinfo/JWKS URLs, `client_id`, requested scopes, `allowed_email_domains`, allowed algorithms, audiences, redirect URIs, `trusted_jwt_issuer_id`, `enabled`, `version`. The encrypted-envelope columns (`encrypted_payload`, `encryption_algorithm`, `encryption_version`, `encrypted_data_key`, `nonce`, `secret_fingerprint`, `masked_secret`) are **removed from 07's spec**; the console must not read, expect, or type them.
- **Method discriminator — use 07's exact values:** `google_oauth` | `generic_oidc` | `jwks`. *(A previous draft of this plan guessed `google` / `byo_jwks`; those names are wrong and must not appear anywhere in the console.)*
- **New scopes:** `moira:auth-settings:{read,write,delete}` — the console's system-key actor must hold them.
- **Mode 3 (bring-your-own JWKS) is the pre-existing `/api/v1/admin/jwt-issuers` surface** — 07 invented nothing new for it, and the console reuses those endpoints rather than a parallel path.
- **Moira's read endpoints carry no secret material at all (D7).** Not a plaintext secret, not a `secret_fingerprint`, not a `masked_secret`, not a `has_secret` boolean. The console derives "is this provider's secret configured?" **exclusively** from its own `authProviderSecret` table. `console/lib/types.ts` must contain **no** secret-shaped field on `AuthProviderSettingsRecord`, and a unit test asserts it (`auth_provider_record_type_has_no_secret_field`).
- **`SecretCipher` is out of scope for the OAuth client secret** — it remains Moira's mechanism for **AI-provider credentials** (`CredentialRecord`), which D7 does not touch. Any sentence in this plan pairing `SecretCipher` with the *OAuth* client secret is a pre-D7 artefact and has been removed.

**Wave 0 gate (retained, narrowed).** Plan 08 still does not begin Wave 1 until plan 07 is **merged**, because these endpoints must exist to be called. The interim env-var fallback behind `console/lib/auth-settings.ts::loadAuthSettings()` is retained **only** as a time-boxed escape hatch if the coordinator explicitly authorises starting Wave 1 against an unmerged-but-frozen 07; it **must not ship**, and a Definition-of-Done checkbox asserts the env path is removed and the Moira-backed path is live.

Everything else in this plan binds to 07's frozen contract as written, and needs no 07 change.

### Components & ownership

| Component | Owner | Lives in |
|---|---|---|
| Moira Rust API (unchanged by this plan) | existing team | `src/` (this repo) |
| `admin_identities`, `setup_state`, claim-status/claim endpoints | plan 07 (backend prerequisite, frozen) | `src/`, `migrations/` |
| DB-backed auth settings (**non-secret config only**, D7) + NOTIFY invalidation | **plan-07 amendment (D-1)** — not this plan | `src/`, `migrations/` |
| Next.js console (BFF) | this plan | new top-level directory `console/` in this repo |
| Better Auth configuration & runtime factory | this plan | `console/lib/auth.ts`, `console/lib/auth-settings.ts`, `console/lib/moira-token.ts` |
| **OAuth client-secret storage, encryption, fingerprinting, rotation (D7)** | **this plan** | `console/lib/provider-secrets.ts` + the `authProviderSecret` table in `console/db/` |
| Better Auth route handler + JWKS publication | this plan (thin wiring only) | `console/app/api/auth/[...all]/route.ts` |
| Console auth schema (`user`/`session`/`account`/`verification`/`jwks`/**`authProviderSecret`**) | this plan | `console/db/` (Better Auth CLI output; **never** `migrations/`) |
| Moira admin API client (BFF↔Moira) | this plan | `console/lib/moira-client.ts` |
| Console container image + Helm release | this plan | `console/Dockerfile`, `charts/moira-console/` |

Moira remains the **only** system of record for admin **authorization**, AI-provider credentials, and runtime configuration. The console's own database holds human-session state, Better Auth key material, **and — per D7 — the OAuth client secret, encrypted at rest**. That single, deliberate exception is what makes the OAuth code exchange possible without a decrypted secret ever crossing a network boundary.

### Data flow

```
Browser (operator)
   │  1. GET / (no cookie)
   ▼
Next.js BFF (console) — page layer
   │  2. Server-side: GET Moira /api/v1/admin/setup/claim-status
   │     (plan-07 endpoint, UNAUTHENTICATED — the ONLY anonymous Moira call)
   ▼
Moira API
   │  3. { "claimed": false }   (the entire response — a single boolean, by design)
   ▼
BFF redirects to /setup (claimed == false)
   │  4. Server-side ONLY: GET Moira /api/v1/admin/setup/auth-methods with
   │     X-Moira-System-Key (D4: authenticated, moira:setup:read; 401 when
   │     unauthenticated; NO anonymous variant). The browser never issues this
   │     request and never receives its raw response.
   ▼
   │  5. Wizard step ORDER IS LOAD-BEARING (D3). Auth provider FIRST:
   │     operator enters auth-provider config (client id, discovery URL /
   │     issuer, allowed_email_domains — NON-EMPTY, client secret)
   │  6. Server action performs the D7 DUAL WRITE, in ONE wizard step, in this
   │     order, with the ENABLE call as the commit point:
   │       6a. POST NON-SECRET config to Moira /api/v1/admin/auth/providers
   │           with X-Moira-System-Key. Moira stores issuer/URLs/client_id/
   │           allowed_email_domains/etc. The row is created DISABLED and
   │           Moira NEVER receives the client secret (D7).
   │       6b. Encrypt the client secret in the console process and UPSERT it
   │           into console_auth.authProviderSecret keyed by the Moira row id
   │           returned by 6a, together with a fingerprint of that row's
   │           client_id. One console-DB transaction.
   │       6c. Only after 6b commits: POST .../{id}/enable with If-Match.
   │     A provider is therefore never ENABLED without its secret present.
   │     Partial success is an OPERATOR-RESOLVABLE FAILURE, never a success —
   │     see "Two-store drift protection" for each partial state and its
   │     keyed, actionable remedy.
   │     The secret is never sent to Moira, never echoed back to the browser,
   │     never re-rendered into the form, and never logged.
   │     ── Until this step completes, step 10 CANNOT succeed: deny-by-default
   │        with no exemption and no bootstrap bypass means an unconfigured or
   │        empty allow-list denies EVERY claim, system-key path included. ──
   ▼
   │  7. Console invalidates its auth-settings cache and rebuilds the Better
   │     Auth instance by COMPOSING Moira's non-secret config with the console-
   │     stored secret (runtime, no redeploy). On load it re-compares Moira's
   │     client_id against the stored fingerprint and fails closed with a
   │     specific, actionable keyed error on mismatch.
   │  8. Operator clicks "Sign in with Google" — Better Auth runs the OAuth
   │     flow (PKCE + state + nonce), verifies email, applies the `hd` /
   │     allowed-domain deny-by-default policy in a databaseHooks.user.create
   │     .before hook
   ▼
   │  9. Server action "Claim admin", two system-key-gated Moira calls, in order:
   ▼
Moira API — POST /api/v1/admin/jwt-issuers   (X-Moira-System-Key)
   │ 10. Registers the console as a trusted_jwt_issuer FIRST — plan 07's claim
   │     endpoint rejects (400 unregistered_trusted_issuer) any issuer not
   │     already a registered, active trusted_jwt_issuers row
   │     (07 module 3 `resolve_issuer_id`). jwks_url = the URL the Better Auth
   │     `jwt` plugin publishes (see Interfaces & Contracts).
   ▼
Moira API — POST /api/v1/admin/setup/claim   (X-Moira-System-Key, Idempotency-Key)
   │ 11. Body ClaimAdminIdentityRequest { issuer, subject, email,
   │     email_verified: true } — email AND email_verified are REQUIRED (D5)
   │     and are sent on EVERY claim, the system-key path included; there is
   │     no optional-email path → grants (issuer, subject) -> moira:admin,
   │     sets setup_state.claimed = true.
   │     If steps 5–6 were skipped: 403 admin_claim_domain_not_allowed, which
   │     the wizard renders as an actionable "add your email domain to the
   │     allow-list" instruction that routes back to the auth-provider step.
   ▼
Better Auth session cookie already set; BFF redirects to /
   │
   ▼
Subsequent admin actions: Browser → BFF (session cookie) → BFF asks the Better
Auth `jwt` plugin for a fresh short-lived token for the session → Moira admin API;
Moira fetches the console's JWKS, verifies, and resolves moira:admin via the
admin_identities grant union in src/security/auth.rs (07 module 4) — never from
any scope claim the console asserts (the console asserts none).
```

The **only** place a Moira system key is used is the setup-time triple (auth-settings write, issuer registration, claim) and, if Mode B is explicitly enabled in an environment, ongoing admin calls (documented fallback, coarser audit). **No arrow in this diagram carries the OAuth client secret to Moira** — that is D7's whole point, and `client_secret_never_appears_in_any_request_to_moira` asserts it mechanically.

### Security boundaries — browser vs BFF vs Moira

- **Browser**: holds only the Better Auth session cookie (httpOnly, `Secure`, `SameSite=Lax`, signed via `BETTER_AUTH_SECRET`). Never receives: Moira system keys, the console's JWT-signing private key, Moira-audience JWTs, OAuth client secrets, or decrypted provider credentials. React Server Components and Server Actions run exclusively on the BFF; client components receive only display-safe, already-redacted data. **Per CONVENTIONS §6 rule 5, no secret is ever passed as a prop into an organism, molecule, or atom** — enforced by a test (see Verification).
- **BFF**: holds `BETTER_AUTH_SECRET`, **`CONSOLE_SECRET_ENCRYPTION_KEY`** (D7), the console DB connection string, and the Moira bootstrap system key (K8s Secret). **OAuth client secrets live in the console's own database, encrypted at rest (D7)** — written by the setup wizard, decrypted server-side at auth-instance construction, held only in process memory for the lifetime of that instance, never on disk in plaintext, never in a cookie, never in a log, never in a `NEXT_PUBLIC_*` variable, never in a client bundle, and **never in any request sent to Moira**. The `jwt` plugin's private key lives in the same console DB, AES-256-GCM-encrypted at rest by Better Auth's default (`disablePrivateKeyEncryption` is deliberately **not** set).
- **Moira**: unchanged trust model — and, per D7, **it never receives and never returns the OAuth client secret**. Its load-bearing invariant ("a decrypted secret never crosses a network boundary") is preserved precisely because the OAuth secret was removed from its store rather than exposed through a read-back endpoint. Otherwise it authenticates the console's token exactly as any other `trusted_jwt_issuer` (`src/security/auth.rs::authenticate_trusted_jwt`), enforcing the per-issuer algorithm allow-list (`none`/`HS*` rejected per `docs/jwt-issuer-management.md`), audience/issuer/JWKS validation, and scope-based authorization (`src/security/authz.rs::ADMIN_SCOPE = "moira:admin"`).

### Atomic Design layering (CONVENTIONS §6, mandatory)

| Layer | Location | Responsibility | May import |
|---|---|---|---|
| **Pages** | `console/app/**/{page,layout,route,error,not-found}.tsx`, `app/**/actions.ts` | routing, auth gating, redirects, server-side data fetching, server actions | organisms, molecules, atoms, `lib/` |
| **Organisms (modules)** | `console/modules/<feature>/**` | feature-aware sections; may call the Moira client and server actions | molecules, atoms, `lib/` |
| **Molecules** | `console/components/molecules/**` | composite presentational components | atoms only |
| **Atoms** | `console/components/atoms/**` | primitives | nothing but React + styles |
| **Shared non-UI** | `console/lib/**` | Moira client, auth, i18n, policy, types | other `lib/` modules only |

**Enforced rules:** dependency direction is strictly one-way (pages → organisms → molecules → atoms); atoms and molecules are **feature-agnostic and presentational** — no Moira API calls, no `next/navigation` side effects, no auth logic, data and callbacks arrive via props; organisms own feature logic; pages stay thin; secrets never descend past the page/server boundary; shared non-UI logic lives in `lib/`, never in `components/`. Violations are a **test failure**, not a review opinion — see `console/tests/unit/architecture/layer-dependencies.test.ts`.

### DB/migration changes

**None to Moira's `migrations/` in this plan.** All identity/authorization state lives in Moira/Postgres and is owned by plan 07 (and D-1). The console owns a **separate** schema, generated and migrated by **Better Auth's own CLI** (`bunx @better-auth/cli generate` → `bunx @better-auth/cli migrate`) into `console/db/`, containing `user`, `session`, `account`, `verification`, the `jwt` plugin's `jwks` table, and — **new under D7** — `authProviderSecret`. This schema is deployed by a console-side job, is never referenced by Moira, and its role has no grants on Moira's tables.

**`authProviderSecret` is declared to Better Auth, not hand-written SQL.** It is added via Better Auth's additional-schema surface (a plugin `schema` declaration in `console/lib/provider-secrets-schema.ts`, wired into the `betterAuth({ plugins: [...] })` array) so that `@better-auth/cli generate` emits it alongside the core tables and `@better-auth/cli migrate` applies it. Consequences that are **binding**: the table follows Better Auth's schema conventions (singular camelCase model name, `id` text primary key, `createdAt`/`updatedAt` timestamps, camelCase field names), it lands in `console/db/` with everything else, and **it is never added to Moira's `migrations/` directory** — a `git diff --stat` check in the Definition of Done enforces that. Full column list in *Console-owned client-secret storage* below.

### API & OpenAPI changes

**None to `src/http/`, `src/domain/`, or the generated OpenAPI document in this plan.** The console is a pure consumer of:
- `GET /api/v1/admin/setup/claim-status` (plan 07, **unauthenticated**) — returns exactly `{ "claimed": bool }` (`SetupClaimStatusResponse`). The **only** signal the `/setup` wizard branches on, and — stated explicitly for contrast with the row below — **the only anonymous Moira call this console makes anywhere**.
- `GET /api/v1/admin/setup/auth-methods` (plan 07, **authenticated**, D4) — `ActorType::SystemKey | TrustedJwt` + `moira:setup:read` → 200 `SetupAuthMethodsResponse { methods: [PublicAuthMethod] }`; unauthenticated ⇒ **401**. **There is no anonymous variant.** The BFF calls it **server-side with `X-Moira-System-Key`**; no browser-side `fetch`, no client component, and no console route that proxies its response to the browser. Asserted by `console/tests/unit/architecture/no-client-side-auth-methods.test.ts`.
- `GET /api/v1/admin/setup/status` (already implemented — `src/http/admin.rs:33-48`, `SetupStatusResponse`) — *structural* readiness, gated to system-key/trusted-JWT actors, carrying **no** admin-claimed field. Used post-auth for the dashboard readiness panel only. Plan 07 leaves it untouched; so does this plan.
- `POST /api/v1/admin/setup/claim` (plan 07) — auth: `X-Moira-System-Key` header **or** `setup_token` body field; a bare `Authorization: Bearer` JWT is rejected 401 unconditionally. Body: `ClaimAdminIdentityRequest { issuer, subject, email, email_verified, scopes?, setup_token? }` with `deny_unknown_fields` — **`email` (`String`) and `email_verified` (`bool`) are REQUIRED on both credential paths (D5)**; omitting either is rejected with the `ErrorResponse` envelope carrying `moira.error.invalid_request` (400 malformed / 422 schema-violating). Responses: 201 new / 200 idempotent replay (same `Idempotency-Key`) with `AdminIdentityRecord` whose `email` is a required `String`; 400 `unregistered_trusted_issuer`; **403 `admin_claim_domain_not_allowed` (deny-by-default, no exemption, no bootstrap bypass — D3)**; 409 `admin_identity_already_claimed`.
- `POST /api/v1/admin/jwt-issuers` + `GET`/`PATCH`/enable/disable/`refresh-jwks` (already implemented, `src/http/admin.rs`).
- Moira's auth-settings endpoints **once D-1 freezes them**.
- All existing admin CRUD endpoints in `src/http/mod.rs` for the MVP screens.

No new Moira endpoints are invented by this plan.

### Backward compatibility

Fully additive. Moira's existing machine-auth paths (system keys, consumer keys, non-console trusted JWT issuers) are untouched. Operators who never deploy the console continue to manage Moira via system keys / direct API calls exactly as today — and **mode 3** (bring-your-own JWT/JWKS) remains the console-free, OAuth-free, air-gap-compatible path, permanently.

### Deployment implications

- **New container**: `console/Dockerfile`, multi-stage build on **Node 24 LTS** (`node:24-slim`), Bun used for install/build inside the image, non-root user, healthcheck.
- **New Helm chart**: `charts/moira-console/` (sibling to `charts/moira/`) with `deployment.yaml`, `service.yaml`, `ingress.yaml`, `secret.yaml`, `configmap.yaml`, `serviceaccount.yaml`, `migration-job.yaml` (Better Auth schema), `hpa.yaml`.
- **Secrets** (K8s `Secret`, env-injected, never `NEXT_PUBLIC_*`, never in the image): `BETTER_AUTH_SECRET`, **`CONSOLE_SECRET_ENCRYPTION_KEY`** (D7 — 32 raw bytes, base64-encoded; the key that encrypts the OAuth client secret at rest), `CONSOLE_DATABASE_URL`, `MOIRA_SYSTEM_KEY`. **The OAuth client secret itself is *not* a deployment secret and is *not* a chart value** — it is entered in the wizard and stored encrypted in the console's database (D7). It never appears in a `Secret`, a `ConfigMap`, an env var, or the image.
  - **`CONSOLE_SECRET_ENCRYPTION_KEY` must be present and ≥32 bytes at boot**, or the console fails closed at startup with a keyed operator error (`console.error.secret_encryption_key_missing`) rather than starting and silently failing every sign-in. A rendered-manifest assertion checks it is sourced from a `Secret`, never a `ConfigMap`.
- **Config** (ConfigMap): `MOIRA_BASE_URL`, `CONSOLE_BASE_URL`, `ALLOWED_CONSOLE_HOSTS`, `MOIRA_ADMIN_API_AUDIENCE`.
- **Network**: console → Moira over cluster-internal service DNS; the console's ingress is separate from Moira's public API ingress (`console.example.com` vs `api.example.com`).
- **Scaling**: sessions are DB-backed, so `replicaCount > 1` is safe with no affinity requirement.

### Failure & recovery

- **Claim attempted before an auth provider with a non-empty `allowed_email_domains` is enabled** (the fresh-deployment default): Moira returns **`403 admin_claim_domain_not_allowed`**. This is **expected behaviour, not a bug** (D3 — deny-by-default, no first-claim exemption, no bootstrap bypass). The wizard must render it as an **actionable setup instruction** — "add your email domain to the allow-list" — with a control that routes the operator back to the auth-provider step, its state preserved. It must never surface as a generic failure, a stack trace, or a "try again" toast. Covered by `console/tests/e2e/setup-wizard-ordering.spec.ts`.
- **Moira unreachable at setup time**: `/setup` shows a retry-with-backoff state; no partial claim is possible — the claim is a single atomic Moira admin command (`src/infra/repositories/admin.rs:560-726`), safe to retry with the same `Idempotency-Key`.
- **Auth settings unreadable from Moira at boot**: the console fails closed — `/login` renders a keyed "auth not configured" state (`console.error.auth_settings_unavailable`) and no sign-in button; it does **not** silently fall back to env.
- **Console-stored secret missing for an enabled Moira provider (D7 drift)**: fail closed. `loadAuthSettings()` omits that provider from the constructed Better Auth instance and `/login` + `/settings/auth` render the keyed `console.error.auth_provider_secret_missing` state with a direct control to enter the secret. The provider's sign-in button is **not** rendered, so an operator never gets an opaque OAuth failure from a provider whose secret this console does not hold.
- **`client_id` drift between the two stores (D7's headline hazard)**: Moira's `client_id` no longer matches the fingerprint stored beside the console's secret — e.g. someone edited the provider row directly against Moira's API, or restored one store from a backup without the other. `loadAuthSettings()` detects this **before** any OAuth flow starts and raises `console.error.auth_provider_client_id_mismatch`, a **specific, actionable** instruction naming the provider and the remedy (re-enter the client secret for the current client id). The OAuth exchange is **never attempted**, so the operator never has to debug an opaque `invalid_client` from Google or the IdP. Asserted by `console/tests/e2e/auth-secret-drift.spec.ts`.
- **Partial dual write during setup or an edit (D7)**: the Moira row and the console secret are written in the same wizard step, with the `enable` call last as the commit point. Every partial state has a defined, operator-resolvable remedy and is **never** reported as success — see *Two-store drift protection*, which enumerates each state, what is left behind, and what the operator does about it.
- **Orphaned console secret (Moira provider deleted)**: `loadAuthSettings()` ignores console secret rows with no matching Moira provider; `/settings/auth` lists them under `console.notice.orphaned_provider_secret` with a delete control, so the two stores can always be reconciled by hand.
- **OAuth failure / denied consent**: Better Auth error callback; no session created, no Moira call made.
- **Claim succeeds at Moira but session-cookie write fails**: idempotent retry — the operator re-authenticates, the BFF re-derives the same `(issuer, subject)`, re-attempts with the same idempotency key; Moira's ledger replays the prior success (no duplicate grant).
- **Signing-key rotation**: handled by the `jwt` plugin (`jwks.rotationInterval`, `jwks.gracePeriod`) — both keys are published in JWKS during the grace window, `kid`-differentiated. Moira's JWKS cache (300s TTL, `src/security/auth.rs::jwks`) picks up the new key on expiry; `POST /api/v1/admin/jwt-issuers/{id}/refresh-jwks` forces it immediately.
- **Console pod crash/restart**: sessions survive (DB-backed); no data loss.

---

## Detailed Implementation

### Toolchain pins (CONVENTIONS §5 — all verified 2026-07-25; do not change without re-verification)

| Tool | Pin | Enforcement |
|---|---|---|
| Next.js | **16.2.11** (latest stable, 2026-07-21; **16.3 is canary — do not use**) | exact pin `"next": "16.2.11"` in `console/package.json` |
| Node.js | **24.x Active LTS** (EOL 2028-04-30; Node 26 is *Current*, not LTS until Oct 2026; Node 22 is Maintenance-only) | `console/.nvmrc` → `24`; `package.json` `"engines": { "node": ">=24 <25" }`; `node:24-slim` in the Dockerfile |
| Bun | **1.3.14** (2026-05-13) | package manager + script runner + unit-test runner; `"packageManager": "bun@1.3.14"`; `oven/bun:1.3.14` build stage |
| React | as bundled with Next.js 16.2.11 | **not pinned independently** |
| Playwright | e2e runner | `bunx playwright test`; version pinned exactly in `package.json` |

- `bun install --frozen-lockfile` in CI; `console/bun.lock` is **committed**.
- Every dependency in `console/package.json` uses an **exact** version (no `^`, no `~`).
- Scripts: `bun run lint`, `bun run typecheck`, `bun test`, `bunx playwright test`, `bun run build`.

### Project layout (Atomic Design, mandatory)

```
console/
  app/                                     # ── PAGES (routing, auth gating, server fetching, server actions)
    layout.tsx
    error.tsx
    not-found.tsx
    page.tsx                               # root: redirects to /setup, /login, or /dashboard
    setup/
      layout.tsx
      page.tsx                             # server component; reads claim-status
      actions.ts                           # "use server": saveAuthSettings(), claimAdmin()
    login/
      page.tsx
    api/
      auth/[...all]/route.ts               # toNextJsHandler(await getAuth()) — Better Auth, incl. JWKS
      health/route.ts
    (console)/
      layout.tsx                           # session guard + nav shell
      dashboard/page.tsx
      providers/page.tsx
      providers/[id]/page.tsx
      providers/actions.ts
      provider-models/page.tsx
      provider-models/actions.ts
      credentials/page.tsx
      credentials/actions.ts
      routes/page.tsx
      routes/actions.ts
      routing-policies/page.tsx
      routing-policies/actions.ts
      applications/page.tsx
      applications/actions.ts
      jwt-issuers/page.tsx
      jwt-issuers/actions.ts
      audit-log/page.tsx
      settings/auth/page.tsx               # read + edit auth settings post-setup; D7 dual write + secret rotation
      settings/auth/actions.ts

  modules/                                 # ── ORGANISMS (feature-aware UI modules)
    setup/
      SetupWizard.tsx
      WelcomeStep.tsx
      AuthSettingsStep.tsx                 # writes auth config into Moira
      SignInClaimStep.tsx
      DoneStep.tsx
    auth/
      SignInPanel.tsx                      # renders one button per enabled auth method
      SignOutButton.tsx
    shell/
      ConsoleNav.tsx
      SessionBanner.tsx
    dashboard/
      ReadinessPanel.tsx
    providers/
      ProviderTable.tsx
      ProviderForm.tsx
    providerModels/
      ProviderModelTable.tsx
      ProviderModelForm.tsx
    credentials/
      CredentialTable.tsx
      CredentialForm.tsx                   # never renders a plaintext secret
      CredentialRotatePanel.tsx
    routing/
      RouteTable.tsx
      RouteForm.tsx
      RoutingPolicyTable.tsx
      RoutingPolicyForm.tsx
    applications/
      ApplicationTable.tsx
      ApplicationForm.tsx
    jwtIssuers/
      JwtIssuerTable.tsx                   # console's own row flagged read-only
    audit/
      AuditLogPanel.tsx
    authSettings/
      AuthSettingsForm.tsx
      ProviderSecretRotatePanel.tsx        # D7: console-side secret rotation (Moira has no rotate-secret)
      ProviderDriftBanner.tsx              # D7: renders the keyed mismatch/missing/undecryptable conditions

  components/
    molecules/                             # ── MOLECULES (composite, presentational, feature-agnostic)
      FormField.tsx
      TableRow.tsx
      DataTable.tsx
      ConfirmDialog.tsx
      StatusBadgeGroup.tsx
      Pagination.tsx
      EmptyState.tsx
      ErrorBanner.tsx
      Toast.tsx
      OnceOnlySecretModal.tsx
      MaskedValue.tsx
    atoms/                                 # ── ATOMS (primitives)
      Button.tsx
      Input.tsx
      Textarea.tsx
      Select.tsx
      Checkbox.tsx
      Label.tsx
      Badge.tsx
      Spinner.tsx
      Icon.tsx
      Heading.tsx
      Text.tsx
      VisuallyHidden.tsx

  lib/                                     # ── SHARED NON-UI
    auth.ts                                # getAuth(): lazily-built, cached Better Auth instance
    auth-settings.ts                       # loadAuthSettings(): Moira non-secret config + console secret, one interface
    provider-secrets.ts                    # D7: encrypt/decrypt/upsert/rotate the client secret; client_id fingerprint
    provider-secrets-schema.ts             # D7: Better Auth schema declaration for `authProviderSecret`
    moira-token.ts                         # jwt-plugin claims policy (issuer/audience/subject/definePayload)
    moira-client.ts                        # typed fetch wrapper for the Moira admin API
    session.ts                             # server-only session helpers
    domain-policy.ts                       # deny-by-default email/domain policy
    errors.ts                              # Moira ErrorResponse → client-safe discriminated union
    types.ts                               # DTOs mirrored from src/domain/*.rs
    env.server.ts                          # server-only env accessor (uses `server-only`)
    i18n/
      index.ts                             # t(key, args, fallback)
      catalog.en.ts                        # console.* keys + English defaults
      moira-keys.ts                        # mirrored moira.error.* / moira.notice.* keys

  db/                                      # Better Auth CLI schema output (NOT Moira migrations/)
                                           #   user, session, account, verification, jwks,
                                           #   authProviderSecret  ← D7, console-owned client secret
  tests/                                   # see Tests below
  middleware.ts
  next.config.ts
  package.json                             # exact pins, engines, packageManager
  bun.lock                                 # committed
  .nvmrc                                   # 24
  tsconfig.json
  playwright.config.ts
  Dockerfile
  README.md
  .env.example                             # server-only vars; none prefixed NEXT_PUBLIC_
```

### Console-owned client-secret storage (**decision D7 — binding**)

D7 is settled: **the console owns the OAuth client secret and stores it in its own database. Moira never stores it and never returns it.** The alternative of adding a Moira read-back endpoint was **considered and rejected**; do not reintroduce it in code, in a follow-up plan, or in review. This section specifies the storage concretely enough to implement without a further decision.

#### The table — `authProviderSecret`, in `console_auth`, managed by Better Auth's CLI

Declared in `console/lib/provider-secrets-schema.ts` and emitted by `bunx @better-auth/cli generate` into `console/db/`, **never** into Moira's `migrations/`. It follows Better Auth's own schema conventions (singular camelCase model, text `id`, camelCase fields, `createdAt`/`updatedAt`):

| Field | Type | Purpose |
|---|---|---|
| `id` | `text` PK | Better Auth id convention |
| `moiraProviderId` | `text`, **unique**, not null | the `id` of the Moira `auth_provider_settings` row this secret belongs to — the **only** join key between the two stores |
| `providerId` | `text`, not null | the Better Auth provider key the secret is handed to (`google`, `moira-oidc`, …), so `getAuth()` can map without re-deriving it |
| `clientIdFingerprint` | `text`, not null | **the drift guard.** Keyed fingerprint of the `client_id` this secret was issued against — see below |
| `encryptedSecret` | `text`, not null | base64 of `nonce ‖ ciphertext ‖ authTag` (AES-256-GCM) |
| `encryptionKeyVersion` | `integer`, not null | which `CONSOLE_SECRET_ENCRYPTION_KEY` version encrypted this row, so the key can be rotated without a flag day |
| `createdAt`, `updatedAt` | `timestamp` | Better Auth convention; `updatedAt` participates in the cache key |

There is **no plaintext column, no masked-secret column, and no "last four characters" column.** The console renders "configured / not configured" from the *existence* of the row, never from any derivative of the secret value. A unit test asserts the model declaration contains no field whose name matches `/(plain|masked|preview|hint|last4)/i`.

#### Encryption at rest, and where the key comes from

- **Algorithm: AES-256-GCM** via Node's built-in `crypto` (`createCipheriv("aes-256-gcm", …)`), 96-bit random nonce per write, 128-bit auth tag. No new dependency.
- **Key: a dedicated `CONSOLE_SECRET_ENCRYPTION_KEY`, *not* `BETTER_AUTH_SECRET`.** Decision and justification:
  1. **Rotation domains must be independent.** `BETTER_AUTH_SECRET` is a *signing* secret for cookies/sessions, and Better Auth explicitly supports rotating it through its versioned `secrets` array. If it also encrypted the client secret, every session-secret rotation would silently render stored OAuth secrets undecryptable — turning a routine, expected operation into a total sign-in outage.
  2. **One key, one purpose.** `BETTER_AUTH_SECRET` is handed to more code paths (cookie signing, CSRF, the library's internals) and therefore has a wider leak surface than a key read once, in one module, to decrypt one class of value.
  3. **Deriving an encryption key from a signing secret conflates purposes** and makes the blast radius of a `BETTER_AUTH_SECRET` disclosure strictly larger than it needs to be.
  The cost — one more K8s `Secret` key to provision — is trivial next to those three, so a dedicated key it is.
- **Format:** 32 raw bytes, base64-encoded in the env var. Absent or short ⇒ **the console fails closed at boot** with `console.error.secret_encryption_key_missing`; it never starts with encryption disabled and never falls back to plaintext storage.
- **Per-record key separation:** the actual data key is `HKDF-SHA256(CONSOLE_SECRET_ENCRYPTION_KEY, salt = "moira-console/v1", info = "auth-provider-secret")`. A second, separately-derived key with `info = "client-id-fingerprint"` is used for fingerprinting, so the two uses never share key material.
- **AAD binds the ciphertext to its row**, mirroring Moira's own AAD discipline: `AAD = "moira-console.auth-provider-secret.v1|" + moiraProviderId + "|" + providerId + "|" + clientIdFingerprint`. A row copied to a different provider, or edited in the database, fails to decrypt. **All AAD components are console-local values**, so decryption is deterministic and independent of Moira's availability; the *drift* check against Moira is a separate, explicit, earlier step (below) precisely so that drift surfaces as an actionable message rather than as a decryption failure.
- **Key rotation** (of `CONSOLE_SECRET_ENCRYPTION_KEY` itself) is supported by `encryptionKeyVersion`: the env carries the active version plus any still-decryptable prior versions; a `bun run rotate-encryption-key` script re-encrypts every row under the new version. Rotating this key does **not** require re-entering any OAuth client secret, and does **not** touch Moira.

#### The `client_id` fingerprint

`fingerprint(clientId) = base64url(HMAC-SHA256(k_fp, clientId))` truncated to 32 characters, where `k_fp` is the `info = "client-id-fingerprint"` HKDF output above. It is **keyed** rather than a bare hash so a leaked console database does not become an offline oracle for confirming guessed client ids. Comparison uses `crypto.timingSafeEqual` on equal-length buffers. The fingerprint is written at the same instant as the secret, from the `client_id` the operator submitted, and is the console's record of *which client this secret belongs to*.

#### `console/lib/provider-secrets.ts` (server-only)

`import "server-only"` at the top. Exposes exactly:

- `putProviderSecret({ moiraProviderId, providerId, clientId, clientSecret })` — fingerprints, encrypts, upserts by `moiraProviderId`, in one console-DB transaction. Idempotent.
- `getProviderSecret({ moiraProviderId, providerId, moiraClientId })` — loads the row, **compares `fingerprint(moiraClientId)` against `clientIdFingerprint` first**, then decrypts. Returns a discriminated result: `{ ok: true, clientSecret }` | `{ ok: false, reason: "missing" }` | `{ ok: false, reason: "client_id_mismatch" }` | `{ ok: false, reason: "undecryptable" }`. **It never throws a value-bearing error and never includes the secret, the ciphertext, or the key in any error, log line, or stack.**
- `deleteProviderSecret(moiraProviderId)` — for provider deletion and orphan reconciliation.
- `listConfiguredProviderIds()` — the "is it configured?" signal for the UI, returning ids only.

The plaintext secret exists only as a local `string` inside `getAuth()`'s construction of the Better Auth options object. It is **never** returned from a server action, never placed in a React prop, never serialised into an RSC payload, never written to a cookie, and never logged. `no-secret-props.test.ts` and the `server-only` guard make the first two mechanical failures rather than review opinions.

### Two-store drift protection (**mandatory — CONVENTIONS D7 consequences**)

D7 buys Moira's secret-envelope invariant at the price of two configuration stores. Without the following, a `client_id` changed in Moira while the console still holds the old client's secret would fail the code exchange with an opaque provider error (`invalid_client` from Google, or worse, a generic 400 from a self-hosted IdP) — the exact failure mode operators cannot diagnose. All three mitigations are **required**, not optional hardening.

#### (a) Same-step dual write; partial success is an operator-resolvable failure

`app/setup/actions.ts::saveAuthSettings()` (and `app/(console)/settings/auth/actions.ts` post-setup) performs **both** writes **in the same step**, in this order, with the **`enable` call as the commit point**:

| # | Write | Why this order |
|---|---|---|
| 1 | `POST /api/v1/admin/auth/providers` (Moira, `X-Moira-System-Key`, `Idempotency-Key`) — **non-secret config only** | Moira is first because its response supplies the `id` the console's secret row is keyed by. Nothing else can generate that key. 07 creates rows **disabled**, so this write alone cannot govern any issuer. |
| 2 | `putProviderSecret(...)` into `console_auth.authProviderSecret`, one transaction | The secret must exist before the provider can be enabled. |
| 3 | `POST /api/v1/admin/auth/providers/{id}/enable` (Moira, `If-Match`) | **The commit point.** A provider is never *enabled* without its secret present, so the only reachable inconsistent state is "configured but disabled" — inert, visible, and fixable. |

**Every partial state, and what happens:**

| Failure point | State left behind | What the operator sees and does |
|---|---|---|
| Step 1 fails | nothing written anywhere | ordinary keyed submit failure; retry with the same `Idempotency-Key` replays rather than duplicating. |
| Step 2 fails after step 1 succeeded | Moira row exists, **disabled**, no console secret. Cannot govern an issuer; cannot run an exchange. | **`console.error.auth_provider_secret_write_failed`** — an explicit "partially saved" state, **never a success toast**. The step stays incomplete and offers two controls: **Retry** (re-runs step 2 against the same `moiraProviderId`; `putProviderSecret` is an idempotent upsert, so retry is always safe) and **Discard** (`DELETE /api/v1/admin/auth/providers/{id}` with `If-Match`, then `deleteProviderSecret`). The wizard does **not** advance. |
| Step 3 fails after steps 1–2 succeeded | secret present, provider **disabled** | **`console.error.auth_provider_enable_failed`** — "the secret is stored; the provider could not be enabled." Retry re-reads the row for a fresh `If-Match` and re-issues `enable`. **No secret re-entry is required**, and the copy says so, so the operator does not needlessly re-fetch a secret from the IdP. |

The ordering makes the reverse partial — a console secret with no Moira provider — **unreachable during a normal write**. It is still reachable by an out-of-band Moira-side deletion, which is why orphan reconciliation exists (below). The wizard treats every one of these as an **operator-resolvable failure**: no silent retry loop, no "probably fine, continue", no partial-success advance.

#### (b) `client_id` fingerprint comparison on every load

`loadAuthSettings()` compares `fingerprint(moiraRow.client_id)` against the stored `clientIdFingerprint` for **every enabled provider, on every (re)load** — that is, on cache miss, TTL expiry, and after `invalidateAuthSettings()`. Outcomes:

- **Match** → decrypt and construct the provider normally.
- **No console row** → `console.error.auth_provider_secret_missing`; the provider is **excluded** from the constructed Better Auth instance and its sign-in button is not rendered.
- **Mismatch** → `console.error.auth_provider_client_id_mismatch`; the provider is **excluded**, and `/login` and `/settings/auth` render a **specific, actionable** instruction naming the provider and the fix. **The OAuth exchange is never attempted**, so the operator debugs a console message that tells them what to do, not an `invalid_client` from Google.
- **Undecryptable** (AAD/key failure) → `console.error.auth_provider_secret_undecryptable`; excluded, with copy pointing at encryption-key provisioning rather than at the IdP.

Fail-closed in all four cases: an unresolvable provider is **omitted**, never constructed with a guessed or empty secret. If *no* provider resolves, `/login` shows the existing `console.error.auth_settings_unavailable` state and no sign-in buttons.

**Orphan reconciliation.** Console secret rows whose `moiraProviderId` matches no Moira provider are ignored by `loadAuthSettings()` and surfaced on `/settings/auth` as `console.notice.orphaned_provider_secret` with a delete control.

#### (c) Tests — named, required

- **E2E (the required mismatch spec):** `console/tests/e2e/auth-secret-drift.spec.ts`
  - `client_id_changed_in_moira_surfaces_actionable_mismatch_error` — with a working provider, patch `client_id` **directly against Moira's API** (simulating an out-of-band edit), reload the console, and assert `/login` renders the `console.error.auth_provider_client_id_mismatch` instruction naming the provider and the remedy.
  - `mismatch_never_reaches_the_oauth_provider` — a network tap over the same run asserts **zero** outbound requests to the IdP's token endpoint: the failure is caught before the exchange, never as an opaque provider error.
  - `mismatch_hides_the_sign_in_button_rather_than_failing_mid_flow` — the affected provider's button is absent, so the operator cannot start a flow that is guaranteed to fail.
  - `re_entering_the_secret_for_the_new_client_id_clears_the_mismatch` — entering the new secret in `/settings/auth` rewrites both the ciphertext and the fingerprint, and sign-in works again **with no redeploy**.
  - `missing_console_secret_surfaces_its_own_distinct_error` — a Moira provider with no console row yields `auth_provider_secret_missing`, **not** the mismatch key; the two conditions are never collapsed, because their remedies differ.
  - `partial_write_leaves_provider_disabled_and_offers_retry_or_discard` — with the console DB write forced to fail, the wizard shows `auth_provider_secret_write_failed`, the Moira row is confirmed **disabled** by a direct API read, the wizard has **not** advanced, and Retry completes the save.
- **Unit:** `console/tests/unit/lib/provider-secrets.test.ts`
  - `fingerprint_is_stable_for_the_same_client_id`
  - `fingerprint_differs_for_a_different_client_id`
  - `fingerprint_is_keyed_not_a_bare_hash` — the output does not equal `sha256(clientId)`, and changing the key changes the output.
  - `fingerprint_comparison_is_constant_time` — the comparison path uses `timingSafeEqual` and never `===` on the raw strings.
  - `get_returns_client_id_mismatch_when_moira_client_id_differs` — the mismatch is detected **before** decryption is attempted.
  - `get_returns_missing_when_no_row_exists` — distinct `reason`, not conflated with mismatch.
  - `encrypt_decrypt_round_trips_and_aad_binding_rejects_a_moved_row` — a row whose `moiraProviderId` is altered fails to decrypt.
  - `errors_never_contain_the_secret_or_the_ciphertext` — every failure result and thrown error is scanned for the fixture secret and the ciphertext.
  - `no_api_returns_the_plaintext_secret_to_a_caller` — only `getProviderSecret` yields plaintext, and only to server-side code.
- **Unit:** `console/tests/unit/lib/auth-settings.test.ts` (extended) — `compose_merges_moira_config_with_console_secret`; `enabled_provider_without_a_console_secret_is_excluded_and_keyed`; `fingerprint_mismatch_excludes_the_provider_and_keys_the_error`; `no_code_path_requests_a_secret_from_moira`.
- **Unit:** `console/tests/unit/lib/moira-client.test.ts` (extended) — `no_request_body_or_header_ever_carries_a_client_secret`; `there_is_no_rotate_secret_method` (the client exposes no method, path constant, or type referencing `rotate-secret`).

#### i18n keys added by D7 (CONVENTIONS §4 — key + English default)

| Key | English default |
|---|---|
| `console.error.auth_provider_client_id_mismatch` | "The client ID stored in Moira for {provider} no longer matches the client secret held by this console. Sign-in for this provider is disabled until you re-enter the client secret for the current client ID in Settings → Auth." |
| `console.error.auth_provider_secret_missing` | "No client secret is stored in this console for {provider}. Enter it in Settings → Auth to enable sign-in with this provider." |
| `console.error.auth_provider_secret_undecryptable` | "This console cannot decrypt the stored client secret for {provider}. Check that CONSOLE_SECRET_ENCRYPTION_KEY matches the key used when the secret was saved, then re-enter the secret if it does not." |
| `console.error.auth_provider_secret_write_failed` | "The provider configuration was saved in Moira, but its client secret could not be stored in this console. The provider has been left disabled. Retry to store the secret, or discard the incomplete provider." |
| `console.error.auth_provider_enable_failed` | "The client secret was stored, but the provider could not be enabled in Moira. Retry enabling it — you do not need to enter the secret again." |
| `console.error.secret_encryption_key_missing` | "CONSOLE_SECRET_ENCRYPTION_KEY is missing or too short. The console cannot store or read OAuth client secrets until a 32-byte key is configured." |
| `console.notice.auth_provider_secret_rotated` | "Client secret updated. New sign-ins use it immediately — no redeploy is needed." |
| `console.notice.orphaned_provider_secret` | "This console holds a client secret for a provider that no longer exists in Moira. Delete it to keep the two stores in sync." |
| `console.authSettings.rotate_secret.action` | "Rotate client secret" |
| `console.authSettings.rotate_secret.body` | "Paste the new client secret from your identity provider. It is stored encrypted in this console and is never sent to Moira." |
| `console.authSettings.secret_configured` | "Client secret configured" |

All are console-originated strings, so they live in `console/lib/i18n/catalog.en.ts` and are covered by the existing `i18n-catalog-coverage.test.ts` and `no-hardcoded-copy.test.tsx` guards. **None of them is a Moira error code**, so none is added to `docs/i18n-response-catalog.json` — D7 removes `auth_provider_secret_rebind_required` from Moira's catalog rather than adding anything to it.

### Secret rotation is a console concern (**D7 — Moira's `rotate-secret` endpoint is deleted**)

`POST /api/v1/admin/auth/providers/{id}/rotate-secret` **no longer exists.** Nothing in this plan, this console, its client, its types, or its docs may reference it. Rotation is performed entirely in the console:

**Rotating only the secret (the common case — the IdP issued a new secret for the same client).**
1. Operator opens `/settings/auth`, selects the provider, clicks **Rotate client secret** (`console.authSettings.rotate_secret.action`).
2. `ProviderSecretRotatePanel` (organism, `console/modules/authSettings/`) collects the new secret in a write-only field. The current secret is **never** displayed, pre-filled, or fetched — there is nothing to display, since the console shows only "configured".
3. `rotateProviderSecret()` server action calls `putProviderSecret(...)` with the **unchanged** `client_id`, re-encrypting under a fresh nonce and rewriting `updatedAt`. The `clientIdFingerprint` is recomputed and is expected to be unchanged.
4. **No Moira call is made at all** — Moira holds no secret material, so its row and its `version` are untouched. This is a strict improvement over the deleted endpoint: rotation no longer needs an `If-Match`, cannot conflict with a concurrent config edit, and cannot fail because of Moira availability.
5. `invalidateAuthSettings()` runs; the next `getAuth()` rebuilds with the new secret. `console.notice.auth_provider_secret_rotated` confirms it. **No redeploy.**

**Rotating the secret *and* changing the `client_id` (a new OAuth client).** This is the dual write again, and takes the same treatment as (a) above: `PATCH /api/v1/admin/auth/providers/{id}` with `If-Match` for the new `client_id`, then `putProviderSecret` with the new `client_id` + new secret (which rewrites the fingerprint), in the **same step**, with the same partial-failure states and the same keyed, actionable remedies. Getting this wrong is precisely what mitigation (b) exists to catch, so the e2e drift spec exercises it.

**Operational note recorded honestly:** rotation is a single cut-over, not a dual-secret overlap. In-flight OAuth code exchanges started before the switch and completing after it will fail and the user retries sign-in — a sub-second window. Where the IdP supports two concurrently valid secrets, the documented procedure is to add the new secret at the IdP, rotate in the console, then retire the old one at the IdP; where it does not, the window is accepted and documented in `docs/admin-console.md`. Existing **sessions are unaffected** — they are console-DB-backed and involve no client secret.

### `console/lib/auth-settings.ts` — runtime auth configuration (CONVENTIONS §7.2)

- `loadAuthSettings(): Promise<AuthSettings>` — server-only. **It composes two sources behind one interface** (D7), and its callers (`getAuth()`, the login page, the settings screen) see no seam:
  1. **Non-secret config from Moira** — `GET /api/v1/admin/auth/providers` with `X-Moira-System-Key`, yielding `id`, `method` (`google_oauth` | `generic_oidc` | `jwks`), `client_id`, issuer/discovery/JWKS URLs, requested scopes, `allowed_email_domains`, allowed algorithms, audiences, redirect URIs, `enabled`, `version`. **Moira returns no secret material of any kind** — the console must not read, type, or expect a `client_secret`, a `secret_fingerprint`, or a `masked_secret`, and **must not invent an endpoint that would return one**. The rejected read-back option stays rejected.
  2. **The client secret from the console's own store** — `getProviderSecret({ moiraProviderId: row.id, providerId, moiraClientId: row.client_id })` per enabled row, which performs the fingerprint drift check before decrypting.
  3. **Compose** into the `AuthSettings` shape below, **excluding** any provider whose secret is missing, mismatched, or undecryptable and attaching the corresponding keyed condition for the UI to render.
- **Cache key spans both stores.** The in-process cache is keyed by `` `${moiraSettingsVersion}:${maxConsoleSecretUpdatedAt}` `` so a change in *either* store invalidates it — a secret rotation that never touches Moira still takes effect. Invalidation triggers: (a) a short TTL (default 60s), (b) an explicit `invalidateAuthSettings()` called by `saveAuthSettings()`, `rotateProviderSecret()`, and the delete/enable/disable actions immediately after a successful write, so any settings or secret change takes effect **without a redeploy**.
- `AuthSettings` shape consumed by the console: `{ version, methods: { google?: { moiraProviderId, clientId, clientSecret, hostedDomain? }, genericOidc?: { moiraProviderId, providerId, clientId, clientSecret, discoveryUrl, issuer } }, allowedEmailDomains: string[], allowedAlgorithms: string[], jwksUrl?: string, unresolvedProviders: { moiraProviderId, method, condition: "secret_missing" | "client_id_mismatch" | "secret_undecryptable" }[] }`. The `clientSecret` values are populated **from the console store, never from a Moira response**, and this object never leaves the server: `unresolvedProviders` (ids and condition keys only) is the sole part rendered, and `no-secret-props.test.ts` enforces that `clientSecret` never becomes a prop.
- **Deny-by-default, with NO exemption (D3)**: an empty or unconfigured `allowedEmailDomains` means *nobody* may sign in **and no claim can succeed** — **including the first claim and including the system-key path**. Never "empty means allow all," and **never a first-claim exemption or bootstrap bypass**: plan 07 deliberately *removed* the system-key carve-out an earlier draft assumed, so the console must not reintroduce one client-side either. The console's own `domain-policy.ts` gate is defence-in-depth **in front of** Moira's authoritative check, never a substitute for it and never more permissive than it.

### `console/lib/auth.ts` — Better Auth instance (verified API shapes)

```ts
import "server-only";
import { betterAuth } from "better-auth";
import { jwt, genericOAuth } from "better-auth/plugins";
import { nextCookies } from "better-auth/next-js";
```

- **Lazy async factory, not a module-level constant.** `export async function getAuth(): Promise<Auth>` awaits `loadAuthSettings()`, builds `betterAuth({...})`, and caches the instance keyed by that call's composite cache key (Moira's settings `version` **and** the console secret store's latest `updatedAt` — D7, so a secret rotation that never touches Moira still rebuilds the instance). A key change rebuilds it. This is what makes §7.2 ("configured in settings at runtime") mechanically true. Every consumer — the route handler, server components, and server actions — calls `await getAuth()`.
- `database` — the console's PostgreSQL store (`CONSOLE_DATABASE_URL`). Required: Better Auth persists `user`/`session`/`account`/`verification`, the `jwt` plugin persists `jwks`, and — per **D7** — `authProviderSecret` holds the encrypted OAuth client secret.
- `baseURL: CONSOLE_BASE_URL`, `basePath: "/api/auth"` (documented default; keep it).
- `secret` from `BETTER_AUTH_SECRET` (32+ bytes, K8s Secret, rotated via Better Auth's versioned `secrets` support).
- `trustedOrigins: [CONSOLE_BASE_URL]` — this is Better Auth's CSRF mechanism (origin validation + Fetch Metadata). **`advanced.disableCSRFCheck` must never be set** (a lint rule and a unit test assert its absence).
- `session: { expiresIn: 28800 /* 8h */, updateAge: 3600, cookieCache: { enabled: true, maxAge: 60 } }`.
- `advanced: { useSecureCookies: true /* prod */, defaultCookieAttributes: { httpOnly: true, secure: true, sameSite: "lax", path: "/" } }`.
- `rateLimit: { enabled: true, window: 10, max: 100, storage: "database", customRules: { "/sign-in/social": { window: 60, max: 10 }, "/oauth2/callback/*": { window: 60, max: 20 } } }`.
- `socialProviders.google` — built from the composed settings: `{ clientId, clientSecret, prompt: "select_account", hd: settings.methods.google.hostedDomain }`, where **`clientId` comes from Moira and `clientSecret` comes from the console's own encrypted store (D7)**, already fingerprint-verified by `loadAuthSettings()`. If the secret is missing, mismatched, or undecryptable the provider is **omitted from this object entirely** rather than constructed with an empty or guessed value. The `hd` option restricts sign-in to a Google Workspace domain and rejects tokens with no `hd` claim when set; it is a **defence-in-depth complement to**, not a replacement for, `lib/domain-policy.ts`.
- `plugins`, in order:
  0. the `authProviderSecret` schema-declaration plugin from `lib/provider-secrets-schema.ts` — contributes **only** a table declaration so `@better-auth/cli generate/migrate` manages it (D7); it adds no endpoints and no hooks.
  1. `genericOAuth({ config: [{ providerId: "moira-oidc", clientId, clientSecret, discoveryUrl, issuer, requireIssuerValidation: true, pkce: true, scopes: ["openid", "email", "profile"], mapProfileToUser }] })` — present only when generic-OIDC is enabled in settings **and its console-held secret resolved cleanly (D7)**. `clientSecret` comes from the console store; it is never read from a Moira response. `discoveryUrl` gives OIDC auto-discovery; `issuer` + `requireIssuerValidation: true` gives strict issuer validation (the plugin's default for `requireIssuerValidation` is `false` — **this plan sets it true explicitly**). Callback path is `${baseURL}/api/auth/oauth2/callback/:providerId`, registered exactly (no wildcards) at the IdP.
  2. `jwt({ ... })` — see `moira-token.ts`.
  3. `nextCookies()` — **must be last** so server actions can set cookies.
- `user.additionalFields: { idpIssuer: { type: "string" }, idpSubject: { type: "string" } }` — populated by each provider's `mapProfileToUser` from the IdP's stable `sub`/`iss`. See the subject-binding note below; this is load-bearing.
- `databaseHooks.user.create.before` — the deny-by-default gate. Rejects with `throw new APIError("FORBIDDEN", { message: ... })` (import `APIError` from `"better-auth/api"`) when: `emailVerified !== true`, or the email's domain is not in `settings.allowedEmailDomains`, or `idpSubject` is absent. Returning `false` from a before-hook also aborts; this plan throws `APIError` so the failure carries a message key. Re-validated **again** server-side in `claimAdmin()` — never trust a value checked once at sign-in for a later privileged action.

### `console/lib/moira-token.ts` — the `jwt` plugin claims policy

Configures the `jwt` plugin. This is the **only** hand-written JWT logic remaining; signing, key storage, rotation, and JWKS publication are the plugin's job.

```ts
jwt({
  jwks: {
    jwksPath: "/.well-known/jwks.json",          // documented custom-path option
    keyPairConfig: { alg: "ES256" },             // see product decision 2
    rotationInterval: 60 * 60 * 24 * 30,
    gracePeriod:      60 * 60 * 24 * 30,
    // disablePrivateKeyEncryption intentionally NOT set — the private key
    // stays AES-256-GCM encrypted at rest (Better Auth default).
  },
  jwt: {
    issuer:   env.MOIRA_BFF_ISSUER_URL,          // e.g. https://console.example.com
    audience: env.MOIRA_ADMIN_API_AUDIENCE,      // must be non-empty; see below
    expirationTime: "120s",
    getSubject: (session) => session.user.idpSubject,
    definePayload: ({ user }) => ({}),           // SEE THE SECURITY RULE BELOW
  },
})
```

**Security rule (non-negotiable, CONVENTIONS §7.5 — preserved verbatim from the previous draft because it is the single most important invariant in this plan):**

> **`definePayload` must never emit a `scope` (or `scp`) claim.** Moira's `actor_from_trusted_claims` (`src/security/auth.rs:555-628`) copies scopes **straight out of the JWT's scope claim**. If the console asserted `scope: "moira:admin"`, Moira would honour it for *any* subject the console signs — making the console's own token logic the sole authorization gate and rendering plan 07's `admin_identities` grant table decorative. By emitting **no** scope claim, the actor's scopes come **only** from 07 module 4's grant union: a granted `(issuer, subject)` resolves to `moira:admin`; an ungranted one resolves to zero scopes and every admin call fails 403 at Moira regardless of any console bug. **Moira stays the system of record for authorization; the console only for authentication.** Asserted by `console/tests/unit/lib/moira-token.test.ts` and again end-to-end by `console/tests/e2e/authorization-denial.spec.ts`.

**No `email`/`email_verified` claims either.** Moira's trusted-JWT verification does not read them (verified: `actor_from_trusted_claims` extracts subject/tenant/application/roles/scopes claims only). Email is consumed exactly once, at claim time, from the `POST /api/v1/admin/setup/claim` request **body**, where plan 07 stores it as an `admin_identities` attribute column. Putting PII in every minted token adds leak surface for zero function.

**`getSubject` is load-bearing and must be set explicitly.** The `jwt` plugin's default subject is the Better Auth **`session.user.id`** — a surrogate key local to the *console's* database. Binding Moira's `admin_identities (issuer, subject)` grant to a console-DB-local id would mean a console database restore, re-seed, or migration silently invalidates every admin grant (or, worse, re-points one at a different human). This plan therefore sets `getSubject: (session) => session.user.idpSubject`, where `idpSubject` is the **IdP's stable `sub`** captured at account creation via `mapProfileToUser` — satisfying CONVENTIONS §7.5 ("identity binds to stable `(issuer, subject)` — never to email alone") and matching what plan 07's claim endpoint stores. `console/tests/unit/lib/moira-token.test.ts` asserts the subject is the IdP `sub` and **not** the Better Auth user id.

**Audience must be non-empty.** Moira skips audience validation entirely when a trusted issuer's `expected_audiences` is empty (`validation.validate_aud = false`, `src/security/auth.rs:327-328`). Registering a non-empty audience is therefore **mandatory** for this issuer, and the production-config test asserts it on both sides.

### `console/app/api/auth/[...all]/route.ts` — Better Auth handler (thin wiring)

```ts
import { toNextJsHandler } from "better-auth/next-js";
import { getAuth } from "@/lib/auth";

const handler = async (req: Request) => {
  const { GET, POST } = toNextJsHandler(await getAuth());
  return req.method === "GET" ? GET(req) : POST(req);
};
export { handler as GET, handler as POST };
```

This single catch-all serves sign-in, callbacks, sign-out, the token endpoint, **and the JWKS document** at the `jwks.jwksPath` the plugin is configured with. **No hand-written JWKS route exists.** The concrete JWKS URL that Moira must be registered with is resolved and asserted at Wave 0 by fetching it (the plugin serves JWKS under its base path by default and `jwks.jwksPath` overrides it) — the console registers **the URL it actually serves**, verified by `console/tests/e2e/jwks.spec.ts`, never a hard-coded guess.

### `console/lib/moira-client.ts`

- `import "server-only"` at the top — a **build failure** if a client component imports it.
- Thin typed wrapper over `fetch` for every admin endpoint the MVP screens use; hand-written types mirroring `src/domain/admin.rs` (`ApplicationRecord`, `ProviderRecord`, `ProviderModelRecord`, `CredentialRecord`, `RouteDefinitionRecord`, `RoutingPolicyRecord`, `TrustedJwtIssuerRecord`, `AuditLogRecord`, `SetupStatusResponse`) and `src/domain/identity.rs` (`SetupClaimStatusResponse`, `ClaimAdminIdentityRequest`, `AdminIdentityRecord`, `SetupAuthMethodsResponse`, `PublicAuthMethod`), switching to generated types once P1-10's committed spec exists.
  - **TypeScript shapes bound to 07's changed DTO (D5) — both fields are NON-OPTIONAL:**
    ```ts
    // console/lib/types.ts — mirrors src/domain/identity.rs after 07's frozen-contract change
    export interface ClaimAdminIdentityRequest {
      issuer: string;
      subject: string;
      email: string;          // REQUIRED — was `string | undefined` in the pre-D5 draft
      email_verified: boolean; // REQUIRED — no default, no `?`
      scopes?: string[];       // omitted → Moira defaults to ["moira:admin"]
      setup_token?: string;
    }
    export interface AdminIdentityRecord {
      // …
      email: string;           // REQUIRED — was `string | null`; a grant cannot exist without an email
    }
    ```
    No `?`, no `| null`, and no `| undefined` on `email`/`email_verified` in either shape. TypeScript's own checker is therefore the first line of enforcement: a call site that omits either field fails `bun run typecheck`.
  - **Generated-client note (P1-10):** when the hand-written types are replaced by a client generated from the committed OpenAPI spec, **the client must be regenerated against the post-D5 schema**. A client generated before this change types `email` as optional and would silently permit an omitting call site; regeneration — not hand-patching — is the required action, and the regenerated output must be re-checked against this section.
- Every call passes, as applicable:
  - `Authorization: Bearer <token from the jwt plugin>` (Mode A, default) — obtained per-request, never cached beyond a single request.
  - `X-Moira-System-Key: <bootstrap key>` — **only** for the setup-time triple (auth-settings write, `POST /api/v1/admin/jwt-issuers`, `POST /api/v1/admin/setup/claim`), and for all admin calls **only** when `MOIRA_ADMIN_MODE=system_key` is explicitly set. The pre-claim `GET /api/v1/admin/setup/claim-status` read is unauthenticated.
  - `Idempotency-Key: <server-generated UUIDv4 per logical operation>` on every mutating call — one key per **form submission**, not per HTTP retry, so retries replay rather than duplicate.
  - `If-Match: <resource_version>` on every PATCH/PUT **and on rotate calls** — verified against current source: `require_if_match` is enforced on `rotate_credential` (`src/http/admin.rs`, commit `0688e2e`) as well as the standard PATCH handlers; Moira returns 400 "If-Match header is required" when absent. The client tracks the version from the prior read and never fabricates one.
- Maps `ErrorResponse` into a client-safe discriminated union via `lib/errors.ts` — **never** forwarding the raw envelope across the server/client boundary. `message_key` and `message_args` cross; `message` crosses as the i18n fallback; `details` does not.

### `console/lib/i18n/` — message rendering (CONVENTIONS §4.6)

- `t(key: string, args?: Record<string, string|number>, fallback?: string): string`. Resolution order: **console catalog (`catalog.en.ts`) → server-supplied `message` (the fallback argument) → the key itself**. This is exactly §4.6: "the console renders `message_key` through its own i18n layer and falls back to the server-supplied `message`."
- **Server-originated conditions** (every Moira error and notice): the UI calls `t(err.messageKey, err.messageArgs, err.message)`. **No component may hardcode English copy for a server-originated condition** — `ErrorBanner` and `Toast` accept `{ messageKey, messageArgs, message }`, never a pre-formatted string.
- **Console-originated strings** (labels, button text, wizard copy, validation hints): every one has a `console.*` key with an English default in `catalog.en.ts` (e.g. `console.setup.welcome.title`, `console.error.auth_settings_unavailable`, `console.credentials.reveal_once_warning`). Components call `t("console...")`; **bare literals in JSX are a test failure**.
- `moira-keys.ts` mirrors the Moira key namespaces (`moira.error.*`, `moira.notice.*`) the console renders, kept in sync with `docs/i18n-response-catalog.json`. A test asserts every mirrored key exists in that JSON, so a Moira-side rename surfaces as a console test failure rather than an untranslated string in production.
- **`moira.error.admin_claim_domain_not_allowed` is an ACTIONABLE SETUP INSTRUCTION, not a generic failure (D3).** It is the *expected* response on a fresh deployment when the claim is attempted before an enabled auth provider carries the operator's email domain, so it is the one server-originated condition the wizard treats as guidance. Handling, in order:
  1. It is **not** routed to the generic `ErrorBanner` failure path and **never** rendered as "something went wrong", a stack trace, or a retry toast.
  2. The console catalog supplies an override rendered *alongside* the server `message`: `console.setup.domain_not_allowed.title` ("Add your email domain to the allow-list"), `console.setup.domain_not_allowed.body` (explains deny-by-default: an unconfigured or empty allow-list denies **every** claim, including this first one, and that this is intended behaviour rather than a bug), and `console.setup.domain_not_allowed.action` ("Edit allowed email domains").
  3. The action control returns the operator to `AuthSettingsStep` with its state preserved and the allowed-email-domains field focused; the offending domain is offered as a pre-filled suggestion via `message_args` (structured data — never pre-formatted English prose).
  4. Per §4.6 the server-supplied `message` remains the fallback if the console key is missing, so the operator never sees a bare key. `moira.error.admin_claim_domain_not_allowed` is listed in `moira-keys.ts` and must exist in `docs/i18n-response-catalog.json`.
  
  The same treatment applies wherever the code can surface post-setup (e.g. `/settings/auth`), so the guidance is not wizard-only. Asserted by `console/tests/e2e/setup-wizard-ordering.spec.ts` and `console/tests/unit/modules/setup/SetupWizard.test.tsx`.
- Structure is locale-ready (`catalog.en.ts` is the first of N); shipping additional locales is out of scope for MVP but requires no refactor.

### Setup wizard (`console/app/setup/**` + `console/modules/setup/**`)

#### Step order is load-bearing (D3) — do not reorder

On a **fresh deployment** the `auth_provider_settings` table is empty, so `allowed_email_domains` is unconfigured. Because plan 07's domain policy is **deny-by-default with no first-claim exemption and no bootstrap bypass**, a claim attempted before an auth provider is configured **and enabled** with a non-empty `allowed_email_domains` **always** returns `403 admin_claim_domain_not_allowed` — on the system-key path too. The auth-provider step therefore **must** precede the claim step; the wizard enforces this as navigation state, not as advice.

| # | Screen (organism) | Route/step | Moira call(s) | Auth used | Gate to advance |
|---|---|---|---|---|---|
| 1 | `WelcomeStep` | `/setup` | `GET .../setup/claim-status` | **none — the only anonymous call** | `claimed === false` (else redirect `/login`) |
| — | *(page-layer preload, no screen)* | `/setup` | `GET .../setup/status`, **`GET .../setup/auth-methods`** | `X-Moira-System-Key`, **server-side only (D4)** | system key present, Moira reachable |
| 2 | **`AuthSettingsStep`** | `/setup` | `POST` on `/api/v1/admin/auth/providers` (non-secret config) → **console-DB secret write (D7)** → `enable` | `X-Moira-System-Key` for the Moira calls; the secret write is console-local | **Moira row saved AND the console secret stored AND the row enabled AND `allowed_email_domains` non-empty** — the wizard blocks step 3 until all four are confirmed. A partial dual write leaves the step **incomplete** with a keyed, actionable remedy (see *Two-store drift protection*), never a success. |
| 3 | `SignInClaimStep` | `/setup` | Better Auth OAuth (no Moira call) | session | verified email whose domain is in the allow-list |
| 4 | *(claim action)* | `/setup` | `POST .../jwt-issuers`, then `POST .../setup/claim` | `X-Moira-System-Key` + `Idempotency-Key` | 201/200 from claim |
| 5 | `DoneStep` | `/setup` | — | — | → `/dashboard` |

Step 2 **cannot** be skipped, deferred, or reordered behind step 3/4. If an operator reaches the claim with step 2 incomplete — e.g. by deep-linking, by a restored draft, or because the provider row exists but is **disabled** — the resulting `403 admin_claim_domain_not_allowed` is rendered as an **actionable setup instruction** that returns them to step 2 with its state preserved (see the i18n handling below).

1. **`app/setup/page.tsx`** (page layer, server component): calls `moiraClient.getSetupClaimStatus()` — unauthenticated `GET /api/v1/admin/setup/claim-status`, **the only anonymous Moira call in the console**. If `claimed === true`, redirect to `/login`. Then, **server-side only**, calls the authenticated `GET /api/v1/admin/setup/status` **and `GET /api/v1/admin/setup/auth-methods`** with `X-Moira-System-Key` (D4: `moira:setup:read`; unauthenticated ⇒ 401; **no anonymous variant exists**). The `auth-methods` result is reduced to a display-safe view model **on the server** and passed down as props; the raw response never crosses to the browser and no client component ever fetches this path. If the root system key is missing, render a blocking keyed state (`console.setup.backend_not_ready`) — the console **cannot** bootstrap Moira's root system key; that remains the operator CLI step (`bootstrap-system-key`, `src/main.rs`), documented in the wizard copy, never automated. Renders `<SetupWizard />`.
2. **`modules/setup/WelcomeStep.tsx`** — explains what claiming does, that it happens exactly once, **and that the auth provider must be configured first because the domain allow-list denies by default**.
3. **`modules/setup/AuthSettingsStep.tsx`** — the CONVENTIONS §7.2 step, and **a hard prerequisite of the claim (D3)**. Collects: auth method (`google_oauth` / `generic_oidc`), client id, discovery URL or issuer, hosted domain (Google), allowed email domains (**deny-by-default; the form refuses to submit an empty list with an explicit keyed warning, and the copy states that an empty list denies every claim including the operator's own first claim**), and the client secret. Submits to `app/setup/actions.ts::saveAuthSettings()`, which performs the **D7 dual write in this single step** — (1) `POST` the **non-secret config** to Moira's auth-settings endpoint with `X-Moira-System-Key`; (2) `putProviderSecret(...)` to store the **client secret encrypted in the console's own database**, keyed by the Moira row id and fingerprinted against its `client_id`; (3) `POST .../{id}/enable`, because 07 creates rows `enabled: false` and a **disabled** row does not govern the issuer, so the claim would still 403. **The client secret is never sent to Moira** (D7). Partial success at any point is an **operator-resolvable failure** with a keyed remedy and no step advance — the full state table is in *Two-store drift protection*. The secret field is write-only in the UI too: never returned by any read (Moira has none to return and the console never reads its own back to the browser), never re-rendered into the form — on revisit the field shows a `MaskedValue` "configured" state derived from the *existence* of the console row (`console.authSettings.secret_configured`), never from any derivative of the secret value — and never passed as a prop into any organism/molecule/atom. On success the action calls `invalidateAuthSettings()` so the next `getAuth()` rebuilds from the new config **and the new secret** with no redeploy, and unlocks the sign-in/claim step.
4. **`modules/setup/SignInClaimStep.tsx`** — "Sign in to become the first admin." **Rendered only once step 3 reports an enabled provider with a non-empty allow-list.** Renders one button per enabled method, driving Better Auth's sign-in. Tagged with an intent marker (a signed, short-lived httpOnly cookie set when the step renders) so the post-callback landing routes into the claim action rather than a normal login.
5. **`app/setup/actions.ts::claimAdmin()`** (`"use server"`):
   - Reads the session via `(await getAuth()).api.getSession({ headers: await headers() })`; re-verifies `emailVerified === true` and re-runs `domain-policy.ts` (defence in depth — never more permissive than Moira's authoritative check).
   - **First** registers the console's own issuer: `GET /api/v1/admin/jwt-issuers` pre-check, then `POST /api/v1/admin/jwt-issuers` with `X-Moira-System-Key` — body `{ issuer: MOIRA_BFF_ISSUER_URL, jwks_url: <the URL the jwt plugin actually serves>, expected_audiences: [MOIRA_ADMIN_API_AUDIENCE], allowed_algorithms: ["ES256"], subject_claim: "sub" }` (fields match `TrustedJwtIssuerCreateRequest`, `src/domain/admin.rs:564`, `deny_unknown_fields` — send nothing extra; **`scopes_claim` is deliberately omitted** because the console mints no scope claim). Registration **must precede** the claim: plan 07's claim endpoint rejects an unregistered issuer with 400 `unregistered_trusted_issuer` (07 module 3 `resolve_issuer_id`).
   - **Then** `POST /api/v1/admin/setup/claim` with `X-Moira-System-Key`, body `{ issuer: MOIRA_BFF_ISSUER_URL, subject: <idpSubject>, email: <verified email>, email_verified: true }` (exact `ClaimAdminIdentityRequest`; `scopes` omitted → 07 defaults to `["moira:admin"]`), `Idempotency-Key` derived deterministically from `(issuer, idpSubject)` so a double-submit replays (200) rather than duplicating.
     > **`email` and `email_verified` are sent on EVERY claim, unconditionally, including the system-key path (D5).** Both are required by 07's DTO and there is **no optional-email path** — the action must never construct a body that omits either, and must never branch on credential type to decide whether to include them. Omitting either yields `moira.error.invalid_request` (400/422) from axum's extractor before the service is reached. Asserted by `console/tests/unit/lib/moira-client.test.ts`.
   - **Handles `403 admin_claim_domain_not_allowed` as an actionable setup step, not a failure** — see the i18n handling below; the action returns the operator to `AuthSettingsStep` rather than dead-ending.
   - Redirects to `DoneStep` → `/dashboard` under a normal authenticated session (issuer registered, grant present, JWKS fetchable — Mode A works immediately).
6. **`modules/setup/DoneStep.tsx`** — confirms the grant (rendering `AdminIdentityRecord.email`, a required `String`) and links to the dashboard.

### MVP admin screens

Pages under `app/(console)/` stay thin (fetch + guard + render); all rendering lives in `modules/`.

| Screen | Page | Organism(s) | Moira endpoints |
|---|---|---|---|
| Dashboard | `app/(console)/dashboard/page.tsx` | `modules/dashboard/ReadinessPanel` | `GET .../setup/status` + `GET .../setup/claim-status` |
| Providers | `app/(console)/providers/page.tsx`, `[id]/page.tsx` | `modules/providers/{ProviderTable,ProviderForm}` | `GET/POST /providers`, `GET/PATCH/DELETE .../{id}`, enable/disable |
| Provider models | `app/(console)/provider-models/page.tsx` | `modules/providerModels/*` | `GET/POST /providers/{id}/models`, `GET/PATCH/DELETE /provider-models/{id}` |
| Credentials | `app/(console)/credentials/page.tsx` | `modules/credentials/{CredentialTable,CredentialForm,CredentialRotatePanel}` | `GET/POST /provider-credentials`, rotate/enable/disable |
| Routes | `app/(console)/routes/page.tsx` | `modules/routing/{RouteTable,RouteForm}` | `GET/POST /routes`, `GET/PATCH/DELETE .../{id}` |
| Routing policies | `app/(console)/routing-policies/page.tsx` | `modules/routing/{RoutingPolicyTable,RoutingPolicyForm}` | `GET/POST /routing-policies`, `GET/PATCH/DELETE .../{id}` |
| Applications | `app/(console)/applications/page.tsx` | `modules/applications/*` | `GET/POST /applications`, `GET/PATCH/DELETE .../{id}`, execution-policy |
| Trusted JWT issuers | `app/(console)/jwt-issuers/page.tsx` | `modules/jwtIssuers/JwtIssuerTable` | `GET/POST /jwt-issuers`, `GET/PATCH/DELETE .../{id}`, enable/disable, refresh-jwks |
| Audit log | `app/(console)/audit-log/page.tsx` | `modules/audit/AuditLogPanel` | `GET /audit-events`, `GET .../{id}` |
| Auth settings | `app/(console)/settings/auth/page.tsx` | `modules/authSettings/{AuthSettingsForm,ProviderSecretRotatePanel,ProviderDriftBanner}` | Moira auth-settings endpoints (**7 ops, non-secret config only — D7**) + the console's own `authProviderSecret` store for the secret and its rotation |

Notes carried forward: **credentials never render a plaintext secret** — the once-only creation response appears in `OnceOnlySecretModal` ("copy now, will not be shown again"), mirroring Moira's `ApiKeySecretResponse` contract, and the value never becomes a prop on any reusable molecule/atom beyond that modal's own render. The **console's own issuer row** is flagged read-only in `JwtIssuerTable` with a typed-issuer-name confirm guard on disable, so an operator cannot casually disable their own login path. The **audit log** shows "showing latest N, no further pages" until P1-4's cursor fix lands in plan 04, rather than a broken "next" control.

**Explicitly excluded from MVP:** system-keys and consumer-keys management (break-glass/bootstrap mechanisms kept CLI/API-only so the console never becomes a vector for over-privileged self-service key minting), agent-profiles, RAG collections/documents, conversation/memory policy screens (P0-3).

### `console/middleware.ts`

- Enforces `ALLOWED_CONSOLE_HOSTS` (comma-separated exact hostnames, **no wildcards**) against the `Host` header before any auth processing, closing host-header-injection open-redirect risk.
- Sets on every response: `Strict-Transport-Security`, `X-Content-Type-Options: nosniff`, `Content-Security-Policy: frame-ancestors 'none'` (plus `X-Frame-Options: DENY`), `Referrer-Policy: strict-origin-when-cross-origin`, and a `Content-Security-Policy` with no `unsafe-inline` for scripts.
- Redirects unauthenticated requests to `/(console)/**` → `/login`, and redirects everything → `/setup` while `claim-status` reports `claimed: false` (server-side call, briefly cached to avoid a Moira round-trip per request).

### Tests (exact file names)

**Unit — `bun test` (CONVENTIONS §3).**

*lib:*
- `console/tests/unit/lib/auth-settings.test.ts` — Moira fetch shape, composite (Moira-version + console-secret-`updatedAt`) cache key, TTL expiry, `invalidateAuthSettings()` forces a reload, fail-closed when Moira is unreachable, **empty `allowedEmailDomains` denies everyone**. **Plus the D7 composition tests, named:** `compose_merges_moira_config_with_console_secret`, `enabled_provider_without_a_console_secret_is_excluded_and_keyed`, `fingerprint_mismatch_excludes_the_provider_and_keys_the_error`, `secret_rotation_alone_invalidates_the_cache`, and `no_code_path_requests_a_secret_from_moira` (no method, path constant, or type in the module expects secret material back from Moira).
- `console/tests/unit/lib/provider-secrets.test.ts` — **the D7 storage suite**, enumerated in full under *Two-store drift protection (c)*: fingerprint stability/difference/keying/constant-time comparison, mismatch-before-decrypt ordering, `missing` vs `client_id_mismatch` as distinct reasons, AES-256-GCM round-trip with AAD binding rejecting a moved row, `errors_never_contain_the_secret_or_the_ciphertext`, and `no_api_returns_the_plaintext_secret_to_a_caller`. Also `dedicated_encryption_key_is_not_better_auth_secret` — the module derives from `CONSOLE_SECRET_ENCRYPTION_KEY` and never reads `BETTER_AUTH_SECRET` — and `boot_fails_closed_without_an_encryption_key`.
- `console/tests/unit/lib/types.test.ts` — `auth_provider_record_type_has_no_secret_field`: the TypeScript mirror of Moira's `AuthProviderSettingsRecord` declares no `client_secret`, `secret_fingerprint`, `masked_secret`, or any field matching `/(secret|masked|fingerprint)/i` (**D7 — Moira's read endpoints carry no secret material at all**), and no `rotate-secret` path constant exists anywhere in `lib/`.
- `console/tests/unit/lib/auth-config.test.ts` — the object handed to `betterAuth()`: `trustedOrigins` set; `advanced.disableCSRFCheck` **absent**; `useSecureCookies` true in prod; cookie attributes `httpOnly`/`secure`/`sameSite=lax`; `rateLimit.enabled` true; `nextCookies()` is the **last** plugin; `genericOAuth` carries `requireIssuerValidation: true` and `pkce: true`; a settings `version` change yields a rebuilt instance.
- `console/tests/unit/lib/moira-token.test.ts` — **the security invariant suite**: `definePayload` output contains **no `scope` and no `scp`**; no `email`/`email_verified`; `getSubject` returns the IdP `sub` and **not** the Better Auth `user.id`; `issuer`/`audience` non-empty and matching env; `expirationTime` ≤ 120s; `keyPairConfig.alg` is in Moira's registered `allowed_algorithms`; `disablePrivateKeyEncryption` is not set.
- `console/tests/unit/lib/domain-policy.test.ts` — deny-by-default, exact-email match, domain match, case-insensitivity, sub-domain non-match, unicode/IDN normalisation.
- `console/tests/unit/lib/moira-client.test.ts` — `Idempotency-Key` present on every mutation and stable per submission; `If-Match` present on PATCH/PUT **and rotate**; `Authorization` vs `X-Moira-System-Key` selection per mode; the unauthenticated claim-status read sends no credential. **Named tests for the D4/D5 propagation:**
  - `claim_request_always_sends_email_and_email_verified` — every claim body built by the client carries **both** fields, on the **system-key path and the setup-token path alike**; no code path produces a body omitting either.
  - `claim_request_has_no_optional_email_branch` — the builder exposes no flag, overload, or credential-type branch that makes `email` omittable (the pre-D5 optional path is gone, not merely unused).
  - `auth_methods_read_sends_system_key` — `GET .../setup/auth-methods` always attaches `X-Moira-System-Key` and is never issued credential-free (D4: an anonymous call would 401, and there is no anonymous variant to fall back to).
  - `claim_status_is_the_only_anonymous_call` — enumerating every method on the client, exactly one (`getSetupClaimStatus`) sends no credential; every other Moira call attaches either `X-Moira-System-Key` or `Authorization: Bearer`.
  - **D7:** `no_request_body_or_header_ever_carries_a_client_secret` — every request the client can construct, across every method and every auth-settings shape, is scanned for the fixture client-secret value; the violation set must be **empty**.
  - **D7:** `there_is_no_rotate_secret_method` — the client exposes no method, no path constant, and no type referencing `rotate-secret`; the auth-provider surface is exactly **7 operations** (list, create, get, patch, delete, enable, disable — §0.2; asserting 10 here would fail).
- `console/tests/unit/lib/errors.test.ts` — `ErrorResponse` → client-safe union; `details` never crosses the boundary; 401/403 map to the sign-out-and-redirect outcome.
- `console/tests/unit/lib/session.test.ts` — server-only session read, no token ever returned to callers.
- `console/tests/unit/lib/i18n.test.ts` — `t()` resolves catalog first, falls back to the server `message` for an unknown `message_key`, falls back to the key when both are absent, interpolates `message_args` as structured data (never pre-formatted prose).
- `console/tests/unit/lib/i18n-catalog-coverage.test.ts` — every `console.*` key referenced in `app/`, `modules/`, and `components/` exists in `catalog.en.ts` with a non-empty English default; every key mirrored in `moira-keys.ts` exists in `docs/i18n-response-catalog.json`.

*atoms (one per atom, CONVENTIONS §6 rule 6):* `console/tests/unit/atoms/{Button,Input,Textarea,Select,Checkbox,Label,Badge,Spinner,Icon,Heading,Text,VisuallyHidden}.test.tsx` — render, prop pass-through, disabled/loading states, accessible name, keyboard focus.

*molecules (one per molecule):* `console/tests/unit/molecules/{FormField,TableRow,DataTable,ConfirmDialog,StatusBadgeGroup,Pagination,EmptyState,ErrorBanner,Toast,OnceOnlySecretModal,MaskedValue}.test.tsx` — composition, callback wiring, error/empty states. `ErrorBanner`/`Toast` additionally assert they render `messageKey` through `t()` and fall back to `message`. `MaskedValue`/`OnceOnlySecretModal` assert the raw value never appears in a `title`/`aria-label`/`data-*` attribute.

*organisms (D7 additions):* `console/tests/unit/modules/authSettings/ProviderSecretRotatePanel.test.tsx` — the current secret is never displayed or pre-filled; the panel submits to `rotateProviderSecret()` and **issues no Moira call** when only the secret changes; the success notice is `console.notice.auth_provider_secret_rotated`. `console/tests/unit/modules/authSettings/ProviderDriftBanner.test.tsx` — renders `auth_provider_client_id_mismatch`, `auth_provider_secret_missing`, and `auth_provider_secret_undecryptable` as **three distinct, actionable** states (never collapsed into one message, never the generic `ErrorBanner`), each with a working control to the remedy.

*organisms:* `console/tests/unit/modules/setup/SetupWizard.test.tsx` (**step order is enforced: the claim step is unreachable until an enabled provider with a non-empty allow-list is confirmed; `admin_claim_domain_not_allowed` renders the actionable `console.setup.domain_not_allowed.*` instruction with a working route back to `AuthSettingsStep`, and not the generic error banner**), `.../setup/AuthSettingsStep.test.tsx` (client-secret field is write-only and never pre-filled; empty allowed-domains blocks submit; **the step is not marked complete until all four of — Moira row saved, console secret stored, row enabled, allow-list non-empty — are confirmed (D7)**; a forced failure of the console-secret write leaves the step incomplete with `console.error.auth_provider_secret_write_failed` and Retry/Discard controls, never a success advance; **the submitted secret never appears in any recorded outbound Moira request**), `.../auth/SignInPanel.test.tsx` (renders exactly the enabled methods; renders the keyed not-configured state when none), `.../providers/ProviderTable.test.tsx`, `.../credentials/CredentialForm.test.tsx` (**never renders a plaintext secret**), `.../audit/AuditLogPanel.test.tsx`, `.../dashboard/ReadinessPanel.test.tsx`, `.../jwtIssuers/JwtIssuerTable.test.tsx` (console's own row read-only).

*architecture:*
- `console/tests/unit/architecture/layer-dependencies.test.ts` — static import-graph scan asserting the one-way rule: atoms import no molecule/organism/`lib`; molecules import only atoms; organisms import no page; nothing under `components/` imports `lib/moira-client` or `lib/auth`; nothing outside `app/`/`modules/` imports `next/navigation`.
- `console/tests/unit/architecture/server-only-guards.test.ts` — `lib/{auth,auth-settings,provider-secrets,moira-client,moira-token,session,env.server}.ts` each begin with `import "server-only"`.
- `console/tests/unit/architecture/no-moira-secret-assumption.test.ts` — **the D7 guard**. Named tests: `no_source_file_references_rotate_secret` (the literal `rotate-secret` appears nowhere under `console/`); `no_type_declares_a_secret_field_on_a_moira_auth_provider_record`; `client_secret_is_only_ever_read_from_provider_secrets_module` (a static scan asserts `clientSecret` is produced only by `lib/provider-secrets.ts` and consumed only by `lib/auth-settings.ts`/`lib/auth.ts`, never from a Moira response shape); `auth_provider_secret_table_is_not_in_moira_migrations` (no file under `migrations/` mentions `authProviderSecret`).
- `console/tests/unit/architecture/no-secret-props.test.ts` — no component prop name matches `/(secret|systemKey|privateKey|clientSecret|apiKey|token|password)/i` anywhere under `modules/` or `components/` (CONVENTIONS §6 rule 5, mechanically enforced).
- `console/tests/unit/architecture/no-hardcoded-copy.test.tsx` — JSX text nodes under `modules/` and `components/` are either `t(...)` calls or props; bare English literals fail.
- `console/tests/unit/architecture/no-client-side-auth-methods.test.ts` — **the D4 guard**. Named tests:
  - `auth_methods_is_never_fetched_from_client_code` — a static scan asserts the literal path `/api/v1/admin/setup/auth-methods` appears **only** in `console/lib/moira-client.ts` and server-side page/action files, and **never** in any file carrying `"use client"`, anywhere under `console/components/**`, or in any `modules/**` file not marked server-only.
  - `no_console_route_proxies_auth_methods_to_the_browser` — no handler under `console/app/api/**` returns the `auth-methods` response (or a superset of it) to the browser, so the console cannot become the anonymous variant Moira deliberately does not offer.
  - `claim_status_is_the_only_anonymous_moira_path_in_client_reach` — asserts the contrast explicitly: `/api/v1/admin/setup/claim-status` is the sole Moira path permitted to be fetched without a credential.

**E2E — Playwright (`bunx playwright test`), against a running console + a real test-fixture Moira + a local mock OIDC provider.**
- `console/tests/fixtures/mock-oidc/server.ts` — the local mock OIDC provider (discovery document, JWKS, authorize/token/userinfo). **Real Google is never used in CI**, per CONVENTIONS §3.
- `console/tests/e2e/setup-wizard.spec.ts` — fresh Moira (bootstrap system key only) → `/setup` → auth-settings step writes **and enables** config into Moira with a non-empty `allowed_email_domains` → mock-OIDC sign-in → claim succeeds → dashboard. Direct API assertions (not through the UI): `claim-status` flips `false`→`true`; the recorded claim request body carried **both `email` and `email_verified`** (D5); a network tap over the whole run shows the browser issued **zero** requests to `/api/v1/admin/setup/auth-methods` (D4); and a **scope-free** console token for the claimed `(issuer, subject)` authorizes `GET /api/v1/admin/setup/status` — proving the 07 grant union, not a minted scope, is what authorizes.
- `console/tests/e2e/setup-wizard-ordering.spec.ts` — **the D3 ordering spec, against a genuinely fresh Moira (bootstrap system key only, `auth_provider_settings` empty).** Named tests:
  - `fresh_deployment_completes_when_auth_provider_is_configured_before_claim` — walking the wizard in its prescribed order (auth provider **saved and enabled** with a non-empty `allowed_email_domains`, *then* sign-in, *then* claim) succeeds end-to-end and flips `claim-status` `false`→`true`.
  - `premature_claim_returns_actionable_domain_not_allowed` — forcing the claim **before** the auth-provider step (deep-link / direct server-action invocation) yields Moira's `403 admin_claim_domain_not_allowed`, and the UI renders the **actionable** `console.setup.domain_not_allowed.*` instruction with a working control back to `AuthSettingsStep` — asserting the generic failure banner is **absent** and no stack trace or raw envelope is shown.
  - `disabled_provider_row_still_denies_the_claim` — a provider row saved but left **disabled** does not govern the issuer, so the claim still 403s with the same actionable rendering; the wizard does not advance past step 2.
  - `empty_allowed_domains_blocks_step_advance` — the auth-provider form refuses to submit an empty allow-list, so the wizard cannot reach the claim step in a state that is guaranteed to 403.
  - `no_bootstrap_bypass_on_the_system_key_path` — a direct system-key `POST .../setup/claim` against the fresh instance, with a fully-populated body, is **still** refused `403 admin_claim_domain_not_allowed`, proving there is no first-claim exemption and no bootstrap bypass to regress against.
- `console/tests/e2e/google-signin.spec.ts` — post-setup sign-in through the mock OIDC provider standing in for Google, including the `hd`/allowed-domain accept and reject cases; asserts session cookie attributes via `page.context().cookies()`.
- `console/tests/e2e/sign-out.spec.ts` — authenticated session → sign out → `/(console)/**` redirects to `/login` → session cookie cleared and the server-side session row is gone.
- `console/tests/e2e/config-round-trip.spec.ts` — create a provider via the UI → assert it appears in the UI list → assert via a direct Moira API read that it exists with the submitted fields → PATCH via the UI → assert a concurrent direct-API patch surfaces an `If-Match` conflict as a keyed toast rather than a silent overwrite.
- `console/tests/e2e/auth-settings-round-trip.spec.ts` — change allowed domains + client id via `/settings/auth` → assert Moira stores them → assert the console applies them **without a restart** → assert **no Moira response contains any secret material at all (D7 — not a secret, not a fingerprint, not a mask)** and the client secret never appears in the rendered HTML. Additionally: rotating **only** the secret via `ProviderSecretRotatePanel` issues **zero** Moira requests, takes effect on the next sign-in with no redeploy, and emits `console.notice.auth_provider_secret_rotated`.
- `console/tests/e2e/auth-secret-drift.spec.ts` — **the D7 drift spec** (required by CONVENTIONS' D7 consequences). Named tests exactly as enumerated in *Two-store drift protection (c)*: `client_id_changed_in_moira_surfaces_actionable_mismatch_error`, `mismatch_never_reaches_the_oauth_provider`, `mismatch_hides_the_sign_in_button_rather_than_failing_mid_flow`, `re_entering_the_secret_for_the_new_client_id_clears_the_mismatch`, `missing_console_secret_surfaces_its_own_distinct_error`, `partial_write_leaves_provider_disabled_and_offers_retry_or_discard`.
- `console/tests/e2e/authorization-denial.spec.ts` — a second identity signs in successfully (authentication OK) but has **no** `admin_identities` grant; every admin screen and every server action fails with Moira's 403, the UI renders the keyed denial state, and a direct API check confirms no mutation occurred. Explicitly asserts the console cannot self-grant.
- `console/tests/e2e/jwks.spec.ts` — the JWKS URL the console registers with Moira is the URL it actually serves; the document is valid JWKS JSON, contains **public key material only** (no `d` parameter, no PEM private header), and every published key has a `kid`.
- `console/tests/e2e/i18n-message-key.spec.ts` — force a Moira error with a known `message_key`; assert the console renders the catalog string; then force an **unknown** `message_key` and assert it renders the server-supplied `message` verbatim (never a hardcoded English string, never the raw key).
- `console/tests/e2e/a11y.spec.ts` — `@axe-core/playwright` on **every page route**: `/`, `/setup` (each step), `/login`, `/dashboard`, `/providers`, `/providers/[id]`, `/provider-models`, `/credentials`, `/routes`, `/routing-policies`, `/applications`, `/jwt-issuers`, `/audit-log`, `/settings/auth`. Zero critical/serious violations gates CI.
- `console/tests/e2e/secret-leak.spec.ts` — a network/console tap across the full authenticated journey asserting no browser-observed response body, no rendered HTML, and no `console.log` ever contains the system key fixture, the OAuth client secret fixture, the console encryption key, a PEM header, or a decrypted provider credential — except the one intentional once-only reveal, which is additionally asserted never to be logged or cached. **Plus the D7 test, named:**
  - `client_secret_never_appears_in_any_request_to_moira` — a recording proxy in front of the fixture Moira captures **every** request the console makes (method, path, query, headers, body) across the whole run: the setup wizard's dual write, the auth-settings edit, a secret rotation, a `client_id` change, and the ordinary admin CRUD journey. The OAuth client-secret fixture value must appear in **zero** of them, and the violation set must be **empty** rather than merely "not found this run". This is the mechanical proof of D7's core claim — the secret is console-owned and never crosses to Moira.
  - `client_secret_never_appears_in_a_browser_visible_response` — the same fixture value appears in no response body, no rendered HTML, no RSC payload, no `data-*` attribute, no `title`/`aria-label`, and no cookie.

**Secret-leak — build-time.**
- `console/tests/secret-leak/bundle-scan.test.ts` — after `bun run build`, scan `.next/static/**/*.js`, `.next/server/**/*.html`, and all SSR-emitted HTML from the e2e run for: the bootstrap system key fixture value, **the OAuth client secret fixture value (D7)**, **the `CONSOLE_SECRET_ENCRYPTION_KEY` fixture value**, `-----BEGIN` (any PEM header), and any env var name matching `/^NEXT_PUBLIC_.*?(SECRET|KEY|TOKEN|PASSWORD)/i`. **Asserts the violation set is empty**, not merely "this run's fixture wasn't found," so the gate catches future regressions before a real secret exists to grep for.

### Documentation

- `console/README.md` — deployment, the pinned toolchain, required env vars (each marked server-only; none are public — **including `CONSOLE_SECRET_ENCRYPTION_KEY` and its rotation runbook**), the Atomic Design layering rules, Mode A vs Mode B, and the "auth is configured in the wizard, not in `.env`" model.
- `docs/admin-console.md` (new, Moira repo root docs) — the exact Moira endpoints the console calls, the `trusted_jwt_issuer` registration shape, the no-scope-claim invariant and why it exists, the deny-by-default domain policy, **and an explicit statement that SAML SSO is not supported and that mode 3 (bring-your-own JWKS) is the path for it**. **Plus a D7 section**: why the client secret is console-owned (Better Auth needs it in process; Moira's invariant that a decrypted secret never crosses a network boundary is preserved by removing the secret from Moira rather than exposing a read-back endpoint), where it lives (`console_auth.authProviderSecret`, AES-256-GCM under `CONSOLE_SECRET_ENCRYPTION_KEY`), the two-store drift model and what each keyed error means operationally, the console-side rotation procedure (including the IdP-side dual-secret overlap recommendation), and an explicit note that **Moira has no `rotate-secret` endpoint and no way to read a client secret back**. Links to `docs/jwt-issuer-management.md` and `docs/public-authentication.md`.

### Deployment assets

- `console/Dockerfile`: multi-stage (`deps` → `builder` → `runner`); `oven/bun:1.3.14` for install/build, `node:24-slim` runtime; `bun install --frozen-lockfile`; non-root user; `HEALTHCHECK` hitting `app/api/health/route.ts`, which checks Moira reachability **without leaking Moira response bodies**.
- `charts/moira-console/`: `Chart.yaml`, `values.yaml` (secrets referenced by name not value; `moiraBaseUrl`, `consoleBaseUrl`, `allowedConsoleHosts`, `replicaCount`), `templates/{deployment,service,ingress,secret,configmap,serviceaccount,migration-job,hpa}.yaml` mirroring `charts/moira/templates/`. `migration-job.yaml` runs the Better Auth schema migration against `CONSOLE_DATABASE_URL` as a pre-install/pre-upgrade hook.

---

## Multi-Agent Workflow

### Waves (disjoint file ownership; parallelizable within a wave, sequential across)

**Wave 0 — Coordinator checkpoint (sequential, blocking).**
- Confirm plan 07's shipped contract matches its Frozen-contract table (`GET .../claim-status` → `{ "claimed": bool }`; `POST .../claim` + `ClaimAdminIdentityRequest`/`AdminIdentityRecord`; the issuer-must-be-preregistered guard).
- **Re-confirm the D3/D4/D5 frozen-contract change** against 07's shipped code, specifically: `ClaimAdminIdentityRequest.email` is a required `String` and `email_verified` has **no** `#[serde(default)]`; `AdminIdentityRecord.email` is `String`; `GET .../setup/auth-methods` returns **401** when called anonymously; and a fully-populated system-key claim against a fresh instance with no enabled provider is refused **403 `admin_claim_domain_not_allowed`** (proving no bootstrap bypass). Any divergence is an escalation, not a local workaround.
- **Confirm or unblock D-1** (Moira DB-backed auth settings). Wave 1 does not start without D-1 merged or signed off as a frozen RFC.
- **Verify D7 as shipped by 07 — no longer a decision, now a conformance check.** The client-secret custody question is **closed** (CONVENTIONS §0 D7): the console owns the secret. Wave 0 verifies, against 07's shipped code, that (i) `auth_provider_settings` carries **no** encrypted-envelope columns (`encrypted_payload`, `encryption_algorithm`, `encryption_version`, `encrypted_data_key`, `nonce`, `secret_fingerprint`, `masked_secret`); (ii) `POST /api/v1/admin/auth/providers/{id}/rotate-secret` **returns 404 / is absent from the OpenAPI document** — the auth-provider surface is **10 operations, not 11**; (iii) no auth-provider response body contains secret material of any kind; (iv) `auth_provider_secret_rebind_required` and its i18n key are **absent** from Moira's catalog and `docs/i18n-response-catalog.json`. Any divergence is an escalation, **not** a local workaround — and specifically, **adding a Moira read-back endpoint is not an available remedy**; it was considered and rejected.
- **Confirm `CONSOLE_SECRET_ENCRYPTION_KEY` provisioning** with ops (32 raw bytes, base64, delivered as a K8s `Secret`, with a documented rotation runbook) — a Wave 1 prerequisite for `lib/provider-secrets.ts`, not a design question.
- **Verify Moira's accepted JWT algorithms** and fix the `keyPairConfig.alg` pin (product decision 2).
- **Resolve the concrete JWKS URL** the Better Auth `jwt` plugin serves under the chosen `jwksPath`, by running it — the registered `jwks_url` is the observed URL, never a guess.
- Confirm the MVP admin-screen list and the console-database placement (product decisions 1 and 3).

**Wave 1 — Scaffolding, toolchain, design system (parallel, disjoint).**
- *Frontend engineer A*: `console/package.json` (exact pins, `engines`, `packageManager`), `bun.lock`, `.nvmrc`, `next.config.ts`, `tsconfig.json`, `playwright.config.ts`, `app/layout.tsx`, `app/error.tsx`, `app/not-found.tsx`.
- *Design-system engineer*: **all** of `console/components/atoms/**` and `console/components/molecules/**` plus their unit tests. Presentational only — this track never touches `lib/` or `modules/`, which is what makes it fully parallel and keeps the layering honest by construction.
- *Backend-integration engineer*: `console/lib/{types,moira-client,errors,env.server}.ts` + tests. Publishes `lib/types.ts` first (~30 min head start) so the other tracks compile against it.
- *i18n engineer*: `console/lib/i18n/**` + `i18n.test.ts`, `i18n-catalog-coverage.test.ts`.
- *Security engineer*: `console/middleware.ts` — **single owner across all waves** (this file is touched again in Wave 2; one owner end-to-end avoids the merge race).

**Wave 2 — Better Auth + runtime settings + setup wizard (parallel, disjoint).**
- *Security/OAuth engineer*: `console/lib/{auth,auth-settings,provider-secrets,provider-secrets-schema,moira-token,domain-policy,session}.ts`, `console/app/api/auth/[...all]/route.ts`, `console/db/**` (Better Auth CLI schema, **including `authProviderSecret`** — D7), `console/app/login/page.tsx`, `console/modules/auth/**`, plus `middleware.ts`'s auth-redirect completion. Owns every security-invariant unit test **and the whole D7 secret-storage/drift surface**.
- *Frontend engineer A*: `console/modules/setup/**`, `console/app/setup/{layout,page,actions}.ts(x)`.
- *Frontend engineer B*: `console/app/(console)/layout.tsx`, `console/modules/shell/**`, `console/app/(console)/dashboard/page.tsx`, `console/modules/dashboard/**`.
- No overlap: `lib/` + `app/api/auth/**` (security), `app/setup/**` + `modules/setup/**` (A), `app/(console)/layout.tsx` + `modules/shell,dashboard/**` (B).

**Wave 3 — Admin CRUD screens (parallel, disjoint — one engineer per resource family; each owns both its `app/(console)/<resource>/**` page+actions and its `modules/<feature>/**` organisms).**
- *Frontend engineer A*: `providers/` + `modules/providers/**`; `provider-models/` + `modules/providerModels/**`.
- *Frontend engineer B*: `credentials/` + `modules/credentials/**`; `routes/` + `routing-policies/` + `modules/routing/**`.
- *Frontend engineer C*: `applications/` + `modules/applications/**`; `jwt-issuers/` + `modules/jwtIssuers/**`; `audit-log/` + `modules/audit/**`.
- *Security/OAuth engineer*: `app/(console)/settings/auth/**` + `modules/authSettings/**` (kept with the auth owner because it performs the **D7 dual write** — non-secret config into Moira and the encrypted client secret into the console's own store — and owns secret rotation and the drift banner).
- Zero cross-directory writes — these four run fully in parallel.

**Wave 4 — Deployment, e2e, hardening (parallel, disjoint).**
- *DevOps engineer*: `console/Dockerfile`, `charts/moira-console/**`, CI workflow additions for the §2 frontend gates.
- *Test engineer*: `console/tests/e2e/**` and `console/tests/fixtures/mock-oidc/**`.
- *Security engineer*: `console/tests/secret-leak/**`, `console/tests/unit/architecture/**`, CSP finalization.
- *Docs engineer*: `console/README.md`, `docs/admin-console.md`.

**Read-only reviewers (every wave).** A security reviewer re-reads `lib/auth.ts`, `lib/auth-settings.ts`, `lib/moira-token.ts`, `middleware.ts`, and every `actions.ts` diff for: (a) any `NEXT_PUBLIC_` prefix near a secret-shaped name; (b) any server action missing a session/authorization re-check; (c) any client component importing `moira-client.ts`/`auth.ts` (a build failure via `server-only`, but reviewed anyway); (d) **any change that would introduce a `scope` claim**; (e) any `advanced.disableCSRFCheck`; (f) **any first-claim exemption, bootstrap bypass, or "empty allow-list means allow" fallback** (D3 — 07 removed the carve-out; the console must not reintroduce it); (g) **any browser-side or proxied fetch of `/api/v1/admin/setup/auth-methods`** (D4 — server-side with the system key only); (h) **any claim body that omits `email` or `email_verified`, or any branch that makes them conditional** (D5 — both required on every path); (i) **any request that would send the OAuth client secret to Moira, any type or call expecting Moira to return secret material, and any reference to a `rotate-secret` endpoint** (D7 — the console owns the secret; the Moira read-back option was rejected and must not be reintroduced); (j) **any dual write that advances the wizard on partial success, or any drift path that lets the OAuth exchange proceed on a `client_id` fingerprint mismatch** (D7 drift protections are mandatory). Findings go back to the owning engineer; this reviewer writes no code.

**Conflict avoidance.** Every wave's file list above has zero intra-wave path overlaps. `middleware.ts` and `lib/types.ts` are the only files touched in more than one wave, and each has a **single designated owner across all waves** (security engineer and backend-integration engineer respectively).

### Pull request (CONVENTIONS §1.4)

One PR against `main` from `plan/08-nextjs-console-google-oauth`, opened only after every §2 gate passes locally, with the required sections: **Plan link** (`plans/08-nextjs-console-google-oauth.md`) · **Findings addressed** (P1-11, P0-3, P1-10, P1-4) · **Migrations included** (none in `migrations/`; console-side Better Auth schema in `console/db/`) · **Breaking API/OpenAPI changes** (none) · **Test evidence** (`bun test` + `bunx playwright test` summaries) · **Rollback procedure** · **Deferred follow-ups**.

---

## Interfaces & Contracts

### BFF↔Moira endpoints and headers

| Call | Headers | Notes |
|---|---|---|
| `GET /api/v1/admin/setup/claim-status` | none (unauthenticated) | plan 07 frozen; response is exactly `{ "claimed": bool }` — the wizard's only branch signal, and **the ONLY anonymous Moira call this console makes** |
| `GET /api/v1/admin/setup/auth-methods` | **`X-Moira-System-Key` — server-side from the BFF only** | plan 07 frozen, **authenticated** (`SystemKey` \| `TrustedJwt` + `moira:setup:read`, D4); unauthenticated ⇒ **401**; **no anonymous variant exists**. Never fetched from the browser, never proxied to it. Deliberately contrasts with `claim-status` above. |
| `GET /api/v1/admin/setup/status` | `X-Moira-System-Key` (setup-time) or `Authorization: Bearer` (post-claim) | pre-existing, unchanged by 07 and by this plan; structural readiness only |
| `POST /api/v1/admin/setup/claim` | `X-Moira-System-Key`, `Idempotency-Key` | plan 07 frozen; body `ClaimAdminIdentityRequest` with **`email: string` and `email_verified: boolean` REQUIRED on every call, system-key path included (D5)** — no optional-email path exists; response `AdminIdentityRecord.email` is a required `string`; 201 new / 200 replay / 400 `unregistered_trusted_issuer` / 400\|422 `invalid_request` (either field omitted) / **403 `admin_claim_domain_not_allowed` (deny-by-default, no exemption, no bootstrap bypass — D3; rendered as an actionable setup instruction)** / 409 `admin_identity_already_claimed`; bare Bearer JWT rejected 401 |
| `POST /api/v1/admin/jwt-issuers` | `X-Moira-System-Key`, `Idempotency-Key` | existing; called once, **before** the claim |
| `GET /api/v1/admin/jwt-issuers` | `X-Moira-System-Key` (setup-time) | existing; already-registered pre-check |
| Moira auth-settings read/write (**7 operations**) | `X-Moira-System-Key`, `Idempotency-Key` + `If-Match` on write | **D-1 — paths/shapes owned and frozen by plan 07's amendment**; this plan binds to them, does not name them. **D7: non-secret config only.** No request carries the OAuth client secret; no response carries secret material of any kind (no plaintext, no fingerprint, no mask). **`rotate-secret` does not exist** — rotation is console-side. |
| Console-owned client secret (**not a Moira call**) | none — a console-DB write via `lib/provider-secrets.ts` | **D7.** Encrypted at rest in `console_auth.authProviderSecret`, written in the same wizard/settings step as the Moira config write, fingerprinted against Moira's `client_id`. Listed here explicitly so the contract table shows the *whole* configuration write, not just its Moira half. |
| All other admin CRUD | `Authorization: Bearer <jwt-plugin token>`, `Idempotency-Key` (mutations), `If-Match` (PATCH/PUT and rotate) | existing, per `src/http/mod.rs` |

### Exact JWT claims Moira expects from the console-minted token

Per `src/security/auth.rs::authenticate_trusted_jwt` / `actor_from_trusted_claims` and the `trusted_jwt_issuers` row the console self-registers:

- `iss` — exact string match against the registered `issuer` column (`jwt.issuer` = `MOIRA_BFF_ISSUER_URL`).
- `kid` (JOSE header) — **required** by Moira (`authenticate_trusted_jwt` rejects tokens without it) and must match a key in the JWKS the `jwt` plugin publishes. Asserted by `jwks.spec.ts` and by the round-trip in `setup-wizard.spec.ts`.
- `alg` (JOSE header) — must be in the registered `allowed_algorithms`; this plan registers `["ES256"]` and pins `jwks.keyPairConfig: { alg: "ES256" }` to match. `none`/`HS*` are rejected by Moira regardless (`docs/jwt-issuer-management.md`).
- `aud` — must match the registered `expected_audiences`; set to a single value, `MOIRA_ADMIN_API_AUDIENCE`. **Non-empty is mandatory**: Moira skips audience validation entirely when `expected_audiences` is empty (`validation.validate_aud = false`, `src/security/auth.rs:327-328`).
- `sub` — the registered `subject_claim` (default `"sub"`), carrying the **IdP's stable subject** via `jwt.getSubject`, not the Better Auth user id and not email.
- **No `scope`/`scp` claim** — authorization comes exclusively from plan 07's `admin_identities` grant union. Moira reads scope claims when present, so *not* minting one is a security property, asserted by `moira-token.test.ts` and `authorization-denial.spec.ts`.
- **No `email`/`email_verified` claims** — Moira's trusted-JWT path does not consume them; email lives only in the one-time claim request body and the `admin_identities.email` column.
- `iat`/`exp` — `jwt.expirationTime: "120s"`, so `exp - iat ≤ 120`. Moira's registered `clock_skew_seconds` tolerates minor drift.
- `jti` — unique per token. Moira does **not** track `jti` (no replay ledger for trusted JWTs); the short `exp` is the actual replay control. `jti` is for console-side log correlation only, not claimed as a Moira-enforced property.

### Scopes/authz

The claimed `(issuer, subject)` is granted `moira:admin` at claim time (plan 07's default), matching `ADMIN_SCOPE` in `src/security/authz.rs`. MVP does not support scoped-down admin roles from the console — that is 09+.

### Error handling & i18n

- Moira's `ErrorResponse { error: { code, message_key, message, message_args, request_id, details } }` (`src/error.rs:52-65`) is mapped by `lib/errors.ts` into a client-safe discriminated union carrying `{ code, messageKey, message, messageArgs }`. `details` and `request_id` stay server-side (`request_id` is logged, not rendered). The UI renders `t(messageKey, messageArgs, message)` — catalog first, server `message` as fallback (CONVENTIONS §4.6).
- `401`/`403` from Moira on an authenticated console session (stale/revoked grant) → the BFF clears the session and redirects to `/login` with a **keyed** flash (`console.notice.session_revoked`), never a raw error dump.
- `409 idempotency_conflict` / `idempotency_in_progress` on a form submit → keyed message `console.error.change_in_progress`, submit disabled until the in-flight request resolves; **no client-side idempotency-key regeneration on retry-click**, matching Moira's replay semantics.
- Every console-originated string has a `console.*` key with an English default in `catalog.en.ts`.

### Session cookie attributes

Better Auth default session cookie, configured via `advanced.defaultCookieAttributes`: `httpOnly: true`, `secure: true` (prod; `false` only for local `http://localhost`), `sameSite: "lax"`, `path: "/"`, `session.expiresIn: 28800` (8h) with `updateAge: 3600` sliding refresh, signed with `BETTER_AUTH_SECRET` (32+ bytes, K8s Secret, rotated via Better Auth's versioned `secrets` support). Sessions are DB-backed in the console store, so sign-out is a genuine server-side revocation, not merely a cookie clear.

### CSRF / PKCE / state / nonce / redirect validation

- **CSRF** — Better Auth's `trustedOrigins` origin validation plus Fetch Metadata checks. `advanced.disableCSRFCheck` is **never** set (unit-tested). Next.js Server Actions are POST-only and same-origin by framework default; every action additionally re-reads and re-authorizes the session server-side rather than trusting any client-supplied identity.
- **PKCE** — enabled for OIDC (`pkce: true` on the `genericOAuth` config; standard for the social providers). `code_challenge_method: S256`.
- **State / nonce** — handled by Better Auth's OAuth flow; mismatch is a hard failure with no session created.
- **Redirect validation** — each callback URL is registered **exactly** (no wildcards, no path-prefix matching) at the IdP: `/api/auth/callback/google` for the social provider and `/api/auth/oauth2/callback/moira-oidc` for the generic-OIDC provider. `middleware.ts` rejects any request whose `Host` is not in `ALLOWED_CONSOLE_HOSTS` before Better Auth processes it.

### Logout

Better Auth's sign-out deletes the server-side session row **and** clears the cookie. No Moira-side call is needed — Moira never held a session, and outstanding minted tokens expire in ≤120s. `sign-out.spec.ts` asserts both the cookie clear and the session-row deletion.

---

## Verification

### Gates (CONVENTIONS §2 — frontend)

```bash
bun install --frozen-lockfile
bun run lint
bun run typecheck
bun test                # unit
bunx playwright test    # e2e
bun run build
```

### Unit
`console/tests/unit/**` exactly as enumerated in Detailed Implementation § Tests — lib modules, **every atom**, **every molecule**, every organism, and the four architecture guards. Run with `bun test`.

### Browser end-to-end (Playwright)
`console/tests/e2e/**` exactly as enumerated — `setup-wizard.spec.ts`, **`setup-wizard-ordering.spec.ts`** (the D3 fresh-deployment ordering + actionable-403 spec), **`auth-secret-drift.spec.ts`** (the D7 mandatory mismatch/partial-write spec), `google-signin.spec.ts` (local mock OIDC, **never real Google in CI**), `sign-out.spec.ts`, `config-round-trip.spec.ts`, `auth-settings-round-trip.spec.ts`, `authorization-denial.spec.ts`, `jwks.spec.ts`, `i18n-message-key.spec.ts` — against a running console + a real test-fixture Moira instance, with a **recording proxy** in front of Moira so `client_secret_never_appears_in_any_request_to_moira` can inspect every outbound request.

### Accessibility
`console/tests/e2e/a11y.spec.ts` with `@axe-core/playwright` on **every page-level route** (list in Detailed Implementation). Zero critical/serious violations gates CI.

### Secret-leak
`console/tests/secret-leak/bundle-scan.test.ts` (build output + SSR HTML, asserting an **empty** violation set) and `console/tests/e2e/secret-leak.spec.ts` (browser-observed responses, rendered HTML, and console output, **plus the outbound-to-Moira request tap**). Together these prove no Moira system key, admin key, OAuth client secret, console encryption key, JWT private key, or decrypted provider credential reaches **any client bundle, HTML payload, browser-visible response, or request sent to Moira** (the last being D7's defining property).

### Production-config tests
A CI job builds the console with `NODE_ENV=production` and a minimal-but-complete fixture, boots it, and asserts: security headers present; `/setup` reachable without a session; `/(console)/**` redirects to `/login` without a session; the JWKS endpoint returns valid JWKS JSON with **no private-key material**; the registered `expected_audiences` is non-empty on both sides; `middleware.ts` rejects an unlisted `Host`.

### Helm / Kubernetes validation
`helm lint charts/moira-console` and `helm template charts/moira-console | kubeconform` (mirroring the existing `charts/moira` gate in `.github/workflows/ci.yml`), plus a rendered-manifest assertion that no secret value appears in a `ConfigMap` (only in `Secret`) — **specifically including `CONSOLE_SECRET_ENCRYPTION_KEY` (D7), and asserting that no OAuth client secret is a chart value at all**, since it is entered in the wizard and stored in the console DB — and that `readOnlyRootFilesystem: true`, `runAsNonRoot: true`, and dropped capabilities match the `charts/moira` hardening baseline.

### Rust gates (regression check only — this plan makes no Rust changes)
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release --locked
```
Plus clean PostgreSQL migration validation (unaffected — this plan adds no Moira migrations).

---

## Definition of Done

**CONVENTIONS §8 compliance checklist**
- [ ] Work performed on branch `plan/08-nextjs-console-google-oauth`; PR opened with all required description sections (§1.4).
- [ ] All §2 frontend gates pass: `bun install --frozen-lockfile`, `bun run lint`, `bun run typecheck`, `bun test`, `bunx playwright test`, `bun run build`. Rust gates green as a regression check.
- [ ] **Unit tests** delivered and passing — including one test per **atom** and per **molecule**, and at least one per organism.
- [ ] **E2E tests** delivered and passing — Playwright, covering setup wizard, sign-in via **local mock OIDC**, sign-out, a config round-trip, and an **authorization-denial** path.
- [ ] Every console-originated string has an i18n **key + English default** in `catalog.en.ts`; every Moira key the console renders exists in `docs/i18n-response-catalog.json`; `i18n-catalog-coverage.test.ts` and `no-hardcoded-copy.test.tsx` pass.
- [ ] **Next.js 16.2.11 · Node 24 LTS · Bun 1.3.14** pinned exactly; `bun.lock` committed; `.nvmrc`/`engines` present; **Atomic Design layering respected with the one-way dependency rule**, proven by `layer-dependencies.test.ts`.
- [ ] Auth config is **runtime/DB-backed** (D-1 live, env fallback **removed**): **non-secret config in Moira, the OAuth client secret console-owned and encrypted at rest (D7)**, composed behind `loadAuthSettings()`; **no `scope` claim in minted JWTs**; domain policy is **deny-by-default**.
- [ ] **No secret-leak**, verified by `bundle-scan.test.ts` (empty violation set) and `secret-leak.spec.ts` — including `client_secret_never_appears_in_any_request_to_moira`.

**D7 conformance — console-owned client secret, no residual read-back assumption**
- [ ] **The blocking Wave 0 custody item is closed by D7 and no longer appears as an open question anywhere in this plan.** No text offers the three former candidate resolutions, and **no Moira read-back endpoint is proposed, called, or planned** — that option was rejected.
- [ ] **Storage.** The client secret lives in `console_auth.authProviderSecret`, declared to Better Auth and emitted by `@better-auth/cli generate`/`migrate` into `console/db/`, **never** in Moira's `migrations/` (verified by `auth_provider_secret_table_is_not_in_moira_migrations` and by `git diff --stat`). AES-256-GCM at rest under the **dedicated** `CONSOLE_SECRET_ENCRYPTION_KEY` (not `BETTER_AUTH_SECRET`), with AAD binding and `encryptionKeyVersion`; the console **fails closed at boot** without a valid key. No plaintext, masked, or preview column exists.
- [ ] **Never leaves the console.** The secret is never sent to Moira, never exposed to the browser, never in a `NEXT_PUBLIC_*` variable, never in a client bundle or RSC payload, never a component prop, never logged. `client_secret_never_appears_in_any_request_to_moira`, `client_secret_never_appears_in_a_browser_visible_response`, `no-secret-props.test.ts`, and `server-only-guards.test.ts` all pass.
- [ ] **`loadAuthSettings()` composes both stores behind one interface** — non-secret config from Moira + secret from the console DB — with a cache key spanning both, so a secret rotation that never touches Moira still takes effect. `compose_merges_moira_config_with_console_secret` and `no_code_path_requests_a_secret_from_moira` pass.
- [ ] **Drift protection (a) — same-step dual write.** The wizard and `/settings/auth` write Moira's config and the console's secret in one step, ordered Moira → console secret → `enable` (the commit point). Every partial state is an **operator-resolvable failure** with a keyed remedy and **no step advance**; `partial_write_leaves_provider_disabled_and_offers_retry_or_discard` passes.
- [ ] **Drift protection (b) — `client_id` fingerprint.** A keyed fingerprint of the `client_id` is stored beside the secret and compared against Moira's `client_id` on **every** load; a mismatch produces the specific, actionable `console.error.auth_provider_client_id_mismatch`, excludes the provider, and **prevents the OAuth exchange from being attempted at all**. Missing / mismatched / undecryptable remain **three distinct** conditions, never collapsed.
- [ ] **Drift protection (c) — tests.** `console/tests/e2e/auth-secret-drift.spec.ts` passes with all six named tests, and `console/tests/unit/lib/provider-secrets.test.ts` passes with the fingerprint-comparison suite.
- [ ] **Rotation is a console concern.** An operator rotates the client secret entirely through `/settings/auth` → `ProviderSecretRotatePanel`, with **zero Moira calls** for a secret-only rotation and no redeploy. **No text, type, client method, path constant, or doc anywhere under `console/` references `POST /api/v1/admin/auth/providers/{id}/rotate-secret`** — `no_source_file_references_rotate_secret` and `there_is_no_rotate_secret_method` pass.
- [ ] **Frozen contract is 7 auth-provider operations (10 including the three setup operations), carrying no secret material.** `auth_provider_record_type_has_no_secret_field` passes; Wave 0's verification that Moira's spec has no `rotate-secret`, no envelope columns, and no `auth_provider_secret_rebind_required` key is recorded in the PR.
- [ ] Every D7 i18n key in the table above exists in `catalog.en.ts` with a non-empty English default and is covered by `i18n-catalog-coverage.test.ts`.

**Plan-07 frozen-contract conformance (D3/D4/D5) — no residual mismatch**
- [ ] **D5 — required email.** `ClaimAdminIdentityRequest.email: string` and `email_verified: boolean` are **non-optional** in `console/lib/types.ts`; `AdminIdentityRecord.email` is `string`. The wizard sends **both** on **every** claim including the system-key path; **no optional-email path or credential-type branch exists anywhere in the plan or the code**; `claim_request_always_sends_email_and_email_verified` and `claim_request_has_no_optional_email_branch` pass. If a generated client has replaced the hand-written types, it was **regenerated** against the post-D5 schema (not hand-patched).
- [ ] **D3 — ordering + actionable error.** The wizard blocks the claim step until an auth provider is **saved, enabled, and carries a non-empty `allowed_email_domains`**; `moira.error.admin_claim_domain_not_allowed` renders as an **actionable setup instruction** routing back to `AuthSettingsStep`, never as a generic failure. `setup-wizard-ordering.spec.ts` passes, including `premature_claim_returns_actionable_domain_not_allowed`, `disabled_provider_row_still_denies_the_claim`, and `no_bootstrap_bypass_on_the_system_key_path`. **No first-claim exemption or bootstrap bypass is reintroduced client-side.**
- [ ] **D4 — auth-methods is authenticated and server-side only.** `GET /api/v1/admin/setup/auth-methods` is called **only** from the BFF with the system key; no browser fetch, no client component reference, no console route proxying it; `no-client-side-auth-methods.test.ts` passes and the `setup-wizard.spec.ts` network tap records **zero** browser requests to that path. **`GET /api/v1/admin/setup/claim-status` is verified to be the only anonymous Moira call** (`claim_status_is_the_only_anonymous_call`).

**Plan-specific**
- [ ] Fresh-instance E2E: the `/setup` wizard writes **and enables** auth settings into Moira (with a non-empty `allowed_email_domains`) **before** claiming the first admin via plan 07's `POST /api/v1/admin/setup/claim` (issuer self-registration first), and `claim-status` flips `false`→`true`.
- [ ] A settings change made in `/settings/auth` takes effect **without a redeploy** (`auth-settings-round-trip.spec.ts`).
- [ ] The console's registered `jwks_url` is the URL the Better Auth `jwt` plugin actually serves, and the published document contains public key material only (`jwks.spec.ts`).
- [ ] `getSubject` binds to the **IdP subject**, not the Better Auth `user.id` (`moira-token.test.ts`).
- [ ] All MVP admin screens perform at least one create/read/update round-trip against real Moira admin endpoints in e2e, with `Idempotency-Key` and `If-Match` correctly sent.
- [ ] Accessibility gate clean on **every** page route.
- [ ] `helm lint` + `kubeconform` clean for `charts/moira-console`.
- [ ] `docs/admin-console.md` documents the BFF↔Moira contract, Mode A vs Mode B, deny-by-default domain policy, and states plainly that **SAML SSO is not supported** (mode 3 is the path).
- [ ] No Moira Rust source under `src/` or file under `migrations/` is modified by this plan (verified by `git diff --stat`: the PR touches only `console/`, `charts/moira-console/`, `docs/admin-console.md`, and CI workflow files).

---

## Risks & Rollback

### Security
- **Token custody.** The highest-risk assets are the `jwt` plugin's private key (console DB, AES-256-GCM-encrypted at rest by Better Auth default — `disablePrivateKeyEncryption` deliberately unset) and the bootstrap system key (K8s Secret). The `server-only` import guard makes leaking either into a client bundle a **build failure**, not a review miss; the bundle scan is a CI gate.
- **The no-scope-claim invariant** is the single load-bearing control that keeps Moira the authorization system of record. Risk: a future contributor "helpfully" adds `scope` to `definePayload`. Mitigations: a dedicated unit test, an e2e denial test, and an explicit per-wave reviewer check item.
- **`getSubject` regression.** Reverting to the Better Auth default subject would silently rebind Moira grants to a console-DB-local surrogate id. Mitigated by `moira-token.test.ts` and called out in `docs/admin-console.md`.
- **New secret class: the OAuth client secret is console-owned (D7).** This is a settled product-owner decision, not an open question. Better Auth needs the plaintext secret in process, and exposing it from Moira would have broken Moira's load-bearing invariant that *a decrypted secret never crosses a network boundary*; the read-back endpoint option was **considered and rejected**. Residual risk is that the console DB becomes a higher-value target. Mitigations: AES-256-GCM at rest under a **dedicated** key with AAD binding (a database-only compromise yields nothing); a dedicated DB role with no grants on Moira's tables; TLS to Postgres; the secret held only in process memory and never on disk in plaintext; `server-only` guards making a client-bundle leak a build failure; the bundle scan and the outbound-request tap as CI gates. Accepted, documented, re-checked in review.
- **Two configuration stores can drift (D7's accepted cost).** A `client_id` changed in Moira while the console still holds the old client's secret would otherwise fail the code exchange with an opaque `invalid_client` from the IdP — the single worst diagnosability outcome of this decision. This is why the three drift protections are **mandatory**: same-step dual write with `enable` as the commit point, a keyed `client_id` fingerprint compared on every load, and the `auth-secret-drift.spec.ts` e2e assertion that the mismatch path yields an actionable console error and **never** reaches the provider's token endpoint. The likeliest regression is a contributor "simplifying" `loadAuthSettings()` by dropping the fingerprint check as redundant; mitigated by the named unit tests, the e2e spec, and reviewer check item (j).
- **Reintroducing a Moira read-back path (D7 regression).** A contributor may reason that "one config store is cleaner" and propose a system-key-only, cluster-internal secret-read endpoint. That option was **explicitly rejected** by D7 because it breaks the never-cross-a-network-boundary invariant that everything else in Moira's credential handling rests on. Mitigations: `no_code_path_requests_a_secret_from_moira`, `auth_provider_record_type_has_no_secret_field`, `no_source_file_references_rotate_secret`, and a named per-wave reviewer check item (i).
- **Reintroducing a first-claim exemption (D3 regression).** The most likely well-intentioned regression in this plan is a contributor "fixing" the fresh-deployment 403 by adding a bootstrap bypass or an empty-list-means-allow fallback in `domain-policy.ts`. 07 deliberately **removed** that carve-out. Mitigations: `no_bootstrap_bypass_on_the_system_key_path` in `setup-wizard-ordering.spec.ts`, the deny-by-default unit tests in `domain-policy.test.ts`, and a named per-wave reviewer check item.
- **Reintroducing an anonymous auth-methods path (D4 regression).** Equally likely: a contributor adding a console API route that proxies `auth-methods` to the browser "to simplify the wizard", recreating exactly the anonymous reconnaissance surface Moira declines to offer. Mitigated by `no-client-side-auth-methods.test.ts` and the `setup-wizard.spec.ts` network tap.
- **Console database is a new stateful component** (previously "none"). It contains no Moira system key, admin key, or AI-provider credential, but it does contain session material, the `jwt` plugin's private key, **and — per D7 — the encrypted OAuth client secret**. Mitigations: dedicated schema and DB role with no grants on Moira's tables; TLS to Postgres; encryption at rest under a key held outside the database; backup/restore documented **including the fact that restoring the console DB without the matching `CONSOLE_SECRET_ENCRYPTION_KEY` renders stored secrets undecryptable — surfaced as the keyed `auth_provider_secret_undecryptable` state, remedied by re-entering the secret**; a restore that loses `jwks` is recoverable by re-registering the issuer's JWKS (Moira's `refresh-jwks` endpoint) — and, because subjects are IdP-derived rather than console-DB-derived, **admin grants survive a console DB rebuild**. A restore that loses `authProviderSecret` costs one secret re-entry per provider and **never** costs an admin grant.
- **Redirect / open-redirect.** Exact redirect-URI registration at each IdP, `middleware.ts` host allow-list, `trustedOrigins`. Residual risk: a misconfigured `ALLOWED_CONSOLE_HOSTS` — mitigated by the production-config test asserting an unlisted host is rejected in CI before every release.
- **Land-grab / first-login race** (the failure mode `plans/01` §4.4 calls out): closed by design — the claim endpoint is Moira-side, system-key-gated, and idempotent per `(issuer, subject)`; a second person reaching `/setup` after a successful claim sees "already claimed" and cannot self-grant. `authorization-denial.spec.ts` proves it end-to-end.
- **Full admin for the single claimed identity** is an accepted MVP limitation (matches plan 01's Mode A), not a defect; multi-admin/scoped roles are 09+.

### Compatibility
Fully additive. Moira's machine-auth surfaces are untouched. Undeploying the console has zero effect on Moira's operability — system-key/CLI administration and **mode 3** (bring-your-own JWKS) continue to work exactly as before the console existed.

### Deployment
- **Risk:** deploying the console before plan 07 (or D-1) ships leaves `/setup` broken. Mitigation: the Wave 0 checkpoint blocks on both; the console's health check fails closed with a clear operator-facing keyed error if the expected endpoints 404, rather than serving a blank page.
- **Risk:** the D-1 interim env fallback ships by accident, leaving auth config build-time and §7.2-non-compliant. Mitigation: an explicit Definition-of-Done checkbox requires the env path to be **removed**, plus `auth-settings-round-trip.spec.ts` proves the Moira-backed path is live.
- **Risk:** algorithm mismatch — the `jwt` plugin's `EdDSA` default versus Moira's registered `allowed_algorithms`. Mitigation: the explicit `ES256` pin, the Wave 0 verification, and a unit test asserting the pin matches what the issuer row registers.
- **Risk:** OAuth client misconfiguration blocks all sign-in. Mitigation: `/login` renders a keyed "auth not configured" state (`console.error.auth_settings_unavailable`) rather than a stack trace, and the wizard is reachable to fix it.
- **Risk (D7):** `CONSOLE_SECRET_ENCRYPTION_KEY` is missing, rotated without re-encryption, or lost with a console-DB restore, making every stored client secret undecryptable and every sign-in fail. Mitigations: boot fails closed with `console.error.secret_encryption_key_missing` rather than starting broken; `encryptionKeyVersion` supports rotation with prior versions still decryptable plus a `rotate-encryption-key` script; the undecryptable state is its **own** keyed error pointing at key provisioning, not at the IdP; recovery is bounded — re-enter one secret per provider, with **no** effect on admin grants, sessions, or Moira state. The rendered-manifest assertion confirms the key is sourced from a `Secret`, never a `ConfigMap`.
- **Risk (D7):** the two stores drift after an out-of-band edit against Moira's API or an uneven backup restore. Mitigation: the mandatory fingerprint check catches it on the next load with an actionable error before any OAuth attempt; `/settings/auth` shows per-provider completeness so drift is visible rather than latent; orphaned console secrets are listed with a delete control.

### Rollback
Undeploy the `charts/moira-console` release; no Moira-side cleanup is required. The console's own schema can be dropped independently. If plan 07's grant itself must be reverted (wrong admin claimed), that is a plan-07-owned break-glass procedure (system-key re-grant / revoke), not this plan's concern — Moira remains the system of record.

### Deferred follow-ups (explicitly punted, not silently dropped)
- Generic-OIDC hardening beyond the `genericOAuth` baseline, multi-provider management UI, GitHub provider — **09**.
- Invitations / additional-admin self-service, ownership transfer, recovery beyond system-key break-glass — **09**.
- **Enterprise SAML SSO — permanently out of scope for the console.** Better Auth does not provide it; mode 3 (bring-your-own JWT/JWKS behind the customer's own IdP or SSO gateway) is the supported path. This is recorded as a limitation, not a roadmap item.
- Generated Moira client types from the committed OpenAPI spec, once P1-10's gate exists (hand-written types until then).
- Working audit-log cursor pagination, once P1-4 lands in plan 04.
- Additional locales beyond `en` — the i18n layer is structured for it; no catalog beyond English ships in MVP.
- Console-side rate limiting beyond Better Auth's built-in `rateLimit` on the setup/claim flow — low risk given the flow is system-key-gated, flagged for a future hardening pass.
