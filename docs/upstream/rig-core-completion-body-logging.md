# Draft: upstream issue for `rig-core` — completion request and response bodies are logged at TRACE

**Status: not filed. Do not file this from an automated account.**

This is a draft for a maintainer of this repository to review and submit, under their own name, to
the `rig` project:

- Upstream repository: <https://github.com/0xPlaygrounds/rig>
  (confirmed as the `repository` field of `rig-core` 0.40.0's own `Cargo.toml`, line 25)
- Upstream issue tracker: <https://github.com/0xPlaygrounds/rig/issues>

**Why a person files it:** it goes out under a name, and it may invite design discussion the
maintainers reasonably expect a person to carry.

When it is filed, record the issue URL in this file and in the execution ledger, and re-check it at
the next `rig-core` version bump (bump procedure: `.claude/skills/moira-rig-integration/SKILL.md`).

Everything below the line was read out of the `rig-core` 0.40.0 sources as vendored by Cargo at
`~/.cargo/registry/src/index.crates.io-*/rig-core-0.40.0/`. Line numbers refer to that tree.
See "Verification notes" at the end for what was checked and what was not.

---

## Suggested title

> `rig::completions` logs full completion request and response bodies at TRACE, with no opt-out at the source

## Suggested body

**Version:** `rig-core` 0.40.0 (crates.io, checksum
`d8731dd5532b3a12ce1613af73073fb2051ef750f50c504778c21d55ae933cac`)

### What happens

Every provider's completion path serialises the **entire** request body — every message, verbatim —
and emits it as a `TRACE` event on the `rig::completions` target. The same happens for the full
response body. There is no feature flag, no redaction hook, and nothing at the call site that hints
this is happening; the only lever a consumer has is their log filter.

The pattern is uniform across providers. Representative sites:

| File (in `rig-core-0.40.0/src/`) | Line | What is interpolated |
| --- | --- | --- |
| `providers/openai/completion/mod.rs` | 1967 | `serde_json::to_string_pretty(&request_body)` — the finalised OpenAI chat-completions request |
| `providers/openai/completion/mod.rs` | 1999 | `serde_json::to_string_pretty(&response)` — the full deserialised response |
| `providers/openai/completion/streaming.rs` | 191 | the finalised streaming request body |
| `providers/anthropic/completion.rs` | 2499 | `serde_json::to_string_pretty(&request)` — the full Anthropic request |
| `providers/anthropic/completion.rs` | 2540 | the full Anthropic response |
| `providers/anthropic/streaming.rs` | 271 | the full Anthropic streaming request |
| `providers/gemini/completion.rs` | 121 | `serde_json::to_string_pretty(&request)` — the full Gemini request |
| `providers/gemini/completion.rs` | 164 | the full Gemini response |
| `providers/openai/responses_api/mod.rs` | 1975, 2008 | request and response bodies |
| `providers/cohere/completion.rs` | 653, 684 | request and response bodies (see the target note below) |
| `providers/ollama.rs` | 652, 691, 735 | request and response bodies, blocking and streaming |
| `providers/xai/completion.rs` | 227, 249 | request and response bodies |

All of them have the same shape:

```rust
if enabled!(Level::TRACE) {
    tracing::trace!(
        target: "rig::completions",
        "OpenAI Chat Completions completion request: {}",
        serde_json::to_string_pretty(&request_body)?
    );
}
```

The body is interpolated into the **message string**, not attached as a structured field, so a
subscriber cannot drop it by field name either. There are 23 `enabled!(Level::TRACE)` guards and 46
`target: "rig::completions"` sites in the crate; the `rig::completions`, `rig::streaming`,
`rig::embedding` and `rig::transcription` targets are all used this way.

**One site is missing its target, which breaks target-based filtering.**
`providers/cohere/completion.rs:653` — the Cohere completion *request* — is the only payload trace
in the crate written without an explicit `target:`, so it falls back to the module path,
`rig_core::providers::cohere::completion`. Its matching response log at `:684` does use
`target: "rig::completions"`. A consumer who filters on `rig` or `rig::*` (the documented-looking
target namespace) therefore silences the Cohere response body but still logs the Cohere request
body, prompts included. That looks like an oversight rather than a design choice, and it is a
one-line fix independent of everything else below.

### Minimal reproduction

```rust
// Cargo.toml: rig-core = "0.40", tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
//             tracing-subscriber = { version = "0.3", features = ["env-filter"] }
use rig_core::OneOrMany;
use rig_core::completion::{CompletionModel, CompletionRequest, Message};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("trace") // or: RUST_LOG=trace, or any broad filter
        .init();

    // Any provider. No valid key is needed — the request body is logged before the
    // HTTP call is made, so the request can fail and the payload is already out.
    let client = rig_core::providers::openai::Client::new("not-a-real-key-placeholder").unwrap();
    let model = client.completion_model("gpt-4o-mini");

    let request = CompletionRequest {
        model: None,
        preamble: Some("you are a helpful assistant".into()),
        chat_history: OneOrMany::one(Message::user("CANARY-a-secret-a-user-typed")),
        documents: vec![],
        tools: vec![],
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    };

    let _ = model.completion(request).await; // fails on auth; the log has already happened
}
```

Stdout contains a `rig::completions` TRACE line —
`OpenAI Responses completion request: { … "CANARY-a-secret-a-user-typed" … }` for the default
`openai::Client` (which is the Responses-API extension, `providers/openai/client.rs:49`), or
`OpenAI Chat Completions completion request: …` for the chat-completions client.

Because the log happens **before** the request is sent — `providers/openai/completion/mod.rs:1967`
logs, 1974 serialises to bytes, 1981 builds the POST; same ordering at
`responses_api/mod.rs:1975` — an invalid key does not prevent the disclosure. The payload is in the
log whether or not the call succeeds.

### Why this matters beyond "then don't enable TRACE"

For a library whose payloads *are* prompts, the log level is the only barrier between an operator
debugging their own code and a stream of user content. That is already uncomfortable when the
payload is a prompt the caller typed.

It becomes a different class of problem once the request carries **retrieved** content. In a RAG or
memory-augmented application the assembled `CompletionRequest` contains passages the caller never
wrote and, in a multi-tenant deployment, may have no right to see outside the request that produced
them. An operator who raises the log level to chase an unrelated routing bug thereby exports other
tenants' document text to wherever logs go — with nothing to indicate that this is what raising the
level does.

The trigger is entirely ordinary: someone sets `RUST_LOG=trace`, or a broad `debug`, while
debugging something else. Log shipping then puts that content in a system with a different retention
policy and a different audience from the application's own data store.

Two adjacent observations, offered in case they are useful rather than as part of the same ask:

1. **Gemini logs a raw response body at `ERROR`, not `TRACE`,** when deserialisation fails —
   `providers/gemini/completion.rs:151-155` and `providers/gemini/interactions_api/mod.rs:176-180`
   and `:438-442` interpolate `body = %response_text`. `ERROR` is enabled in essentially every
   deployment, so a malformed or unexpected provider response puts generated content into the log
   of an application that never opted into verbose logging. This one does not depend on the
   operator's log level at all.
2. **The completion span is an `info_span!` carrying `gen_ai.system_instructions`** — the preamble
   — as an attribute (for example `providers/openai/completion/mod.rs:1935`,
   `providers/anthropic/completion.rs:2467`, `providers/gemini/completion.rs:104`,
   `agent/runner.rs:79`). Under an OpenTelemetry bridge or a `fmt` layer configured to render span
   fields, the system prompt therefore reaches the exporter at `INFO`. That may well be the
   intended semconv behaviour; flagging it only because it means "don't enable TRACE" is not on its
   own a sufficient answer. (`gen_ai.input.messages` / `gen_ai.output.messages` are declared as
   `field::Empty` on those spans but, as far as I can see, never recorded in 0.40 — so the message
   bodies themselves do not currently reach spans.)

### What a consumer can do today, and why it is not enough

A per-target `EnvFilter` directive does work: `RUST_LOG=myapp=trace,rig=info` keeps the consumer's
own `TRACE` logs and drops rig's payload events while still letting rig's warnings and errors
through. So this is suppressible without losing one's own verbose logging — the problem is not that
it is impossible, it is that it is *opt-out, undiscoverable, and defeated by the obvious thing*:
a bare `RUST_LOG=trace` set by an operator who has never read rig's source re-enables it, and no
amount of care in the application's code can prevent that.

The robust form is therefore a subscriber layer **below** the `EnvFilter`, which no value of
`RUST_LOG` can widen back open:

```rust
// Drops verbose events from targets that log prompt payloads.
// Sits below the EnvFilter, so it holds however `RUST_LOG` is set — an operator who
// needs `myapp=trace` to debug routing must not have to accept every prompt and every
// retrieved document as the price. INFO and above still pass, so upstream warnings and
// errors are never hidden.
fn suppresses_provider_payload_logs(metadata: &tracing::Metadata<'_>) -> bool {
    let verbose = *metadata.level() > tracing::Level::INFO;
    let payload_bearing = metadata.target() == "rig" || metadata.target().starts_with("rig::");
    !(verbose && payload_bearing)
}

// tracing_subscriber::registry()
//     .with(env_filter)
//     .with(filter_fn(suppresses_provider_payload_logs))
//     .with(fmt_layer)
```

This holds, but it is the wrong place for it. A consumer has to know the hazard exists, know the
target names, and re-derive the filter; every consumer who does not, ships the exposure by default.
It also blanket-drops *all* verbose `rig` events, including ones that would be genuinely useful,
because there is nothing that distinguishes a payload-bearing event from an operational one. And,
as noted above, it silently misses `providers/cohere/completion.rs:653`, which is on
`rig_core::…` rather than `rig::…` — a workaround built on target prefixes inherits every
inconsistency in how targets are assigned.

### Would any of these be welcome?

Framed as a question rather than a request — you may well have context that rules some of these out:

1. **Not logging request/response bodies by default.** Put them behind an explicit opt-in — a
   feature flag such as `log-request-bodies`, or a client builder method — so the default is safe
   and enabling it is a deliberate act.
2. **A redaction hook**: let the consumer supply a closure that decides what, if anything, of a
   body reaches the log.
3. **A dedicated target** such as `rig::completions::payload`, distinct from operational events on
   `rig::completions`. That alone would let consumers filter precisely instead of silencing `rig`
   wholesale, and looks like the cheapest change that materially helps.
4. At minimum, **documenting it** — that `TRACE` on these targets emits full request and response
   bodies, and what that means for applications whose requests carry retrieved or user-derived
   content. Plus, separately, reconsidering the `ERROR`-level body log in the Gemini
   deserialisation-failure path.
5. Independently of all of the above, **giving `providers/cohere/completion.rs:653` the
   `rig::completions` target** so that target-based filtering is at least consistent.

Happy to open a PR for whichever direction is welcome.

### How it was found

A canary test asserting that no retrieved document text appears in captured log output, added while
building a RAG ingestion and retrieval path. It was not found by reading the library — which is
rather the point: nothing at the call site suggests the body is logged.

---

## Notes for the reviewer, not for the issue

- **Strip anything Moira-specific before filing.** The suggested body above is already written to be
  generic — no finding IDs, no internal paths, no repository name. Check once more before pasting.
- Moira's local mitigation is `suppresses_provider_payload_logs` in `src/config/telemetry.rs:321`,
  wired into the subscriber stack at `src/config/telemetry.rs:203`, plus the separate OTLP export
  allow-list `exports_only_moira_owned_telemetry` (`src/config/telemetry.rs:280`) which exists
  because the span-level exposure (observation 2 above) is at `INFO` and so cannot be handled by a
  level carve-out. Both carry a documented reversal condition: remove them when `rig-core` gains a
  way to disable or redact this at the source.
- The mitigation had a real gap worth remembering: it was tested for its *predicate* and not its
  *wiring*. Removing the `.with(filter_fn(...))` line from `init` left the whole library test suite
  green. That is now covered by
  `the_payload_log_suppression_is_wired_into_the_subscriber_stack`.
- **A latent hole in Moira's own filter, found while writing this.**
  `PAYLOAD_BEARING_LOG_TARGET_PREFIXES` is `["rig"]`, matched as `target == "rig"` or
  `target.starts_with("rig::")`. The un-targeted Cohere request log lands on
  `rig_core::providers::cohere::completion`, which matches neither, so that one site would not be
  suppressed. **It is not currently exploitable here** — a search of `src/` finds no Cohere
  provider in Moira, so the code path is never reached. Two things follow: do not widen the prefix
  to `rig_core` reflexively (that is a denylist growing by one entry per discovery), and if a
  Cohere variant is ever added to `ProviderType`, this must be handled first. Whether upstream
  fixes the target is worth watching at the next version bump either way.

## Verification notes

Verified for this draft (2026-08-05):

- Version: `Cargo.toml` pins `rig-core = "0.40"`; `Cargo.lock` resolves `rig-core 0.40.0` from
  crates.io with the checksum quoted above. Source read from the Cargo registry checkout.
- Every file/line/target in the table above was read in that checkout, not inferred.
- `target: "rig::completions"` appears at 46 sites across providers (counted by grep); the table
  lists the request/response body ones for the providers Moira uses plus a representative sample of
  others. Not every one of the 46 is a body dump — some open spans.
- The Cohere target inconsistency was established by scanning every multi-line `tracing::trace!(`
  in the crate for a following `target:` line; `providers/cohere/completion.rs:653` is the only
  payload-bearing site without one.
- The `RUST_LOG=myapp=trace,rig=info` claim follows from `EnvFilter`'s documented per-target
  directive precedence and from the fact that these events carry an explicit target; it was
  reasoned from the call sites, **not** executed as a test.
- The `ERROR`-level Gemini body logs and the `info_span!` + `gen_ai.system_instructions` attribute
  were read directly at the lines cited.
- `gen_ai.input.messages` / `gen_ai.output.messages` are declared `field::Empty` at five sites and a
  search of the crate found no `record(...)` call for either — hence the hedged wording.

Not verified:

- The reproduction above was **not compiled or run**; it is written from the call-site reading
  (the trace happens before the HTTP send). Compile it before pasting it into the issue, or replace
  it with a prose description of the steps.
- No check was made of whether this already exists as an upstream issue, or of behaviour in any
  `rig-core` version other than 0.40.0. Search the tracker before filing.
