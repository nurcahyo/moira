---
name: moira-rig-tools
description: Define, register, and execute LLM tools through rig-core 0.40 inside Moira. Covers the exact 0.40 `Tool` trait (NAME, Args, Output, Error, description, parameters, call, call_with_extensions, call_structured, classify_error), JSON Schema authoring for tool parameters, ToolSet/ToolSetBuilder/ToolServer assembly, wiring `ToolDefinition` and `ToolChoice` into `CompletionRequest`, the multi-turn tool loop and turn budgets, tool-result round-trip into chat history, dynamic (RAG-retrieved) tools, tool failure/timeout classification, mapping tool errors into `ExecutionFailure`/`AppError`, and the security rules that keep credentials and internal prompts out of tool surfaces. Use when adding or changing a tool, enabling tool calling on an execution path, populating `CompletionRequest.tools` or `tool_choice`, handling `ToolCallStarted`/`ToolCallDelta`/`ToolCallCompleted`/`ToolResult` runtime events, exposing tool results over the public API, building an agent tool loop, or reviewing any code that imports `rig_core::tool`.
---

# Moira Rig Tools

## Core Rule

Rig owns tool dispatch primitives (`Tool`, `ToolSet`, `ToolServer`, `ToolDefinition`, `ToolExecutionResult`). Moira owns which tools a caller is allowed to see, the credentials a tool runs under, the turn budget, the failure taxonomy, and the event contract. Implement `rig_core::tool::Tool` directly. Do not wrap it in a Moira `Tool`-like trait, and do not build a parallel LLM/tool abstraction — the only Moira abstraction over Rig is `RuntimeFactory` / `RuntimeModelHandle` (`src/orchestration/runtime_factory.rs`).

Read `.agents/skills/moira-rig-integration/SKILL.md` first; it owns the boundary rules this skill specialises.

## As-Built State (verify before assuming otherwise)

Tool **execution** is currently disabled in Moira. Do not describe it as working.

| Fact | Location |
|---|---|
| `CompletionRequest.tools` is always `Vec::new()`, `tool_choice` always `None` | `src/application/execution.rs::build_completion_request` |
| `RuntimeCompletionOutput` is `{ text, usage, provider_request_id }` — no tool calls; `text_from_choice` drops every `AssistantContent::ToolCall` | `src/orchestration/runtime_factory.rs` |
| Streamed tool calls *are* surfaced as `RuntimeStreamItem::ToolCallStarted` / `ToolCallDelta` | `src/orchestration/runtime_factory.rs` |
| `RuntimeEventType::{ToolCallStarted, ToolCallDelta, ToolCallCompleted, ToolResult}` exist | `src/domain/runtime.rs` |
| All four tool event types are filtered out of the public SSE stream (`map_runtime_event` returns `None`) | `src/application/public.rs` |
| Public `Tool`-role input messages are rejected: `unsupported_message_role` / "tool messages require an approved tool registry" | `src/application/public.rs::map_public_message` |
| The public DTO **already carries** `tools: Vec<PublicToolDeclaration>` | `src/domain/public.rs` |
| `validate_request` enforces `maximum_tool_count`, then `policy.tools_enabled` + scope `moira:execution:use-tools`, then **hard-rejects** any non-empty `tools` with `unsupported_tool` / "client-defined tools are not registered in this phase" | `src/application/public.rs::validate_request` |
| `PublicApiSettings::maximum_tool_count` already exists (default `32`); there is **no** turn-budget setting yet | `src/config/settings.rs` |
| `rig-core = "0.40"` with default features (`reqwest`, `derive`, `rustls`) → `derive` on, **`rmcp` off** (no MCP tools without a feature bump) | `Cargo.toml` |

Enabling tool execution is therefore a multi-file change, not a one-line change. See "Workflow: enabling tool execution".

## The `Tool` trait — exact rig-core 0.40 shape

`rig-core-0.40.0/src/tool/mod.rs:133-247`. The trait is **flat**. There is no `definition()` method in 0.40 (web docs at rig.rs still show one — the vendored source wins). Rig synthesises `ToolDefinition` from `name()` + `description()` + `parameters()` via `tool_definition(&dyn ToolDyn)`.

```rust
pub trait Tool: Sized + WasmCompatSend + WasmCompatSync {
    const NAME: &'static str;

    type Error: std::error::Error + WasmCompatSend + WasmCompatSync + 'static;
    type Args: for<'a> Deserialize<'a> + WasmCompatSend + WasmCompatSync;
    type Output: Serialize;

    fn name(&self) -> String { Self::NAME.to_string() }        // provided
    fn description(&self) -> String;                            // required
    fn parameters(&self) -> serde_json::Value;                  // required, JSON Schema
    fn call(&self, args: Self::Args)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + WasmCompatSend;   // required

    fn call_with_extensions(&self, args: Self::Args, _extensions: &ToolCallExtensions)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + WasmCompatSend;   // provided -> call
    fn classify_error(&self, error: &Self::Error) -> ToolFailure;                      // provided -> ToolFailure::other
    fn call_structured(&self, args: Self::Args, extensions: &ToolCallExtensions)
        -> impl Future<Output = Result<ToolReturn<Self::Output>, Self::Error>> + WasmCompatSend; // provided
}
```

### Override decision table

| You need | Override | Why |
|---|---|---|
| Plain function of args | `call` only | Simplest; everything else defaults correctly. |
| Per-call caller context (tenant, request id, scoped token) | `call_with_extensions` | The agent loop drives `call_structured`, whose default delegates here. `call`'s body becomes unreachable on the structured path. |
| Handled failure that still shows text to the model, tool-authored denial, or result metadata | `call_structured` returning `ToolReturn` | Supersedes `call_with_extensions` on the structured path. |
| Machine-readable failure classes for policy/metrics | `classify_error` | Applied to every `Err(Self::Error)` regardless of which entry point ran. |
| A different advertised name per instance | `name` | The value of `name()` **at registration time** is the single source of truth for both advertisement and dispatch — not `NAME`. |

If you override both `call_with_extensions` and `call_structured`, Rig only calls `call_structured` on the structured (agent / `ToolSet::call_structured`) path — the `call_with_extensions` body runs only if your `call_structured` delegates to it explicitly, as the example below does. The same applies to `call`: under dynamic dispatch it is dead code unless something calls `Tool::call` directly.

### Argument and output serialization quirks

- `parse_tool_args` normalises a bare `null` to `{}` before deserializing `Args` (`src/tool/mod.rs:346-363`). Models send `null` for all-optional-argument tools; do not work around it.
- A JSON parse failure never reaches your code as an error: it becomes a `ToolFailureKind::InvalidArgs` outcome with model-visible text `failed to parse tool arguments: {err}` (`src/tool/mod.rs:443-451`).
- `serialize_tool_output` passes a `String` output through **verbatim**; anything else is `serde_json::to_value(..).to_string()` (`src/tool/mod.rs:339-344`). A `type Output = String` tool that returns JSON text is indistinguishable from a structured tool at the wire — prefer a real struct.
- `ToolResultContent::from_tool_output` re-parses output that looks like `{"type":"image",...}` or `{"response":...,"parts":[...]}` into image content (`src/completion/message.rs:929`). Rig's own agent path uses it (`src/agent/prompt_request/mod.rs:492`), so a tool returning arbitrary user JSON can have its output reinterpreted as an image. `ToolResultContent::text(..)` — used by `Message::tool_result{,_with_call_id}` and by the loop below — is verbatim and does not re-parse; prefer it for Moira-authored results.
- `Message::tool_result` / `Message::tool_result_with_call_id` each build a `Message::User` carrying **exactly one** `ToolResult`. They are correct for a single-tool-call turn only; for parallel tool calls build a `Vec<UserContent>` and push one `Message::User` (see the loop below).

## Complete tool implementation

Moira style: `thiserror` error enum, full-word names, lowercase error messages with no trailing period, explicit timeout, no `#[derive(Debug)]` on anything that can hold a secret.

```rust
use std::time::Duration;

use rig_core::tool::{Tool, ToolCallExtensions, ToolFailure, ToolReturn};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Per-call caller context injected by Moira. Never serialized to the model.
/// `ToolCallExtensions::insert` requires `Clone + Send + Sync + 'static`.
#[derive(Clone)]
pub struct ToolCallerScope {
    pub external_tenant_id: String,
    pub external_user_id: Option<String>,
    pub request_id: String,
    pub allow_ledger_history: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerToolError {
    #[error("caller scope was not supplied to the tool")]
    MissingCallerScope,
    #[error("ledger lookup exceeded the tool timeout")]
    Timeout,
    #[error("ledger entry was not found")]
    NotFound,
    #[error("ledger transport failed")]
    Transport,
    #[error("ledger returned http {status}")]
    Upstream { status: u16 },
}

#[derive(Debug, Deserialize)]
pub struct LedgerLookupArgs {
    pub entry_id: String,
    #[serde(default)]
    pub include_history: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LedgerEntry {
    pub entry_id: String,
    pub balance_minor_units: i64,
    pub currency: String,
}

/// `Output: Serialize` is what the model literally sees, so the type must be
/// able to express a refusal. `ToolReturn::denied(output)` still serializes
/// `output` as the model-visible text (`ToolReturn::into_execution_result`,
/// `src/tool/result.rs:635-663`) — never hand back a fabricated success shape.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LedgerLookupOutput {
    Found(LedgerEntry),
    Denied { reason: String },
}

pub struct LedgerLookupTool {
    client: reqwest::Client,
    base_url: url::Url,
    call_timeout: Duration,
}

impl LedgerLookupTool {
    pub fn new(client: reqwest::Client, base_url: url::Url, call_timeout: Duration) -> Self {
        Self {
            client,
            base_url,
            call_timeout,
        }
    }
}

impl std::fmt::Debug for LedgerLookupTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LedgerLookupTool(<redacted>)")
    }
}

impl Tool for LedgerLookupTool {
    const NAME: &'static str = "ledger_lookup";

    type Error = LedgerToolError;
    type Args = LedgerLookupArgs;
    type Output = LedgerLookupOutput;

    fn description(&self) -> String {
        "look up the current balance of a ledger entry by identifier".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "entry_id": {
                    "type": "string",
                    "description": "ledger entry identifier"
                },
                "include_history": {
                    "type": "boolean",
                    "description": "include the change log for the entry",
                    "default": false
                }
            },
            "required": ["entry_id"],
            "additionalProperties": false
        })
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        // `call` is required by the trait but unreachable on every Rig dispatch
        // path this tool is used on (ToolSet/agent -> call_structured ->
        // call_with_extensions). This tool cannot run without a caller scope, so
        // fail loudly instead of silently running unscoped. Tests call
        // `call_with_extensions` with a populated `ToolCallExtensions`.
        Err(LedgerToolError::MissingCallerScope)
    }

    async fn call_with_extensions(
        &self,
        args: Self::Args,
        extensions: &ToolCallExtensions,
    ) -> Result<Self::Output, Self::Error> {
        let scope = extensions
            .get::<ToolCallerScope>()
            .ok_or(LedgerToolError::MissingCallerScope)?;

        let url = self
            .base_url
            .join(&format!("entries/{}", args.entry_id))
            .map_err(|_| LedgerToolError::NotFound)?;

        let response = tokio::time::timeout(
            self.call_timeout,
            self.client
                .get(url)
                .header("x-moira-tenant", &scope.external_tenant_id)
                .header("x-request-id", &scope.request_id)
                .send(),
        )
        .await
        .map_err(|_| LedgerToolError::Timeout)?
        .map_err(|_| LedgerToolError::Transport)?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(LedgerToolError::NotFound);
        }
        if !status.is_success() {
            return Err(LedgerToolError::Upstream {
                status: status.as_u16(),
            });
        }

        response
            .json::<LedgerEntry>()
            .await
            .map(LedgerLookupOutput::Found)
            .map_err(|_| LedgerToolError::Transport)
    }

    fn classify_error(&self, error: &Self::Error) -> ToolFailure {
        match error {
            LedgerToolError::MissingCallerScope => {
                ToolFailure::permission_denied(error.to_string()).with_code("missing_caller_scope")
            }
            LedgerToolError::Timeout => ToolFailure::timeout(error.to_string()),
            LedgerToolError::NotFound => {
                ToolFailure::not_found(error.to_string()).with_http_status(404)
            }
            LedgerToolError::Transport => ToolFailure::network(error.to_string()),
            LedgerToolError::Upstream { status } => ToolFailure::provider(error.to_string())
                .with_http_status(*status)
                .with_retryable(*status >= 500),
        }
    }

    async fn call_structured(
        &self,
        args: Self::Args,
        extensions: &ToolCallExtensions,
    ) -> Result<ToolReturn<Self::Output>, Self::Error> {
        // Tool-authored refusal: the tool ran its own scope check and declined.
        // A framework hook returning Flow::Skip yields ToolOutcome::Skipped instead;
        // a tool can never author Skipped.
        let scope = extensions
            .require::<ToolCallerScope>()
            .map_err(|_| LedgerToolError::MissingCallerScope)?;
        if args.include_history && !scope.allow_ledger_history {
            return Ok(ToolReturn::denied(LedgerLookupOutput::Denied {
                reason: "ledger history is not available to this caller".to_string(),
            }));
        }
        self.call_with_extensions(args, extensions)
            .await
            .map(ToolReturn::success)
    }
}
```

`rig_core::tool_macro` (re-exported `rig_derive::rig_tool`, `src/lib.rs:189`, available because `derive` is a default feature) generates this boilerplate from a free function. Do not use it for Moira tools: it hides `classify_error`, gives no place for `ToolCallExtensions`, and produces a name that is not reviewable at the registry.

### JSON Schema rules for `parameters()`

- Always a top-level `{"type": "object", "properties": {...}, "required": [...]}`. Providers reject non-object parameter schemas.
- Set `"additionalProperties": false` — it is what makes OpenAI strict-schema mode viable and stops models inventing fields.
- Every property needs a `"description"`; it is the only guidance the model gets beyond the tool description.
- Keep the schema in sync with `Args` by hand, or derive it with `rig_core::schemars` (already a dependency path Moira uses for `output_schema` in `src/application/execution.rs`). If you derive it, do it in `parameters()` — never store a schema in the database that can drift from `Args`.
- `parameters()` is called on every request that advertises the tool. Keep it allocation-cheap and side-effect free; never read a secret or hit the network in it.

## Assembling tools

Three assembly surfaces exist in 0.40. Pick by lifetime.

| Surface | Type | Use when |
|---|---|---|
| `ToolSet::builder().static_tool(..).dynamic_tool(..).build()` | `ToolSet` | Fixed per-request tool list; you drive the loop yourself. This is Moira's shape. |
| `ToolServer::new().tool(..).dynamic_tools(sample, index, toolset).run()` (module `rig_core::tool::server`) | `ToolServerHandle` | Shared, mutable-at-runtime registry across agents. `add_tool`, `remove_tool`, `append_toolset`, `call_tool_with_extensions` are async and take `&self` behind an `RwLock`. |
| `AgentBuilder::tool(..)` / `.tools(Vec<Box<dyn ToolDyn>>)` | typestate builder | Only inside a Rig `Agent`; see `.agents/skills/moira-rig-agents-rag/SKILL.md`. |

```rust
use rig_core::tool::{ToolCallExtensions, ToolSet};

let tools: ToolSet = ToolSet::builder()
    .static_tool(LedgerLookupTool::new(client.clone(), base_url.clone(), timeout))
    .static_tool(InvoiceSearchTool::new(client, base_url, timeout))
    .build();

// Vec<ToolDefinition> in registration order; feed straight into CompletionRequest.tools.
let definitions = tools.get_tool_definitions()?;   // Result<_, ToolSetError>
```

Determinism and collision rules (`src/tool/mod.rs:553-778`):

- `ToolSet` is an `IndexMap`, so definition order == registration order, stable across processes. Safe to hash for an idempotency key.
- Re-registering a name **replaces in place**, keeps position, and logs `tracing::warn!`. A silent replacement is a Moira registry bug — validate uniqueness before building the set, do not rely on the warn.
- `delete_tool` uses `shift_remove`, preserving survivor order.
- `add_tools(other)` merges; existing names are replaced in place.

Execution entry points on `ToolSet`:

```rust
async fn call(&self, toolname: &str, args: String) -> Result<String, ToolSetError>;
async fn call_with_extensions(&self, toolname: &str, args: String, &ToolCallExtensions)
    -> Result<String, ToolSetError>;
async fn call_structured(&self, toolname: &str, args: String, &ToolCallExtensions)
    -> ToolExecutionResult;   // never Err; unknown name -> ToolOutcome::Error(kind = NotFound)
```

`args` is a **`String` of JSON**, while `ToolCall.function.arguments` is a `serde_json::Value` — always `to_string()` it at the call site. Prefer `call_structured`: it is the only variant that yields a `ToolOutcome` you can map onto Moira's failure taxonomy without parsing strings.

`ToolCallExtensions::EMPTY` is `pub(crate)`. From Moira, construct with `ToolCallExtensions::new()` and `insert` your typed scope; read it in the tool with `get::<T>()` or `require::<T>()` (which returns `MissingExtension` naming the absent type). `insert` requires `T: Clone + Send + Sync + 'static`, so the scope type must derive `Clone`. Values are keyed by `TypeId` — never insert a bare `String`; always use a newtype.

`ToolServer`, `ToolServerHandle`, and `ToolServerError` live in `rig_core::tool::server` (not re-exported from `rig_core::tool`); `Tool`, `ToolDyn`, `ToolSet`, `ToolSetBuilder`, `ToolSetError`, `ToolError`, and `tool_definition` are on `rig_core::tool` directly, and `ToolCallExtensions`, `ToolResultExtensions`, `MissingExtension`, `ToolExecutionResult`, `ToolFailure`, `ToolFailureKind`, `ToolOutcome`, `ToolReturn`, `ToolReturnOutcome` are re-exported there.

## Wiring tools into a request

`CompletionRequest` (`src/completion/request.rs:668-694`) is a plain struct literal in Moira — any upstream field addition breaks the call site deliberately. Populate two fields:

```rust
use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::completion::message::ToolChoice;

Ok(CompletionRequest {
    model: None,
    preamble: agent_profile.and_then(|profile| profile.preamble.clone()),
    chat_history,
    documents: Vec::new(),
    tools: tool_definitions,          // Vec<ToolDefinition>, registration order
    temperature,
    max_tokens,
    tool_choice,                      // Option<ToolChoice>
    additional_params,
    output_schema,
})
```

`ToolChoice` (`src/completion/message.rs:1345-1355`) is `Auto` (default) | `None` | `Required` | `Specific { function_names: Vec<String> }`. Map Moira's request DTO onto it in `src/application`, never in `src/orchestration`.

Rules:

- `tools` must be empty when the resolved model lacks a tool-use capability. Capability gating has two halves: the required-capability list is **assembled** in `src/application/public.rs` while preparing the execution (where `"vision"` and `"structured_output"` are pushed onto `required_capabilities`), flows through `ExecutionOptions::required_capabilities` into `EffectiveExecutionPolicy::required_capabilities`, and is **matched** against each candidate's `capabilities` by `capabilities_match` in `src/application/execution.rs`. Push a tool capability string in `public.rs`; do not special-case it inside `capabilities_match`. Do not send tools to a model that cannot use them.
- `ToolChoice::Required` plus an empty `tools` vector is a client error; reject it as `ExecutionFailureClass::InvalidExecutionRequest` before the provider call.
- `ToolChoice::Specific` names must be a subset of the advertised names; otherwise the provider returns a 4xx and `classify_completion_error` (`src/orchestration/runtime_factory.rs`) maps it status-first to `ProviderUpstreamError` — an opaque "provider request failed with HTTP 400" with no hint of the real cause. Validate the subset in `src/application` before the call.
- Provider-hosted tools (`web_search`, etc.) are a different type: `ProviderToolDefinition { kind: String, config: serde_json::Map }`, appended to `additional_params.tools` by `CompletionRequest::with_provider_tool(s)` (`src/completion/request.rs:752-800`). Moira already writes `additional_params` as `{"moira":{"request_id":...}}`, which survives the merge because it is an object — a **non-object** `additional_params` is silently replaced by an empty map. Verify the merged JSON in a test if you add provider tools.

## Workflow: enabling tool execution in Moira

1. Read `.agents/skills/moira-rig-integration/SKILL.md` and `skills/moira-project-structure/SKILL.md`.
2. Place code by responsibility: `Tool` impls and `ToolSet` construction in `src/orchestration` (Rig types never escape it); tool registry/permission records and Moira-facing tool DTOs in `src/domain`; registry resolution, allow-listing, and turn-budget policy in `src/application`; public request/response tool shapes in `src/application/public.rs` + `src/http`.
3. Extend `RuntimeCompletionOutput` with `tool_calls: Vec<rig_core::completion::message::ToolCall>` and stop discarding them in `text_from_choice` — today the non-streaming path drops every `AssistantContent::ToolCall`. Without this, the loop cannot see a tool call.
4. Add a tool-registry resolution step alongside credential resolution in `src/application/execution.rs`, scoped by tenant/application the same way credentials are, producing a `ToolSet` plus a `Vec<ToolDefinition>`.
5. Populate `CompletionRequest.tools` / `tool_choice` in `build_completion_request`; add a `maximum_tool_turns` field to the runtime policy (`src/config/settings.rs` naming: full words, no abbreviations — `maximum_tool_count` already exists there and caps *declared* tools, not turns).
6. Drive the loop (next section) inside the orchestration execution boundary.
7. Emit `RuntimeEventType::ToolCallStarted`, `ToolCallDelta`, `ToolCallCompleted`, and `ToolResult` through `EventCollector::push_stream` with the same `send_timeout = idle_timeout` and `StreamBackpressureExceeded` handling as `OutputTextDelta`.
8. Decide the public contract explicitly. `map_runtime_event` currently returns `None` for all four tool events; unfiltering any of them is a public API change requiring `.agents/skills/moira-openapi/SKILL.md` and a redaction review of the payload.
9. Lift the two public-API rejections only once a real registry exists, and lift them together:
   - `validate_request` — the `unsupported_tool` / "client-defined tools are not registered in this phase" branch that fires for any non-empty `request.tools`. Keep the `maximum_tool_count`, `policy.tools_enabled`, and `moira:execution:use-tools` scope checks that precede it; they are already correct.
   - `map_public_message` — the `unsupported_message_role` / "tool messages require an approved tool registry" branch. Both error codes and messages are public contract; changing them is an OpenAPI change.
10. Persist tool activity in the attempt record; keep `UsageSummary` sourced from Rig only (`usage_from_rig`), never synthesized from tool counts.

## The multi-turn tool loop

Rig's own agent loop (`AgentRunner`, `AgentRun`) is documented in `.agents/skills/moira-rig-agents-rag/SKILL.md`. At Moira's boundary you drive the loop yourself over `RuntimeModelHandle`, because Moira owns retries, deadlines, circuit breaking, permits, and cancellation.

```rust
use rig_core::OneOrMany;
use rig_core::completion::message::{
    AssistantContent, Message, ToolCall, ToolResultContent, UserContent,
};
use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::tool::{ToolCallExtensions, ToolFailureKind, ToolSet};

use crate::domain::{ExecutionFailure, ExecutionFailureClass};
use crate::orchestration::RuntimeModelHandle;

pub struct ToolLoopOutcome {
    pub text: String,
    pub turns: usize,
}

/// `maximum_tool_turns` counts TOTAL model calls, matching Rig's own semantics.
/// One tool call plus a final answer needs at least 2.
pub async fn run_tool_loop(
    handle: &RuntimeModelHandle,
    tools: &ToolSet,
    definitions: Vec<ToolDefinition>,
    mut request: CompletionRequest,
    extensions: &ToolCallExtensions,
    maximum_tool_turns: usize,
) -> Result<ToolLoopOutcome, ExecutionFailure> {
    if maximum_tool_turns == 0 {
        return Err(ExecutionFailure::new(
            ExecutionFailureClass::InvalidExecutionRequest,
            "tool turn budget must be at least one",
        ));
    }

    let mut history: Vec<Message> = request.chat_history.iter().cloned().collect();

    for turn in 1..=maximum_tool_turns {
        request.tools = definitions.clone();
        request.chat_history = OneOrMany::many(history.clone()).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::InvalidExecutionRequest,
                "execution command must contain at least one message",
            )
        })?;

        let output = handle.completion(request.clone()).await?;

        // Requires RuntimeCompletionOutput to carry tool_calls (see workflow step 3).
        if output.tool_calls.is_empty() {
            return Ok(ToolLoopOutcome {
                text: output.text,
                turns: turn,
            });
        }

        // One assistant message holding every tool call of the turn ...
        let assistant_content: Vec<AssistantContent> = output
            .tool_calls
            .iter()
            .cloned()
            .map(AssistantContent::ToolCall)
            .collect();
        let content = OneOrMany::many(assistant_content).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::ProviderInvalidResponse,
                "provider returned an empty assistant turn",
            )
        })?;
        history.push(Message::Assistant {
            id: output.provider_request_id.clone(),
            content,
        });

        // ... then EXACTLY ONE user message holding every tool result, in call
        // order. Rig's own driver does the same (`AgentRun::tool_results` appends
        // a single `Message::User`, src/agent/run/mod.rs:974-1022) because that is
        // what providers require for parallel tool calls — Anthropic rejects
        // tool_result blocks split across several user turns.
        let mut results: Vec<UserContent> = Vec::with_capacity(output.tool_calls.len());
        for tool_call in &output.tool_calls {
            let result = execute_tool(tools, tool_call, extensions).await?;
            let content = OneOrMany::one(ToolResultContent::text(result));
            results.push(match tool_call.call_id.clone() {
                // Note the asymmetry: `UserContent::tool_result_with_call_id`
                // takes `call_id: String`, while `Message::tool_result_with_call_id`
                // takes `Option<String>`.
                Some(call_id) => {
                    UserContent::tool_result_with_call_id(tool_call.id.clone(), call_id, content)
                }
                None => UserContent::tool_result(tool_call.id.clone(), content),
            });
        }
        let content = OneOrMany::many(results).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::InternalError,
                "tool execution produced no tool results",
            )
        })?;
        history.push(Message::User { content });
    }

    Err(ExecutionFailure::new(
        ExecutionFailureClass::DeadlineExceeded,
        "execution exceeded the configured tool turn budget",
    ))
}

async fn execute_tool(
    tools: &ToolSet,
    tool_call: &ToolCall,
    extensions: &ToolCallExtensions,
) -> Result<String, ExecutionFailure> {
    let arguments = tool_call.function.arguments.to_string();
    let execution = tools
        .call_structured(&tool_call.function.name, arguments, extensions)
        .await;

    // `outcome()` borrows (`&ToolOutcome`); the enum is #[non_exhaustive], so
    // prefer its accessors over an exhaustive match.
    let outcome = execution.outcome();
    tracing::info!(
        tool_name = %tool_call.function.name,
        outcome = outcome.as_str(),                       // success | error | skipped | denied
        failure_kind = outcome.error_kind().map(ToolFailureKind::as_str),
        "tool call completed"
    );

    // Every outcome stays in-band: the model sees `model_output` and can recover.
    // An unregistered tool name arrives here as
    // ToolOutcome::Error(ToolFailure { kind: ToolFailureKind::NotFound, .. }) —
    // not a distinct outcome variant, and never an Err.
    Ok(execution.model_output().to_string())
}
```

Loop invariants you must preserve:

- **Turn budget counts model calls, not tool calls.** Rig's own default is `1` (`AgentRunner::from_agent` uses `agent.default_max_turns.unwrap_or(1)`, `src/agent/runner.rs:306`), which is the single most common footgun: an agent with tools and no explicit budget fails with `PromptError::MaxTurnsError` right after the first tool call. Surface the budget as an explicit runtime-policy field, never a hidden default.
- **History shape is fixed**: one assistant message containing N `AssistantContent::ToolCall`, then **one** user message containing N `UserContent::ToolResult`. Rig's `AgentRun::tool_results` (`src/agent/run/mod.rs:974-1022`) matches results against pending calls by tool-call id **as a multiset** — every pending id must be answered exactly once, duplicates within a turn are allowed, an unknown or already-answered id is a protocol violation, and an empty result set cancels the run.
- **Results are persisted in call order**, never completion order, even if you execute concurrently (`AgentRunner::tool_concurrency`). Rig accepts results in any order; providers do not.
- **Committed output is terminal.** `ExecutionFailure::new` (`src/orchestration/controls.rs`) derives `retryable` / `fallback_eligible` from the class alone, so you must override both to `false` **after** construction once any delta or tool call has been emitted downstream — exactly as `execute_rig_stream` and `attempt_timeout_failure` do in `src/application/execution.rs`, gated on `EventCollector::output_committed`. Re-running a side-effecting tool after a partial stream is a correctness bug, not a retry.
- **Cancellation stays out-of-band.** Rig masks stream aborts as clean EOF via an `"aborted"` substring match (`src/streaming.rs:459-465`); use Moira's `CancellationToken`, and abandon in-flight tool futures on cancel rather than inferring cancellation from the stream.
- **Deadline and idle timeout still apply.** Wrap the whole loop in the existing attempt timeout; tool execution time counts against it.

## Tool failure and timeout policy

Rig has **no generic per-tool timeout**. `ToolSet` and `ToolServer` never bound a tool call; only MCP tools get `DEFAULT_MCP_TOOL_TIMEOUT` via `rmcp_tool_with_timeout`, and the `rmcp` feature is off in Moira. Every Moira tool must bound itself with `tokio::time::timeout` and map elapsed to `ToolFailure::timeout` through `classify_error`.

`ToolFailureKind` → default retryability (`src/tool/result.rs:68-84`), and the label emitted by `as_str()`:

| Kind | `as_str()` | Default `retryable` | Map to `ExecutionFailureClass` when it must terminate the attempt |
|---|---|---|---|
| `InvalidArgs` | `invalid_args` | `Some(false)` | keep in-band; let the model retry with corrected args |
| `Timeout` | `timeout` | `Some(true)` | `ProviderTimeout` |
| `Cancelled` | `cancelled` | `Some(false)` | `RequestCancelled` |
| `NotFound` | `not_found` | `Some(false)` | keep in-band |
| `PermissionDenied` | `permission_denied` | `Some(false)` | `CredentialForbidden` or `RouteForbidden` per cause |
| `RateLimited` | `rate_limited` | `Some(true)` | `CapacityExhausted` |
| `Provider` | `provider` | `None` | `ProviderUpstreamError` |
| `Network` | `network` | `Some(true)` | `ProviderConnectionFailed` |
| `Other` | `other` | `None` | `InternalError` |

Decision rule: **prefer in-band**. A `ToolOutcome::Error` whose `model_output` describes the failure lets the model recover within the same execution and costs one extra turn. Escalate to an `ExecutionFailure` only when the tool cannot succeed for this caller at all (permission, cancellation, exhausted budget) or when the failure is Moira infrastructure rather than the tool.

`ToolOutcome::as_str()` yields `"success" | "error" | "skipped" | "denied"` — use those verbatim as metric labels and event payload values so they stay comparable with Rig's own telemetry. Do not invent a parallel vocabulary.

Never route a tool failure through `AppError::Upstream`. `ExecutionFailure` is carried inside a *successful* `ExecutionOutcome`; `AppError` is reserved for infrastructure faults and factory/config failures (`src/error.rs`, `src/application/execution.rs`). If a new terminal class is needed, add an `ExecutionFailureClass` variant — `failure_code` in `src/application/public.rs` has no `_` arm, so the compiler forces you to define the public code and `failure_http_status` mapping.

Distinguish the three "did not run" states, they are not synonyms:

- `ToolOutcome::Skipped` — framework hook returned `Flow::Skip` (this is also how Rig's approval policies deny). Crate-authored only.
- `ToolOutcome::Denied` — the **tool** refused after its own check (`ToolReturn::denied` / `ToolExecutionResult::denied`).
- `ToolOutcome::Error(ToolFailure { kind: ToolFailureKind::NotFound, .. })` from `ToolSet::call_structured` — the tool name is not registered. This means the model hallucinated a tool or the registry drifted; log the advertised name list, not the arguments. (The same kind also appears when a *registered* tool classifies its own 404 that way — disambiguate on `ToolSet::contains`, not on the kind.)

## Dynamic (retrieved) tools

Use `ToolEmbedding` when the tool catalogue is larger than a request should advertise (`src/tool/mod.rs:250-275`).

```rust
pub trait ToolEmbedding: Tool {
    type InitError: std::error::Error + WasmCompatSend + WasmCompatSync + 'static;
    type Context: for<'a> Deserialize<'a> + Serialize;   // persisted alongside the vector
    type State: WasmCompatSend;                          // runtime deps injected at init
    fn embedding_docs(&self) -> Vec<String>;             // empty => never retrieved
    fn context(&self) -> Self::Context;
    fn init(state: Self::State, context: Self::Context) -> Result<Self, Self::InitError>;
}
```

Register with `ToolSet::builder().dynamic_tool(..)`, embed via `toolset.schemas()?` (`Vec<ToolSchema>`, keyed by the **registered** name, embedding-typed tools only), index them, and retrieve per prompt.

Hard rule: `Context` is persisted in the vector store. Put configuration there — endpoints, model keys, feature flags. Put credentials, decrypted material, and tokens in `State`, injected at `init` from Moira's credential resolution path. A secret in `Context` is a secret written to the vector store.

See `.agents/skills/moira-rig-agents-rag/SKILL.md` for the index/agent wiring.

## Security rules

Non-negotiable. A tool is a model-controlled call into Moira; treat every argument as untrusted input from the caller's model.

1. **Never accept a credential as a tool argument.** `Args` is model-authored. Credentials arrive only via `ToolCallExtensions` (Moira-authored, never serialized to the model) or via `ToolEmbedding::State`.
2. **Never return a secret in `Output`.** `Output: Serialize` goes verbatim into chat history and into every subsequent request. No API keys, hashes, ciphertext, nonces, peppers, decrypted JWT material, connection strings, or raw provider error bodies.
3. **Never expose internal prompts, preambles, protected instructions, routing decisions, or credential metadata through a tool** — not as output, not in `description()`, not in `parameters()` descriptions.
4. **Sanitize upstream errors the way the Rig boundary already does.** `safe_provider_error_message` yields class + status only. A tool's `model_output` on failure must be a short classified message; put the detail in `ToolFailure.message` (metadata, not model-visible) and in `ToolResultExtensions` (never sent to the model).
5. **Do not log arguments or outputs at `info` or above.** Rig itself logs args at `tracing::debug!(target: "rig", ...)` in `ToolSet::call_with_extensions` and `ToolServerHandle::call_tool_with_extensions` — ensure production filtering keeps `rig` at `warn` or the arguments of every tool call land in logs.
6. **Implement manual `Debug`** on any tool struct holding a client, key, or `SecretString`, exactly as `RuntimeModelHandle` and `ResolvedCredential` do. Never `#[derive(Debug)]` on those.
7. **Scope every tool to the caller.** Tenant/application/user scoping must be enforced inside the tool from `ToolCallExtensions`, not inferred from arguments — a model can name any tenant id it likes.
8. **Rewriting a result never hides a failure.** Rig's `RewriteResult` changes only the model-visible string; `outcome` and `extensions` keep the raw structured result (`src/agent/hook.rs:540-546`). Do not build a redaction step that also mutates the outcome.
9. **Tool schemas and examples are public API surface.** Anything you put in a `ToolDefinition` can end up in the OpenAPI document — apply the same exclusions as `.agents/skills/moira-openapi/SKILL.md` step 8.

## Pitfalls

- Writing `fn definition(&self) -> ToolDefinition`. Does not exist in 0.40; the trait is `description()` + `parameters()`. Web docs are stale.
- Assuming `NAME` is the dispatch key. The registered `name()` value is, and it can differ per instance.
- Passing `tool_call.function.arguments` (a `Value`) where a `String` is required. Always `.to_string()`.
- Expecting `ToolSet::call_structured` to return `Err` for an unknown tool. It returns `ToolOutcome::Error(ToolFailure { kind: NotFound, .. })` inside a `ToolExecutionResult`; only `call` / `call_with_extensions` return `Err(ToolSetError::ToolNotFoundError)`.
- Matching `ToolOutcome`, `ToolReturnOutcome`, or `ToolFailureKind` exhaustively, or struct-literalling `ToolFailure` / `ToolExecutionResult`. All five are `#[non_exhaustive]`: matches need a wildcard arm, and the structs can only be built through their constructors (`ToolFailure::timeout(..)`, `ToolExecutionResult::failed(..)`, …).
- Splitting parallel tool results across several `Message::User` turns. Providers require them in one user message.
- Relying on `call` being invoked by an agent loop. It is not; `call_structured` is. Put logic in `call_with_extensions`.
- Setting a turn budget of 1 with tools attached. Guarantees failure after the first tool call.
- Re-executing tools on a retry after output was already committed downstream.
- Registering two tools with the same name and expecting an error. You get a silent in-place replacement plus a `warn` log.
- Adding MCP tools. `rmcp` is not an enabled feature in Moira; enabling it is a dependency decision, not a tool change.
- Extending `src/orchestration/executor.rs` (legacy raw-`reqwest` V1 path) with tool support. Its only consumer `src/http/chat.rs` is not even compiled — `src/http/mod.rs` declares no `mod chat;`. All new work goes through `RuntimeFactory` / `RuntimeModelHandle`.

## Related skills

- `.agents/skills/moira-rig-integration/SKILL.md` — boundary, seam, verification and upgrade procedure.
- `.agents/skills/moira-rig-completions/SKILL.md` — `CompletionRequest` construction and response handling.
- `.agents/skills/moira-rig-streaming/SKILL.md` — streamed `ToolCall` / `ToolCallDelta` items and event backpressure.
- `.agents/skills/moira-rig-agents-rag/SKILL.md` — `Agent`, `AgentRunner`, hooks, dynamic tool retrieval.
- `.agents/skills/moira-rig-errors-testing/SKILL.md` — failure classification and the scripted-provider test harness.
- `.agents/skills/moira-openapi/SKILL.md` — required whenever tool shapes reach the HTTP contract.
- `skills/moira-project-structure/SKILL.md` — module placement.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests must prove, at minimum: `parameters()` is a valid object schema whose `required` names all exist in `properties`; every registered tool name is unique before `ToolSet` construction; `classify_error` maps each error variant to the intended `ToolFailureKind`; an argument-parse failure produces an `InvalidArgs` outcome rather than a panic or a terminal failure; the tool loop terminates at the configured turn budget; every tool-call id in an assistant turn receives exactly one tool result; and no tool output, log line, or error message contains a credential (assert against the literal secret used by the fixture, as `tests/execution_lifecycle.rs` does with `sk-lifecycle-secret`).
