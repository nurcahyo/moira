# Rig Integration

Installed dependency: `rig-core = 0.40.0`.

Moira uses official Rig APIs:

- `rig_core::completion::CompletionModel`
- `rig_core::completion::CompletionRequest`
- `rig_core::completion::Message`
- `rig_core::streaming::StreamedAssistantContent`
- `rig_core::client::CompletionClient`
- Provider clients from `rig_core::providers::{openai, anthropic, gemini, deepseek, azure}`

Dispatch strategy: Moira uses a small enum over official Rig completion model types. This keeps static provider typing intact without creating a second generic LLM client abstraction.

Supported in Phase 3:

- `openai`: Rig OpenAI chat completions client
- `openai_compatible`: Rig OpenAI chat completions client with normalized `/v1` base URL
- `local`: same as `openai_compatible`, intended for explicitly allowed local development providers
- `anthropic`: Rig Anthropic completion model
- `gemini`: Rig Gemini completion model
- `deepseek`: Rig DeepSeek completion model
- `azure_openai`: Rig Azure OpenAI completion model

Configured but not executable:

- `custom`

Partially supported:

- agent profiles are persisted and loaded for preamble/temperature/max-token defaults, but side-effecting tools remain disabled.
