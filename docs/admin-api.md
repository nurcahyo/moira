# Moira Admin API

Phase 2 and Phase 3 admin APIs live under `/api/v1/admin`. Public non-admin routes are limited to `/health/live`, `/health/ready`, `/openapi.json`, and `/docs`.

Admin requests authenticate with exactly one of:

- `Authorization: Bearer <trusted-jwt>`
- `X-Moira-System-Key: <raw-system-key>`
- `X-Consumer-Key: <raw-consumer-key>`

Conflicting credentials are rejected. All state-changing versioned resources use `If-Match: "<version>"` and return `ETag: "<version>"`. Credential rotation requires `If-Match`; the expected version participates in the idempotency command hash and is checked inside the mutation transaction.

The ten core application, provider, provider-model, credential, API-key, and JWT-issuer create/rotate operation identities support atomic `Idempotency-Key` execution. Successful creates return and replay `201`; rotations return and replay `200`. A reused key with different command input returns `409 idempotency_conflict`, while a bounded wait for an active winner returns `409 idempotency_in_progress`. Deterministic business failures replay with their original sanitized error and a fresh request ID. Raw API-key secrets are available only to the winning request.

List endpoints use `limit` with default `50` and max `200`. Responses are shaped as `{ "data": [], "pagination": { "next_cursor": null, "has_more": false } }`.

## Endpoint Groups

- Setup readiness: `GET /api/v1/admin/setup/status`
- Applications: `/api/v1/admin/applications`
- Providers: `/api/v1/admin/providers`
- Provider models: `/api/v1/admin/providers/{provider_id}/models` and `/api/v1/admin/provider-models/{id}`
- Provider credentials: `/api/v1/admin/provider-credentials`
- User credential convenience: `/api/v1/admin/users/{external_user_id}/provider-credentials`
- Trusted JWT issuers: `/api/v1/admin/jwt-issuers`
- System keys: `/api/v1/admin/system-keys`
- Consumer keys: `/api/v1/admin/consumer-keys`
- Audit events: `/api/v1/admin/audit-events`
- Route definitions: `/api/v1/admin/routes`
- Routing policies: `/api/v1/admin/routing-policies`
- Agent profiles: `/api/v1/admin/agent-profiles`
- Provider runtime policies: `/api/v1/admin/providers/{provider_id}/runtime-policy`
- Application execution policies: `/api/v1/admin/applications/{id}/execution-policy`
- Application routing defaults (context router, issue #213): `/api/v1/admin/applications/{id}/routing-defaults` — `If-Match` optional (same posture as provider runtime policies), requiring `moira:routing-defaults:read` / `moira:routing-defaults:write`
- Runtime diagnostics: `/api/v1/admin/runtime/diagnose`, disabled by default and requiring `moira:runtime:diagnose`
- Containerised Claude runners (issue #275, workstream R2 of #272): `/api/v1/admin/runners` — see below

OpenAPI is served at `/openapi.json`; admin paths are exposed only when `MOIRA_DOCS__EXPOSE_ADMIN=true`.

## Containerised Claude runners (`/api/v1/admin/runners`)

Docker socket access is root-equivalent on its host, and neither the console nor the Moira API
process holds it. A separate service, `moira-runner`, is the only component that talks to the
Docker Engine API; it binds loopback by default and is never exposed to the internet. The trust
chain is:

```
Console (no Docker) -> Moira (no Docker) -> moira-runner -> Docker Engine API
```

Everything under `/api/v1/admin/runners` is Moira's half of that chain: server-to-server HTTP
calls into `moira-runner`, plus a `claude_runners` mirror table so a runner can be named, listed
and audited without leaking a Docker container id.

| Operation | Route | Scope |
|---|---|---|
| Provision | `POST /api/v1/admin/runners` | `moira:runners:write` |
| List | `GET /api/v1/admin/runners` | `moira:runners:read` |
| Get (refreshes) | `GET /api/v1/admin/runners/{id}` | `moira:runners:read` |
| Submit authorization code | `POST /api/v1/admin/runners/{id}/authorization-code` | `moira:runners:write` |
| Finalize | `POST /api/v1/admin/runners/{id}/finalize` | `moira:runners:write` **and** `moira:credentials:write` |
| Delete | `DELETE /api/v1/admin/runners/{id}` | `moira:runners:delete` |

Lifecycle: `provisioning → awaiting_authorization → exchanging → ready → linked`, with `failed`
and `expired` as the other terminal states. `linked` is Moira's own state and means the token was
stored as a provider credential; `moira-runner` never emits it, and Moira refuses to accept it
from the control plane.

**The minted token never leaves the Moira process.** Finalize reads it from the runner's one-shot
token endpoint and moves it straight into the existing credential chain — AAD build,
`SecretCipher` encryption, fingerprint, mask, audit row, idempotent command envelope — and stores
the resulting `credential_id` on the runner row. It is never returned in a response body, never
logged, never traced, never placed in an error. The console has no way to read it back. The
operator's pasted authorization code is treated the same way: forwarded to the runner service and
dropped, never stored and never audited.

Two consequences worth knowing before operating this:

- **`GET /api/v1/admin/runners/{id}` writes.** It refreshes the mirror from the runner service,
  which is what surfaces `authorization_url` at all, and that bumps `version`. The state
  transitions therefore carry **no `If-Match`** — they are guarded by a from-state check inside
  the same transaction as the write, which is a stronger precondition. `DELETE` does require
  `If-Match`.
- **A failed finalize is unrecoverable for that runner.** The token endpoint is one-shot. If the
  credential write fails after the token has been read, Moira stores nothing — no credential row,
  no state change — and the second attempt answers `409 runner_token_unavailable`. Delete the
  runner and provision a new one.

Configuration lives in `[claude_runner]` (`config/default.toml`): `enabled` (off by default),
`base_url`, `auth_token` (supply via `MOIRA_CLAUDE_RUNNER__AUTH_TOKEN`, never a committed file),
and `request_timeout_ms`. While `enabled` is false the whole surface's write side answers
`503 runner_service_disabled` before any network call. Production start-up refuses an enabled
runner with no token, and refuses a non-loopback `http://` base URL.

### What is measured, and what is not

State this before relying on the flow, because the gap is easy to read past:

- **Measured.** Container start with a daemon-allocated tty, the authorization URL scraped from the
  tty stream, and code submission over the Engine API attach endpoint — the last reproduced twice
  against the real hardened runner image. Every Moira-side behaviour on this page is covered by
  `tests/claude_runners.rs` against a fake runner service.
- **Not measured, anywhere.** Capturing the minted token from a *genuinely valid* authorization
  code. The invalid-code path exercises the whole exchange and fails at the far end, so the
  remaining risk is small — but "small" is not "proven", and no test in this repository or the
  runner's can close it, because doing so needs a real Claude account completing a real login.
  Treat the first production finalize as the experiment that settles it.

Honest limit, repeated from issue #272: **N containers on ONE Claude account is not N× capacity.**
Rate limits attach to the account. Multi-instance is legitimate only when each instance is a
distinct account or seat you actually hold.

**What finalize stores is a subscription credential, not an API key.** Anthropic's terms treat
subscription OAuth authentication as being for ordinary individual use of Claude Code and the
other native Claude apps, and direct developers building products or services to API-key
authentication. These endpoints drive the **official** `claude` CLI inside a container, which
changes the mechanism, not the purpose — a subscription is never a safety net for anyone but its
owner. Note in particular that a runner provisioned at the default `global` scope produces a
credential that credential resolution will hand to any tenant with none of its own. Read
[`claude-subscription-boundary.md`](claude-subscription-boundary.md); the refusal that closes
that gap is specified in [#307](https://github.com/nurcahyo/moira/issues/307) and is not built
yet.

Setup readiness is a read-only structural check. It reports coarse component states and whether
the default route has an executable application, provider, model, policy, and compatible global
or application credential. It does not decrypt credentials, contact providers, or return resource
identifiers, names, counts, or secret metadata. Access requires a system key or trusted JWT with
`moira:setup:read`; `moira:admin` implies that scope.
