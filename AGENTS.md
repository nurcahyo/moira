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

Merging between the long-lived branches: a merge of `develop` into `main`, or `main` into `develop`, **must use a merge commit** (`gh pr merge <N> --merge`) — never `--squash`, never `--rebase`. Squashing a sync writes a new commit with no ancestry link to the source branch, which permanently diverges the two branches, makes `git merge-base --is-ancestor` and every "is this merged" check lie, and makes each later promotion re-conflict; PR #102 did exactly this. Feature and plan branches merging into `develop` still squash as normal. Read `plans/CONVENTIONS.md` §1A before merging either direction — GitHub cannot enforce the `main` → `develop` half, so it is on you.

Core checks before handoff:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Keep Moira's boundary: Moira orchestrates runtime config, identity claims, credentials, routing, and streaming; Rig owns AI execution primitives.
