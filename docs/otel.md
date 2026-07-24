# OpenTelemetry

Phase 6 adds configuration for OpenTelemetry:

```text
MOIRA_TELEMETRY__OTEL_ENABLED=true
MOIRA_TELEMETRY__OTEL_ENDPOINT=http://otel-collector:4317
```

Current tracing remains `tracing`-based JSON or pretty logs. The production target is a trace with spans for:

- HTTP
- auth
- routing
- retrieval
- Rig execution
- provider calls
- persistence
- streaming
- background workers

Logs and traces must never include prompts, secrets, JWTs, provider keys, embeddings, or raw retrieved context.

Exporter wiring is tracked in `docs/todo.md`.
