# Public Authorization

Authorization is deny-by-default and scope based.

- `moira:responses:create` for `POST /api/v1/responses`
- `moira:responses:stream` for `POST /api/v1/responses/stream`
- `moira:responses:read` for response reads
- `moira:executions:read` for execution reads/lists
- `moira:usage:read` for usage queries
- `moira:models:read` for model discovery
- `moira:routes:read` for route discovery
- `moira:capabilities:read` for capability discovery
- `moira:execution-policies:read` and `moira:execution-policies:write` for admin execution policy

Override scopes are separate: `moira:execution:override-route`, `moira:execution:override-model`, `moira:execution:override-provider`, `moira:execution:override-credential`, `moira:execution:override-timeout`, and `moira:execution:use-tools`.

Consumer callers are isolated to their bound application and delegated identity. Cross-application resources are hidden or denied.

Only system keys and the development admin actor may use global public read
privileges. Standalone trusted JWT callers must be bound to one internal
application UUID. Missing application bindings are denied on public routes, and
malformed configured application claims fail authentication instead of becoming
unrestricted access. Conversation and memory reads follow the same rule.
Tenant-specific models and routes are visible only to callers with the matching
tenant identity; tenant-less callers see only policies without a tenant binding.
Active route definitions without an executable routing policy are not exposed to
non-privileged callers.
