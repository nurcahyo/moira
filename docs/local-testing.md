# Testing Moira locally

What you can exercise on a laptop, in what order, and what each failure means.

The API is testable end to end today: a real prompt goes through routing, a real
provider answers, and the tokens are accounted. The console has caught up some of
the way — `POST /api/setup` has a page driving it now, `/setup` — but it still
needs an IdP reachable over TLS, and it still serves **one** sign-in provider.
[The console](#the-console) says exactly what is there and what is not, and
[Two sign-in providers, locally](#two-sign-in-providers-locally) says which parts
of the multi-provider work you can exercise on a laptop today and which you
cannot.

## Cold start

```bash
make setup          # env files, console deps, containers, schema, system key
make start          # containers, migrations, API in the foreground
```

Then, in a second shell:

```bash
make seed           # a provider to route to — see below
make smoke          # health, contract, and a real completion
```

`make smoke` prints one line per check and `SMOKE PASSED` only if every one held.

### `make setup` in detail

| step | what it does | why it can't be skipped |
| --- | --- | --- |
| `env` | writes `.env` and `console/.env.local` | `Settings::load` reads `config/default.toml`, `config/local.toml` and the environment — **never** a `.env` file. Nothing in the dependency tree does dotenv loading. |
| `console-install` | `bun install` | `console-db` and `console-dev` both fail on module resolution without it |
| `up` | Postgres (pgvector) + Redis, waiting for healthy | |
| `migrate` | applies the SQL migrations | |
| `bootstrap-key` | mints a root system key into **both** env files | the plaintext is printed once; only its Argon2 hash is stored. Rows already in `system_api_keys` cannot be recovered, so "a key exists" is not a reason to skip this. |

Every `make` recipe sources `.env` first. **A bare `cargo run` does not** — it
starts on `config/default.toml` defaults: no database, `allow_insecure_dev_key`,
`allow_insecure_dev_pepper`. That process looks healthy and is not the one you
configured.

### The port

`config/default.toml` says 8080, but on a developer machine 8080 is routinely
some other project's dev server, and a port that *answers* is worse than one that
is closed: `make health` gets a 404 from a stranger's app and reports this service
down. So `scripts/dev-env.sh` picks the first free port from 8080, 8100, 8101,
8102, 8103 and writes the same value into `MOIRA_SERVER__PORT` and the console's
`MOIRA_API_URL`. Override with `MOIRA_PORT=… make env`.

An existing `.env`'s port always wins, including under `make env-force` — a scan
run while the service is up finds its own port occupied and would otherwise
relocate a running deployment out from under every client that knows where it is.

`make health`, `make openapi`, `make docs` and `make help` all read the port back
from `.env`, so they follow it. README's `127.0.0.1:8080` examples do not.

## `make seed` — a migrated database is not a runnable one

Migration `0005` seeds the `general` route and nothing else. Four more rows have
to exist before a prompt can execute:

```
provider ──┬── provider_model ──┐
           └── provider_credential   ├── routing_policy ── route (general)
```

`routing_policy` has three NOT NULL foreign keys — `route_id`, `provider_id`,
`provider_model_id` — which fixes the order. `make seed` creates all of it, plus an
application and a consumer key. Re-running reuses a row only when it already
matches what you asked for this run; a provider's `base_url` or a routing
policy's `provider_model_id` that has drifted from `MOIRA_SEED_BASE_URL` /
`MOIRA_SEED_MODEL` is PATCHed back into agreement, not silently left alone.

By default it targets `http://127.0.0.1:8000/v1` and asks that endpoint which
models it serves. Point it elsewhere:

```bash
MOIRA_SEED_BASE_URL=http://192.168.1.13:8000/v1 make seed
MOIRA_SEED_MODEL=Qwen/Qwen3-4B                  make seed   # skip discovery
```

A private-network or plain-`http://` base URL is accepted only because `.env` sets
`MOIRA_PROVIDER_SECURITY__ALLOW_PRIVATE_PROVIDER_URLS` and `…ALLOW_HTTP_PROVIDER_URLS`.
Production validation refuses both outright.

> **Values with spaces must be quoted in `.env`.** The file is sourced by the
> shell, so `MOIRA_SEED_NAME=Local vLLM` runs `vLLM` as a command with
> `MOIRA_SEED_NAME=Local` in its environment — a *temporary* assignment that never
> persists. The variable stays unset, the default silently wins, and you get a
> duplicate provider. Write `MOIRA_SEED_NAME="Local vLLM"`.

## Executing a prompt

```bash
set -a; . ./.env; set +a

curl -s -X POST "http://127.0.0.1:$MOIRA_SERVER__PORT/api/v1/responses" \
  -H 'Content-Type: application/json' \
  -H "X-Consumer-Key: $MOIRA_CONSUMER_KEY" \
  -d '{"input":[{"role":"user","content":[{"type":"input_text","text":"Say OK"}]}],
       "max_output_tokens":64,"temperature":0}'
```

`X-Moira-System-Key` works too. `/api/v1/responses/stream` is the same body over
SSE, emitting `response.created`, `response.routing.started`, output deltas and a
terminal event, each with a monotonic `sequence`.

### The four failures you will hit first

| response | cause | fix |
| --- | --- | --- |
| `403 forbidden` — *missing required scope `moira:responses:create`* | `MOIRA_AUTH__CALLER__ENABLED=false` yields an **anonymous** actor with **zero** scopes. Disabling caller auth does not open the public plane. | send `X-Consumer-Key` or `X-Moira-System-Key` |
| `403 route_override_forbidden` | sending `"route"` in the body is an *override*, and the default execution policy sets `route_overrides_allowed = false` | omit `route` — and `model`, `provider`, `credential_id`, `timeout_ms`. Default routing prefers `route_key = 'general'`. |
| `404 credential_not_found` — *no eligible provider credential* | a `provider_credentials` row is **mandatory**, even for an endpoint that needs no key | `make seed` creates one holding an unused placeholder string |
| `404 not_found` on `POST /v1/responses` | the OpenAI-compatible entry point is off by default | use `/api/v1/responses`, or set `MOIRA_PUBLIC_API__OPENAI_RESPONSES_COMPAT_ENABLED=true` |

Two more that cost time:

- **`provider_type` is `open_ai_compatible` on the wire** — with an underscore
  between `open` and `ai`. The SQL CHECK constraint spells it `openai_compatible`;
  copying *that* into a request body fails deserialization. Same trap for
  `deep_seek` and `azure_open_ai`.
- **Omitting `capabilities` when creating a provider model** stores jsonb `null`,
  not "no constraints". Any request asking for a named capability then fails
  `no_eligible_model`, which says nothing about the row being under-specified.
  `make seed` sends an explicit object.

### Is it actually configured?

```bash
curl -s -H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" \
  "http://127.0.0.1:$MOIRA_SERVER__PORT/api/v1/admin/setup/status"
```

Moira diagnoses itself here, and it is stricter than a green health probe:
`"status": "ready"` means all of database, root system key, application, route,
provider, model, credential, routing policy and executable path are present.
Anything missing is named in `missing`.

`make smoke` asserts on this.

## What this configuration leaves open

`MOIRA_AUTH__ADMIN__ENABLED=false` does not merely relax admin auth — it
**removes** it. Every `/api/v1/admin/*` CRUD route answers a request carrying no
credential at all, as a synthesised `DevAdmin` actor holding `moira:admin`:

```
GET /api/v1/admin/providers      no header -> 200
GET /api/v1/admin/setup/status   no header -> 403   # one of the exceptions
```

The CRUD routes are the rule; the setup and identity routes are not. Four of them
do their own gating and a header-less `DevAdmin` gets nowhere:

| route | admits |
| --- | --- |
| `GET /api/v1/admin/setup/status` | system key or trusted JWT (`require_setup_actor`) |
| `GET /api/v1/admin/setup/auth-methods` | the same — it calls the same function |
| `POST /api/v1/admin/setup/claim` | a system key and nothing else |
| `POST /api/v1/admin/admin-invites/redeem` | a bearer JWT only (`verify_trusted_jwt_identity`) |

`require_setup_actor` (`src/application/setup.rs`) returns `Forbidden` for
`DevAdmin`, `ConsumerKey` and `Anonymous` alike, so "a real system key" is one of
two answers — a trusted JWT also passes. `GET /api/v1/admin/setup/claim-status`
goes the other way and is unauthenticated by design.

The listener is bound to `127.0.0.1`, so this is
reachable only from the machine itself — but do not bind it wider while admin auth
is off, and do not carry this `.env` anywhere but a laptop.

Startup names the whole set:

```
WARN moira: unsafe development configuration is active
  features=["admin_auth_disabled", "insecure_jwks_urls", "http_provider_urls", "automatic_migrations"]
```

## Off by default

| endpoint | setting |
| --- | --- |
| `POST /v1/responses` (OpenAI-compatible) | `public_api.openai_responses_compat_enabled = false` |
| `POST /api/v1/admin/runtime/diagnose` | `runtime.diagnostic_endpoint_enabled = false` — returns 404 with a *valid* body; an invalid one 422s first, which reads as "enabled but wrong" |
| admin paths in `GET /openapi.json` | `MOIRA_DOCS__EXPOSE_ADMIN=false` — the full contract is 100 paths / 152 operations (both pinned by `src/http/mod.rs`'s router test); 23 of those paths are not under `/api/v1/admin/`, and they are what the public document keeps |

## Rotating keys

`make env-force` rewrites the layout of both env files and **keeps** the master
key, both peppers and the console secrets. That is deliberate:

- `MOIRA_SECRETS__MASTER_KEY_BASE64` seals every `provider_credentials` row
- `MOIRA_API_KEYS__PEPPER_BASE64` peppers every live API key hash
- `BETTER_AUTH_SECRET` encrypts the console's stored ES256 signing key

Minting new ones does not fail and does not warn. The old rows simply stop being
readable, at use time, one endpoint at a time. `make env-rotate` does rotate them,
and asks first.

## The console

`make console-dev` serves it on <http://localhost:3000>. `/` redirects to `/login`,
and `/login` renders. **Signing in has never been proven against a real Moira, in
a browser, on this console.** That gap is narrower than it used to be — the write
path and the wizard screen that drives it both exist now — but "exists" and
"proven" are different claims, and this section keeps them separate.

**Proven, first-hand, today:** Moira's own API path, end to end —
`make setup` / `make start` / `make seed` / `make smoke` / `make execute-test`,
the last of which produced a real completion. Separately, an operator signed in
through a browser against a local OIDC provider and got a Moira-routed answer —
but that was the **commerce-os platform console**, not this one. How that is
wired, and exactly what was observed, is documented in that repo, not copied
here: see
[commerce-os's `DEV-GUIDE.md`](https://github.com/motrait/commerce-os/blob/develop/DEV-GUIDE.md).

**Not proven, here:** this repo's own console has never been driven against a
real Moira end to end. Its e2e suite (`console/e2e/setup-wizard.e2e.ts`) runs the
wizard against a **stub** Moira on loopback TLS (`console/e2e/support/moira-setup-stub.ts`)
and stops on purpose at the `sign_in` step — one test asserts *positively* that
`claim` is not reached, so the gap stays visible instead of silently closing
itself once someone assumes it's covered. Reaching `claim`, and any real sign-in,
needs a completed OAuth round trip through a mock IdP inside the e2e environment;
that harness does not exist yet (issue #72). It also needs the IdP reachable over
**https** — see the missing item below.

What has landed — `console/app/api/setup/route.ts`, the single door setup writes
through:

- `GET /api/setup` returns a display-safe view of the deployment's auth methods.
- `POST /api/setup` takes one of two actions, `provision` and `claim`. It is the
  production caller of `runSetupProvisioning`, `claimAdminIdentity` and
  `consoleIssuerConfigFor` in `console/lib/setup-flow.ts`, and the first
  production caller of `ConsoleSecretStore.put()` — so the OAuth client secret
  now has somewhere to go. Without a `console_provider_secret` row, every enabled
  provider still resolves to `console_secret_unavailable` and renders no sign-in
  button.
- It runs without a console session on purpose — setup precedes the first admin —
  and is gated by `withSetupWindow` in `console/lib/setup-window.ts` instead: no
  bootstrap system key is a 404, and a deployment Moira already reports as claimed
  is a 409.

What has landed since — `console/app/setup/` and `console/modules/setup/`: `/setup`
is a five-step wizard (welcome → auth_settings → sign_in → claim → done) and it
is the UI caller of `POST /api/setup`. A trusted issuer, an auth provider and the
first admin no longer have to be created by hand — by the shipped code path, not
yet by anyone who has actually walked it in a browser against a real Moira (see
above). The page calls its own route handler **in process**, so Moira's raw
auth-methods response never reaches the browser, and it answers < 400 in every
window state — the window being closed is a configuration fact, not an error.

What is still missing:

1. **Moira refuses any non-`https` auth-provider URL**, with no escape hatch —
   unlike provider URLs, which have two. See `validate_https_url` in
   `src/application/auth_settings.rs`; it rejects with
   `auth_provider_url_not_allowed`. So the IdP you point the wizard at has to be
   reachable over TLS from the console process.

So the login page is honest when it says *"No sign-in provider is enabled yet."*
On a fresh database `auth_provider_settings` and `trusted_jwt_issuers` are both
empty, and no screen can fill them.

What already works: the console reaches Moira (`MOIRA_SYSTEM_KEY` is its bootstrap
credential — `console/lib/auth-runtime.ts` returns a keyed refusal without ever
contacting Moira when it is unset), its own database is migrated, and
`/api/health` answers.

A mock sign-in is possible without any Google credential:
`console/tests/support/mock-idp.ts` is a real TLS OIDC server whose `/authorize`
auto-redirects with no consent screen — real discovery and JWKS documents, real
ES256 ID tokens, and a token endpoint that checks `client_id`, `client_secret`,
`redirect_uri` and the PKCE `code_verifier`, so it refuses a wrong secret with
`401 invalid_client` exactly as Google would.

### Two sign-in providers, locally

The console refuses to resolve sign-in when more than one auth provider is
enabled — `ambiguous_enabled_providers`, in `console/lib/auth-config.ts`. That
refusal is deliberate and still standing; it comes down only after Stage 4A is
deployed. See [console-multi-provider-rollout.md](console-multi-provider-rollout.md).

So the honest answer has two halves: **the multi-provider machinery does run on a
laptop, through the test harness. It does not run in a browser, and the thing
stopping it is the guard, not your setup.**

#### 1. Run the multi-provider suite — this is the real thing

```bash
make up                     # Postgres on 127.0.0.1:5432, if it is not already
cd console && bun install   # once
bun test tests/integration/multi-provider.test.ts
```

Roughly 30 seconds; expect `17 pass`, `0 fail`. No Google credential, no GitHub
OAuth app, no network.

It is not a mock of the resolution — it *is* the resolution. Two genuinely
different providers (`generic_oidc` against `mock-idp.ts`, `github_oauth` against
`mock-github.ts`), both over TLS, resolved by the shipped `resolveAuthConfigs`,
served by a real console bound to a real socket, over real PostgreSQL. It drives
both authorization-code flows to completion and mints a Moira-bound token from
each session. The assertions with teeth:

- the two `iss` values **differ**, and each equals its own trusted issuer's
  registered string. Under the defect this closes, both tokens carry
  `bffIssuerUrl`, and `admin_identities` — keyed `(issuer, subject)` — collapses
  two IdPs returning the same `sub` into **one** admin grant (finding F24);
- one human, two providers, **two** grants — asserted in SQL;
- a pre-4B account still resolves to the frozen `moira-console-idp` id and is not
  orphaned by the upgrade;
- GitHub's verified primary address wins over its attacker-settable public
  profile address, and an unverified one produces no session at all.

`ambiguityGuard` is bypassed here on purpose: the fixture resolves through
`resolveAuthConfigs` and deliberately not through `loadAuthConfigs`, which is the
only caller that applies the guard. The comment saying so is at the
`resolveFixture` helper in that file.

Two failure modes worth naming:

| you see | it means |
| --- | --- |
| a connection error naming the DSN, redacted | Postgres is not up. This suite does **not** skip silently — a missing or wrong URL is an error, because a vanishing database suite is how this repository has previously turned a gate green without running it. |
| `0 pass`, and `console-db-availability.test.ts` red | `CONSOLE_SKIP_DB_TESTS=1` is set in your environment. That escape hatch exists, and using it fails a test on purpose. |

The database is `console_auth_test` on the same local server, created for you if
absent, and separate from Moira's `moira` on purpose — two migration ledgers in
one database is the failure that separation prevents. Override with
`CONSOLE_TEST_DATABASE_URL`.

#### 2. Watch the guard refuse, in one second

```bash
cd console && bun test tests/unit/lib/auth-config.test.ts -t "ambiguityGuard"
```

`3 pass`, `46 filtered out`. The middle one is the whole state of play in a
single assertion: two providers, both of which **resolved successfully**, and the
guard refuses the resolution anyway.

#### 3. In a browser, you will hit the guard — here is how to tell

You are not doing anything wrong. Both halves of the console refuse a second
enabled provider, in two different places, with two different messages.

**Through `/setup`.** The wizard's `slug` field looks like the way to add a
second provider. It is not — it selects a console-issuer *namespace*, and a run
that would take the deployment-wide enabled count above one is refused before it
writes anything:

```
409  {"error":{"code":"setup_single_enabled_provider_only", …}}
```

That is `provisioningAdmissionFor` in `console/app/api/setup/route.ts`. It is a
limit, not a permission, so signing in first does not get you past it — the
previous design let the legitimate operator through and then locked them out of
their own wizard on the next reload.

**Through Moira's admin API**, bypassing the console (`POST /api/v1/admin/auth/providers`
plus `POST …/{id}/enable`, which Moira permits when each row is bound to its own
trusted issuer). Reload `/login`:

> More than one sign-in provider is enabled. The console will not guess which one
> governs — disable all but one in Moira.

**Zero buttons, for either provider.** Not one button, not a broken button.

Read that message as *"the guard is doing its job"*, and distinguish it from the
other refusals, which really do mean your setup is wrong:

| what `/login` says | what is actually wrong |
| --- | --- |
| *More than one sign-in provider is enabled…* | Nothing. This is `ambiguityGuard`. Disable one row and sign-in returns. |
| *No sign-in provider is enabled yet…* | The normal first-run state. Finish `/setup`. |
| *…client secret…* (`console_secret_unavailable`) | No `console_provider_secret` row for that provider, or the stored `client_id` has drifted from Moira's. Re-run provisioning. |
| *…machine trust method…* | The enabled row's `method` is `jwks`. Nobody can sign in through it. |

One more thing that surprises people: the guard fires on the **count**, after
resolution. Two enabled rows produce that message even if neither of them would
have resolved anyway — so it can mask a second, real configuration problem. Fix
the count first, then read the message again.

## Reference

| command | |
| --- | --- |
| `make doctor` | toolchain, containers, listening ports |
| `make health` | `/health/live` and `/health/ready` |
| `make openapi` | the OpenAPI 3.1 document |
| `make docs` | the Scalar reference in a browser |
| `make psql` | psql against the Moira database |
| `make logs` | container logs |
| `make gates` | the six merge gates |
| `make reset` | **destructive** — drops both data volumes |
