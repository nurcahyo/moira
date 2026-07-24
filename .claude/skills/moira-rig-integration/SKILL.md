---
name: moira-rig-integration
description: Authoritative entry point for any work that touches Rig (rig-core 0.40) inside Moira. Defines the Moira/Rig ownership boundary, the RuntimeFactory and RuntimeModelHandle seam, the rule against building a parallel LLM abstraction, where Rig imports are allowed in the module layout, how to verify every API against the vendored crate before writing code, and the rig-core upgrade procedure. Routes to the specialised sibling skills. Use when adding or changing a provider, building or altering a CompletionRequest, touching completion or streaming execution, mapping CompletionError, adding tool or agent/RAG behaviour, bumping the rig-core version, reviewing code that imports rig_core, or whenever you are unsure which Rig skill applies.
---

# Moira Rig Integration

Read and follow `../../../.agents/skills/moira-rig-integration/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
