# Runtime Diagnostics

Phase 3 does not expose a public prompt API.

Available diagnostic surfaces:

- CLI: `cargo run -- execute-test -- --prompt "Hello" --route general`
- Optional admin endpoint: `POST /api/v1/admin/runtime/diagnose`

The endpoint is disabled by default with `runtime.diagnostic_endpoint_enabled = false` and requires `moira:runtime:diagnose` when enabled.

Both diagnostics use normal route/model/credential resolution and never print or return provider secrets. They may return normalized runtime events and execution outcomes for operator troubleshooting.
