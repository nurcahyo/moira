# Provider Pools

Moira models provider pools through routing policies, not a second provider abstraction.

```mermaid
flowchart TD
    A["Route"] --> B["Policy 1"]
    A --> C["Policy 2"]
    A --> D["Policy 3"]
    B --> E["Provider/model/credential"]
    C --> F["Provider/model/credential"]
    D --> G["Provider/model/credential"]
```

Every pool member remains subject to provider status, model status, credential scope, runtime policy, capacity, and circuit state. A user-scoped credential is never shared across another user's effective pool.
