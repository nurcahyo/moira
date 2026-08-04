# Testing Moira locally

What you can exercise on a laptop, in what order, and what each failure means.

The API is testable end to end today: a real prompt goes through routing, a real
provider answers, and the tokens are accounted. **The console is not, in a
browser** — the provisioning write path has landed as `POST /api/setup`, but no
page drives it yet. [The console](#the-console) says exactly what is there and
what is not.

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
application and a consumer key, and reuses anything that already matches.

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
and `/login` renders. **Signing in still does not work from the browser**, but the
gap is narrower than it was: the write path exists, the screen that drives it does
not.

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

What is still missing:

1. **No setup wizard page.** `console/app/` contains `(console)`, `api`, `invite`,
   `layout.tsx` and `login` — there is no `setup` route segment. Nothing in the UI
   calls `/api/setup`, so a trusted issuer, an auth provider and the first admin
   have to be created by hand against that endpoint.
2. **Moira refuses any non-`https` auth-provider URL**, with no escape hatch —
   unlike provider URLs, which have two. See `validate_https_url` in
   `src/application/auth_settings.rs`; it rejects with
   `auth_provider_url_not_allowed`.

So the login page is honest when it says *"No sign-in provider is enabled yet."*
On a fresh database `auth_provider_settings` and `trusted_jwt_issuers` are both
empty, and no screen can fill them.

What already works: the console reaches Moira (`MOIRA_SYSTEM_KEY` is its bootstrap
credential — `console/lib/auth-runtime.ts` returns a keyed refusal without ever
contacting Moira when it is unset), its own database is migrated, and
`/api/health` answers.

A mock sign-in is possible without any Google credential:
`console/tests/support/mock-idp.ts` is a real TLS OIDC server whose `/authorize`
auto-redirects with no consent screen. Wiring it up means driving `POST /api/setup`
yourself, because no page does.

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
