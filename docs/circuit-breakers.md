# Circuit Breakers

Circuit breakers are in-memory per provider/model.

```mermaid
stateDiagram-v2
    [*] --> Closed
    Closed --> Open: threshold reached
    Open --> HalfOpen: open duration elapsed
    HalfOpen --> Closed: probe success
    HalfOpen --> Open: probe failure
```

Counted failures:

- connection failure
- timeout
- provider 5xx/unavailable
- repeated rate limiting
- invalid upstream response

Not counted:

- caller cancellation
- caller authorization failure
- invalid request
- unsupported capability
- Moira configuration validation failure

Circuit state is not synchronized across service instances in Phase 3.
