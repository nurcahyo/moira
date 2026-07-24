# Memory Extraction

Memory extraction is modeled through `memory_extraction_runs` and source references on `memory_records`.

```mermaid
flowchart TD
    A["Completed response"] --> B["Extraction policy"]
    B --> C["Existing execution service"]
    C --> D["Validated candidates"]
    D --> E["Memory records"]
```

Automatic extraction is not active yet. Explicit memory creation rejects secret-like content and records safe audit metadata.

