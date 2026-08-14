# Claude subscription via a local sidecar

Decision 1 in `plans/12-feature-expansion-brainstorm.md` §1 is **(a) sidecar**: a
local, OpenAI-compatible proxy holds the Claude subscription session, and Moira
registers it as an ordinary `open_ai_compatible` provider — the exact same
mechanism the console already uses for a local vLLM box. Moira gains **no new
execution code** for this: `RuntimeFactory::build_completion_model`'s
`ProviderType::OpenAiCompatible` arm is unchanged, `require_credential_type` is
unchanged, and the Moira/Rig boundary stays intact.

This document is the Wave-0 spike runbook: how to run the sidecar, how to point
Moira at it, and how to smoke-test the whole chain with a real subscription
session on your own machine. It is deliberately **not** a claim that this ships
a production integration — see "What this does and does not prove" below.

## Why a sidecar and not a raw token against the API

Read `plans/12-feature-expansion-brainstorm.md` §1 in full before changing
anything here; this section is a summary, not the authority. As of the 2026-08-14
verification recorded there:

- Anthropic blocked subscription OAuth tokens from non-Claude-Code clients at
  the API layer (~Jan 2026) and then reinstated third-party agent usage **only
  through the Agent SDK / Claude Code route** (Max plan, usage-credit caveats).
  **Moira calling `api.anthropic.com` directly with a subscription token is off
  the table and stays off the table.**
- The sanctioned shapes are (b) a native runner that spawns the real `claude`
  CLI / Agent SDK per request, or (a) a sidecar that fronts the CLI's own
  authenticated session behind an OpenAI-compatible HTTP surface. The owner
  chose (a) first: it costs an extra local process, not a new Moira execution
  backend.
- **R1 (policy volatility) is the headline risk, not a footnote.** This route
  has reversed three times in eight months. Never make it the only configured
  route to a model family — keep an API-key-backed candidate in the failover
  chain — and never send Claude-Code-mimicking fingerprint headers (`x-app: cli`,
  `user-agent: claude-cli/...`) from Moira's own HTTP clients. The sidecar's own
  client doing the talking is what keeps this option honest; Moira impersonating
  one would not be.

## Architecture

```
 operator's machine
 ┌─────────────────────────────────────────────────────────┐
 │  claude CLI session (claude setup-token / claude login)  │
 │            │                                              │
 │            ▼                                              │
 │  sidecar (hermes-proxy-style): binds 127.0.0.1:<port>,    │
 │  speaks OpenAI-compatible /v1/chat/completions, /v1/models│
 │            │                                              │
 │            ▼                                              │
 │  Moira: provider_type = "open_ai_compatible"               │
 │         base_url = "http://127.0.0.1:<port>/v1"            │
 │         credential_type = "api_key" (placeholder, unless   │
 │           the sidecar itself requires a local bearer key)  │
 └─────────────────────────────────────────────────────────┘
```

Moira never talks to Anthropic for this route. It talks to the sidecar exactly
as it would talk to a local vLLM box, because from Moira's point of view that
is what it is: an OpenAI-compatible HTTP endpoint on `127.0.0.1`.

## Running the sidecar

The sidecar is an external tool, not part of this repository, and its exact
install steps are outside Moira's control — treat the following as the shape,
not a pinned version:

1. Install a hermes-proxy-style sidecar that fronts a Claude subscription
   session (see the hermes-proxy project referenced in
   `plans/12-feature-expansion-brainstorm.md` §1's option table). Follow its own
   install instructions; this repo does not vendor or pin it.
2. Authenticate the subscription session the sidecar will use. Two ways, both
   sanctioned because the real `claude` CLI is the client doing the OAuth dance:
   - Interactive: `claude login` (or the sidecar's own login command), once, on
     the machine that will run the sidecar.
   - Headless / long-lived: `claude setup-token`, which prints a long-lived
     token for exactly this kind of non-interactive use. **Never commit this
     token to the repository, a log, or a shell history file you intend to
     keep.**
3. Start the sidecar bound to `127.0.0.1` on a port of your choosing. It should
   answer `GET /v1/models` and `POST /v1/chat/completions` in the OpenAI wire
   shape — the same two endpoints `scripts/seed-local.sh` and the console's
   "Connect a local endpoint" shortcut already probe for any OpenAI-compatible
   provider.
4. Confirm it locally before touching Moira at all:
   ```bash
   curl -s http://127.0.0.1:<port>/v1/models
   ```

## Registering the sidecar in Moira

Two ways, both landing on the exact same rows — a sidecar is not a new provider
type, it is an `open_ai_compatible` provider whose `base_url` happens to be
`127.0.0.1`:

- **Console (recommended):** `/settings/llm` → "Connect a local endpoint" →
  enter `http://127.0.0.1:<port>/v1` → "Ask the endpoint" → select the model(s)
  it reports → "Register the selected models". This runs the same
  provider → model → credential → routing-policy chain
  `docs/provider-credential-management.md` and `scripts/seed-local.sh` document,
  bound to the seeded `general` route. The credential row it creates is a
  server-generated placeholder API key — the sidecar authenticates to Anthropic
  on its own, and Moira's `Authorization` header to `127.0.0.1` is never read
  by anything that enforces it.
- **`scripts/seed-local.sh`:**
  ```bash
  MOIRA_SEED_BASE_URL=http://127.0.0.1:<port>/v1 \
  MOIRA_SEED_NAME="Claude subscription (sidecar)" \
    scripts/seed-local.sh
  ```

Either way, the provider this creates is an **ordinary `open_ai_compatible`
row**. `GET /api/v1/admin/providers` shows it exactly like a vLLM box; nothing
in Moira's execution path can tell the two apart, which is the entire point of
choosing shape (a).

## Storing the subscription token as an `oauth2` credential

Separately from the sidecar's own (placeholder) credential above, the console's
`/settings/llm` page also has a **"Connect Claude subscription"** panel. It
takes a long-lived token — the output of `claude setup-token` — and stores it
through Moira's **existing** `POST /api/v1/admin/provider-credentials` endpoint
as `credential_type: "oauth2"`, attached to a dedicated `anthropic`-type
provider row the panel manages (`display_name: "Claude subscription (sidecar)"`,
find-or-create, matching `plans/12-feature-expansion-brainstorm.md` §1's design:
*"Anthropic needs no new provider_type: it stays 'anthropic', discriminated by
`credential_type = 'oauth2'` vs `'api_key'`"*). Re-submitting a fresh token
rotates the existing row in place via `POST .../rotate` rather than creating a
duplicate.

**This credential is storage only as of this change.** It is deliberately not
wired to any `provider_models` or `routing_policies` row, so it is never
resolved by execution — and even if it were, `RuntimeFactory`'s
`ProviderType::Anthropic` arm still gates on `require_credential_type(...,
&[CredentialType::ApiKey])` (`src/orchestration/runtime_factory.rs:112`), so an
`oauth2` credential on that provider type fails closed with a config error
today rather than being silently accepted. What it *does* do is satisfy the
"OAuth tokens live encrypted in the database" requirement and give the two
future consumers a row to read once they exist:

- the `oauth-token-refresh` `JobDispatcher` — **blocked on issue #90** (no real
  `JobDispatcher` exists in the codebase yet; see
  `plans/12-feature-expansion-brainstorm.md` §1's current-state note), and
- a future native-runner execution backend (option (b) in the table above), if
  the sidecar's operational cost or stability ever makes that the better
  choice.

No plaintext token is ever sent to the browser: the panel's form posts to a
Next.js route handler (`app/api/settings/llm/claude-subscription/route.ts`),
which is the only place the value exists outside Moira's own encrypted storage,
for the duration of one request.

## Failure visibility (R2)

A runtime health check on the sidecar/subscription route (session validity,
sidecar liveness, surfaced as a keyed provider-health error rather than a
generic timeout the failover chain silently absorbs) is desirable per R2 in
`plans/12-feature-expansion-brainstorm.md` §8, but wiring it end-to-end touches
the same not-yet-built `"provider-health-check"` worker job the plan already
flags as reserved-but-unbuilt. That wiring is out of scope for this change;
today a sidecar that has quietly lost its session behaves like any other
OpenAI-compatible endpoint returning errors — an ordinary provider failure, not
yet a distinguished "your subscription session expired" message. Track this
alongside the `oauth-token-refresh` worker as follow-up work, not as something
this runbook's spike script can paper over.

## The spike script — local only, never CI

`scripts/claude-subscription-sidecar-spike.sh` is the human-run Wave-0 spike:
it smoke-tests a running sidecar directly, then optionally proves the whole
chain through a locally running Moira using a **real** subscription-backed
sidecar. Per the owner-approved testing policy
(`plans/12-feature-expansion-brainstorm.md` §1, "Testing policy for this
workstream"):

- Local smoke tests **may** use a real, locally-running `claude` CLI session or
  a real sidecar. That is what this script is for.
- **CI has no `claude` binary, no sidecar, and no real subscription.** This
  script refuses to run when `CI` is set, and nothing under `tests/` — Rust or
  console — depends on it existing. CI coverage for the OpenAI-compatible wire
  shape this route exercises comes from the repository's existing scripted
  Axum test server and the console's mocked-Moira-client unit tests, not from
  this script.
- **Never commit a token.** The script reads `CLAUDE_SUBSCRIPTION_TOKEN` from
  the environment for the credential-storage half of the smoke test and never
  writes it to a file, a log line, or a git-tracked path.

Usage:

```bash
# 1. Start your sidecar first (see "Running the sidecar" above), then:
SIDECAR_BASE_URL=http://127.0.0.1:8317/v1 \
  scripts/claude-subscription-sidecar-spike.sh

# 2. Optionally also prove the Moira round trip (requires `make run` and
#    `make bootstrap-key` already done in another terminal):
SIDECAR_BASE_URL=http://127.0.0.1:8317/v1 \
MOIRA_SYSTEM_KEY="$MOIRA_SYSTEM_KEY" \
CLAUDE_SUBSCRIPTION_TOKEN="$(claude setup-token)" \
  scripts/claude-subscription-sidecar-spike.sh
```

## What this does and does not prove

Proves: an OpenAI-compatible sidecar fronting a real Claude subscription
session can be registered in Moira with zero new execution code, and a prompt
routed through it returns real token usage — the same evidence
`plans/12-feature-expansion-brainstorm.md` §1's phasing asks Wave-0 to produce
for decision 1.

Does not prove: that the sidecar remains stable under load, that Anthropic's
policy on this route holds (R1), or that a lost sidecar session fails loudly
rather than as a generic timeout (R2, tracked above as follow-up). Treat a
green spike run as "the mechanics work today," not as a production
readiness signal.

## Related reading

- `plans/12-feature-expansion-brainstorm.md` §1 — the decision record this
  document implements.
- `docs/provider-credential-management.md` — the credential endpoints this
  flow calls; no new Moira endpoint was added for it.
- `docs/project-structure.md` and `docs/console-architecture.md` — where the
  console-side pieces (`lib/claude-subscription.ts`,
  `modules/llm/ConnectClaudeSubscriptionPanel.tsx`,
  `app/api/settings/llm/claude-subscription/route.ts`) sit relative to the rest
  of the console.
