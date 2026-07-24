---
name: moira-rig-completions
description: Expert guidance for non-streaming completion execution through Rig (rig-core 0.40) in Moira. Covers CompletionModel usage, field-by-field CompletionRequest semantics and how each provider actually encodes those fields on the wire, Message/UserContent/AssistantContent/OneOrMany construction from Moira's public DTOs, reading CompletionResponse, token-usage extraction into UsageSummary, safe use of additional_params, and parameter/determinism policy. Use when building or changing a CompletionRequest, adding or altering request parameters, mapping caller messages into rig_core::completion::Message, adding an output_schema or provider-specific parameter, reading choice/raw_response/usage, changing usage_from_rig, or debugging why a request field did not reach the provider.
---

# Moira Rig Completions

## Core Rule

Moira builds one `rig_core::completion::CompletionRequest` and hands it to one `RuntimeModelHandle`. Rig owns encoding, transport, and decoding. Moira owns what goes into the request and what is narrowed out of the response.

There is exactly one request-construction site (`build_completion_request` in `src/application/execution.rs`), exactly one message-normalisation site (`map_public_message` in `src/application/public.rs`), and exactly one usage-conversion site (`usage_from_rig` in `src/orchestration/runtime_factory.rs`). Adding a second of any of these is a review-blocking defect.

Read `.agents/skills/moira-rig-integration/SKILL.md` first — it owns the boundary, the enum-dispatch rationale, and the vendored-source verification rule. This skill assumes it.

## Where Completion Code Lives

| Concern | File |
|---|---|
| Public DTO → `Message` | `src/application/public.rs` (`map_public_messages`, `map_public_message`, `text_only_content`) |
| `ExecutionCommand` + agent profile → `CompletionRequest` | `src/application/execution.rs` (`build_completion_request`) |
| Non-streaming invocation | `src/application/execution.rs` (`execute_rig_completion`) → `RuntimeModelHandle::completion` |
| Enum dispatch → generic call | `src/orchestration/runtime_factory.rs` (`completion_with_model<M>`) |
| Response narrowing | `src/orchestration/runtime_factory.rs` (`output_from_response`, `text_from_choice`, `usage_from_rig`) |

Streaming is a different contract — see `.agents/skills/moira-rig-streaming/SKILL.md`. Tools are disabled today — see `.agents/skills/moira-rig-tools/SKILL.md`.

## CompletionModel: What You May Call

`rig_core::completion::CompletionModel` (`rig-core-0.40.0/src/completion/request.rs:613-664`):

```rust
pub trait CompletionModel: Clone + WasmCompatSend + WasmCompatSync {
    type Response: WasmCompatSend + WasmCompatSync + Serialize + DeserializeOwned;
    type StreamingResponse: Clone + Unpin + WasmCompatSend + WasmCompatSync
        + Serialize + DeserializeOwned + GetTokenUsage;
    type Client;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self;
    fn completion(&self, request: CompletionRequest)
        -> impl Future<Output = Result<CompletionResponse<Self::Response>, CompletionError>> + WasmCompatSend;
    fn stream(&self, request: CompletionRequest)
        -> impl Future<Output = Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>> + WasmCompatSend;
    fn completion_request(&self, prompt: impl Into<Message>) -> CompletionRequestBuilder<Self> { … }
    fn composes_native_output_with_tools(&self) -> bool { false }
}
```

Rules:

1. **Call `completion` directly with a hand-built `CompletionRequest`.** Do not use `completion_request(prompt)` / `CompletionRequestBuilder` in Moira. The builder takes a single `prompt: impl Into<Message>` plus a separate history, which does not match `ExecutionCommand.messages: Vec<Message>`, and `build()` rewrites `preamble` (see below).
2. **Never call `.send()` or `.stream()` on the builder.** Those clone the model and bypass `classify_completion_error`; every provider call must funnel through `completion_with_model` / `start_stream_with_model` so failures are classified and sanitised once.
3. The trait is not object-safe (associated types, `impl Into<String>`, RPITIT). Keep the `RuntimeModelHandle` match; add a new method to the enum rather than trying to erase the type.
4. `composes_native_output_with_tools()` is an agent-loop hint only. Moira does not run Rig's agent loop; ignore it unless you adopt `.agents/skills/moira-rig-agents-rag/SKILL.md`.

## CompletionRequest: Field-by-Field

`rig-core-0.40.0/src/completion/request.rs:666-694`. Ten public fields, **no `Default` impl**, struct-literal constructed in Moira:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)] // request.rs:667
pub struct CompletionRequest {
    pub model: Option<String>,
    pub preamble: Option<String>,
    pub chat_history: OneOrMany<Message>,
    pub documents: Vec<Document>,          // request::Document, NOT message::Document
    pub tools: Vec<ToolDefinition>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub tool_choice: Option<ToolChoice>,
    pub additional_params: Option<serde_json::Value>,
    pub output_schema: Option<schemars::Schema>,
}
```

Fill every field explicitly. **Never write `..Default::default()`** — the absence of `Default` is the forcing function that breaks the build when Rig adds a field, and that break is wanted.

`CompletionRequest` derives `Debug`, `Clone`, `Serialize`, and `Deserialize`. That makes it trivially loggable and trivially serialisable, and it carries the full prompt (`preamble`, `chat_history`) plus whatever is in `additional_params`. Never `{:?}` it, never `serde_json::to_value` it into an event payload, a tracing field, or a persisted record.

### `model`

Per-request override of the handle's model string. All of OpenAI (`providers/openai/completion/mod.rs:1853` `request_model.unwrap_or(model)`), Anthropic (`providers/anthropic/completion.rs:2456-2459`), and Gemini (`providers/gemini/completion.rs:96`, `resolve_request_model`) honour it.

Moira sets it to `None`. Model identity is part of the `RuntimeCacheKey` and of the routing decision; overriding it here would make the cached handle lie about which model was billed. Keep it `None` unless you also thread the override through routing, the cache key, and the usage record.

**Azure trap:** `completion_path` is called with `self.model`, not the request override (`providers/openai/completion/mod.rs:1977`; Azure's override at `providers/azure.rs:568`). Setting `model` on an Azure request changes the JSON body but *not* the deployment in the URL — the two would disagree.

### `preamble`

`Option<String>`, documented as legacy but **fully honoured by every provider Moira uses**:

| Provider | Where the preamble lands | Source |
|---|---|---|
| OpenAI / Azure / DeepSeek | prepended as `Message::system` before all converted history | `providers/openai/completion/mod.rs:1757` |
| Anthropic | first element of the `system` content-block array, before history system blocks | `providers/anthropic/completion.rs:2341-2353` |
| Gemini | first `Part` of `system_instruction`, before history system parts | `providers/gemini/completion.rs:253-260` |

Azure and DeepSeek share OpenAI's encoder because their model types are aliases of the same generic: `deepseek::CompletionModel = openai::completion::GenericCompletionModel<DeepSeekExt, H>` (`providers/deepseek.rs:159-160`) and `azure::CompletionModel = openai::completion::GenericCompletionModel<AzureExt, H>` (`providers/azure.rs:555-556`). Anything true of the OpenAI chat-completions body is true of theirs except where an `Ext` const or `finalize_request_body` overrides it. That applies to every row of every table below.

Moira sets it from `AgentProfileRecord.preamble` (`src/application/execution.rs:1619`), so operator instructions always precede caller-supplied system messages. That ordering is the point — do not move the profile preamble into `chat_history`, or a caller-supplied system message could precede it.

`CompletionRequestBuilder::build()` behaves differently: it inserts `Message::system(preamble)` at `chat_history[0]` and sets `preamble: None` (`request.rs:1044-1073`). The two paths are **not** interchangeable — a builder-built request has `preamble == None`, which is why provider telemetry spans (`gen_ai.system_instructions`, e.g. `anthropic/completion.rs:2467`, `gemini/completion.rs:104`) record `None` for builder callers and the real string for Moira's hand-built requests. Empty strings are skipped by Anthropic (`anthropic/completion.rs:2342-2343`) and Gemini (`gemini/completion.rs:253`) but **not** by OpenAI (`openai/completion/mod.rs:1757` does not filter); do not pass `Some("")`.

Never place internal prompts, routing rationale, credential context, or tenant identifiers into `preamble`. It is transmitted verbatim to a third party.

### `chat_history`

`OneOrMany<Message>` — the last element is always treated as the prompt. Built from `ExecutionCommand.messages`:

```rust
let chat_history = OneOrMany::many(command.messages.clone()).map_err(|_| {
    ExecutionFailure::new(
        ExecutionFailureClass::InvalidExecutionRequest,
        "execution command must contain at least one message",
    )
})?;
```

`OneOrMany::many` is fallible with `EmptyListError` (`one_or_many.rs:101-113`); there is no `From<Vec<T>>` and no `FromIterator`. `OneOrMany::is_empty()` always returns `false` (`one_or_many.rs:88-90`) — never use it as a guard. Every method requires `T: Clone` (`one_or_many.rs:27`).

An empty message list must fail as `InvalidExecutionRequest` (422), never as a provider error.

### `documents`

`rig_core::completion::Document { id, text, additional_props }` (`request.rs:295-303`) — **not** `rig_core::message::Document`. Providers fold it into history through `chat_history_with_documents()`, which is `pub(crate)` (`request.rs:739-749`) and therefore not callable from Moira. The message it inserts *is* inspectable: `CompletionRequest::normalized_documents()` is `pub` (`request.rs:713-715`) and returns exactly what gets injected — one `Message::User` whose `content` holds one `UserContent::Document(… Some(DocumentMediaType::TXT))` per document (`:717-737`), inserted after the leading run of `Message::System` messages. Each document renders through `Display` as `<file id: {id}>\n{text}\n</file>\n`, with a sorted `<metadata k: "v" … />` prefix when `additional_props` is non-empty (`request.rs:305-325`). Use `normalized_documents()` in tests rather than asserting on a provider body.

Moira sets `documents: Vec::new()`. Populating it means Moira is doing retrieval — read `.agents/skills/moira-rig-agents-rag/SKILL.md` before you do, and route retrieved text through the same content policy as caller input.

### `tools` / `tool_choice`

Always `Vec::new()` / `None` today. Tool execution is deliberately off; only streamed tool-call deltas are surfaced. `ToolDefinition { name, description, parameters }` (`request.rs:327-335`), `ToolChoice::{Auto, None, Required, Specific { function_names }}` (`message.rs:1345-1355`). Providers that do not support tools warn and drop them (`providers/openai/completion/mod.rs:1800-1806`). Any change here belongs to `.agents/skills/moira-rig-tools/SKILL.md`.

### `temperature`

`Option<f64>`. Precedence in Moira: `command.options.temperature` → `agent_profile.temperature` → **provider default**. Moira supplies no default of its own, and must not start supplying one: a Moira-side default silently changes behaviour differently on every provider.

Provider encoding:

| Provider | Encoding | Caveat |
|---|---|---|
| OpenAI / Azure / DeepSeek | top-level `temperature`, omitted when `None` | — |
| Anthropic | top-level `temperature`, omitted when `None` | — |
| Gemini | `generationConfig.temperature` | **Silently dropped** unless a `generationConfig` already exists |

The Gemini case is a real defect surface. `create_request_body` applies temperature and `max_tokens` via `generation_config.map(|mut cfg| …)` (`providers/gemini/completion.rs:240-250`) — `Option::map` on `None` stays `None`. A `generationConfig` exists only if the caller put one in `additional_params` **or** `output_schema` is set (`:233-238` calls `get_or_insert_with`). So a Gemini request with `temperature: Some(0.2)`, no `output_schema`, and Moira's current `additional_params` (`{"moira": {…}}`) sends **no temperature at all**.

If Moira ever routes to Gemini with parameters that must be honoured, `build_completion_request` has to seed a `generationConfig` into `additional_params` for `ProviderType::Gemini`. Build it from Rig's own types rather than hand-rolled JSON, so a rename in 0.41 is a compile error instead of a silent drop — both are public (`providers/gemini/completion.rs:534` `pub mod gemini_api_types`):

```rust
use rig_core::providers::gemini::completion::gemini_api_types::{
    AdditionalParameters, GenerationConfig,
};

// Gemini only: force a generationConfig to exist so temperature/max_tokens survive.
// `GenerationConfig::default()` is NOT neutral — null the two fields Rig will fill.
let config = GenerationConfig {
    temperature: None,
    max_output_tokens: None,
    ..GenerationConfig::default()
};
let params = AdditionalParameters::default()
    .with_config(config)
    .with_params(json!({ "moira": { "request_id": command.request_id } }));
let additional_params = Some(serde_json::to_value(params)?);
```

Leave `temperature` / `max_tokens` on `CompletionRequest`; Rig's `generation_config.map(…)` then fills them in. Do not set them twice.

**`GenerationConfig::default()` is opinionated** (`gemini/completion.rs:1631-1653`): `temperature: Some(1.0)` and `max_output_tokens: Some(4096)`, everything else `None`. Passing it unmodified injects both values whenever Moira's own fields are `None`, silently overriding Gemini's model defaults and capping output at 4096 tokens. This already bites today by a different route: setting `output_schema` on a Gemini request calls `get_or_insert_with(GenerationConfig::default)` (`:235`), so **a structured-output request to Gemini gets `temperature: 1.0` and `maxOutputTokens: 4096` for free** unless Moira sets them explicitly. Any Gemini structured-output work must set both fields deliberately.

### `max_tokens`

`Option<u64>`. Precedence: `command.options.max_tokens` → `agent_profile.max_tokens as u64` (`src/application/execution.rs:1628`). The profile field is `Option<i64>` and the `as` cast **wraps**, not saturates: a stored `-1` becomes `18446744073709551615`, which every provider rejects with a 400 that classifies as `ProviderUpstreamError` — retryable and fallback-eligible. Reject non-positive `max_tokens` at the admin write boundary; do not add a clamp here, because that would hide a corrupt profile row.

| Provider | Behaviour when `None` |
|---|---|
| OpenAI / Azure / DeepSeek | field omitted; provider default applies |
| Anthropic | **hard error** unless the model has a built-in default |
| Gemini | `generationConfig.maxOutputTokens`, dropped with the same `generationConfig` caveat as `temperature` |

Anthropic requires `max_tokens`. `CompletionModel::make` → `new()` → `Ext::default_max_tokens(&model)` = `default_max_tokens_for_model(model)` (`providers/anthropic/completion.rs:2441-2443`, `:1553-1565`, `:52-54`). That table only recognises `claude-opus-4*`, `claude-sonnet-4*`, and `claude-haiku-4-5*` prefixes and returns `None` for everything else (`:1661-1675`, test at `:2603-2605`). When both the request field and the model default are absent, `completion()` returns `CompletionError::RequestError("`max_tokens` must be set for Anthropic")` (`:2479-2488`) — **before any HTTP request**.

That error has no HTTP status, so `classify_completion_error` falls into its substring branch. The `Display` string is `RequestError: \`max_tokens\` must be set for Anthropic`, which contains none of `timeout`/`connect`/`dns`/`json`/`parse`/`response`, so it lands in the fallthrough arm as `ProviderUpstreamError` — which `is_retryable` and `is_fallback_eligible` both accept (`src/orchestration/controls.rs:628-653`). A permanent configuration fault therefore burns the retry budget and every fallback candidate.

Two fixes, in order of preference: require `max_tokens` at admin-validation time for any Anthropic model key outside Rig's table, or default it in `build_completion_request`. Note that Rig has a second constructor, `GenericCompletionModel::with_model` (`:1567-1578`), which falls back to `default_max_tokens_with_fallback(model)` = `2_048` (`:1677-1679`) — but `RuntimeFactory` reaches the model through `client.completion_model(model_key)` (`src/orchestration/runtime_factory.rs:121`) → `CompletionClient::completion_model` → `CompletionModel::make` (`rig-core-0.40.0/src/client/completion.rs:28-30`) → `new()`, which does **not** apply that fallback. Do not switch constructors to paper over the problem: 2048 is an arbitrary cap that would silently truncate output.

### `additional_params`

`Option<serde_json::Value>`. This is the only escape hatch for provider parameters that `CompletionRequest` does not model (`top_p`, `seed`, `stop`, penalties, `thinking`, `safetySettings`, …). It is also the sharpest edge in the request.

**All three provider encoders `#[serde(flatten)]` it onto the top level of the request body**, alongside `model` / `messages` / `temperature`:

- OpenAI / Azure / DeepSeek — `providers/openai/completion/mod.rs:1582-1583`
- Anthropic — `providers/anthropic/completion.rs:1844-1845`
- Gemini — `providers/gemini/completion.rs:2099-2100` (after `AdditionalParameters` peels off `generationConfig` at `:228-231`)

Safety rules:

1. **Never use a key that collides with a first-class body field** (`model`, `messages`, `tools`, `temperature`, `max_tokens`, `system`, `contents`, `tool_choice`, `response_format`, `output_config`). Flatten offers no conflict detection; you get a corrupted body.
2. **`tools` is reserved, and the three encoders disagree on how.** Anthropic and Gemini `remove("tools")` from `additional_params` and reinterpret the value as provider-hosted tool definitions, erroring on a bad shape (`anthropic/completion.rs:2395-2409`, `gemini/completion.rs:321-335`). The OpenAI-compatible encoder does **not**: it has no extraction step, so a `tools` key in `additional_params` is flattened next to the struct's own first-class `tools` field and the body carries a duplicate `"tools"` key. Rig's only supported way to add provider-hosted tools on that path is `CompletionRequest::with_provider_tool` / `with_provider_tools` (`request.rs:752-763`), which merge into `additional_params.tools` for you. Never hand-write a `tools` key.
3. **`generationConfig` is reserved for Gemini** and is strongly typed as `GenerationConfig` (`gemini/completion.rs:1541`, `rename_all = "camelCase"`). A wrong *type* on a known key fails `serde_json::from_value::<AdditionalParameters>` and surfaces as `CompletionError::JsonError` (`gemini/completion.rs:231`); an *unknown* key inside `generationConfig` is silently dropped, because neither struct sets `deny_unknown_fields`. The `JsonError` has no HTTP status and its `Display` starts with `JsonError:`, so `classify_completion_error` matches `"json"` and returns `ProviderInvalidResponse` — a *circuit-opening* class (`src/orchestration/controls.rs:655-665`). A pure request-construction bug in Moira can therefore open the Gemini circuit for every tenant. Construct it from `GenerationConfig` rather than raw JSON.
4. **Everything you put there is sent verbatim to the provider.** Moira currently injects `{"moira": {"request_id": command.request_id}}` whenever `command.metadata` is non-null. That means a `moira` object appears as a top-level field of the provider request body. Strict OpenAI-compatible gateways that reject unknown top-level fields will 400. Treat this as correlation metadata only: never add tenant IDs, user IDs, credential IDs, application IDs, internal prompts, or routing internals.
5. Build it per provider, not globally, once any provider-specific key exists. A `generationConfig` sent to OpenAI is an unknown top-level field.
6. If you need to merge two fragments, do it in Moira with explicit `serde_json::Map` insertion. Rig's `json_utils` module is `pub(crate)` (`src/lib.rs:159`), so `json_utils::merge` is unreachable; `CompletionRequestBuilder::additional_params` merges while `additional_params_opt` replaces (`request.rs:968-988`), but neither is reachable through the hand-built path.

### `output_schema`

`Option<rig_core::schemars::Schema>`. Always construct it through `rig_core::schemars` — using a locally-declared `schemars` dependency would be a different type and would not compile. Moira parses `ExecutionOptions.output_schema: Option<Value>` with `serde_json::from_value::<rig_core::schemars::Schema>`, mapping failure to `ExecutionFailureClass::StructuredOutputInvalid` (422).

| Provider | Encoding |
|---|---|
| OpenAI / Azure | `response_format.json_schema { name, strict: true, schema }`, merged into `additional_params`; `name` comes from the schema's `title`, else `"response_schema"` (`openai/completion/mod.rs:1822-1849`) |
| Anthropic | `output_config.format` as `json_schema` (`anthropic/completion.rs:2363-2373`) |
| Gemini | `generationConfig.responseMimeType = "application/json"` + `responseJsonSchema` (`gemini/completion.rs:233-238`). Creates the `generationConfig` via `GenerationConfig::default()`, which also injects `temperature: 1.0` and `maxOutputTokens: 4096` — see the `temperature` section. |
| DeepSeek | **dropped with a `tracing::warn!`** — `SUPPORTS_RESPONSE_FORMAT = false` (`providers/deepseek.rs:60`, drop at `openai/completion/mod.rs:1809-1813`) |

Two more OpenAI subtleties: the schema is *withheld* on the first turn when tools are present and no tool result is in history (`:1818-1820`), and `super::sanitize_schema` mutates the schema before sending. Do not assume the provider sees byte-identical JSON to what the caller submitted.

**Known gap:** Moira passes `output_schema` through but hardcodes `structured_output: None` in both run paths (`src/application/execution.rs:1450`, `:1581`). The schema constrains the provider; nothing parses or validates the result back. If you implement structured output, parse from `RuntimeCompletionOutput.text`, fail with `StructuredOutputInvalid`, and do not introduce a second response-narrowing site.

## Mapping Table: Moira Domain → Rig Type → Conversion Rule

| Moira type / field | Rig type | Conversion rule |
|---|---|---|
| `PublicMessageRole::System \| Developer` | `Message::System { content: String }` | `Message::system(text_only_content(msg)?)`; gated on `policy.caller_system_instructions_allowed`, else `AppError::unprocessable("unsupported_message_role", …)`. Image parts rejected as `"unsupported_input_type"`. |
| `PublicMessageRole::User` | `Message::User { content: OneOrMany<UserContent> }` | Map every part, then `OneOrMany::many(parts)`; `EmptyListError` → `AppError::unprocessable("invalid_execution_request", "user message is empty")`. |
| `PublicContentPart::InputText { text }` | `UserContent::Text(Text)` | `UserContent::text(text.clone())` (`message.rs:663-665`). `Text` carries an `additional_params` field, so it is never a bare `String`. |
| `PublicContentPart::InputImage { image_url }` | `UserContent::Image(Image)` | `UserContent::image_url(url.clone(), None, None)` → `DocumentSourceKind::Url` (`message.rs:696-707`). No `ImageMediaType`, no `ImageDetail`; add them only if a provider requires them. Gated separately on `policy.vision_enabled` and pushes `"vision"` into `required_capabilities` (`src/application/public.rs:914`, `:1228-1231`). |
| `PublicMessageRole::Assistant` | `Message::Assistant { id: None, content }` | `Message::assistant(text_only_content(msg)?)`. Note the variant has an `id: Option<String>` field — exhaustive matches and struct literals must account for it. |
| `PublicMessageRole::Tool` | — | Rejected: `AppError::unprocessable("unsupported_message_role", "tool messages require an approved tool registry")`. Do not silently map to `Message::tool_result`. |
| `ExecutionCommand.messages: Vec<Message>` | `CompletionRequest.chat_history: OneOrMany<Message>` | `OneOrMany::many(clone)` → `InvalidExecutionRequest` on empty. |
| `AgentProfileRecord.preamble: Option<String>` | `CompletionRequest.preamble` | Direct. Always precedes caller system messages on every provider. |
| `ExecutionOptions.temperature: Option<f64>` | `CompletionRequest.temperature` | `options.or(profile.temperature)`; no Moira default. |
| `ExecutionOptions.max_tokens: Option<u64>` | `CompletionRequest.max_tokens` | `options.or(profile.max_tokens.map(\|v\| v as u64))`; profile field is `i64`. |
| `ExecutionOptions.output_schema: Option<Value>` | `CompletionRequest.output_schema: Option<rig_core::schemars::Schema>` | `serde_json::from_value` → `StructuredOutputInvalid` on failure. |
| `ExecutionCommand.metadata: Value` (non-null) | `CompletionRequest.additional_params` | `json!({ "moira": { "request_id": command.request_id } })` — the request id, **not** the metadata. Metadata itself never leaves Moira. |
| (routing decision) | `CompletionRequest.model` | Always `None`; model identity lives in the handle and the cache key. |
| — | `documents`, `tools`, `tool_choice` | Always empty / `None` today. |
| `CompletionResponse.choice: OneOrMany<AssistantContent>` | `RuntimeCompletionOutput.text: String` | `text_from_choice`: keep `AssistantContent::Text(t) => t.text`, drop everything else, `join("")`. |
| `CompletionResponse.usage: Usage` | `UsageSummary` | `usage_from_rig` — zero is "not reported", not "zero tokens". |
| `CompletionResponse.message_id: Option<String>` | `RuntimeCompletionOutput.provider_request_id` | Direct, but `None` for every provider Moira uses. |
| `CompletionResponse.raw_response: M::Response` | — | Dropped. Recoverable only by adding a bound (see below). |

## Reading CompletionResponse

`rig-core-0.40.0/src/completion/request.rs:487-499`:

```rust
#[derive(Debug)]
pub struct CompletionResponse<T> {
    pub choice: OneOrMany<AssistantContent>,
    pub usage: Usage,
    pub raw_response: T,
    pub message_id: Option<String>,
}
```

Only `Debug` is derived — no `Clone`, no `Serialize`. Consume it by value.

`AssistantContent` (`message.rs:58-69`) has **four** variants and is `#[serde(untagged)]`:

```rust
pub enum AssistantContent { Text(Text), ToolCall(ToolCall), Reasoning(Reasoning), Image(Image) }
```

Moira's narrowing:

```rust
fn text_from_choice(choice: OneOrMany<AssistantContent>) -> String {
    choice
        .into_iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
```

Rules:

1. **Keep the catch-all arm and keep dropping `Reasoning`.** Provider reasoning content must not appear in `output_text`; that is a deliberate exclusion, not an oversight. The OpenAI non-streaming path pushes `AssistantContent::reasoning(...)` into the choice for backends that expose it (`openai/completion/mod.rs:1154-1159`).
2. **OpenAI refusals arrive as `AssistantContent::Text`.** OpenAI's own wire enum (`openai::completion::AssistantContent`, `openai/completion/mod.rs:273`) has a `Refusal { refusal }` variant, but the decoder collapses both `Text` and `Refusal` into `completion::AssistantContent::text(s)` (`:1142-1145`). By the time the value reaches `text_from_choice` a refusal is indistinguishable from normal output. Do not build "was this refused" logic on the choice — the signal is not there.
3. **`join("")`, not `join("\n")`.** Multiple text blocks are contiguous segments of one message.
4. **An empty assistant message is a hard error, not empty text.** OpenAI returns `CompletionError::ResponseError("Response contained no message or tool call (empty)")` when the message has no non-empty text, no reasoning, and no tool calls (`openai/completion/mod.rs:1180-1184`). That error carries no HTTP status, so `classify_completion_error`'s substring branch matches `"response"` and yields `ProviderInvalidResponse` — which `is_circuit_failure` accepts but `is_retryable` and `is_fallback_eligible` both reject (`src/orchestration/controls.rs:628-665`). A truncated or filtered empty completion can therefore open the provider circuit. Do not "fix" this by loosening the substring match; if it needs changing, change it once in `classify_completion_error` and cover it in `tests/support/mock_openai.rs`.
5. **`message_id` is `None` for OpenAI Chat Completions, Anthropic, Gemini, DeepSeek, and Azure** (`openai/completion/mod.rs:1196`, `anthropic/completion.rs:260`, `gemini/completion.rs:529`). Only the OpenAI Responses API populates it. `RuntimeCompletionOutput.provider_request_id` is consequently always `None` on the current handles — never build a feature that assumes it.

To recover a real provider request id, read it off `raw_response` behind a trait bound rather than adding a per-provider match. `rig_core::telemetry::ProviderResponseExt` (`telemetry/mod.rs:10-30`) exposes `get_response_id() -> Option<String>`, and every `Response` type behind the five `RuntimeModelHandle` arms implements it — four distinct types, since `azure::CompletionModel` reuses `openai::CompletionResponse`: `openai/completion/mod.rs:1201`, `anthropic/completion.rs:69`, `deepseek.rs:202`, `gemini/completion.rs:596`.

Widen the bound on `completion_with_model` and keep `output_from_response` as the single narrowing site — do not inline the narrowing to get at the id:

```rust
use rig_core::telemetry::ProviderResponseExt;

fn output_from_response<T: ProviderResponseExt>(
    response: CompletionResponse<T>,
) -> RuntimeCompletionOutput {
    let provider_request_id = response
        .message_id
        .clone()
        .or_else(|| response.raw_response.get_response_id());
    RuntimeCompletionOutput {
        text: text_from_choice(response.choice),
        usage: usage_from_rig(response.usage),
        provider_request_id,
    }
}

async fn completion_with_model<M>(
    model: &M,
    request: CompletionRequest,
) -> Result<RuntimeCompletionOutput, ExecutionFailure>
where
    M: RigCompletionModel,
    M::Response: ProviderResponseExt,
{
    let response = model
        .completion(request)
        .await
        .map_err(classify_completion_error)?;
    Ok(output_from_response(response))
}
```

The bound propagates to every `RuntimeModelHandle` arm; all five satisfy it today. `ProviderResponseExt` also offers `get_output_messages()`, `get_text_response()`, and `get_usage()` — none of which Moira may call. `raw_response` is the full provider payload; do not serialise it into events, logs, or persistence, and do not route it through a second narrowing path.

## Usage Extraction

`Usage` (`request.rs:532-550`) is seven non-optional `u64` fields; **`0` is the documented sentinel for "the provider reported nothing"** (`:530-531`). `Usage::has_values()` is `*self != Self::new()` (`:570-572`) and is the only way to tell "unknown" from "genuinely zero" — and even it cannot separate the two per-field.

```rust
pub fn usage_from_rig(usage: Usage) -> UsageSummary {
    if !usage.has_values() {
        return UsageSummary::default();
    }
    UsageSummary {
        input_tokens: non_zero(usage.input_tokens),
        output_tokens: non_zero(usage.output_tokens),
        cached_input_tokens: non_zero(usage.cached_input_tokens),
        reasoning_tokens: non_zero(usage.reasoning_tokens),
        total_tokens: non_zero(usage.total_tokens),
    }
}
```

Rules:

1. Keep the sentinel translation in `usage_from_rig` and nowhere else. `non_zero` is a private helper in the same file (`src/orchestration/runtime_factory.rs`) that maps `0 → None`; every other layer sees `Option<u64>`.
2. `Usage::cache_creation_input_tokens` and `Usage::tool_use_prompt_tokens` exist in Rig but have no `UsageSummary` field. Adding one means a `UsageSummary` field, a `usage_records` migration, and an OpenAPI change — follow `.agents/skills/moira-openapi/SKILL.md`.
3. `impl Add`/`AddAssign for Usage` are plain field-wise `+` (`request.rs:581-608`), **not saturating** — overflow panics in debug and wraps silently in release. Do not accumulate raw `Usage` across attempts; accumulate `UsageSummary` in Moira.
4. Providers differ in what they populate. OpenAI derives `output_tokens` as `completion_tokens.unwrap_or(total_tokens.saturating_sub(prompt_tokens))` (`openai/completion/mod.rs:1344-1364`); some providers report only `total_tokens`. Never compute `total = input + output` in Moira.
5. `GetTokenUsage` is only needed on the streaming path (`M::StreamingResponse`). On the non-streaming path, `CompletionResponse.usage` is already normalised — do not call `token_usage()` on `raw_response`.
6. Failed attempts persist `UsageSummary::default()` by design (`src/application/execution.rs`); usage from a failed provider call is not billed.

## Parameter and Determinism Policy

1. **Rig 0.40 models exactly two sampling parameters**: `temperature` and `max_tokens`. There is no `top_p`, `seed`, `stop`, `frequency_penalty`, `presence_penalty`, or `n` on `CompletionRequest`. Anything else goes through `additional_params` under that section's rules, or does not go at all.
2. **Determinism is not available.** There is no seed field, and providers are non-deterministic even at `temperature = 0`. Do not document, promise, or test for reproducible completions. Test against the scripted server in `tests/support/mock_openai.rs`, never against a live provider's exact text.
3. **Precedence is fixed**: request options → agent profile → provider default. Never insert a Moira default between the profile and the provider — it would apply inconsistently across providers and become an undocumented part of the public contract.
4. **Validate ranges at the public boundary**, in `src/application/public.rs`, as `AppError::unprocessable`. Today neither `temperature` nor `max_tokens` is range-checked there; a `temperature: 50.0` reaches the provider and returns a 400 that Moira classifies as `ProviderUpstreamError` — retryable and fallback-eligible, so one bad request can hammer every candidate. Adding validation is the correct fix; loosening classification is not.
5. **`max_tokens` bounds are a routing concern**, not a request concern. If a model's context window matters, model it on `provider_models` and enforce it before `build_completion_request`.
6. Anything that changes what the caller can send, or what an error looks like, is an API change — follow `.agents/skills/moira-openapi/SKILL.md`.

## Workflow

1. Read `.agents/skills/moira-rig-integration/SKILL.md`, then `skills/moira-project-structure/SKILL.md`.
2. Verify every type path, field name, and method signature against the vendored crate before writing the call:
   ```bash
   RIG=/Users/nalhide/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.40.0
   rg -n 'pub struct CompletionRequest' -A 25 "$RIG/src/completion/request.rs"
   rg -n 'pub struct CompletionResponse' -A 12 "$RIG/src/completion/request.rs"
   rg -n 'impl UserContent' -A 60 "$RIG/src/completion/message.rs"
   rg -n 'additional_params|preamble|temperature|max_tokens' "$RIG/src/providers/<provider>/completion.rs"
   ```
   If a signature is not in the vendored tree, the API does not exist in 0.40.0.
3. Decide which of the three sites owns the change. New caller-visible input → `map_public_message` in `src/application/public.rs`. New request parameter or new provider-side encoding → `build_completion_request` in `src/application/execution.rs`. Response narrowing → `output_from_response` / `text_from_choice` / `usage_from_rig` in `src/orchestration/runtime_factory.rs`. Do not add a third.
4. For any request field you add or change, check its encoder in **every** provider Moira ships (`openai/completion/mod.rs`, `anthropic/completion.rs`, `gemini/completion.rs`, plus the `deepseek.rs` / `azure.rs` extension consts). Silent drops are the norm, not the exception — Gemini drops `temperature`/`max_tokens` without a `generationConfig`, DeepSeek drops `output_schema`, non-tool providers drop `tools`. Record what each provider does in the PR body.
5. Keep `CompletionRequest` struct-literal construction with all ten fields named. No `..Default::default()`.
6. Preserve redaction. Nothing from `additional_params`, `preamble`, `chat_history`, or `raw_response` may be logged, persisted as-is, or echoed into an error. Provider error text stays behind `safe_provider_error_message` (`src/orchestration/runtime_factory.rs:490`). `expose_secret()` stays at the provider builder only (`runtime_factory.rs:92`), and `RuntimeModelHandle`'s hand-written `Debug` must keep redacting every arm (`:52-62`, test at `:535`).
7. If failure classification changes, change it once in `classify_completion_error` and read `.agents/skills/moira-rig-errors-testing/SKILL.md`.
8. Extend `tests/support/mock_openai.rs` scripts and assert on the recorded request body — that harness is the only thing that proves a field actually reached the wire.
9. Update `docs/rig-integration.md` when the request contract changes.

## Pitfalls

- `..Default::default()` on `CompletionRequest` — there is no `Default`, and adding one locally would defeat the intended compile break on upstream changes.
- Using `CompletionRequestBuilder` and expecting `preamble` to survive; `build()` clears it and injects a system message instead.
- Assuming `Some("")` for `preamble` is inert — Anthropic and Gemini skip it, OpenAI sends an empty system message.
- Assuming Gemini honours `temperature` / `max_tokens`. It does not unless a `generationConfig` exists.
- Seeding a Gemini `generationConfig` with a bare `GenerationConfig::default()` — it carries `temperature: 1.0` and `maxOutputTokens: 4096`, not `None`. The same values leak in automatically whenever `output_schema` is set on Gemini.
- Assuming Anthropic tolerates `max_tokens: None`. Unknown model strings have no default and fail pre-flight, misclassified as retryable.
- Setting `CompletionRequest.model` on Azure — the deployment in the URL comes from the handle, so body and URL disagree.
- Hand-writing a `tools` key into `additional_params`. Anthropic and Gemini remove and reinterpret it; the OpenAI-compatible encoder does not, and emits a duplicate `"tools"` key in the body. Use `CompletionRequest::with_provider_tool` if you genuinely need provider-hosted tools.
- Matching `AssistantContent` without a catch-all — it has four variants (`Text`, `ToolCall`, `Reasoning`, `Image`) and is `untagged`.
- Treating `Usage` zeros as real counts, or accumulating `Usage` with `+` (non-saturating).
- Reading `provider_request_id` and expecting a value; it is `None` on every current handle.
- Pattern-matching `AssistantContent::Text` as a `String`; `Text` is a struct with `text` and `additional_params` (`message.rs:305-312`).
- Adding parameters to `additional_params` unconditionally across providers — an unknown top-level key can 400 on strict OpenAI-compatible gateways.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests covering a completion change must prove: the request body recorded by `tests/support/mock_openai.rs` contains exactly the fields expected (and none that were meant to be omitted); the exact credential reached the provider; empty message lists fail as `InvalidExecutionRequest` rather than as a provider error; an invalid `output_schema` fails as `StructuredOutputInvalid`; zero-valued provider usage maps to absent `UsageSummary` values; and no secret, raw provider body, `additional_params` content, or prompt text appears in any surfaced message.
