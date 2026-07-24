---
name: moira-rig-agents-rag
description: Decide whether Moira should use a rig-core 0.40 Agent, Extractor, or vector-store abstraction at all, and wire it correctly when the answer is yes. Covers AgentBuilder configuration (preamble, static and dynamic context, tools, turn budget, output schema, memory), the Prompt/Chat/TypedPrompt traits and PromptRequest, Extractor and OutputMode structured output with schemars, embeddings (EmbeddingModel, EmbeddingsBuilder, Embed derive), vector stores (VectorStoreIndex, InMemoryVectorStore, the pgvector path over Moira's own tables instead of rig-postgres), end-to-end RAG wiring, and the removal of the pipeline module in 0.40.0. Use when considering an Agent, Extractor, embeddings, a vector store, retrieval-augmented prompting, conversation memory, memory extraction, conversation summarisation, or RAG ingestion anywhere in Moira, and read it before adding any of these — the default answer for the public response path is to stay at the CompletionModel level.
---

# Moira Rig Agents and RAG

## Core Rule

`Agent`, `Extractor`, and the vector-store traits are optional layers Rig builds **on top of** `CompletionModel`. Moira executes at the `CompletionModel` level and the public `/api/v1/responses` path must stay there. Introducing an `Agent` moves the run loop — retries, turn budget, tool execution, continuation — from Moira into Rig, and every Moira guarantee attached to a single provider call moves with it.

Adopt an Agent only where Moira genuinely wants Rig to own the loop, and only where the whole loop can be accounted for as **one** provider attempt. Everywhere else, keep building `CompletionRequest` and calling `RuntimeModelHandle`.

Read `.agents/skills/moira-rig-integration/SKILL.md` first. It defines the boundary this skill operates inside; nothing here overrides it. Never introduce a parallel LLM abstraction over `Agent` any more than over `CompletionModel`.

## 0.40.0 Reality Check

Verify before you write. These are the traps that come from pre-0.40 docs, blog posts, and `main`-branch examples.

| Claim you may have seen | Truth in rig-core 0.40.0 |
|---|---|
| `rig::pipeline`, `Op`, `TryOp`, `parallel!`, `agent_ops` | **Removed.** `src/lib.rs:145-177` lists no `pipeline` module; `grep -rn pipeline src/` returns only prose doc-comments. CHANGELOG 0.40.0: "remove the experimental pipeline module (#1941)". Compose in Moira with `async fn` plus `tokio::try_join!` / `futures::future::try_join_all`. |
| `PromptRequest::multi_turn(n)` / `StreamingPromptRequest::multi_turn(n)` | Gone; `grep -rn multi_turn src/` hits only test names. Use `max_turns(n)` (`src/agent/prompt_request/mod.rs:153`) or `AgentBuilder::default_max_turns(n)` (`src/agent/builder.rs:200`). CHANGELOG 0.40.0 marks this breaking: the value is now a **total model-call budget including the initial call**, so a tool-then-answer flow needs `2`, not `1`. |
| `index.top_n(query, n)` | `top_n<T>(req: VectorSearchRequest<Self::Filter>)` (`src/vector_store/mod.rs:88`). The trait also has `type Filter: SearchFilter`. |
| `VectorSearchRequest::builder()…build()?` | `build()` is **infallible** (`src/vector_store/request.rs:327`); a typestate guarantees `query` and `samples`. Upstream comments still show `build()?` — they are stale. |
| "any provider can embed" | `EmbeddingModel` exists for `openai`, `azure`, `gemini` only. `anthropic` and `deepseek` have none. |
| `rig-postgres` drops into Moira's schema | Its table layout is hardcoded to `id, document, embedded_text, embedding`; only the table name is configurable. It cannot express Moira's schema. See "pgvector" below. |

Vendored authority: `/Users/nalhide/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.40.0`. Upstream says `rig::…` because of `extern crate self as rig;`; in Moira write `rig_core::…`.

## Decision Rule: Agent vs Raw CompletionModel

Stay on `RuntimeModelHandle` (raw `CompletionModel`) when **any** of these is true:

- The call serves `/api/v1/responses`, streaming or not.
- The caller supplies the message list, and Moira must forward it verbatim.
- Moira must own retry, provider fallback, per-attempt events, the stream idle timeout, backpressure, the committed-output rule, or circuit-breaker sampling at the granularity of a single provider call.
- You only need static instructions or pre-retrieved context — fold them into `CompletionRequest.preamble` / `.documents` instead.

Reach for `Agent` only when **all four** hold:

1. The behaviour requires a multi-turn tool loop (tool call → tool result → continuation) that Moira does not implement and does not want to implement.
2. The entire loop can be treated as one provider attempt: one concurrency permit, one deadline, one circuit-breaker sample, one usage record, one `ExecutionAttemptUpdate`.
3. The preamble, tool set, and retrieval configuration are server-owned runtime config — never caller input.
4. The result is not the public streaming response contract.

That intersection in Moira is the **internal, non-public work**: memory extraction into `memory_records` (`memory_extraction_runs`), conversation summarisation (`conversation_summaries`), RAG ingestion enrichment (`rag_ingestion_runs`), and context planning (`context_plans`). Those are background jobs behind Moira's own job accounting, not requests on the response path.

Never appropriate:

- Replacing `RuntimeModelHandle::completion` or `::start_stream`.
- Any path where the caller controls `preamble`, `tools`, or the retrieval index.
- Streaming to the public SSE contract via `StreamingPromptRequest`. Moira's `RuntimeStreamItem` mapping is defined over `StreamedAssistantContent`; the agent path emits `MultiTurnStreamItem<R>` (`src/agent/prompt_request/streaming.rs:47`), a different type with a different item vocabulary. See `.agents/skills/moira-rig-streaming/SKILL.md`.

### What an Agent Costs You

| Moira guarantee | Under `CompletionModel` (today) | Under `Agent` |
|---|---|---|
| Retry / backoff | per provider call, Moira-owned | per whole loop; Rig re-calls internally without Moira seeing it |
| Provider fallback | per attempt, pre-output only | only after the loop returns |
| Per-attempt runtime events | one attempt = one call | one attempt = N calls; use `PromptResponse::completion_calls()` to reconstruct |
| Stream idle timeout, backpressure, committed-output rule | enforced per stream item | not available on the non-streaming prompt path |
| Circuit-breaker sampling | one sample per provider call | one sample per loop |
| Error classification | `CompletionError` → `classify_completion_error` | `PromptError`, a superset; needs its own mapping arm |
| Turn budget | not applicable | **must be set explicitly** or the implicit budget is 1 model call |

If you cannot accept every row, you do not want an `Agent`.

## AgentBuilder

`AgentBuilder<M, ToolState>` (`src/agent/builder.rs`) is a typestate over tool configuration: `NoToolConfig` (default) → `WithBuilderTools` (via `tool`/`tools`/`dynamic_tools`) or `WithToolServerHandle` (via `tool_server_handle`). The two tool states are mutually exclusive at the type level.

Available in every state (`impl<M, ToolState>`, `builder.rs:136-286`):

| Method | Effect |
|---|---|
| `name(&str)`, `description(&str)` | Logging identity; `description` is for sub-agent composition |
| `preamble(&str)`, `without_preamble()`, `append_preamble(&str)` | System prompt; `append_preamble` joins with `\n` |
| `context(&str)` | Push a static `Document` with auto id `static_doc_{n}` |
| `dynamic_context(sample, index)` | Retrieve `sample` documents per prompt from a `VectorStoreIndexDyn` |
| `tool_choice(ToolChoice)` | `rig_core::completion::message::ToolChoice` |
| `default_max_turns(usize)` | Total model-call budget, including the initial call |
| `temperature(f64)`, `max_tokens(u64)`, `additional_params(Value)` | Request parameters |
| `output_schema::<T>()`, `output_schema_raw(Schema)`, `output_mode(OutputMode)` | Structured output |
| `memory(impl ConversationMemory)`, `conversation(id)` | Conversation history backend |
| `add_hook(impl AgentHook<M>)` | Hook stack (advanced; out of scope for Moira today) |

State transitions live on `impl<M> AgentBuilder<M, NoToolConfig>`: `new(model)` (`:293`), `tool` (`:354`), `tools` (`:385`), `dynamic_tools` (`:519`), `tool_server_handle` (`:325`), `build() -> Agent<M>` (`:553`). `rmcp_tool*` requires the non-default `rmcp` feature; Moira does not enable it.

`AgentBuilder::new(model)` is the constructor to use — Moira already holds a concrete `CompletionModel` inside `RuntimeModelHandle`. `CompletionClient::agent(model_key)` (`src/client/completion.rs:50`) is only `AgentBuilder::new(self.completion_model(model))`; it needs a live client, which `build_completion_model` does not keep, so reaching for it means re-resolving the credential and rebuilding a client outside `ProviderRuntimeCache`. It is also the wrong model for OpenAI: `openai::Client` is `Client<OpenAIResponsesExt, _>` and its `Self::CompletionModel` is the Responses-API model, whereas Moira calls `.completions_api()` first and stores `openai::completion::CompletionModel` (= `GenericCompletionModel<OpenAICompletionsExt, _>`). Wrap the handle you already have.

`Agent<M>` is generic over `M` and `CompletionModel` is not object-safe, so an agent per provider needs the same enum treatment `RuntimeModelHandle` already uses. There is no `Box<dyn>` shortcut.

```rust
use rig_core::agent::{Agent, AgentBuilder};
use rig_core::completion::CompletionModel as RigCompletionModel;
use rig_core::providers::{anthropic, azure, deepseek, gemini, openai};

/// Server-owned agent configuration. Never populated from caller input.
pub struct InternalAgentSpec {
    pub preamble: String,
    pub retrieval_samples: usize,
    pub max_model_calls: usize,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
}

pub enum RuntimeAgentHandle {
    OpenAi(Agent<openai::completion::CompletionModel>),
    Anthropic(Agent<anthropic::completion::CompletionModel>),
    Gemini(Agent<gemini::completion::CompletionModel>),
    DeepSeek(Agent<deepseek::CompletionModel>),
    AzureOpenAi(Agent<azure::CompletionModel>),
}

impl std::fmt::Debug for RuntimeAgentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variant = match self {
            Self::OpenAi(_) => "OpenAi",
            Self::Anthropic(_) => "Anthropic",
            Self::Gemini(_) => "Gemini",
            Self::DeepSeek(_) => "DeepSeek",
            Self::AzureOpenAi(_) => "AzureOpenAi",
        };
        write!(f, "RuntimeAgentHandle::{variant}(<redacted>)")
    }
}

fn build_internal_agent<M, I>(model: M, spec: &InternalAgentSpec, index: I) -> Agent<M>
where
    M: RigCompletionModel,
    I: rig_core::vector_store::VectorStoreIndexDyn + Send + Sync + 'static,
{
    let mut builder = AgentBuilder::new(model)
        .preamble(&spec.preamble)
        .dynamic_context(spec.retrieval_samples, index)
        .default_max_turns(spec.max_model_calls);

    if let Some(temperature) = spec.temperature {
        builder = builder.temperature(temperature);
    }
    if let Some(max_tokens) = spec.max_tokens {
        builder = builder.max_tokens(max_tokens);
    }

    builder.build()
}
```

The hand-written `Debug` is not optional. `Agent<M>` is `#[derive(Clone)] #[non_exhaustive]` and implements no `Debug` (`src/agent/completion.rs:641-643`), so `#[derive(Debug)]` on anything holding one does not compile. Write it and redact. For reference, the underlying `Client<Ext, H>` does have a hand-written `Debug` (`src/client/mod.rs:188`) that drops `Authorization` and `*api-key*` headers but still prints `base_url` — never widen a Moira `Debug` to reach it.

## Prompt, Chat, TypedPrompt, and PromptRequest

`Agent<M>` implements `Prompt`, `Chat`, `TypedPrompt`, `Completion<M>`, and the streaming variants (`src/agent/completion.rs:703-910`).

- `Prompt::prompt(prompt)` returns **`PromptRequest<Standard, M>`**, a builder that is `IntoFuture` — not a bare future. Chain configuration before `.await`.
- `Chat::chat(prompt, &mut Vec<Message>)` appends the prompt and every message produced during the turn into `chat_history`. Callers must **not** push the user prompt themselves.
- `TypedPrompt::prompt_typed::<T>()` returns a `TypedPromptRequest`, resolving to `Result<T, StructuredOutputError>` (`rig_core::completion::StructuredOutputError`, `src/completion/request.rs:249`) — a *different* error type from `PromptError`, so it needs its own mapping arm.

`PromptRequest` setters (`src/agent/prompt_request/mod.rs`): `max_turns`, `history`, `conversation`, `without_memory`, `tool_extensions`, `tool_concurrency`, `max_invalid_tool_call_retries`, `add_hook`, `extended_details`.

**Always set the turn budget.** `max_turns` doc, verbatim: "Set the total model-call budget, including the initial call and every retry or continuation. Zero emits no model calls; one permits only the initial call. Exceeding the budget returns `PromptError::MaxTurnsError`." With no `default_max_turns` the implicit budget is one call, so a tool-call-then-answer flow silently fails unless the budget is ≥ 2.

**Always use `.extended_details()`** on any path that must account for usage. `PromptResponse` exposes `output()`, `usage()`, `messages()`, `content()`, `completion_calls()`, `requests()`. `Usage` implements `Add`/`AddAssign`, so aggregation across turns is free, and `usage_from_rig` in `runtime_factory.rs` remains the single `Usage → UsageSummary` conversion point.

```rust
use rig_core::agent::{Agent, PromptResponse};
use rig_core::completion::{CompletionModel as RigCompletionModel, Message, Prompt, PromptError};

async fn run_internal_agent<M>(
    agent: &Agent<M>,
    prompt: Message,
    history: Vec<Message>,
    max_model_calls: usize,
) -> Result<PromptResponse, PromptError>
where
    M: RigCompletionModel + 'static,
{
    agent
        .prompt(prompt)
        .history(history)
        .max_turns(max_model_calls)
        .without_memory()
        .extended_details()
        .await
}
```

`tool_extensions(ToolCallExtensions)` is the out-of-band per-call channel: values reach `Tool::call_with_extensions` **without ever entering the model context**. That is the only correct way to pass Moira's tenant id, application id, identity claims, or a resolved credential handle into a tool. Never put them in the preamble, in a context `Document`, or in tool arguments. See `.agents/skills/moira-rig-tools/SKILL.md`.

`PromptError` (`src/completion/request.rs:148`) is wider than `CompletionError`: `CompletionError`, `ToolError`, `ToolServerError`, `MaxTurnsError`, `PromptCancelled`, `UnknownToolCall`. It forwards `provider_response_status()` / `provider_response_body()` / `provider_response_json()` to the inner completion error. Any Agent adoption needs a `PromptError → ExecutionFailure` mapping that delegates the `CompletionError` arm to the existing `classify_completion_error` and adds a distinct `ExecutionFailureClass` for `MaxTurnsError`. Do not string-scrape, and do not surface tool or provider text into the public message — see `.agents/skills/moira-rig-errors-testing/SKILL.md`.

## Structured Output: OutputMode and Extractor

Two mechanisms. Pick deliberately.

| Need | Use | Why |
|---|---|---|
| One-shot extraction from text, no other tools | `Extractor<M, T>` | Owns its own retry loop and a `submit` tool; `extract_with_usage` returns `ExtractionResponse { data, usage }` |
| Typed output from an agent that must also call tools | `AgentBuilder::output_schema::<T>()` + `output_mode` + `prompt_typed::<T>()` | `OutputMode` routes the schema so it does not suppress tool calls |
| Schema forwarded to the provider, response parsed by Moira | today's `CompletionRequest.output_schema` | Already wired; see `.agents/skills/moira-rig-completions/SKILL.md` |

`OutputMode` (`src/agent/run/output_mode.rs`, `#[non_exhaustive]`): `Auto` (default, provider-aware), `Tool` (synthetic output tool, default name `final_result`), `Native` (provider `response_format`/`format`), `Prompted` (schema in the system prompt, raw text back).

**Only `Native` is constrained by the provider.** `Tool` and `Prompted` are best-effort — the model is asked, not forced. Validate before persisting or returning anything produced under `Tool` or `Prompted`.

`Extractor` mechanics (`src/extractor.rs`): an internal submit tool named `submit` (`SUBMIT_TOOL_NAME`, `:47`) whose `parameters()` is `json!(schema_for!(T))` (`:422`), built with `ToolChoice::Required` and `OutputMode::Native` (`:327-330`) — it implements its own tool-based output, so it opts out of `OutputMode` routing. `ExtractorBuilder::preamble` (`:337`) **appends** under an `ADDITIONAL INSTRUCTIONS` banner rather than replacing the built-in one. `retries(retries: u64)`; `retries(0)` still makes one attempt (`for i in 0..=self.retries`). If the model emits more than one `submit` call, the **first** is used (`arguments.into_iter().next()`, `:288`) — upstream's warning line claims "using the last one" and is wrong; do not design around the log text.

```rust
use rig_core::extractor::{ExtractionError, Extractor, ExtractorBuilder};
use rig_core::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Candidate for `memory_records`. `Option<T>` + `#[schemars(required)]` is the
/// idiomatic "must appear, may be null" pair that makes `submit` reliable.
/// Rustdoc comments become JSON-Schema descriptions and are how you steer fields.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[schemars(crate = "rig_core::schemars")]
struct MemoryCandidate {
    /// Canonical one-sentence statement of the durable fact.
    #[schemars(required)]
    statement: Option<String>,
    /// One of: preference, fact, goal, constraint, instruction.
    #[schemars(required)]
    memory_type: Option<String>,
    /// Confidence between 0 and 1.
    #[schemars(required)]
    confidence: Option<f64>,
}

async fn extract_memory_candidate<M>(
    model: M,
    transcript: &str,
) -> Result<MemoryCandidate, ExtractionError>
where
    M: rig_core::completion::CompletionModel,
{
    let extractor: Extractor<M, MemoryCandidate> = ExtractorBuilder::new(model)
        .preamble("Extract at most one durable memory. Emit nulls when unsure.")
        .retries(1)
        .build();

    extractor.extract(transcript).await
}
```

`#[schemars(crate = "rig_core::schemars")]` is required because Moira has no direct `schemars` dependency and the derive macro otherwise expands to paths rooted at `schemars::`. rig-core re-exports the crate (`pub use schemars;`, `src/lib.rs:185`) and enables schemars' default features, which include `derive` (schemars 1.2.1, the version `Cargo.lock` resolves). The `crate = "…"` attribute is supported by `schemars_derive` and this exact form has been compiled against Moira's dependency graph. If you ever need `schemars` types outside a derive, add the direct dependency pinned to 1.2.1 rather than inventing a workaround.

Structured output must not leak the schema of internal Moira tables into caller-visible errors. Extraction failures are `ExtractionError::{NoData, DeserializationError, CompletionError}` (`src/extractor.rs:59`); map them to a Moira failure class and log the class, not the model's raw text.

**Retry usage is under-reported.** `extract_with_usage`'s rustdoc claims usage "accumulates across all retry attempts", but the code only does `usage += u` on the **successful** attempt (`:181-184`); `extract_json_with_usage` returns bare `Err` on failure, so the tokens burned by every failed attempt are dropped. With `retries(n)` you can be billed for `n + 1` calls and record one. If Moira must bill or budget extraction accurately, either use `retries(0)` and drive the retry loop yourself, or reconcile against provider-side accounting. Do not trust the returned `Usage` as the full cost.

## Embeddings

`EmbeddingModel` (`src/embeddings/embedding.rs:61`) has an associated `const MAX_DOCUMENTS`, an associated `type Client`, and RPITIT methods — like `CompletionModel`, it is **not object-safe**. An embedding path therefore needs its own small enum, not a `Box<dyn>`, and not a widening of `RuntimeModelHandle`: only three of Moira's five providers can embed.

```rust
// `RigEmbeddingModel` must be in scope for `ndims`/`embed_text`/`embed_texts`;
// `EmbeddingsClient` belongs in the construction code, not here — an unused
// import fails `cargo clippy --all-targets -- -D warnings`.
use rig_core::embeddings::{Embedding, EmbeddingError, EmbeddingModel as RigEmbeddingModel};
use rig_core::providers::{azure, gemini, openai};

#[derive(Clone)]
pub enum RuntimeEmbeddingHandle {
    OpenAi(openai::embedding::EmbeddingModel),
    AzureOpenAi(azure::EmbeddingModel),
    Gemini(gemini::embedding::EmbeddingModel),
}

impl std::fmt::Debug for RuntimeEmbeddingHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variant = match self {
            Self::OpenAi(_) => "OpenAi",
            Self::AzureOpenAi(_) => "AzureOpenAi",
            Self::Gemini(_) => "Gemini",
        };
        write!(f, "RuntimeEmbeddingHandle::{variant}(<redacted>)")
    }
}

impl RuntimeEmbeddingHandle {
    pub fn dimensions(&self) -> usize {
        match self {
            Self::OpenAi(model) => model.ndims(),
            Self::AzureOpenAi(model) => model.ndims(),
            Self::Gemini(model) => model.ndims(),
        }
    }

    pub async fn embed_text(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        match self {
            Self::OpenAi(model) => model.embed_text(text).await,
            Self::AzureOpenAi(model) => model.embed_text(text).await,
            Self::Gemini(model) => model.embed_text(text).await,
        }
    }

    pub async fn embed_texts(&self, texts: Vec<String>) -> Result<Vec<Embedding>, EmbeddingError> {
        match self {
            Self::OpenAi(model) => model.embed_texts(texts).await,
            Self::AzureOpenAi(model) => model.embed_texts(texts).await,
            Self::Gemini(model) => model.embed_texts(texts).await,
        }
    }
}
```

Construction rules:

- `EmbeddingsClient::embedding_model(model_key)` / `embedding_model_with_ndims(model_key, ndims)` (`src/client/embeddings.rs:25,44`). Use the `_with_ndims` form to honour `application_embedding_policies.embedding_dimension`; Rig only maps identifier → dimension for the three `text-embedding-*` OpenAI constants (`model_dimensions_from_identifier`, `src/providers/openai/embedding.rs:57-63`) and otherwise falls back to `0`.
- Do **not** call `.completions_api()` on the OpenAI client used for embeddings. `openai::Client` (Responses extension) yields `openai::embedding::EmbeddingModel`; `CompletionsClient` yields `GenericEmbeddingModel<OpenAICompletionsExt, _>`, a different type. Build the embedding client separately from the completion client, through the same credential-resolution and redaction path as `build_completion_model`.
- Model keys are plain strings. `openai::TEXT_EMBEDDING_3_LARGE` (3072), `TEXT_EMBEDDING_3_SMALL` / `TEXT_EMBEDDING_ADA_002` (1536), `gemini::EMBEDDING_001`, `gemini::EMBEDDING_004` exist as constants but Moira should read the key from `provider_models`.

`EmbeddingsBuilder<M, T>` (`src/embeddings/builder.rs`) batches for you:

| Method | Signature |
|---|---|
| `new(model)` | `-> Self` (`:68`) |
| `document(T)` | `-> Result<Self, EmbedError>` (`:76`) |
| `documents(impl IntoIterator<Item = T>)` | `-> Result<Self, EmbedError>` (`:87`) |
| `build()` | `-> Result<Vec<(T, OneOrMany<Embedding>)>, EmbeddingError>` (`:105`) |
| `build_with_usage()` | `-> Result<(Vec<(T, OneOrMany<Embedding>)>, Usage), EmbeddingError>` (`:115`) |

It chunks into `M::MAX_DOCUMENTS` batches (`:135`) and runs `max(1, 1024 / M::MAX_DOCUMENTS)` concurrent requests (`:147`). All three of openai, azure, and gemini declare `MAX_DOCUMENTS = 1024`, so that is one in-flight request — Rig will not parallelise for you. Because `MAX_DOCUMENTS` is an associated const it cannot be reached through the enum; call `EmbeddingsBuilder` inside each match arm, or batch manually against `application_embedding_policies.batch_size`.

Rig does **not** rate-limit, retry, or time-bound embedding calls. Concurrency permits, `timeout_ms`, retry, and circuit breaking stay Moira's, exactly as for completions.

`Embed` / `#[derive(Embed)]` (rig-core feature `derive`, in `default = ["reqwest", "derive", "rustls"]`) marks the fields that produce embedding text. `use rig_core::Embed;` imports both the trait (`lib.rs:181`) and the derive macro (`lib.rs:189`) — they occupy different namespaces, so one `use` is enough.

```rust
use rig_core::Embed;
use serde::Serialize;

// Only `#[embed]` fields produce embedding text. A scalar field yields one
// embedding; an `#[embed] Vec<String>` yields one per element, which is why a
// document maps to `OneOrMany<Embedding>` rather than a single vector.
#[derive(Embed, Serialize, Clone, Debug)]
struct RagChunkRecord {
    chunk_id: String,
    section_title: Option<String>,
    #[embed]
    chunk_text: String,
}
```

Facts that matter:

- `Embedding { pub document: String, pub vec: Vec<f64> }` — **`f64`**, while pgvector columns are `float4`. The downcast is Moira's, done explicitly.
- `Embedding`'s `PartialEq` compares `document` only, never the vector. Do not use it to assert vector equality in tests.
- `embed_texts_with_usage` returns `Usage::default()` unless the provider overrides it. Do not assume embedding token accounting is populated.
- `Embedding.document` carries the source text. For encrypted columns (`rag_chunks.chunk_text_encrypted`, `memory_records.content_encrypted`) that means plaintext is in memory — never log it, never serialise it into a runtime event, never persist it alongside the vector.

## Vector Stores

```rust
pub trait VectorStoreIndex: WasmCompatSend + WasmCompatSync {
    type Filter: SearchFilter + WasmCompatSend + WasmCompatSync;

    fn top_n<T: for<'a> Deserialize<'a> + WasmCompatSend>(
        &self,
        req: VectorSearchRequest<Self::Filter>,
    ) -> impl Future<Output = Result<Vec<(f64, String, T)>, VectorStoreError>> + WasmCompatSend;

    fn top_n_ids(
        &self,
        req: VectorSearchRequest<Self::Filter>,
    ) -> impl Future<Output = Result<Vec<(f64, String)>, VectorStoreError>> + WasmCompatSend;
}
```

Two blanket impls come free once you implement it (`src/vector_store/mod.rs:119`, `:190`):

- `VectorStoreIndexDyn` — required by `dynamic_context` / `dynamic_tools`. Granted only when `Self::Filter: Debug + Clone + SearchFilter<Value = serde_json::Value> + Send + Sync + Serialize + DeserializeOwned + 'static`. A custom filter type must derive all of those or the agent builder will not accept the index.
- `Tool` named `search_vector_store`, args `{query, samples, threshold}`, output `Vec<VectorStoreOutput { score, id, document }>`. Handing an index to `.tool(index)` gives model-driven retrieval; `.dynamic_context(n, index)` gives automatic retrieval. They are different products — choose one deliberately.

`InMemoryVectorStore` (`src/vector_store/in_memory_store.rs`) is for tests and fixtures only. `from_documents`, `from_documents_with_ids`, `from_documents_with_id_f`, then `.index(model)` at `:452` which **consumes** the store — clone the embedding model first, as every upstream example does.

### pgvector: implement `VectorStoreIndex`, do not adopt `rig-postgres`

`rig-postgres` at the `rig-core-v0.40.0` tag hardcodes `INSERT INTO {} (id, document, embedded_text, embedding) VALUES ($1, $2, $3, $4)` with only the table name configurable, and its search selects `id, document, embedding <op> $1 as distance` from that one table. Moira's `rag_chunk_embeddings` and `memory_embeddings` carry `embedding_version`, `superseded_at`, `dimension`, `embedding_model_id`, and reach tenancy through `rag_collections` / `applications`. The schemas do not meet. Read `rig-postgres` as a reference for the pgvector SQL shape only; implement the two trait methods on a Moira type.

```rust
use rig_core::vector_store::request::{Filter, VectorSearchRequest};
use rig_core::vector_store::{VectorStoreError, VectorStoreIndex};
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use uuid::Uuid;

/// Retrieval over `rag_chunk_embeddings`. Every scope predicate is a constructor
/// argument resolved from Moira's runtime config — never model- or caller-supplied.
pub struct RagChunkIndex {
    pool: PgPool,
    embedder: RuntimeEmbeddingHandle,
    collection_id: Uuid,
    embedding_version: i32,
    expected_dimension: usize,
    best_effort: bool,
}

impl std::fmt::Debug for RagChunkIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RagChunkIndex")
            .field("collection_id", &self.collection_id)
            .field("embedding_version", &self.embedding_version)
            .finish_non_exhaustive()
    }
}

impl VectorStoreIndex for RagChunkIndex {
    type Filter = Filter<serde_json::Value>;

    async fn top_n<T: DeserializeOwned + Send>(
        &self,
        req: VectorSearchRequest<Self::Filter>,
    ) -> Result<Vec<(f64, String, T)>, VectorStoreError> {
        match self.search(req).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(similarity, id, document)| {
                    serde_json::from_value(document)
                        .map(|document| (similarity, id, document))
                        .map_err(VectorStoreError::JsonError)
                })
                .collect(),
            // `dynamic_context` joins every index with `try_join_all`; one error
            // fails the whole completion. Honour
            // `application_embedding_policies.failure_behavior` here instead.
            Err(error) if self.best_effort => {
                tracing::warn!(target: "moira::rag", error = %error, "retrieval degraded");
                Ok(Vec::new())
            }
            Err(error) => Err(error),
        }
    }

    async fn top_n_ids(
        &self,
        req: VectorSearchRequest<Self::Filter>,
    ) -> Result<Vec<(f64, String)>, VectorStoreError> {
        Ok(self
            .search(req)
            .await?
            .into_iter()
            .map(|(similarity, id, _)| (similarity, id))
            .collect())
    }
}
```

The private `search` helper embeds the query and runs the SQL:

```rust
impl RagChunkIndex {
    async fn search(
        &self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> Result<Vec<(f64, String, serde_json::Value)>, VectorStoreError> {
        let embedding = self
            .embedder
            .embed_text(req.query())
            .await
            .map_err(VectorStoreError::EmbeddingError)?;

        if embedding.vec.len() != self.expected_dimension {
            return Err(VectorStoreError::DatastoreError(
                "embedding dimension does not match the collection policy".into(),
            ));
        }

        // pgvector columns are float4; `Embedding.vec` is Vec<f64>.
        let query_vector: Vec<f32> = embedding.vec.iter().map(|value| *value as f32).collect();
        let limit = i64::try_from(req.samples()).unwrap_or(i64::MAX);

        // Distance is cosine (`<=>`). Convert to a similarity so higher is better
        // before returning it — `VectorStoreIndex` names the first tuple slot a score.
        // Apply `req.threshold()` against that similarity, and document the direction.
        // ...
    }
}
```

Before writing that query, settle two things that this skill will not guess for you:

- **Binding the vector.** Moira's `Cargo.toml` has no `pgvector` crate, so `sqlx` cannot bind `Vec<f32>` to a `vector` column today. Either add `pgvector` with its `sqlx` feature — which is exactly what `rig-postgres` does — or render the literal and cast with `::vector`. Decide explicitly; do not assume a bind works.
- **Threshold direction.** `rig-postgres` puts the raw pgvector *distance* in the first tuple slot and compiles `threshold` into `PgSearchFilter::gt("distance", t)`, so under cosine distance a higher threshold means *less* similar. `VectorStoreIndex` documents that slot as a *score*, and `Tool::parameters` describes `threshold` as a *similarity* threshold. The two conventions contradict. Pick one for Moira, encode it once, and cover it with a test.

If retrieval must be filtered by tenant, collection, or embedding version through Rig's filter DSL rather than through constructor arguments, implement `SearchFilter` on a `MoiraSearchFilter` (mirroring `rig-postgres`'s `PgSearchFilter`) so predicates compile into the `WHERE` clause. The trait is tagless-final with exactly five operations — `eq`, `gt`, `lt`, `and`, `or` (`src/vector_store/request.rs:117-125`) — and nothing else is portable. Prefer constructor arguments: a filter reachable from a model-driven `search_vector_store` tool call is caller-influenced, and scope must not be.

## End-to-End RAG in Moira

The whole wiring, for an internal job:

```
runtime config
  ├─ resolve embedding provider/model from `application_embedding_policies`
  │     (independent of the completion provider — anthropic/deepseek cannot embed)
  ├─ RuntimeEmbeddingHandle          (redacted Debug, credential exposed once)
  ├─ RagChunkIndex { pool, embedder, collection_id, embedding_version, best_effort }
  │     └─ impl VectorStoreIndex  ⇒ VectorStoreIndexDyn + Tool for free
  ├─ RuntimeModelHandle arm → AgentBuilder::new(model)
  │     .preamble(server_owned)
  │     .dynamic_context(samples, index)
  │     .default_max_turns(n)
  │     .build()
  └─ agent.prompt(msg).history(h).max_turns(n).extended_details().await
        └─ PromptResponse::{usage, requests, completion_calls} → UsageSummary / events
```

Ingestion is the mirror image: chunk → `EmbeddingsBuilder` (or `embed_texts` against `batch_size`) → downcast `f64`→`f32` → insert into `rag_chunk_embeddings` with the resolved `embedding_model_id`, `dimension`, and a new `embedding_version`, marking the prior version `superseded_at`. Rig has no insert path for Moira's schema; `InsertDocuments` is not worth implementing against a versioned table.

### How `dynamic_context` actually behaves

`src/agent/completion.rs:316-385`. Read this before designing retrieval:

- The retrieval query is the current prompt's raw text, falling back to the most recent message in chat history that has any (`prompt.rag_text().or_else(|| chat_history.iter().rev().find_map(Message::rag_text))`). No rewriting, no HyDE, no embedding cache. `rag_text` is `pub(crate)` (`src/completion/message.rs:590`) so Moira cannot reuse it — if Moira wants query rewriting, do it before calling Rig and either fold the context into the prompt or expose the index as an explicit `Tool`.
- All registered `(sample, index)` pairs are searched concurrently with `try_join_all`, so **any one index failure fails the whole completion** as `CompletionError::RequestError`. Degrade inside the index impl, as shown above.
- **No threshold and no filter are ever set** on the generated `VectorSearchRequest`. Both must be enforced by the index implementation.
- The score is discarded; `Document.id` is the store's id.
- Retrieved documents are serialised with `serde_json::to_string_pretty` — the model sees the whole JSON payload. Return the minimum: chunk text and a stable id. Never include ciphertext, credential material, internal ids that are not already public, restricted-sensitivity memories, or other tenants' rows.
- The dyn path runs `prune_document`, which drops any JSON array longer than 400 elements. Never place raw vectors in the document payload.
- Documents land in `CompletionRequest.documents` and are normalised into a message placed after the preamble and leading system messages, before prior history.

If retrieval is best-effort per `application_embedding_policies.failure_behavior = 'continue_without_semantic_retrieval'`, that behaviour lives in the index impl, and the degradation must be recorded as a Moira runtime event — Rig will not tell anyone.

## Conversation Memory

`ConversationMemory` (`src/memory.rs:93`) is three methods over boxed futures and `&str` ids — genuinely object-safe, unlike `CompletionModel`. `AgentBuilder::memory(backend)` stores it as `Arc<dyn ConversationMemory>`; the id comes from `AgentBuilder::conversation(id)` or `PromptRequest::conversation(id)`, and `PromptRequest::without_memory()` opts a request out. **If no conversation id is set anywhere, memory is silently bypassed** — an easy way to ship a broken feature.

`load(&str) -> Vec<Message>` runs before the prompt; `append(&str, Vec<Message>)` runs **inline before the agent returns**, on the response path, and receives the user prompt, the assistant response, and every tool-call/tool-result pair from the turn. Rig's own docs say to keep it cheap. A `PgConversationMemory` over `conversations` / `conversation_messages` is a low-risk fit if `append` is one batched multi-row INSERT; push extraction, summarisation, and embedding to the existing async run tables. Content columns are encrypted (`content_encrypted`) — decrypt on `load`, encrypt on `append`, and never write plaintext into `content_plain` for a policy that requires encryption.

Moira already owns conversation persistence through its own repositories. Adopting `ConversationMemory` means Rig writes history on Moira's behalf; do that only if the same agent is the sole writer for that conversation. `conversation_messages` carries `constraint conversation_messages_sequence_unique unique (conversation_id, sequence_number)`, so two concurrent writers do not merely interleave — they deadlock or violate the constraint. Allocate `sequence_number` inside the same transaction as the insert.

## Multi-Agent Composition

With the pipeline module gone, the sanctioned composition mechanism is **wrapping an `Agent<M>` in a `Tool`** — `AgentBuilder::description()` exists for exactly this. For Moira that is a last resort: a sub-agent is a nested, unbudgeted, unobserved provider call inside another provider call. If a job needs two model steps, write two `async fn`s in Moira, one per step, each with its own permit, deadline, usage record, and failure class. That is the boundary-correct shape and it is also what upstream now does after deleting the pipeline.

## Pitfalls

- Assuming a `pipeline`/`Op`/`TryOp` API exists. It does not, in any form, in 0.40.0.
- Forgetting the turn budget. Default is one model call; tool loops need ≥ 2 and fail with `MaxTurnsError` otherwise.
- Using `.prompt()` without `.extended_details()` and then having no usage to record.
- Building an agent with `CompletionClient::agent(key)` instead of `AgentBuilder::new(model)`. It needs a client Moira's factory does not retain, so it forces a credential re-resolve outside `ProviderRuntimeCache`, and on `openai::Client` it yields the Responses-API model instead of the `.completions_api()` model Moira stores.
- Calling `.completions_api()` on the client you then ask for an embedding model; the embedding model type differs between the two OpenAI extensions.
- Routing an embedding request to an Anthropic or DeepSeek provider. Resolve the embedding provider independently.
- Storing `Embedding.vec` (`Vec<f64>`) into a `vector` column without an explicit `f32` downcast, or without checking length against `application_embedding_policies.embedding_dimension`.
- Letting a retrieval failure kill a completion because `dynamic_context` uses `try_join_all`.
- Returning fat JSON documents from `top_n`: the model sees the whole serialised payload, and `prune_document` will silently delete long arrays.
- Trusting `OutputMode::Tool` or `Prompted` output to match the schema. Only `Native` is provider-constrained.
- Trusting `Extractor::extract_with_usage`'s `Usage` as the full cost of an extraction. Failed retry attempts are billed by the provider and dropped by Rig.
- Deriving `JsonSchema` without `#[schemars(crate = "rig_core::schemars")]` while Moira has no direct `schemars` dependency.
- Passing tenant or credential context through the preamble or a context document instead of `tool_extensions`.
- Reaching for `#[derive(Debug)]` on a struct holding an `Agent<M>` or a provider `EmbeddingModel`. None of those types implement `Debug` (all are `#[derive(Clone)]` only), so it will not compile; hand-write a redacting `Debug` instead of widening bounds until it does.
- Adding `rig-postgres` "because Moira already runs pgvector". The schemas are incompatible; only the SQL shape is reusable.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Any agent, extractor, embedding, or vector-store work must additionally prove, with tests:

- The turn budget is set explicitly and `MaxTurnsError` maps to a distinct failure class with a sanitised message.
- `PromptResponse::usage()` reaches `UsageSummary` through `usage_from_rig`, and `requests()` matches the number of provider calls observed by the scripted server in `tests/support/mock_openai.rs`.
- A failing index returns `Ok(vec![])` under `continue_without_semantic_retrieval` and does not fail the completion, and returns `Err` otherwise.
- Retrieval is scoped: a query from one application, tenant, or collection never returns another's rows, and never returns a `superseded_at`-set embedding version.
- Nothing surfaced to the model or to a runtime event contains ciphertext, a plaintext credential, decrypted material outside its intended scope, or an internal prompt.
- Embedding dimension mismatches fail loudly rather than being written to the column.

If database behaviour changes, validate migrations against the local pgvector Postgres container.

## Related Skills

- `.agents/skills/moira-rig-integration/SKILL.md` — the boundary, the `RuntimeFactory` seam, the vendored-source verification rule.
- `.agents/skills/moira-rig-providers/SKILL.md` — provider client construction and credential-type gating, which an embedding handle must mirror.
- `.agents/skills/moira-rig-completions/SKILL.md` — `CompletionRequest`, `preamble`, `documents`, `output_schema`; the layer an Agent sits on.
- `.agents/skills/moira-rig-streaming/SKILL.md` — why the public SSE path stays on `start_stream`.
- `.agents/skills/moira-rig-tools/SKILL.md` — `Tool`, `ToolSet`, `ToolChoice`, `tool_extensions`.
- `.agents/skills/moira-rig-errors-testing/SKILL.md` — `PromptError` mapping, sanitisation, and the scripted-provider test harness.
- `skills/moira-project-structure/SKILL.md` — module placement.
- `.agents/skills/moira-openapi/SKILL.md` — if any of this becomes caller-visible.
