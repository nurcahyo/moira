---
name: moira-rig-streaming
description: Bridge rig-core 0.40 streaming completions to Moira's runtime event pipeline and Axum SSE contract. Covers obtaining a StreamingCompletionResponse from a Rig CompletionModel, exhaustive StreamedAssistantContent handling, boxing and pinning with the correct Send/Unpin/'static bounds, draining to capture final Usage, translating chunks into RuntimeStreamItem and RuntimeEventEnvelope, mid-stream error classification, cancellation and idle timeouts, ordering and flush guarantees, and testing streams without a network. Use when adding or changing streaming execution, adding a streamed chunk kind, touching the stream idle timeout, backpressure, cancellation, usage capture, tool-call deltas, reasoning deltas, the RuntimeStreamItem or RuntimeEventType enums, the public SSE event mapping, or any test that exercises a streamed provider response.
---

# Moira Rig Streaming

## Core Rule

Rig owns SSE transport, frame parsing, and the chunk taxonomy. Moira owns event envelopes, sequencing, idle timeouts, cancellation, backpressure, failure classification, and the public SSE contract.

There is exactly **one** Rig-stream-to-Moira adapter: `start_stream_with_model` in `src/orchestration/runtime_factory.rs`. Extend it. Never add a second adapter, never re-parse provider SSE bytes, and never build a parallel LLM streaming abstraction.

The anti-pattern has a name and a corpse. The deleted `src/orchestration/executor.rs::stream_chat` (removed in plan 06, along with `src/http/chat.rs` and the `ChatCompletionRequest` / `ChatMessage` DTOs) used Rig only to compute a base URL, then raw-`reqwest` POSTed `/chat/completions` and forwarded `response.bytes_stream()` as opaque `provider_chunk` events — no chunk parsing, no usage capture, no cancellation, and only a whole-request `reqwest` `.timeout()` rather than a per-chunk idle bound. If a change starts to look like that, it is wrong; do not reintroduce it under a new name.

Read `.agents/skills/moira-rig-integration/SKILL.md` before touching the seam, and `.agents/skills/moira-rig-completions/SKILL.md` for the non-streaming twin of this path.

## The Four Hops

| Hop | Location | Input | Output |
|---|---|---|---|
| 1 | `runtime_factory.rs::start_stream_with_model` | `StreamedAssistantContent<M::StreamingResponse>` | `RuntimeStreamItem` |
| 2 | `application/execution.rs::execute_rig_stream` | `RuntimeStreamItem` | `RuntimeEventEnvelope` via `EventCollector::push_stream` |
| 3 | `EventCollector` → `ExecutionStreamHandle` (`domain/runtime.rs`) | `RuntimeEventEnvelope` | bounded `mpsc` + `oneshot` outcome + `CancellationToken` |
| 4 | `application/public.rs::supervise_public_stream` → `http/public.rs::sse_event` | `RuntimeEventEnvelope` | `PublicSseEnvelope` → `axum::response::sse::Event` |

A new streamed signal that must reach clients requires a change at every hop plus a `RuntimeEventType` variant. A signal that must only be captured internally stops at hop 2 or 3.

## Obtaining a Stream in rig-core 0.40

`CompletionModel::stream` is a method on the completion trait itself (`rig-core-0.40.0/src/completion/request.rs:639-644`; the trait starts at `:613`). There is **no** `StreamingCompletionModel` trait in 0.40.

```rust
fn stream(
    &self,
    request: CompletionRequest,
) -> impl std::future::Future<
    Output = Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>,
> + WasmCompatSend;
```

Rules:

1. The outer `Result` is the HTTP handshake only. Per-chunk failures arrive later as `Err` **items**. Classify both through `classify_completion_error`.
2. `completion` and `stream` are RPITIT, so `CompletionModel` is not object safe. Dispatch through the `RuntimeModelHandle` enum, never `Box<dyn CompletionModel>`.
3. Each of Moira's five handles declares a different `Self::StreamingResponse`:

   | Handle | Rig model type | `StreamingResponse` |
   |---|---|---|
   | `OpenAi` | `openai::completion::CompletionModel` | `openai::completion::streaming::StreamingCompletionResponse<Ext::StreamingUsage>` (`openai/completion/mod.rs:1901`) |
   | `Anthropic` | `anthropic::completion::CompletionModel` | `anthropic::streaming::StreamingCompletionResponse` (`anthropic/completion.rs:2438`) |
   | `Gemini` | `gemini::completion::CompletionModel` | `gemini::streaming::StreamingCompletionResponse` (`gemini/completion.rs:85`) — note the `streaming` module, not `completion` |
   | `DeepSeek` | `deepseek::CompletionModel` = `openai::completion::GenericCompletionModel<DeepSeekExt, H>` (`deepseek.rs:159`) | same OpenAI generic, with DeepSeek's `StreamingUsage` |
   | `AzureOpenAi` | `azure::CompletionModel` = `openai::completion::GenericCompletionModel<AzureExt, H>` (`azure.rs:555`) | same OpenAI generic, with `openai::Usage` |

   The enum therefore **cannot** return one `StreamingCompletionResponse<R>`; it normalises at item level to `RuntimeItemStream`.
4. Keep the per-variant match thin — one arm per variant delegating to the generic free function, exactly as `RuntimeModelHandle::start_stream` does. Monomorphisation, not dynamic dispatch, is what makes the differing associated types work.
5. The OpenAI-compatible path injects `stream_options.include_usage = true` with a shallow merge that preserves caller keys (`src/providers/openai/completion/streaming.rs:166-184`). It is gated on `OpenAICompatibleProvider::STREAM_INCLUDE_USAGE`, which defaults to `true` (`src/providers/openai/completion/mod.rs:1415`) and is not overridden by `AzureExt` (`src/providers/azure.rs:558`) or `DeepSeekExt` (`src/providers/deepseek.rs:51`) — so all three of Moira's OpenAI-shaped providers get it. Do not set it yourself in `additional_params`.

## StreamedAssistantContent — Exhaustive Handling

`StreamedAssistantContent<R>` (`src/streaming.rs:1040-1079`) is **not** `#[non_exhaustive]`. Match every variant explicitly and never write a `_` arm: an upstream variant addition must break the Moira build.

| Variant | Rig payload | Moira mapping today | Rule |
|---|---|---|---|
| `Text(Text)` | `Text { text, additional_params }` | `RuntimeStreamItem::TextDelta { text: delta.text }` | Forward `.text` only. `additional_params` is `#[serde(flatten)]` provider metadata; serialising the whole `Text` would leak unmodelled provider keys into the public payload. |
| `ToolCall { tool_call, internal_call_id }` | complete `ToolCall` | `ToolCallStarted { internal_call_id, name, arguments }` | `internal_call_id` is Rig-generated and is the correlation key against deltas. Never substitute `tool_call.id`. |
| `ToolCallDelta { id, internal_call_id, content }` | `ToolCallDeltaContent::Name \| Delta` | `ToolCallDelta { id, internal_call_id, content }` | Deltas are **not** aggregated by Rig into `choice`. If Moira ever needs assembled arguments it must accumulate them itself. |
| `Reasoning(Reasoning)` | complete reasoning block | dropped | Reasoning may contain chain-of-thought and provider-encrypted payloads. Dropping is deliberate; see the security rule below. |
| `ReasoningDelta { id, reasoning }` | partial reasoning text | dropped | Same. `UsageSummary.reasoning_tokens` is still captured from `Final`. |
| `Final(R)` | provider final response | `UsageUpdated { usage: usage_from_rig(response.token_usage()) }` | Always last, emitted at most once, and **never emitted when the stream terminated with an error**. |
| `Unknown(Value)` | provider-native item Rig does not model | dropped | `#[serde(untagged)]` makes `Unknown` match anything, which is why it is declared last. Forward it only after deciding, per-field, that nothing sensitive rides along. |

Variants that never reach a consumer: `RawStreamingChoice::TextStart` and `TextAdditionalParams` are folded into the accumulated `choice` (`src/streaming.rs:472-483`), and `MessageId` is captured into `stream.message_id` (`src/streaming.rs:543-546`). All three recurse via `poll_next_unpin` instead of yielding, so a consumer never sees them. Final `choice` aggregation happens on end-of-stream (`src/streaming.rs:444-457`).

**Security:** reasoning content, `Unknown` payloads, and `Text::additional_params` are unmodelled provider data. Do not log them, do not persist them, and do not widen the public SSE payload to include them without an explicit decision recorded in `docs/`. Never place credentials, decrypted material, or internal prompts in any streamed payload.

## Boxing, Pinning, and Bounds

`StreamingCompletionResponse<R>` wraps `Abortable<Pin<Box<dyn Stream + Send>>>` (`src/streaming.rs:243-262`), so it is already `Unpin` given its own `R: Clone + Unpin + GetTokenUsage` bound. Consume it with a plain `let mut stream` plus `futures_util::StreamExt` in scope. Do **not** `Box::pin` it, and do not reach for `pin_mut!`.

Box only at the erasure point, which is Moira's own alias:

```rust
pub type RuntimeItemStream =
    Pin<Box<dyn Stream<Item = Result<RuntimeStreamItem, ExecutionFailure>> + Send>>;
```

This is the exact bound set the repo declares on the generic adapter:

```rust
where
    M: RigCompletionModel,
    M::StreamingResponse:
        Clone + Unpin + rig_core::completion::GetTokenUsage + Serialize + Send + 'static,
```

- `Clone + Unpin + GetTokenUsage` are the bounds `StreamingCompletionResponse<R>` itself declares (`src/streaming.rs:243-245`).
- `'static` is the only bound genuinely *added* here: the `async_stream::stream!` body captures `stream` by value and the result is erased into an owned `Box<dyn Stream + Send>`. The rest restate what `CompletionModel::StreamingResponse` already requires (`Clone + Unpin + WasmCompatSend + WasmCompatSync + Serialize + DeserializeOwned + GetTokenUsage`, `request.rs:617-623`), and `WasmCompatSend: Send` on native targets (`src/wasm_compat.rs:8,14`). Keep the explicit list anyway — it matches the repo and documents intent — but do not treat it as evidence that Rig leaves those bounds open.
- Do **not** add `Sync`. Nothing in the pipeline shares the stream across tasks, and `CompletionModel` already implies `WasmCompatSync` on the associated type, so restating it buys nothing.
- The import alias is mandatory: `use rig_core::completion::CompletionModel as RigCompletionModel` — Moira has its own model concepts and the bare name collides.

## Draining for Final Usage

`stream.usage()` is `self.response.token_usage()` (`src/streaming.rs:313-315`). Facts that drive the code:

1. Usage is populated only after the provider's `FinalResponse` chunk is observed, and that chunk is the **last** item (`src/providers/internal/openai_chat_completions_compatible.rs:375-392`).
2. If the stream terminated with an error, `FinalResponse` is never yielded and usage stays at the zero sentinel. Report the failure; do not report a zero-usage success.
3. Zero is Rig's documented sentinel for "provider reported nothing". Use `Usage::has_values()`; `usage_from_rig` already early-returns `UsageSummary::default()` on it and maps each zero field to `None` via `non_zero`.
4. `total_tokens` is **not** computed uniformly across providers. Anthropic synthesises it as `input + cached_input + cache_creation + output` (`src/providers/anthropic/streaming.rs:201-215`); OpenAI reports the provider value. Never assert `input + output == total`.
5. Duplicate `FinalResponse` chunks are dropped by Rig (`src/streaming.rs:527-542`), which is why a plain `reported_usage = usage.has_any()` assignment is safe rather than an accumulating `|=`.
6. `stream.choice` is populated only at end-of-stream, and `stream.message_id` only once the provider emits a `MessageId` chunk. Read both after the loop — `message_id` becomes `FinalMetadata.provider_request_id`.
7. `usage_from_rig` is lossy on purpose: `UsageSummary` carries only `input_tokens`, `output_tokens`, `cached_input_tokens`, `reasoning_tokens`, `total_tokens`. Rig's `Usage::cache_creation_input_tokens` and `Usage::tool_use_prompt_tokens` are **dropped**. Adding either to a streamed payload means widening `UsageSummary` and every persistence and public-DTO hop that carries it, not just this adapter.

Order of preference in the adapter: in-band `Final` usage → post-loop `stream.usage()` fallback → always a terminal `FinalMetadata`.

## Worked Example — the Stream Adapter

This is `start_stream_with_model` (`src/orchestration/runtime_factory.rs:337-409`) with a reasoning-delta arm added to show how a new chunk kind is threaded through. Lines marked `// EXTENSION` do not exist today; adding them also requires a `RuntimeStreamItem::ReasoningDelta` variant, an arm in `execute_rig_stream`, a `RuntimeEventType` variant, and a `map_runtime_event` decision — see "Adding a Streamed Signal" below.

```rust
async fn start_stream_with_model<M>(
    model: &M,
    request: CompletionRequest,
) -> Result<RuntimeItemStream, ExecutionFailure>
where
    M: RigCompletionModel,
    M::StreamingResponse:
        Clone + Unpin + rig_core::completion::GetTokenUsage + Serialize + Send + 'static,
{
    // The handshake is awaited eagerly; a failure here is pre-output and stays
    // retryable and fallback-eligible.
    let mut stream = model
        .stream(request)
        .await
        .map_err(classify_completion_error)?;

    Ok(Box::pin(async_stream::stream! {
        let mut reported_usage = false;

        while let Some(item) = stream.next().await {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    // In-band failure terminates the stream. Rig will not yield
                    // FinalResponse after an error, so there is no usage to salvage.
                    yield Err(classify_completion_error(error));
                    return;
                }
            };

            match item {
                StreamedAssistantContent::Text(delta) => {
                    yield Ok(RuntimeStreamItem::TextDelta { text: delta.text });
                }
                StreamedAssistantContent::ToolCall {
                    tool_call,
                    internal_call_id,
                } => {
                    yield Ok(RuntimeStreamItem::ToolCallStarted {
                        internal_call_id,
                        name: tool_call.function.name,
                        arguments: tool_call.function.arguments,
                    });
                }
                StreamedAssistantContent::ToolCallDelta {
                    id,
                    internal_call_id,
                    content,
                } => {
                    yield Ok(RuntimeStreamItem::ToolCallDelta {
                        id,
                        internal_call_id,
                        content: serde_json::to_value(content).unwrap_or(Value::Null),
                    });
                }
                // EXTENSION: surface partial reasoning text without the
                // provider-encrypted or redacted blocks carried by Reasoning.
                StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                    yield Ok(RuntimeStreamItem::ReasoningDelta { id, text: reasoning });
                }
                StreamedAssistantContent::Final(response) => {
                    let usage = usage_from_rig(response.token_usage());
                    reported_usage = usage.has_any();
                    yield Ok(RuntimeStreamItem::UsageUpdated { usage });
                }
                // Exhaustive by design: no `_` arm, so an upstream variant
                // addition fails the build instead of being silently dropped.
                StreamedAssistantContent::Reasoning(_)
                | StreamedAssistantContent::Unknown(_) => {}
            }
        }

        // Clean drain only. Providers that never send a usage chunk fall back to
        // the aggregate, which is still the zero sentinel when nothing was reported.
        if !reported_usage {
            let usage = usage_from_rig(stream.usage());
            if usage.has_any() {
                yield Ok(RuntimeStreamItem::UsageUpdated { usage });
            }
        }

        // message_id is only populated after the stream drains.
        yield Ok(RuntimeStreamItem::FinalMetadata {
            provider_request_id: stream.message_id.clone(),
        });
    }))
}
```

Required scope for the snippet, matching the real imports at the top of `runtime_factory.rs`: `std::pin::Pin`, `futures_util::{Stream, StreamExt}`, `rig_core::completion::{CompletionError, CompletionModel as RigCompletionModel, CompletionRequest, GetTokenUsage, Usage}`, `rig_core::streaming::StreamedAssistantContent`, `serde::Serialize`, `serde_json::Value`.

## Mid-Stream Errors, Cancellation, Timeouts

**Error classification.** `classify_completion_error` (`runtime_factory.rs:530`) is the single `CompletionError -> ExecutionFailure` conversion point. `CompletionError` is `#[non_exhaustive]`; never match it directly. It prefers `provider_response_status()` and only falls back to lowercased substring matching, which is brittle — `"response"` routes many errors to `ProviderInvalidResponse`, which trips the circuit breaker but is neither retryable nor fallback-eligible. Never widen the public message: `safe_provider_error_message` emits class plus status only, and `public_provider_failure_retains_keyed_i18n_error_contract` (`tests/execution_lifecycle.rs`) pins the literal string as public API.

**Committed-output rule.** Once any `OutputTextDelta`, `ToolCallStarted`, or `ToolCallDelta` event has been delivered, later failures must be forced terminal:

```rust
if committed {
    failure.retryable = false;
    failure.fallback_eligible = false;
}
```

This applies to in-band item errors, idle-timeout expiry, and the attempt deadline (`attempt_timeout_failure(bounded_by_total_deadline, output_committed)`, `execution.rs:2114`). Retrying or failing over after bytes have reached the client would duplicate output. The classes that skip retry and fallback but still trip the breaker are enumerated by `is_retryable` / `is_fallback_eligible` / `is_circuit_failure` in `src/orchestration/controls.rs` — `ProviderInvalidResponse` is in the third list only.

**Cancellation.** Rig masks abort as EOF: `StreamingCompletionResponse::cancel()` aborts, and the resulting `ProviderError` whose message contains `"aborted"` is converted to `Poll::Ready(None)` (`src/streaming.rs:459-465`). That is a **string match**, so a genuine provider error containing the word "aborted" is silently swallowed as a clean end-of-stream. Consequences:

- Never rely on Rig's terminal state to detect cancellation. Moira tracks it out of band with `tokio_util::sync::CancellationToken`, raced in `tokio::select!` against both `start_stream` and every `stream.next()`, producing `ExecutionFailureClass::RequestCancelled`.
- Do not call `StreamingCompletionResponse::cancel()`. Drop the stream and let Moira's token be the authority.
- `ExecutionStreamHandle` cancels on `Drop`, and consumer disconnect is detected via `public_tx.closed()`, which cancels, drains, and persists a cancellation.

**Pause.** `pause()` / `resume()` busy-wait — a paused stream calls `cx.waker().wake_by_ref()` and returns `Poll::Pending` on every poll (`src/streaming.rs:437-440`). Never use `PauseControl` for backpressure.

**Backpressure.** The provider stream is a lazy `async_stream::stream!` polling a `GenericEventSource` (`src/providers/internal/openai_chat_completions_compatible.rs:229`) with no channel and no unbounded buffer, so consumer backpressure propagates to the HTTP body for free. Just poll slower. Moira's bounded `mpsc` is the only queue; a full or late consumer yields `ExecutionFailureClass::StreamBackpressureExceeded`.

**Timeouts.** Wrap each `stream.next()` in `tokio::time::timeout(idle_timeout, ...)`, never the whole stream — `idle_timeout` is `Duration::from_millis(candidate.runtime_policy.stream_idle_timeout_ms.max(1) as u64)` (`execution.rs:522`) and is a per-chunk liveness bound. The same value is the `push_stream` send deadline. `ProviderRuntimePolicyRecord` is **not** applied to Rig's HTTP client — `RigRuntimeFactory::build_completion_model` takes it as `_policy` and ignores it (`runtime_factory.rs:90`) — so these `tokio` deadlines are the only timeout enforcement in the streaming path.

## Ordering and Flush Guarantees

1. Rig preserves arrival order and a tool call splits text blocks; `["first", ToolCall, "second"]` stays three items (`src/streaming.rs:983-1001`).
2. `FinalResponse`/usage is always last on a clean drain; `FinalMetadata` is Moira's own terminal item and produces **no** event — it only sets `provider_request_id`.
3. Emit before you accumulate. `execute_rig_stream` awaits `push_stream(...)?` and only then does `text.push_str(&delta)` and `mark_output_committed()`. This keeps the persisted transcript a subset of what was actually delivered.
4. Two independent sequence spaces: `EventCollector.next_sequence` for `RuntimeEventEnvelope`, and a separate monotonic counter for `PublicSseEnvelope`. Never assume they align.
5. `EventCollector::push` (lifecycle events) forwards through `forward_now`, which is non-blocking `try_send` and converts `Full` into `StreamBackpressureExceeded` and `Closed` into a cancellation; `push_stream` (provider output) awaits `tx.send` under `tokio::time::timeout(send_timeout, ..)` raced against the cancellation token. Do not swap them.
6. HTTP-level flush is Axum's `Sse` plus `KeepAlive::new().interval(heartbeat_seconds).text("heartbeat")`, with `cache-control: no-cache, no-store` and `x-accel-buffering: no` from `sse_headers`. Events carry `.event(event_type)`, `.id(sequence)`, `.data(json)`.
7. Once the SSE body has started, the response is **HTTP 200 forever** and every failure rides as a `response.failed` envelope — `failure_http_status` is used only on the non-streaming `create_response` path. Pre-stream rejections in `PublicExecutionService::stream_response` still return ordinary HTTP errors: `Idempotency-Key` present → 422 `idempotency_not_supported_for_stream`, `policy.streaming_enabled == false` → 403 `streaming_not_supported`, plus authz and rate-limit errors. Never state that the streaming route cannot fail with a non-200.
8. Tool-call event types are filtered out of the public SSE by `map_runtime_event` (they return `None`), as are `ExecutionStarted/Completed/Failed`. Adding a runtime event does not make it public.

## Adding a Streamed Signal

1. Add the `StreamedAssistantContent` arm in `start_stream_with_model` and the `RuntimeStreamItem` variant.
2. Add the arm in `execute_rig_stream`; decide whether it counts as committed output (`mark_output_committed()`) — anything a client can observe as content does.
3. Add the `RuntimeEventType` variant and its `push_stream` payload; keep payload keys snake_case and free of provider-raw blobs.
4. Add the `map_runtime_event` arm — returning `None` keeps it internal, a `("response.*", payload)` pair makes it public. `map_runtime_event` has no `_` arm; that is the forcing function.
5. If it becomes public, document it with `.agents/skills/moira-openapi/SKILL.md`. The streamed contract lives on the dedicated route `POST /api/v1/responses/stream` (`src/http/public.rs:81`, handler `stream_response`) — **not** on `POST /api/v1/responses` with a `stream` flag. The OpenAI-compat route `POST /v1/responses` (`src/http/public.rs:359`, handler `openai_responses_compat`) is the one that branches on request `stream: true`, and it reuses the same `sse_event` mapping.
6. Update `docs/rig-integration.md` and, for anything client-visible, `docs/streaming-api.md`.

## Testing a Stream Without a Network

Three levels, cheapest first. Moira has **no** `[dev-dependencies]` section; tests use the main dependency set (`tokio`, `futures-util`, `async-stream`, `axum`, `serde_json`).

**Level 1 — item-stream unit test, no Rig types.** Build a `RuntimeItemStream` from `futures_util::stream::iter` and assert order plus in-band failure. This is the pattern of `semantic_stream_preserves_item_order_and_in_band_failures` (`runtime_factory.rs:729`):

```rust
let mut items: RuntimeItemStream = Box::pin(stream::iter(vec![
    Ok(RuntimeStreamItem::TextDelta { text: "first".to_string() }),
    Ok(RuntimeStreamItem::UsageUpdated {
        usage: UsageSummary { output_tokens: Some(1), ..UsageSummary::default() },
    }),
    Err(ExecutionFailure::new(
        ExecutionFailureClass::ProviderInvalidResponse,
        "provider stream item failed",
    )),
]));
```

**Level 2 — adapter unit test against a stub `CompletionModel`.** `rig_core::streaming` is public, so a stub can be built with no new dependencies. `StreamingCompletionResponse::stream(inner: StreamingResult<R>)` is public and `StreamingResult<R> = Pin<Box<dyn Stream<Item = Result<RawStreamingChoice<R>, CompletionError>> + Send>>` (`src/streaming.rs:232-233`). `Usage` is `Copy` and `Default`. The stub below has been compiled against rig-core 0.40.0 inside this repo with `cargo check --tests`; the only imports it needs beyond the adapter's are `rig_core::streaming::{RawStreamingChoice, StreamingCompletionResponse, StreamingResult}`, `rig_core::completion::CompletionResponse`, and `serde::Deserialize`.

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StubStreamingResponse {
    usage: Usage,
}

impl GetTokenUsage for StubStreamingResponse {
    fn token_usage(&self) -> Usage {
        self.usage
    }
}

#[derive(Clone)]
struct StubStreamModel {
    chunks: Vec<RawStreamingChoice<StubStreamingResponse>>,
}

impl RigCompletionModel for StubStreamModel {
    type Response = StubStreamingResponse;
    type StreamingResponse = StubStreamingResponse;
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        Self { chunks: Vec::new() }
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "stub model does not support blocking completion".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let chunks = self.chunks.clone();
        let inner: StreamingResult<Self::StreamingResponse> =
            Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok)));
        Ok(StreamingCompletionResponse::stream(inner))
    }
}
```

The `async fn` impl style is exactly what rig-core's own `MockCompletionModel` uses against these RPITIT signatures (`src/test_utils/completion.rs:259-300`), including the explicit `StreamingResult<_>` annotation before `StreamingCompletionResponse::stream` — keep the annotation, it drives the unsize coercion to `Pin<Box<dyn Stream + Send>>`. The trait declares `+ WasmCompatSend` on both returned futures, so keep non-`Send` values out of the bodies.

Script chunks with `RawStreamingChoice::{Message, ToolCall, ToolCallDelta, FinalResponse, MessageId}`. Note the shapes differ: `Message(String)` and `FinalResponse(R)` and `MessageId(String)` are tuple variants, `ToolCall(RawStreamingToolCall)` is a **tuple** variant wrapping a struct with public fields (`src/streaming.rs:145-160`) and a `RawStreamingToolCall::empty()` constructor (`:164`), while `ToolCallDelta { id, internal_call_id, content }` is a struct variant. Then assert the resulting `RuntimeStreamItem` sequence, the zero-usage sentinel, and that an `Err` chunk terminates the stream.

Optional variant: `rig-core` exposes `MockCompletionModel`, `MockStreamEvent`, and `MockResponse` behind `#[cfg(any(test, feature = "test-utils"))]` (`rig-core-0.40.0/src/lib.rs:171-173`). Using them means adding `rig-core = { version = "0.40", features = ["test-utils"] }` under a new `[dev-dependencies]` section — a dependency change that needs explicit approval, so prefer the local stub.

**Level 3 — integration over the scripted provider.** `tests/support/mock_openai.rs` is an Axum server bound to `127.0.0.1:0` serving hand-written OpenAI SSE frames (`sse_delta`, `sse_usage`, terminal `data: [DONE]`). This is the only level that exercises Rig's real SSE parser, so any change to chunk handling needs coverage here. Behaviour is selected by the `ProviderScript` enum; the streaming arms are `Stream`, `HeldStream`, `StreamErrorAfterDelta`, `StreamErrorAfterToolCall`, `StalledStream`. Sequencing is deterministic via `ScriptGate` (`wait_arrived` / `release` / `wait_completed` / `wait_connection_closed`) bounded by `WAIT_TIMEOUT = Duration::from_secs(5)`, and `ConnectionGuard`'s `Drop` signals abnormal client disconnect.

Behaviours that must stay covered: first delta observed while the provider is still gated (proves no buffering), post-delta failure is non-retryable and non-fallback-eligible, idle-timeout terminalisation, consumer-disconnect cancellation persistence, and sanitised provider errors that leak neither the raw body nor the API key. `LifecycleFixture` requires a live Postgres and silently skips when absent — verify the test actually ran before claiming it passes.

## Pitfalls

| Pitfall | Consequence | Correct move |
|---|---|---|
| Reading `stream.usage()` mid-stream | Zero sentinel misread as "no tokens used" | Drain first; prefer in-band `Final` |
| Treating an errored stream as a zero-usage success | Silent truncation billed as success | Errors terminate; no `FinalResponse` means no usage |
| Relying on Rig's EOF to detect cancellation | `"aborted"` substring match swallows real errors | Out-of-band `CancellationToken` |
| `Box::pin`ning `StreamingCompletionResponse` | Redundant; it is already `Unpin` | Box only at `RuntimeItemStream` |
| `_ =>` arm on `StreamedAssistantContent` | New upstream variants silently dropped | Exhaustive match, no wildcard |
| Retry or fallback after a delta shipped | Duplicated output to the client | Committed-output rule |
| `timeout` around the whole stream | Long healthy responses killed | Per-`next()` idle timeout |
| `pause()` for backpressure | Executor task spins | Poll slower; the source is pull-based |
| Serialising `Text` wholesale | Unmodelled provider keys leak into the public payload | Forward `.text` only |
| Asserting `input + output == total` | Fails on Anthropic | Treat `total_tokens` as provider-reported |
| Matching on `CompletionError` variants | `#[non_exhaustive]`; breaks on upgrade | Go through `classify_completion_error` |
| New `RuntimeEventType` assumed public | Event never reaches clients | Add the `map_runtime_event` arm deliberately |
| Documenting streaming under `POST /api/v1/responses` | Wrong OpenAPI operation | `POST /api/v1/responses/stream`; only the compat route branches on `stream: true` |
| "Streams always return 200" | Misses 422/403 pre-stream rejections | 200 holds only once the SSE body has started |
| Expecting `cache_creation_input_tokens` or `tool_use_prompt_tokens` downstream | `usage_from_rig` drops them | Widen `UsageSummary` and every hop, or accept the loss |

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Streaming changes must additionally prove: item order is preserved, an in-band failure terminates the stream, usage is captured from a clean drain and is the zero sentinel otherwise, a post-delta failure is neither retryable nor fallback-eligible, the idle timeout fires per chunk, consumer disconnect cancels and persists, and no provider body, credential, or reasoning payload appears in any event, error message, or log.

## Siblings

- `.agents/skills/moira-rig-integration/SKILL.md` — boundary, `RuntimeFactory`, verification and upgrade procedure.
- `.agents/skills/moira-rig-providers/SKILL.md` — per-provider client construction and credential gating.
- `.agents/skills/moira-rig-completions/SKILL.md` — the non-streaming path and `CompletionRequest` construction.
- `.agents/skills/moira-rig-tools/SKILL.md` — tool definitions and tool-call semantics behind the delta events.
- `.agents/skills/moira-rig-agents-rag/SKILL.md` — `MultiTurnStreamItem` and the agent-level streaming surface.
- `.agents/skills/moira-rig-errors-testing/SKILL.md` — `CompletionError` taxonomy and the scripted-provider harness.
- `skills/moira-project-structure/SKILL.md` — module placement.
- `.agents/skills/moira-openapi/SKILL.md` — documenting the `text/event-stream` contract.
