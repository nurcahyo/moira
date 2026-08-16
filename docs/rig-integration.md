# Rig Integration

Installed dependency: `rig-core = 0.40.0`.

Moira uses official Rig APIs:

- `rig_core::completion::CompletionModel`
- `rig_core::completion::CompletionRequest`
- `rig_core::completion::Message`
- `rig_core::streaming::StreamedAssistantContent`
- `rig_core::client::CompletionClient`
- `rig_core::client::EmbeddingsClient`
- `rig_core::embeddings::{EmbeddingModel, Embedding, EmbeddingError}`
- Provider clients from `rig_core::providers::{openai, anthropic, gemini, deepseek, azure, chatgpt}`

Dispatch strategy: Moira uses a small enum over official Rig completion model types. This keeps static provider typing intact without creating a second generic LLM client abstraction.

Supported in Phase 3:

- `openai`: Rig OpenAI chat completions client
- `openai_compatible`: Rig OpenAI chat completions client with normalized `/v1` base URL
- `local`: same as `openai_compatible`, intended for explicitly allowed local development providers
- `anthropic`: Rig Anthropic completion model. Accepts an `api_key` credential today. An
  `oauth2` credential on this provider type — the shape the containerised runners
  (`docs/claude-runners.md`) and the sidecar path mint — is **subscription-backed**, and
  subscription access carries the individual-use boundary that API keys do not: see
  `docs/claude-subscription-boundary.md`. There is no opt-in gate for it yet, unlike
  `chatgpt_oauth` below ([#307](https://github.com/nurcahyo/moira/issues/307)).
- `gemini`: Rig Gemini completion model
- `deepseek`: Rig DeepSeek completion model
- `azure_openai`: Rig Azure OpenAI completion model
- `chatgpt_oauth`: rig-core 0.40's native ChatGPT-subscription provider
  (`rig_core::providers::chatgpt`, `ResponsesCompletionModel`), reusing the OpenAI Responses API
  engine against `chatgpt.com/backend-api/codex`. **Refused unless
  `provider_security.allow_chatgpt_subscription = true`** — off by default in every environment,
  including production. This is a deliberate ToS risk-acceptance opt-in, not a sanctioned
  integration path: ChatGPT/Codex subscriptions are personal, single-user under OpenAI's terms,
  with no carve-out for third-party, multi-tenant use (issue #216,
  `docs/chatgpt-subscription-spike.md`). Credential = `credential_type: oauth2`, the access
  token read from the `access_token` payload field, mirrored into
  `chatgpt::ChatGPTAuth::AccessToken` — a thin, fully-expressible construction with no file I/O
  and no device-code flow. `chatgpt::ChatGPTAuth::OAuth` (rig-core's own local-file/device-code
  login) is never constructed; it is the wrong shape for a multi-tenant server process. Does
  **not** emit `output_schema`: `chatgpt::ResponsesCompletionModel::create_request`
  unconditionally clears `additional_parameters.text` (and `temperature`, `max_output_tokens`,
  and several `additional_params` keys) on every request, so `structured_output` is never a
  satisfiable routing capability for this provider (`application/execution.rs`,
  `provider_emits_output_schema`).

Configured but not executable:

- `custom`

Partially supported:

- agent profiles are persisted and loaded for preamble/temperature/max-token defaults, but side-effecting tools remain disabled.

## Embeddings

Verified against the vendored `rig-core 0.40.0` rather than assumed. Implemented in
`src/orchestration/embedding.rs`, the embedding twin of `src/orchestration/runtime_factory.rs`
and a widening of the same Rig seam — `tests/rig_boundary.rs` lists both files explicitly.

Surface actually used:

- `rig_core::embeddings::EmbeddingModel` — `const MAX_DOCUMENTS: usize`, `type Client`,
  `fn make(&Client, impl Into<String>, Option<usize>)`, `fn ndims(&self) -> usize`,
  `async fn embed_texts(impl IntoIterator<Item = String>) -> Result<Vec<Embedding>, EmbeddingError>`.
- `rig_core::embeddings::Embedding { document: String, vec: Vec<f64> }`. Note **`f64`**, while
  pgvector stores `float4`; the narrowing happens once, at the boundary.
- `rig_core::client::EmbeddingsClient::embedding_model_with_ndims`, blanket-implemented for
  `Client<Ext, H>` wherever `Ext: Capabilities<H, Embeddings = Capable<M>>`.

Dispatch is an enum over the concrete Rig embedding model types, for the same reason the
completion side uses one and additionally a hard one: `EmbeddingModel` carries an associated
`const` and an associated type, so it is not object-safe and `Box<dyn EmbeddingModel>` does not
compile.

### Embedding support by provider

`Capabilities::Embeddings` in rig-core 0.40, checked per provider module:

- `openai`: **supported** (`Capable`, on both the responses and the completions extension)
- `openai_compatible`: **supported** (same client)
- `local`: **supported** (same client)
- `azure_openai`: **supported** (`Capable`)
- `gemini`: **supported** (`Capable`)
- `deepseek`: **not supported** — `type Embeddings = Nothing`
- `chatgpt` (`chatgpt_oauth`): **not supported** — `type Embeddings = Nothing`
- `anthropic`: **not supported** — exposes no embedding model at all
- `custom`: not executable, as for completions

An application whose embedding policy names a non-embedding provider gets an explicit refusal
(`embedding_provider_unsupported`) and its documents ingest to `'failed'`, never a silent skip
that would leave the operator believing retrieval works.
