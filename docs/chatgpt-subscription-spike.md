# ChatGPT Subscription Execution — Feasibility Spike

Research spike closing #212 (Workstream C of the plan 12 feature-expansion delivery). Scope: does
executing against `chatgpt.com/backend-api/codex` — the ChatGPT-subscription-gated backend the
Codex CLI uses — belong in Moira, and how? **This document is research only. No execution code
changed.**

Primary sources read for this spike: `plans/12-feature-expansion-brainstorm.md` §1 ("Subscription
providers — Claude (OAuth), ChatGPT (OAuth), DeepSeek (API key)"), `.agents/skills/moira-rig-integration/SKILL.md`,
`.agents/skills/moira-rig-providers/SKILL.md`, `.agents/skills/moira-rig-completions/SKILL.md`, and
the vendored `rig-core` crate at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.40.0/src/providers/` — the exact
version Moira depends on (`Cargo.toml`: `rig-core = "0.40"`; `Cargo.lock`: pins `0.40.0`).

## Headline finding

**Plan §1's premise for the "raw client" option is out of date.** The table in §1 ("ToS risk
framing, per provider") and consolidated risk **R5** both assert that `chatgpt.com/backend-api/codex`
"is a bespoke, reverse-engineered wire format with **no rig-core provider**" and that building a
client for it would be exactly the "parallel LLM abstraction" `CLAUDE.md` forbids.

That is no longer true of the rig-core version this repository actually vendors. **rig-core 0.40.0
ships a first-party `chatgpt` provider module** — `rig_core::providers::chatgpt` — that targets
`https://chatgpt.com/backend-api/codex` directly and implements `rig_core::completion::CompletionModel`
(and streaming) for it, including its own unit tests. It is not feature-gated: `providers/mod.rs:98`
declares `pub mod chatgpt;` unconditionally, alongside `openai`, `anthropic`, `deepseek`, etc.

This changes the shape of the answer to question 1 below from "no" to "yes, and cheaply" — but it
does **not** change the ToS analysis (question 3), which is carried forward unchanged. The
recommendation in this document therefore still lands on **NO-GO for production wiring today**, but
for a different reason than the plan currently states: the blocker is now a policy decision the
owner has not made, not a missing Rig-boundary path.

## 1. Can `backend-api/codex` be wrapped as a `CompletionModel` without a parallel abstraction?

**Yes**, as evidenced directly in the vendored source.

### What rig-core 0.40.0 actually ships

`rig-core-0.40.0/src/providers/chatgpt/mod.rs` (868 lines) and `chatgpt/auth/{mod,native,wasm}.rs`:

- `CHATGPT_API_BASE_URL = "https://chatgpt.com/backend-api/codex"` (`mod.rs:34`) — the exact
  endpoint this spike was asked to evaluate.
- `pub type Client<H = reqwest::Client> = client::Client<ChatGPTExt, H>;` (`mod.rs:114`) — the same
  generic `client::Client<Ext, H>` every other Rig provider uses (see
  `.agents/skills/moira-rig-providers/SKILL.md` "Rig 0.40 Client Model"), not a bespoke transport.
- `Capabilities<H>::Completion = Capable<ResponsesCompletionModel<H>>` (`mod.rs:172`), and:
  ```rust
  impl<H> completion::CompletionModel for ResponsesCompletionModel<H>
  where
      Client<H>: HttpClientExt + Clone + Debug + 'static,
      H: Clone + Default + Debug + WasmCompatSend + WasmCompatSync + 'static,
  {
      type Response = responses_api::CompletionResponse;
      type StreamingResponse = responses_api::streaming::StreamingCompletionResponse;
      type Client = Client<H>;
      // ...
  }
  ```
  (`mod.rs:489-552`) — a real, non-`#[non_exhaustive]`, trait-satisfying impl of the exact trait
  question 1 asks about. Both `completion()` and `stream()` are implemented; `stream()` returns
  `StreamingCompletionResponse<responses_api::streaming::StreamingCompletionResponse>`, the same
  streaming envelope type the OpenAI Responses API path already produces.
- Wire format is **not bespoke at the rig-core layer**. `ChatGPTExt` implements
  `openai::responses_api::ResponsesProviderExt` and overrides exactly one method:
  ```rust
  impl responses_api::ResponsesProviderExt for ChatGPTExt {
      // The ChatGPT backend rejects the `system` role in `input`, so every
      // system message — including mid-conversation ones — is lifted into the
      // top-level `instructions` field.
      fn system_instructions_placement(&self) -> responses_api::SystemInstructionsPlacement {
          responses_api::SystemInstructionsPlacement::AllInstructions
      }
  }
  ```
  (`mod.rs:162-169`). Request construction, SSE parsing, and tool-schema handling all delegate to
  `openai::responses_api::GenericResponsesCompletionModel<Ext, H>`
  (`openai/responses_api/mod.rs:1242`, `impl completion::CompletionModel for
  GenericResponsesCompletionModel<Ext, H>` at `:1917`) — the same generic engine used by the ordinary
  OpenAI Responses API. `chatgpt::ResponsesCompletionModel::create_request` (`mod.rs:382-420`) calls
  that shared builder, then applies subscription-specific fixups (merges in default instructions,
  forces `stream = true`, drops `background`/`metadata`/`parallel_tool_calls`/`service_tier`/`store`/
  `text`/`top_p`/`user`, and always requests `Include::ReasoningEncryptedContent`). This is a thin,
  declarative delta on top of an existing rig-core wire engine, not a hand-rolled parser.
- Auth is header-based and unremarkable: `Authorization: Bearer <token>` plus a `session_id` and an
  optional `ChatGPT-Account-Id` header (`add_auth_headers`, `mod.rs:422-439`) — the same shape as
  every other bearer-token provider Moira already integrates.
- Model catalog constants are declared (`GPT_5_4`, `GPT_5_4_PRO`, `GPT_5_3_CODEX`,
  `GPT_5_3_CODEX_SPARK`, `GPT_5_3_INSTANT`, `GPT_5_3_CHAT_LATEST`; `mod.rs:41-51`), but `model_key` is
  a free `impl Into<String>` on `ResponsesCompletionModel::new` (`mod.rs:343`), so any model string
  the backend accepts works, matching how Moira treats `provider_models.model_key` for every other
  provider.

### How this maps onto Moira's existing `RuntimeFactory` pattern

`.agents/skills/moira-rig-providers/SKILL.md`'s "Choosing Native vs OpenAI-Compatible" table already
states the rule that applies here: *"Vendor has a dedicated rig-core 0.40 provider module with its
own wire format → New `ProviderType` variant using that module. Follow the workflow below."* That
workflow (migration → `ProviderType` variant → row mapping → credential gate → factory arm →
wire-shape review → OpenAPI → tests → docs) is exactly what plan §1 already sketched for a
hypothetical `ChatGptOauth` variant (`plans/12-feature-expansion-brainstorm.md:106`) — it just
assumed, incorrectly, that no rig-core module existed to point that variant at. The factory arm
itself would follow the same three beats as every other provider (`gate credential → build client →
completion_model(model_key)`), directly parallel to the existing `ProviderType::DeepSeek` arm:

```rust
require_credential_type(credential.credential_type, &[CredentialType::Oauth2])?;
let client = chatgpt::Client::builder()
    .api_key(chatgpt::ChatGPTAuth::AccessToken {
        access_token: secret.as_str().to_string(),
        account_id: credential.config.get("account_id").and_then(Value::as_str).map(str::to_string),
    })
    .build()
    .map_err(|err| safe_config_error("chatgpt", err))?;
Ok(RuntimeModelHandle::ChatGpt(client.completion_model(model_key)))
```

(Illustrative only — not written to the tree; this spike does not touch Rust.) `ChatGPTAuth` is a
two-variant enum (`mod.rs:52-58`): `AccessToken { access_token, account_id }` for bring-your-own-token
use, or `OAuth` for rig-core-managed login. The `AccessToken` variant is the one that matches every
other provider arm in `RuntimeFactory` — Moira already decrypts a stored secret and passes it as a
bare string (`secret.as_str()`); this provider accepts that shape directly. `credential_secret_field`
(`src/security/crypto.rs:100`) already maps `CredentialType::Oauth2 → "access_token"`, so no new
credential-payload shape is needed either — the `oauth2` plumbing plan §1 already designs (decision 1,
the `provider_credentials` table, the planned `oauth-token-refresh` `JobDispatcher`) is directly
reusable, not ChatGPT-specific work.

**One real gap, not a parallel-abstraction problem, but worth naming:** the `OAuth` auth source
manages token refresh internally, but only by reading/writing a local JSON file
(`~/.config/chatgpt/auth.json` by default, `chatgpt/auth/native.rs:9-151`) and, when the cached token
is stale and `allow_device_flow` is left at its default `true`, blocking on an interactive device-code
login printed to stdout (`emit_device_code_prompt`, `native.rs:287-296`) — both wrong for a
multi-tenant server process. Moira would need to bypass that entirely by supplying `AccessToken`
directly (as above) and running its own refresh, mirroring what plan §1 already scoped for the
`oauth-token-refresh` worker. The refresh endpoint and grant shape are visible in the vendored source
and match a standard OAuth refresh call: `POST https://auth.openai.com/oauth/token`, form-encoded
`grant_type=refresh_token`, `refresh_token`, `client_id=app_EMoamEEZ73f0CkXaXp7hrann`, `scope=openid
profile email` (`native.rs:235-284`). Nothing about this requires touching the local-file/device-code
code path at all.

**Conclusion for question 1:** representable, without a parallel LLM abstraction, using the exact
`RuntimeFactory` seam Moira already has. This reverses the plan's current "no rig-core provider"
premise. It does **not** by itself make building this a good idea — see question 3.

## 2. Local OpenAI-compatible sidecar (mirrors decision 1, Option C)

Still valid, and still the right default recommendation — now for a slightly different reason than
"it's the only technically clean path."

A sidecar (e.g. a hermes-proxy-style process, as already referenced in plan §1 for both Claude and
ChatGPT) authenticates as the real Codex/ChatGPT client on the operator's own machine and exposes a
local OpenAI-compatible `/v1/chat/completions`-shaped endpoint. Moira then registers it as an
ordinary `ProviderType::OpenAiCompatible` provider with a `base_url` override — the exact same arm
`.agents/skills/moira-rig-providers/SKILL.md` documents today, already shipped, already tested, no
new `ProviderType`, no new rig-core module, **zero new Moira code**.

Compared honestly against the native-provider finding in §1 above:

| | Native `rig_core::providers::chatgpt` | Sidecar (Option C) |
|---|---|---|
| New Moira code | One new `ProviderType` variant + factory arm (small, well-trodden shape) | None |
| New rig-core surface used | A provider rig-core's own maintainers built for this exact endpoint | None — reuses the existing OpenAI-compatible arm verbatim |
| Where the subscription session lives | Inside Moira's process, as a decrypted access/refresh token pair in `provider_credentials` | On the operator's own machine, inside the sidecar process; Moira never holds ChatGPT-account credentials at all |
| Blast radius of compromise | A DB leak exposes a live ChatGPT/Codex account token (plan §1's **R4**, "blast radius... increases in kind, not degree") | A Moira DB leak exposes nothing ChatGPT-specific; the sidecar's own local exposure is the operator's existing responsibility |
| Multi-tenant fan-out risk | High if wired into `RuntimeFactory` and made selectable by any application/tenant — this is precisely the personal-subscription-behind-a-multi-tenant-gateway shape ToS restricts | Low — the sidecar is inherently single-operator, single-session; nothing in Moira's request path changes who is "the ChatGPT user" |
| Token refresh | Moira's responsibility (new `oauth-token-refresh` dispatcher work, already planned generically in §1) | The sidecar's responsibility, out of Moira's scope entirely |

The native path is now *cheaper to build* than the plan assumed, but the sidecar remains the
*lower-risk* path, because it keeps the personal-subscription boundary outside Moira's multi-tenant
process entirely — which is the actual thing ToS cares about, not whether the wire client is
first-party or hand-rolled. Recommendation: **keep decision 1's working hypothesis (sidecar first)
for ChatGPT, unchanged.**

## 3. ToS posture — carried forward, not softened

This spike does not change the ToS analysis in plan §1, and does not soften it:

- ChatGPT/Codex subscriptions are, by OpenAI's general terms and by OSS-ecosystem convention (as
  plan §1 already documents), personal/single-user. There is no ChatGPT-specific carve-out analogous
  to Anthropic's mid-2026 reinstatement of third-party agent usage "through the Agent SDK" (plan §1's
  Claude Option B). No equivalent sanctioned route has been identified for ChatGPT subscriptions.
- The existence of a first-party rig-core client does not change this. If anything, it sharpens the
  risk: a clean, well-tested `CompletionModel` impl makes it *easy* to wire a single operator's
  ChatGPT subscription behind `RuntimeFactory` and have it silently serve every application/tenant
  configured to route to it — exactly the fan-out misuse pattern personal-subscription terms exist to
  prevent, and exactly the shape plan §1's **R1** ("policy volatility is the headline risk, not a
  footnote") and **R4** (blast-radius) already warn about for the Claude side.
- One data point worth recording precisely because it cuts the other way: rig-core's default
  `originator` is `"rig"` and its default `User-Agent` is `rig/{version} ({os} {arch}; rig)`
  (`mod.rs:35`, `default_user_agent`, `mod.rs:618-626`) — it does **not** spoof the Codex CLI's own
  fingerprint the way plan §1 explicitly warns Moira's own HTTP clients must never do for Claude
  (`x-app: cli`, `user-agent: claude-cli/...`). rig-core identifies itself honestly as rig traffic
  hitting a subscription-only backend. That is a meaningfully different — and better — posture than
  impersonation, but it does not make hitting a subscription-gated backend from a third-party,
  potentially multi-tenant product sanctioned. Self-identifying honestly while still using a session
  the backend expects to come from the first-party client is still a ToS question, not a technical
  one, and it is the owner's call to make, not this spike's.
- Testing-policy note for whoever runs the eventual Wave-0 spike: any local smoke test against the
  live `chatgpt.com/backend-api/codex` endpoint must use the owner's own personal ChatGPT session on
  the owner's own machine, exactly as plan §1's owner-approved testing policy already specifies for
  Claude, and must never be exercised from CI (no such credential exists there and none should be
  added).

## GO / NO-GO recommendation

- **GO — sidecar (Option C) stays the recommended default for ChatGPT subscription execution.**
  Zero new Moira/rig-core execution code, keeps the personal-subscription boundary outside Moira's
  multi-tenant process, mirrors the identical recommendation already made for Claude in plan §1
  decision 1. No further spike work is needed to justify this path; it was already the "working
  hypothesis" and this research does not disturb it.

- **NO-GO — do not wire `rig_core::providers::chatgpt` into `RuntimeFactory` for production use yet.**
  This is a change of *reason*, not a change of *outcome*, from plan §1's current text: the blocker
  is no longer "impossible without a parallel abstraction" (this spike shows that premise is false
  against the currently vendored rig-core 0.40.0) — it is that **no owner ToS risk-acceptance exists
  for a native ChatGPT execution path**, the same gate plan §1 already imposes on Claude's Option B
  (native Agent SDK runner) before any code lands there. Building the `RuntimeFactory` arm before that
  decision is made would let the engineering get ahead of the policy call it depends on.

### Concrete next step

1. **Correct the stale premise, separately from this spike.** Plan §1's Option-D table cell for
   ChatGPT and consolidated risk **R5** both assert "no rig-core provider" — that is now
   demonstrably false for rig-core 0.40.0. Recommend a short, scoped follow-up docs change to
   `plans/12-feature-expansion-brainstorm.md` (not made in this PR, to keep this spike narrowly
   scoped to #212) updating those two spots to point at this document's findings, without changing
   the plan's GO/NO-GO conclusion.
2. **No `RuntimeFactory` code for ChatGPT until the owner makes the same call already made for
   Claude's Option B.** If/when the owner decides to evaluate a native ChatGPT execution path, scope
   it as its own decision (mirroring decision 1), gated on explicit ToS risk acceptance — at that
   point the engineering cost is known to be small: one new `ProviderType` variant following
   `.agents/skills/moira-rig-providers/SKILL.md`'s existing "Workflow: Adding a New Provider," reusing
   the `oauth2` credential plumbing plan §1 already designed, and feeding the `auth.openai.com`
   refresh endpoint discovered in this spike into the already-planned generic `oauth-token-refresh`
   `JobDispatcher` (which is provider-agnostic by design and needs no ChatGPT-specific branch).
3. **Continue the sidecar path as already phased.** Plan §1's Wave-0 local spike (proving Options B
   and C end-to-end on the owner's machine, for Claude) should be extended to also smoke-test a
   sidecar fronting a ChatGPT Pro/Plus session, using the owner's own personal subscription, on the
   owner's own machine — never through Moira's multi-tenant request path, and never in CI.
