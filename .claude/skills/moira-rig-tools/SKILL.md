---
name: moira-rig-tools
description: Define, register, and execute LLM tools through rig-core 0.40 inside Moira. Covers the exact 0.40 `Tool` trait (NAME, Args, Output, Error, description, parameters, call, call_with_extensions, call_structured, classify_error), JSON Schema authoring for tool parameters, ToolSet/ToolSetBuilder/ToolServer assembly, wiring `ToolDefinition` and `ToolChoice` into `CompletionRequest`, the multi-turn tool loop and turn budgets, tool-result round-trip into chat history, dynamic (RAG-retrieved) tools, tool failure/timeout classification, mapping tool errors into `ExecutionFailure`/`AppError`, and the security rules that keep credentials and internal prompts out of tool surfaces. Use when adding or changing a tool, enabling tool calling on an execution path, populating `CompletionRequest.tools` or `tool_choice`, handling `ToolCallStarted`/`ToolCallDelta`/`ToolCallCompleted`/`ToolResult` runtime events, exposing tool results over the public API, building an agent tool loop, or reviewing any code that imports `rig_core::tool`.
---

# Moira Rig Tools

Read and follow `../../../.agents/skills/moira-rig-tools/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
