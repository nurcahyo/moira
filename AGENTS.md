# AGENTS.md

Use `skills/moira-project-structure/SKILL.md` before making structural changes, adding new APIs, touching provider orchestration, modifying credential/security behavior, or reorganizing modules.

Use `.agents/skills/moira-openapi/SKILL.md` whenever adding or changing HTTP routes, API DTOs, parameters, status codes, authentication, headers, streaming, metrics, or API documentation.

Use `.agents/skills/moira-rig-integration/SKILL.md` for any work that touches Rig (`rig-core` 0.40): providers, completions, streaming, tools, agents/RAG, error mapping, or a version bump. It is the hub and routes to the specialist skills:

- `.agents/skills/moira-rig-providers/SKILL.md` — provider clients, base URLs, credentials, Azure endpoints.
- `.agents/skills/moira-rig-completions/SKILL.md` — non-streaming `CompletionRequest`/`CompletionResponse` and usage.
- `.agents/skills/moira-rig-streaming/SKILL.md` — streamed chunks, runtime events, SSE mapping.
- `.agents/skills/moira-rig-tools/SKILL.md` — tool definitions, tool sets, tool loop.
- `.agents/skills/moira-rig-agents-rag/SKILL.md` — agents, extractors, embeddings, vector stores, RAG.
- `.agents/skills/moira-rig-errors-testing/SKILL.md` — `CompletionError` classification, sanitisation, Rig-boundary tests.

Core checks before handoff:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Keep Moira's boundary: Moira orchestrates runtime config, identity claims, credentials, routing, and streaming; Rig owns AI execution primitives.
