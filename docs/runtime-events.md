# Runtime Events

Moira exposes an internal event contract for future transports.

```mermaid
flowchart TD
    A["Rig stream"] --> B["Moira mapper"]
    B --> C["RuntimeEventEnvelope"]
    C --> D["Bounded channel"]
```

Events include execution start, routing start, route selected, agent profile unavailable, model selected, provider attempt started, output text delta, tool call markers, usage update, provider attempt failure, fallback selected, execution completed, and execution failed.

`agent_profile_unavailable` is an **operator** signal and is the only one of these emitted for a condition that does not change the request's outcome. It fires when the selected route's `agent_profile_id` is set but the profile no longer resolves — it has been disabled or soft-deleted, and neither operation clears the route's reference — so the execution proceeds without that profile's `preamble`, `temperature` and `max_tokens` and still reports `succeeded`. A route that simply has no profile is the normal case and emits nothing. Whether Moira should also *refuse* such a request is an open product decision (fail-closed versus observable fail-open); the observability is the part both answers share, so it ships ahead of the decision. The same condition also writes a `warn!` and an `agent_profile.unavailable` audit row against the execution id.

Not every runtime event is a public SSE event. `map_runtime_event` in `src/application/public.rs` is exhaustive over this enum and drops the internal terminal events, the tool-call markers and `agent_profile_unavailable` — the last because its payload names an internal route and agent profile, which is admin-plane shape a caller cannot act on. It reaches operators through `POST /api/v1/admin/runtime/diagnose`, which returns every envelope verbatim.

Events include request id, execution id, monotonic sequence, timestamp, event type, and safe payload. They do not include secrets, raw provider headers, or raw upstream error bodies.
