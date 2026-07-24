---
name: moira-rig-providers
description: Build and configure Rig (rig-core 0.40) provider clients inside Moira's RuntimeFactory. Covers the generic Client<Ext, H> builder surface and its type-state call order, per-provider construction for openai, anthropic, gemini, deepseek, and azure, base-URL normalisation rules including normalize_openai_base_url, Azure api-version and deployment semantics, credential injection through secrecy with the redaction invariants, custom reqwest backends and timeouts, the RuntimeModelHandle enum dispatch pattern, and the end-to-end workflow for adding a new provider variant. Use when adding or changing a ProviderType arm in build_completion_model, altering base_url or credential-type gating, wiring Azure endpoints, choosing between a native and an OpenAI-compatible provider, attaching a custom HTTP client, or reviewing anything under src/orchestration/runtime_factory.rs.
---

# Moira Rig Providers

Read and follow `../../../.agents/skills/moira-rig-providers/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
