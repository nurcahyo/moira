# Runtime Architecture

Phase 3 adds Moira's internal execution kernel without exposing public prompt APIs.

```mermaid
flowchart TD
    A["ExecutionCommand"] --> B["Execution policy"]
    B --> C["Task router"]
    C --> D["Model router"]
    D --> E["Credential resolver"]
    E --> F["Runtime factory"]
    F --> G["Official Rig CompletionModel"]
    G --> H["Moira runtime events"]
    H --> I["ExecutionOutcome"]
    G --> J["execution_attempts and usage_records"]
```

Moira owns identity, application boundaries, routing policy, credential selection, retries, deadlines, circuit breakers, and normalized persistence. Rig owns provider clients, completion models, completion requests, agents, tools, and provider stream parsing.

No prompt body, raw provider response body, provider secret, raw auth header, or full JWT claims are persisted by Phase 3.
