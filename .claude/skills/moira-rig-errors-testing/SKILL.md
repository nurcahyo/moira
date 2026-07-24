---
name: moira-rig-errors-testing
description: Map rig-core 0.40 failures onto Moira's ExecutionFailure, ExecutionFailureClass and AppError, and test the Rig boundary. Covers every CompletionError variant (plus EmbeddingError, ToolError, ToolSetError, PromptError, StructuredOutputError), the status-first classification rule and its substring fallback, retry / fallback / circuit-breaker derivation, the committed-output override, the sanitisation contract that keeps provider bodies and secrets out of public messages, what may and may not be logged at the Rig boundary, and the four test levels the repo uses — pure classification unit tests, network-free client construction with rig-core test-utils, the scripted OpenAI-compatible Axum server, and the Postgres lifecycle fixture. Use when changing classify_completion_error, adding or remapping an ExecutionFailureClass, altering failure_http_status or failure_code, touching safe_provider_error_message or safe_config_error, deciding whether a failure is retryable or fallback-eligible, adding tracing around provider calls, or writing, fixing, or reviewing any test that exercises rig_core.
---

# Moira Rig Errors and Testing

Read and follow `../../../.agents/skills/moira-rig-errors-testing/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
