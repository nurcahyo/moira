# Plan — onboarding xAI Grok, Moonshot Kimi K3, and the current Gemini Flash tier

**Status:** plan only. No code has been written. Every model id, price, and capability below was
read from the vendor's own documentation or from the vendored `rig-core` 0.40 source on
2026-08-17, and each is cited in §7.

**Why a plan and not a patch:** the research turned up three findings that change the shape of the
work, one of which invalidates the obvious approach. They are in §1. Read them before estimating.

---

## 1. The three findings that shape this work

### 1.1 rig-core 0.40 already ships all three clients

`rig-core-0.40.0/src/providers/` contains `xai/`, `moonshot.rs`, and `gemini/`. No new HTTP client,
no new streaming adapter, and no new error mapping surface has to be written from scratch. This is
substantially less work than the DeepSeek onboarding it will otherwise resemble.

### 1.2 rig's model-id constants are stale — and it does not matter

| Provider | Newest constant in rig 0.40 | Current flagship |
|---|---|---|
| xAI | `GROK_4 = "grok-4-0709"` | `grok-4.6` |
| Moonshot | `KIMI_K2_5 = "kimi-k2.5"` | `kimi-k3` |
| Gemini | `GEMINI_3_FLASH_PREVIEW = "gemini-3-flash-preview"` | `gemini-3.7-flash` |

**This does not block anything.** `completion_model` is declared
`fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel`
(`rig-core-0.40.0/src/client/completion.rs:28`) — it accepts an arbitrary string. The `pub const`
values are convenience only.

**Do not gate this work on a `rig-core` version bump.** Moira passes its own catalog's model ids
straight through. A bump would be cosmetic and would drag in the whole Rig-boundary review that
`.claude/skills/moira-rig-integration/SKILL.md` requires for a version change.

### 1.3 Gemini Flash is a catalog addition, not a provider

`ProviderKind` already has `Gemini` (`src/domain/models.rs:13`), seeded since
`migrations/0002_runtime_config.sql`. The Flash tier needs catalog rows, following the shape of
`migrations/0028_deepseek_v4_catalog.sql`, and nothing else. Scoping it as a "new provider" would
be wrong by a factor of roughly ten.

---

## 2. The finding that should change an existing ticket

The ADR at `docs/decision-session-identity-and-conversation-scope.md` §9 item 6 calls for a
`provider_model_pricing` table. **This research shows that the obvious schema —
`(provider, model, input_price, output_price, cached_price)` — cannot express what these three
vendors actually charge.** Two shapes break it:

**Prices are time-dated.** Gemini 3.6/3.7 Flash list `$0.75` input through 2026-12-31 and `$1.50`
from 2027-01-01 — the rate doubles on a published date. Separately, Moonshot's `moonshot-v1`
family sunsets on 2026-08-31, which is *two weeks from this document*.

**Prices are context-tiered.** xAI charges double above a 200,000-token input threshold on every
current model: `grok-4.6` is `$2.00` input below the threshold and `$4.00` at or above it. A single
price per model is wrong for every xAI row.

So the table needs `effective_from` / `effective_to` and an input-size tier bound, and the lookup
needs the request's own input size. Discovering this after the table shipped would mean a migration
plus a rewrite of every cost figure already computed under it. **Fold this into the pricing ticket
before it starts.**

---

## 3. xAI Grok

- **Base URL:** `https://api.x.ai` (`rig-core-0.40.0/src/providers/xai/client.rs:20`)
- **Wire format:** OpenAI-compatible
- **Caching:** **explicit** — the caller places the breakpoints
- **Notable:** built-in server-side tools billed per call, not per token

| Model | Context | Input | Cached | Output |
|---|---|---|---|---|
| `grok-4.6` | 500k | $2.00 / $4.00 ≥200k | $0.50 / $1.00 | $6.00 / $12.00 |
| `grok-4.5` | 500k | $2.00 / $4.00 | $0.30 / $0.60 | $6.00 / $12.00 |
| `grok-4.3` | 1M | $1.25 / $2.50 | $0.20 / $0.40 | $2.50 / $5.00 |
| `grok-build-0.1` | 256k | $1.00 / $2.00 | $0.20 / $0.40 | $2.00 / $4.00 |

Per-call tools: web and X search $5/1K calls, code execution $5/1K, file attachments $10/1K,
collections search $2.50/1K. **Moira's usage record has no concept of a per-call charge**, so these
are invisible to cost accounting today. Either refuse the built-in tools at the boundary or model
them; do not let them ship unmodelled.

---

## 4. Moonshot Kimi K3

- **Base URL:** `https://api.moonshot.ai/v1` global, `https://api.moonshot.cn/v1` China
  (`rig-core-0.40.0/src/providers/moonshot.rs:40`, `:42`)
- **Also offers an Anthropic-compatible base** at `https://api.moonshot.ai/anthropic`
  (`moonshot.rs:44`) with a separate builder taking an `AnthropicKey`
- **Caching:** automatic context caching

| Model | Context | Input (cache miss) | Input (cache hit) | Output |
|---|---|---|---|---|
| `kimi-k3` | 1,048,576 | $3.00 | $0.30 | $15.00 |

Also listed by the vendor: Kimi K2.7 Code, Kimi K2.6. **`moonshot-v1` sunsets 2026-08-31** — if any
catalog row is written for it, write the retirement date with it.

**Decide deliberately which base URL to use.** The Anthropic-compatible endpoint is tempting
because Moira's Anthropic path is the better-exercised one, but it routes a Moonshot model through
Anthropic-shaped error and usage mapping. The OpenAI-compatible base is the honest default; the
Anthropic base should be a separate, justified decision if it is ever taken.

---

## 5. Gemini Flash

Catalog-only. `gemini-3.7-flash` and `gemini-3.6-flash` share pricing:

| Window | Input | Output | Cached input | Cache storage |
|---|---|---|---|---|
| through 2026-12-31 | $0.75 | $3.75 | $0.075 | $0.50 /1M/hour |
| from 2027-01-01 | $1.50 | $7.50 | $0.15 | $1.00 /1M/hour |

Context is 1,048,576 input / 65,536 output, multimodal in, text out.

**The storage rent is the trap.** Gemini's explicit `CachedContent` charges by the hour whether or
not the cache is read. One cache object per idle conversation is a standing loss. Gemini's
*implicit* caching has no rent and is the correct default; explicit caching should be reachable only
by deliberate configuration, never by default.

---

## 6. Proposed tickets

Ordered. Each is independently shippable; 6.1 blocks nothing but informs everything after it.

**T1 — Extend the pricing-table design before it is built.**
Fold `effective_from`/`effective_to` and a context-tier bound into the `provider_model_pricing`
design (ADR §9.6). Blocked-by: nothing. Blocks: T5, and any cost figure anyone quotes.

**T2 — Gemini Flash catalog rows.**
`gemini-3.7-flash`, `gemini-3.6-flash`, with dated pricing. Follow `0028_deepseek_v4_catalog.sql`.
No new `ProviderKind`, no new client. Smallest ticket here; ship it first for the catalog-shape
precedent that T3 and T4 reuse.

**T3 — xAI Grok provider.**
New `ProviderKind::XAi`, credential wiring, catalog rows, `build_completion_model` arm delegating
to rig's `xai` client. Decide explicitly whether the built-in per-call tools are refused at the
boundary or modelled (§3). Context-tiered pricing lands here, so it depends on T1.

**T4 — Moonshot Kimi provider.**
New `ProviderKind::Moonshot`, OpenAI-compatible base, `kimi-k3` catalog row. Record the
Anthropic-base decision from §4 in the PR description even if the answer is "not taken".

**T5 — Per-provider cache semantics.**
The three providers disagree: xAI is explicit, Moonshot is automatic, Gemini has both with rent on
one of them. Model this as a provider capability rather than a per-call flag, so a routing decision
can read it. Depends on T1 and on `cache_creation_input_tokens` mapping (ADR §9.7).

**T6 — Update `.claude/skills/moira-rig-providers/SKILL.md`.**
Add the three providers, and record §1.2 prominently — the stale-constant trap will otherwise cost
the next person a needless version bump. Ships last, describing what actually landed.

### Deliberately not proposed

- A `rig-core` version bump (§1.2).
- Treating Gemini Flash as a new provider (§1.3).
- Enabling prompt caching for any of them. That stays gated on the ADR's measurement prerequisites;
  nothing in this plan authorises turning it on.

---

## 7. Sources

Vendor documentation, fetched 2026-08-17:

- [xAI — models and pricing](https://docs.x.ai/developers/models)
- [Kimi — K3 pricing](https://platform.kimi.ai/docs/pricing/chat-k3)
- [Kimi — chat model pricing index](https://platform.kimi.ai/docs/pricing/chat)
- [Google — Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing)

Source read locally: `rig-core` 0.40.0, at `src/providers/{xai,gemini}/`, `src/providers/moonshot.rs`,
and `src/client/completion.rs:28`.

**Verify prices at implementation time.** Every figure here is a list price on a dated page, and §2
exists precisely because two of these vendors have already published a date on which their rates
change.
