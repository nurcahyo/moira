# AGENTS.md

Use `skills/moira-project-structure/SKILL.md` before making structural changes, adding new APIs, touching provider orchestration, modifying credential/security behavior, or reorganizing modules.

Use `.agents/skills/moira-openapi/SKILL.md` whenever adding or changing HTTP routes, API DTOs, parameters, status codes, authentication, headers, streaming, metrics, or API documentation.

Core checks before handoff:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Keep Moira's boundary: Moira orchestrates runtime config, identity claims, credentials, routing, and streaming; Rig owns AI execution primitives.
