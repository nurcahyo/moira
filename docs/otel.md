# OpenTelemetry

Moira can bridge its `tracing` spans to an OTLP collector. Export is **off by default**.

```text
MOIRA_TELEMETRY__OTEL_ENABLED=true
MOIRA_TELEMETRY__OTEL_ENDPOINT=http://otel-collector:4318
```

The exporter speaks **OTLP/HTTP** (`http-proto`), so the endpoint is the HTTP port — `4318`, not the
gRPC `4317`. A base URL gets `/v1/traces` appended; a URL that already carries a path is taken as a
fully specified signal endpoint and used as-is. `otel_enabled=true` with no endpoint is a startup
error rather than a silent no-op, because a pipeline that is switched on but exports nowhere is
worse than one that refuses to start.

## What is exported

**Only spans and events whose `tracing` target is Moira's own** — `moira` and `moira::*`. Everything
else is dropped at the bridge layer. Today that means two spans:

| Span | Source | Level |
|---|---|---|
| `http_request` | `src/lib.rs` (`redacted_request_span`) | `DEBUG` |
| `execution_attempt` | `src/application/execution.rs` | `DEBUG` |

Both are `DEBUG`, and the shipped `env_filter` is `moira=info,tower_http=info`, so flipping
`OTEL_ENABLED=true` alone produces an **empty trace stream**. Widen the filter as well:

```text
MOIRA_TELEMETRY__ENV_FILTER=moira=debug,tower_http=debug
```

Extending coverage to SQL, Redis, routing, retrieval, streaming and workers is tracked in
[docs/todo.md](./todo.md); each of those spans has to be Moira's own to be exported.

## What is not exported, and why you cannot turn it back on

Third-party spans never leave the process, **at any level, under any filter**.

This matters because of what is on them. `rig-core` 0.40 opens an `info_span!` on target
`rig::completions` carrying `gen_ai.system_instructions` — the system preamble — as a span
attribute, and after plan 11 Moira's assembled context can include retrieved RAG chunk and memory
text belonging to documents the caller never typed. `tracing-opentelemetry` bridges every span the
subscriber records, so before this guard existed `env_filter` was the only thing between that
attribute and a remote collector. Note the level: `info_span!`. A bare `info` — not `debug`, not
`trace` — was already enough.

`src/config/telemetry.rs` therefore applies an **allow-list of Moira-owned target roots to the
bridge layer itself**. A global filter can only ever narrow what reaches a layer, so the allow-list
sits below `env_filter` and no value of `MOIRA_TELEMETRY__ENV_FILTER` or `RUST_LOG` can widen it
back open. Turning your log level up to debug Moira does not start shipping prompts.

Two consequences worth knowing:

- **Log output is unchanged.** The allow-list is on the OTLP bridge only. Third-party warnings and
  errors still reach stdout exactly as `env_filter` says they should. What changes is that they no
  longer reach the *collector*; provider outcomes reach it on Moira's own `execution_attempt` span.
- **Adding a dependency's spans to your traces is a deliberate edit**, not a configuration change:
  add the target root to `EXPORTABLE_TARGET_ROOTS` in `src/config/telemetry.rs`, after reading what
  that crate actually puts on the span. A new dependency that starts emitting spans is excluded
  until somebody does that.

There is a separate, log-side suppression for the same class of problem: `rig-core` also logs the
entire completion request body at `TRACE`. See [docs/rag-security.md](./rag-security.md).

## Resource attributes

The only Moira-supplied resource attribute is `service.name`, from
`MOIRA_TELEMETRY__SERVICE_NAME`. Nothing request-scoped, caller-supplied or secret is added.

The configured endpoint is kept out of every error string this module produces — an endpoint can
carry an auth token in userinfo or a query parameter, and `opentelemetry-otlp`'s own `InvalidUri`
error echoes the URI verbatim, so exporter build failures are reduced to `scheme://host:port` first.

Logs and traces must never include prompts, secrets, JWTs, provider keys, embeddings, or raw
retrieved context.

## Shutdown

Spans are buffered by a `BatchSpanProcessor` and exported on a timer. The final batch is only
flushed if the tracer provider is shut down explicitly, so `init` returns a `TelemetryGuard` the
process must hold and shut down before exit. Dropping it is not sufficient. The flush is bounded at
5 seconds: an unreachable collector may delay exit, never block it.
