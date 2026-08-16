# Claude subscription vs API key — the boundary

**This is the canonical section.** Every other document in this repository that touches
Claude subscription credentials — the sidecar, the containerised runners, `moira-runner`,
the admin API, `.env.example` — carries one short paragraph and links here. Change the
argument here, not in six places.

Moira is a generic LLM router. It can be pointed at a Claude **subscription** (through the
containerised runner or a local sidecar) or at an ordinary **Anthropic API key**. Those two
are not interchangeable, and the difference is not technical.

## The distinction, in one paragraph

Anthropic's published terms treat subscription OAuth authentication — the login behind
Claude Code and the other native Claude apps — as being for **ordinary individual use** of
those apps. Developers **building a product or a service** are directed to API-key
authentication obtained through the Anthropic Console. The line is drawn around *who is
using it and for what*, not around *which process issues the HTTP request*. An API key is
metered, billed, and sold for exactly the use Moira is built for; a subscription is sold to
a person for their own work.

## The wrong inference, closed explicitly

Both subscription paths in this repository work by wrapping the **official** `claude` CLI:
the containerised runner runs `claude setup-token` inside a locked-down container
([`claude-runners.md`](claude-runners.md)), and the sidecar shape fronts the CLI's own
authenticated session behind an OpenAI-compatible endpoint
([`claude-subscription-sidecar.md`](claude-subscription-sidecar.md)). Using the real client
is what keeps those paths honest about *authentication* — nothing here impersonates a
client id it was never issued, and nothing forges Claude-Code fingerprint headers.

It does **not** follow that subscription-backed serving is therefore fine.

> Routing through the official CLI changes the mechanism, not the purpose. If a
> subscription is answering requests from your customers, it is being used to run a
> service, and a sidecar or a container in the middle does not change that. If you read
> "we route through the official CLI" and concluded "so multi-tenant subscription-backed
> serving is allowed", that is the inference this section exists to close.

The honest one-line rule Moira is designed around:

> **A subscription is never a safety net for anyone but its owner.**

## The policy is genuinely unsettled — two dated points

Neither of these is permission. They are recorded so a reader knows the ground has moved
and may move again, and does not mistake today's observed behaviour for a settled position.

| Date | What happened |
|---|---|
| **2026-02-19** | Anthropic's compliance documentation required API-key authentication for the Agent SDK. |
| **2026-06-15** | Anthropic **paused** that change. `claude -p` and third-party app usage still draw on subscription limits. |

"Paused" is not "reversed", and it is not a carve-out for building a product on someone's
personal subscription. The route this repository supports has changed direction several
times inside a single year. Treat any subscription-backed credential as one candidate in a
routing policy, never as the only configured route to a model family, and keep an
API-key-backed candidate in the failover chain.

## What this means for a Moira deployment

**Fine.** Running Moira against your own Claude subscription, for your own individual use —
your prompts, your work, no one else's traffic passing through it. This is the case the
runner and sidecar paths were built for.

**Not fine.** Pointing a subscription — yours, or your organisation's — at traffic from
other people's customers, or at a multi-tenant deployment where the subscription is the
quota that everyone silently lands on. That is building a service on a personal
entitlement, and it is the case Moira is designed to refuse rather than warn about.

**The right tool for serving other people is an API key.** It is metered, it is sold for
this, and it scales without a policy question attached.

## What Moira does about it

This section describes the **design**, tracked as
issue [#307](https://github.com/nurcahyo/moira/issues/307). Parts of it are not implemented
yet; where that is true it says so.

- **An opt-in gate, default off.** A subscription-backed Claude credential is refused at
  construction with a keyed error unless the deployment has explicitly opted in. This
  mirrors the existing ChatGPT gate exactly — `provider_security.allow_chatgpt_subscription`
  and `chatgpt_subscription_opt_in_required`, see
  [`provider-management.md`](provider-management.md). **Planned, not built.**
- **The declaration belongs to the credential, not just the deployment.** Which credential
  is subscription-backed, who declared it, when, and the scope it was declared for — an
  audit row, on the same path every credential creation is already audited through.
  **Planned, not built.**
- **The composition rule, which is a refusal and not a warning.** A subscription-backed
  credential must never serve traffic outside the scope it was declared for. A
  `global`-scoped subscription credential is **refused** for a tenant-scoped request rather
  than quietly used, because that is precisely the prohibited case: one operator's personal
  subscription answering someone else's customers. **Planned, not built — and the current
  behaviour is the opposite**, see the next section.
- **Fallback semantics.** A tenant with no credential of its own falls back to the platform
  credential **only if that credential is an API key**; if the platform credential is
  subscription-backed, the request is refused. A tenant whose *own* credential expired or
  was revoked **fails closed** — the owner already chose to bring their own credential, and
  silently spending the platform's quota instead is not a kindness. **Planned, not built.**
- **A tenant connecting its own subscription gets the same choice**, recorded against that
  tenant. The same reasoning, one level down. **Planned, not built.**
- **Loud where it cannot be certain.** Moira cannot tell personal traffic from commercial
  traffic from the inside, and does not pretend to. Where a subscription-backed provider is
  reachable from public execution traffic, health and dashboard output carries a persistent
  warning. That is a warning, not a refusal — the single hard refusal is the scope rule
  above. **Planned, not built.**

## Current behaviour, stated plainly

Today, credential resolution has no idea whether a credential is subscription-backed.

`PgRuntimeRepository::resolve_runtime_credential`
(`src/infra/repositories/runtime.rs`) picks exactly one credential row with a single query.
Its `where` clause admits `user`-, `application`-, `tenant`- and `global`-scoped rows into
the same candidate set, and its `order by` ranks them — `scope_type = 'tenant'` at rank 7,
the `else` arm (`global`) at rank 8 — before `priority`, `last_validated_at`, `last_used_at`
and `created_at` break ties.

So tenant does outrank global, but global is **not excluded**: it is the last resort, and it
wins whenever no narrower row survives. The filters include `status = 'active'`,
`deleted_at is null` and `(expires_at is null or expires_at > now())`, so a tenant
credential that expired or was revoked simply drops out of the candidate set and the
platform's `global` row is selected in its place, silently.

That is the silent fallback the design above forbids, and it is reachable today: the
containerised runner defaults new runners to `global` scope
(`ClaudeRunnerRecord::scope`, `src/domain/runners.rs`), and finalize stores the minted
subscription token as an ordinary `oauth2` credential at that scope. Nothing in resolution
distinguishes it from an API key.

## Capacity, honestly

**N containers on one Claude account is not N× capacity.** Rate limits attach to the
account, not to the process running against it. Five runner containers against one
subscription give you one account's worth of throughput spent from five places — and
deliberately fanning a single subscription across parallel workers is the exact pattern
Anthropic's usage policy targets. Five runners are five times the capacity only when they
are backed by five separate subscriptions you legitimately hold. For capacity out of a
single account relationship, the honest path is the metered API key.

## A console wizard does not make anything lawful

The design includes a setup screen presenting the choice in plain language — *API key
(default, fits any use)* vs *my own subscription (my individual use only)* — rather than a
checkbox reading "I agree". What that screen buys is real but limited: it makes the safe
option the default, it makes the operator aware of the distinction at the moment they
choose, and it creates a record of who chose what. It does not make a prohibited use
permitted.

It also is not the primary surface. Operators deploying Moira from a fork, a container
image, or the Helm chart never see a console screen at all. That is why this boundary lives
in the README, in `.env.example`, and in every operator document that could lead someone
toward subscription-backed serving — the documentation is the surface that reaches
everyone.

## Related reading

- [`claude-runners.md`](claude-runners.md) — the containerised runner that mints the token.
- [`claude-subscription-sidecar.md`](claude-subscription-sidecar.md) — the local sidecar shape.
- [`moira-runner.md`](moira-runner.md) — the control service behind the runners.
- [`provider-management.md`](provider-management.md) — the ChatGPT opt-in gate this design mirrors.
- [`chatgpt-subscription-spike.md`](chatgpt-subscription-spike.md) — the same question, answered for OpenAI.
- [`decisions-taken.md` §9](decisions-taken.md#9-subscription-backed-claude-access-is-an-explicit-opt-in-and-never-crosses-a-scope-boundary) — the decision record.
