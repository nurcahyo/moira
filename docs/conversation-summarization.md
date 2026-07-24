# Conversation Summarization

Conversation summaries are modeled as immutable records in `conversation_summaries` with coverage boundaries and supersession.

```mermaid
flowchart TD
    A["Message threshold"] --> B["Existing execution service"]
    B --> C["Summary draft"]
    C --> D["Immutable summary version"]
```

Automatic summarization execution is not enabled yet. The schema and policy fields are in place for a summarizer that uses the existing execution service, not direct provider calls.

