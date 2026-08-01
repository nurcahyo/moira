# Draft: upstream issue for `rig-core` — completion request bodies are logged at TRACE

**Status: not filed.** This is a draft for a human to review and submit to
[`0xPlaygrounds/rig`](https://github.com/0xPlaygrounds/rig). It is written from ledger finding
**F16**; Moira's local mitigation is in `src/config/telemetry.rs` and is deliberately a stopgap.

**Why a human files it:** it goes under a person's name and may invite discussion the maintainers
expect a person to carry. Everything factual below has been verified against `rig-core` 0.40 as
consumed by this repository.

---

## Suggested title

> `rig::completions` logs the full completion request body at TRACE, with no way to disable or redact it

## Suggested body

**Version:** `rig-core` 0.40

### What happens

`rig-core` emits the **entire completion request body** — every message, verbatim — on the
`rig::completions` target at `TRACE`. There is no feature flag, no redaction hook, and no way to
suppress it at the source: the only lever a consumer has is the global log filter.

### Why that is a problem beyond "don't enable TRACE"

For a library whose payloads are prompts, the log level is the *only* barrier between an operator
debugging their own code and a stream of user content. That is already uncomfortable when the
payload is a prompt the caller typed.

It becomes a different class of problem once the request carries **retrieved** content. In a
RAG or memory-augmented application the assembled `CompletionRequest` contains passages the caller
never wrote and, in a multi-tenant deployment, may have no right to see outside the request that
produced them. An operator raising a log level to debug routing then exports other tenants' document
text to wherever logs go — with no indication that this is what raising the level does.

The trigger is ordinary: someone sets `RUST_LOG=trace` or a broad `debug` while chasing an unrelated
bug.

### What a consumer currently has to do

Suppress it below the filter, so it holds regardless of how the operator configures logging:

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
```

This works, but it is the wrong place for it. A consumer has to know the hazard exists, know the
target name, and re-derive the filter — and every consumer that does not, ships the exposure. It
also blanket-drops *all* verbose `rig` events, including ones that would be genuinely useful for
debugging, because there is no way to distinguish payload-bearing events from the rest.

### What would help, roughly in order of preference

1. **Do not log request bodies by default.** Put them behind an explicit opt-in — a feature flag
   such as `log-request-bodies`, or a builder method — so the default is safe and enabling it is a
   deliberate act.
2. **A redaction hook**: let the consumer supply a closure that decides what, if anything, of a
   request body reaches the log.
3. **A dedicated target** such as `rig::completions::payload`, distinct from operational events.
   That alone would let consumers filter precisely instead of silencing `rig` wholesale, and is
   probably the cheapest change that materially helps.
4. At minimum, **document it** — that TRACE on this target emits full request bodies, and what that
   means for applications whose requests carry retrieved or user-derived content.

Happy to open a PR for any of these if a maintainer indicates which direction is welcome.

### How it was found

A canary test asserting that no retrieved document text appears in captured log output, added while
building a RAG ingestion and retrieval path. It was not found by reading the library — which is
rather the point: nothing at the call site suggests the request body is logged.

---

## Notes for the reviewer, not for the issue

- **Do not include Moira's finding IDs, internal file paths, or repository details.** The snippet
  above is deliberately generic.
- The local mitigation had a real gap worth remembering: it was tested for its *predicate* and not
  its *wiring*. Removing the `.with(filter_fn(...))` line from `init` left all 598 library tests
  green. The wiring is now covered.
- If a maintainer asks for a reproduction, the smallest one is a `CompletionRequest` with a
  distinctive string in a non-first message, `RUST_LOG=trace`, and a capturing subscriber.
