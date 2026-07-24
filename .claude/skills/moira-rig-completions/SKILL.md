---
name: moira-rig-completions
description: Expert guidance for non-streaming completion execution through Rig (rig-core 0.40) in Moira. Covers CompletionModel usage, field-by-field CompletionRequest semantics and how each provider actually encodes those fields on the wire, Message/UserContent/AssistantContent/OneOrMany construction from Moira's public DTOs, reading CompletionResponse, token-usage extraction into UsageSummary, safe use of additional_params, and parameter/determinism policy. Use when building or changing a CompletionRequest, adding or altering request parameters, mapping caller messages into rig_core::completion::Message, adding an output_schema or provider-specific parameter, reading choice/raw_response/usage, changing usage_from_rig, or debugging why a request field did not reach the provider.
---

# Moira Rig Completions

Read and follow `../../../.agents/skills/moira-rig-completions/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
