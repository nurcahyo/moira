# Task Routing

Task routing answers what kind of task should run.

```mermaid
flowchart TD
    A["Route hint?"] -->|Authorized| B["Active route by key"]
    A -->|Missing| C["Deterministic rules"]
    C --> D["coding route if configured"]
    C --> E["default route"]
    E --> F["general route preferred"]
```

Precedence:

1. Authorized explicit route hint.
2. Deterministic rule match.
3. Active default route, preferring `general`.
4. Routing failure.

Route definitions live in `route_definitions`. They do not own instantiated Rig agents.
