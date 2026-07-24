---
name: moira-rig-errors-testing
description: Map rig-core 0.40 failures onto Moira's ExecutionFailure, ExecutionFailureClass and AppError, and test the Rig boundary. Covers every CompletionError variant (plus EmbeddingError, ToolError, ToolSetError, PromptError, StructuredOutputError), the status-first classification rule and its substring fallback, retry / fallback / circuit-breaker derivation, the committed-output override, the sanitisation contract that keeps provider bodies and secrets out of public messages, what may and may not be logged at the Rig boundary, and the four test levels the repo uses — pure classification unit tests, network-free client construction with rig-core test-utils, the scripted OpenAI-compatible Axum server, and the Postgres lifecycle fixture. Use when changing classify_completion_error, adding or remapping an ExecutionFailureClass, altering failure_http_status or failure_code, touching safe_provider_error_message or safe_config_error, deciding whether a failure is retryable or fallback-eligible, adding tracing around provider calls, or writing, fixing, or reviewing any test that exercises rig_core.
---

# Moira Rig Errors and Testing

## Core Rule

Rig owns the failure taxonomy; Moira owns the classification, the retry/fallback decision, and every byte that reaches a caller or a log. Exactly one function converts a Rig error into a Moira failure — `classify_completion_error` in `src/orchestration/runtime_factory.rs`. Adding a second conversion site is a review-blocking defect.

Provider response bodies, provider error text, credentials, decrypted material, and agent-profile preambles never leave the orchestration layer. What leaves is a class, an HTTP status number, and a fixed-format sentence.

Read `.agents/skills/moira-rig-integration/SKILL.md` first for the ownership boundary and the vendored-source verification rule.

## Rig's Error Surface (rig-core 0.40.0)

Paths are relative to `/Users/nalhide/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.40.0/`.

| Type | Source | `#[non_exhaustive]` | Reaches Moira today |
|---|---|---|---|
| `completion::CompletionError` | `src/completion/request.rs:87-123` | yes | yes — the only one |
| `embeddings::embedding::EmbeddingError` | `src/embeddings/embedding.rs:20-56` | yes | no |
| `tool::ToolError` | `src/tool/mod.rs:46-57` | no | no |
| `tool::ToolSetError` | `src/tool/mod.rs:534-551` | no | no |
| `completion::PromptError` | `src/completion/request.rs:147-189` | no | no |
| `completion::StructuredOutputError` | `src/completion/request.rs:248-261` | no | no |
| `client::ProviderClientError` | `src/client/mod.rs:66-86` | yes | no (Moira never uses `from_env`) |
| `client::ClientBuilderError` | `src/client/mod.rs:46-59` | yes | yes — via `build()?` in the factory |
| `http_client::Error` | `src/http_client/mod.rs:14-36` | no | indirectly, inside `CompletionError::HttpError` |

`CompletionError` variants (`src/completion/request.rs:87-123`):

- `HttpError(http_client::Error)` — transport *and* every non-2xx provider response.
- `JsonError(serde_json::Error)` — request encode or response decode failure.
- `UrlError(url::ParseError)`.
- `RequestError(Box<dyn Error + Send + Sync>)` — request construction; `MessageError::ConversionError` also funnels here (`src/completion/message.rs:1368-1372`).
- `ResponseError(String)` — Rig-authored parse diagnostic.
- `ProviderError(String)` (`src/completion/request.rs:117-118`) — Rig-authored diagnostic; deliberately carries **no** recoverable provider body or status. Frozen by the vendored test `completion_error_provider_error_is_not_a_provider_response` (`src/completion/request.rs:1477-1491`).
- `ProviderResponse(ProviderResponseError)` — the provider's verbatim body, with `status: Option<StatusCode>` (`src/provider_response.rs:11-16`).

Because it is `#[non_exhaustive]`, every `match` in Moira needs a catch-all arm, and classification must key on the inspection helpers rather than on variant names.

### The inspection helpers are the contract

`impl_provider_response_helpers!(CompletionError)` (`src/completion/request.rs:125`, macro at `src/provider_response.rs:65-162`) generates:

- `provider_response_status() -> Option<http::StatusCode>` — reads `ProviderResponse.status`, else delegates to `http_client::Error::non_success_status()`, which answers only for `InvalidStatusCode` and `InvalidStatusCodeWithMessage` (`src/http_client/mod.rs:40-47`); `None` for everything else (`src/provider_response.rs:153-159`).
- `provider_response_body() -> Option<&str>` — the raw provider payload. **Diagnostic only. Never surface it.**
- `provider_response_json() -> Result<Option<Value>, serde_json::Error>` — `Ok(None)` for an absent or empty body.
- `from_http_response(status, body)` / `from_provider_body(body)` — public constructors, which is what makes classification unit-testable with no network.

Three behaviours that drive Moira's mapping:

1. `HttpClientExt for reqwest::Client` (`src/http_client/mod.rs:155`) converts any non-2xx into `Error::InvalidStatusCodeWithMessage(status, body)` **before** the provider module sees the response (`:172-174`, via `non_success_status_error` at `:69-76`). So provider 4xx/5xx normally arrive as `CompletionError::HttpError`, and `provider_response_status()` recovers the code. This is the happy path for classification.
2. `from_http_response` routes a **2xx** carrying a provider-authored error envelope into `ProviderResponse { status: Some(2xx), body }` (`src/provider_response.rs:82-94`, exercised by the vendored test at `src/providers/openai/completion/mod.rs:3039-3079`). `provider_response_status()` then returns `Some(202)`, not an error status.
3. Every *transport* failure — connect refused, DNS failure, socket timeout, body read error — is wrapped as `Error::Instance(Box<reqwest::Error>)` by `instance_error` (`:59-62`). This is the branch that breaks the substring fallback; see the reqwest-`Display` note under the mapping table.

## The Single Classification Point

`src/orchestration/runtime_factory.rs:464-495`, as built:

```rust
pub fn classify_completion_error(error: CompletionError) -> ExecutionFailure {
    let status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    let class = match status {
        Some(401 | 403) => ExecutionFailureClass::ProviderAuthenticationFailed,
        Some(408) => ExecutionFailureClass::ProviderTimeout,
        Some(429) => ExecutionFailureClass::ProviderRateLimited,
        Some(500..=599) => ExecutionFailureClass::ProviderUnavailable,
        Some(_) => ExecutionFailureClass::ProviderUpstreamError,
        None => {
            let text = error.to_string().to_ascii_lowercase();
            if text.contains("timeout") || text.contains("timed out") {
                ExecutionFailureClass::ProviderTimeout
            } else if text.contains("connect") || text.contains("dns") {
                ExecutionFailureClass::ProviderConnectionFailed
            } else if text.contains("json") || text.contains("parse") || text.contains("response") {
                ExecutionFailureClass::ProviderInvalidResponse
            } else {
                ExecutionFailureClass::ProviderUpstreamError
            }
        }
    };
    ExecutionFailure::new(class, safe_provider_error_message(class, status))
}

fn safe_provider_error_message(class: ExecutionFailureClass, status: Option<u16>) -> String {
    match status {
        Some(status) => format!("provider request failed with HTTP {status} ({class:?})"),
        None => format!("provider request failed ({class:?})"),
    }
}
```

Rules for changing it:

1. **Status first, always.** Widen the `Some(..)` arms before touching the substring branch. Substring matching on `Display` is brittle by acknowledgement — Rig can change any message string in a patch release.
2. Never add a variant-name `match` on `CompletionError`. It is `#[non_exhaustive]`; classification must survive new upstream variants.
3. `safe_provider_error_message` is the only message producer. It consumes a class and a `u16` and nothing else. Do not thread `error` into it.
4. The message format is a **public API contract** (see below). Changing the string is a breaking change.
5. Call sites: `completion_with_model` (`runtime_factory.rs:330-334`), `start_stream_with_model` at stream open (`:346-349`), and per-item mid-stream (`:356-359`). A mid-stream `Err` terminates the stream — `yield Err(..); return;`.

## Mapping Table

`ExecutionFailure` is **not** an `AppError`. It rides in the `failure: Option<ExecutionFailure>` field of `ExecutionOutcome` (`domain/runtime.rs:413-424`), which the execution service returns as `Ok(..)`; `failed_outcome` builds it (`application/execution.rs:1640-1659`). The `AppError` column is the error the non-stream endpoint `POST /api/v1/responses` finally emits via `AppError::coded(failure_http_status(class), failure_code(class), failure.message)` (`application/public.rs:255-259`, `src/error.rs:78-84`). Streaming never produces an `AppError` from a provider failure — the HTTP status is already 200 and the failure arrives as a `response.failed` SSE envelope (`http/public.rs:110-138`).

| Rig failure (how it arises) | `provider_response_status()` | `AppError` finally emitted | `ExecutionFailureClass` | retry | fallback | circuit |
|---|---|---|---|---|---|---|
| `HttpError(InvalidStatusCodeWithMessage(401\|403, _))` — bad or revoked key | `Some(401\|403)` | `Api{502, provider_authentication_failed}` | `ProviderAuthenticationFailed` | no | no | no |
| `HttpError(…(408, _))` — upstream request timeout | `Some(408)` | `Api{504, provider_timeout}` | `ProviderTimeout` | **yes** | **yes** | yes |
| `HttpError(…(429, _))` — rate limit | `Some(429)` | `Api{502, provider_rate_limited}` | `ProviderRateLimited` | **yes** | **yes** | yes |
| `HttpError(…(500..=599, _))` — upstream outage | `Some(5xx)` | `Api{502, provider_unavailable}` | `ProviderUnavailable` | **yes** | **yes** | yes |
| `HttpError(…(other non-2xx, _))` — 400/404/413/422 | `Some(n)` | `Api{502, provider_upstream_error}` | `ProviderUpstreamError` | **yes** | **yes** | yes |
| `HttpError(InvalidStatusCode(n))` — status with no body | `Some(n)` | as per `n` above | as per `n` above | — | — | — |
| `ProviderResponse{status: Some(2xx), body}` — provider error envelope on a success status | `Some(2xx)` | `Api{502, provider_upstream_error}` | `ProviderUpstreamError` | **yes** | **yes** | yes |
| `ProviderResponse{status: None, body}` — non-HTTP transport error payload | `None` | `Api{502, provider_invalid_response}` | `ProviderInvalidResponse` † | no | no | **yes** |
| `HttpError(Instance(reqwest))` — connect refused / DNS failure / socket timeout | `None` | `Api{502, provider_upstream_error}` | `ProviderUpstreamError` ‡ | **yes** | **yes** | yes |
| `HttpError(Instance(reqwest))` — response body read/decode failure | `None` | `Api{502, provider_invalid_response}` | `ProviderInvalidResponse` ‡ | no | no | **yes** |
| `HttpError(StreamEnded \| NoHeaders \| InvalidContentType \| Protocol \| InvalidHeaderValue)` | `None` | `Api{502, provider_upstream_error}` | `ProviderUpstreamError` | **yes** | **yes** | yes |
| `JsonError(serde_json::Error)` — malformed provider JSON | `None` | `Api{502, provider_invalid_response}` | `ProviderInvalidResponse` | no | no | **yes** |
| `ResponseError(String)` — Rig parse diagnostic | `None` | `Api{502, provider_invalid_response}` | `ProviderInvalidResponse` † | no | no | **yes** |
| `ProviderError(String)` — Rig diagnostic, incl. mid-stream transport (`from_stream_transport`) | `None` (by design) | depends on the string | substring branch | varies | varies | varies |
| `UrlError(url::ParseError)` — malformed base URL | `None` | `Api{502, provider_upstream_error}` | `ProviderUpstreamError` ⚠ | **yes** | **yes** | yes |
| `RequestError(Box<dyn Error>)` — request build / `MessageError::ConversionError` | `None` | `Api{502, provider_upstream_error}` | `ProviderUpstreamError` ⚠ | **yes** | **yes** | yes |
| `ClientBuilderError` from `build()?` — wrapped by `safe_config_error` | n/a | `Api{502, provider_configuration_invalid}` | `ProviderConfigurationInvalid` | no | no | no |
| Moira `tokio::time::timeout` around the call (`execution.rs:496-503`) | n/a | `Api{504, provider_timeout}` or `{504, deadline_exceeded}` | `ProviderTimeout` / `DeadlineExceeded` | see override | see override | — |
| Stream idle timeout (`stream_idle_timeout_ms`) | n/a | SSE `response.failed` | `ProviderTimeout` | see override | see override | — |
| `EventCollector::push_stream` send timeout / dead consumer | n/a | SSE `response.failed` | `StreamBackpressureExceeded` | no | no | no |

† `ProviderResponse`'s and `ResponseError`'s `Display` prefixes (`"ProviderResponseError: …"`, `"ResponseError: …"`) themselves contain the substring `response`, so they land on `ProviderInvalidResponse` unless the body happens to contain `timeout`/`connect`/`dns`, which wins earlier in the chain. Body-dependent classification — do not rely on it for new behaviour.

‡ **`ProviderConnectionFailed` is currently unreachable, and so is the substring path to `ProviderTimeout`.** `reqwest` 0.13.4's `Display` prints only its own kind string and the URL — it never walks the source chain (`reqwest-0.13.4/src/error.rs:236-281`). Every connect refusal, DNS failure, and socket timeout is `Kind::Request`, so the whole chain renders as:

```text
HttpError: Http client error: error sending request for url (http://host:8000/v1/chat/completions)
```

That string contains none of `timeout`, `timed out`, `connect`, `dns`, `json`, `parse`, `response`, so it falls to the `else` branch — `ProviderUpstreamError`. Only `Kind::Decode` (`"error decoding response body"`) matches, on `response`, giving `ProviderInvalidResponse`. Consequences you must internalise before touching the substring branch:

- Do not write a test asserting `ProviderConnectionFailed` from a real socket failure. It will fail. The class exists and is wired through `is_retryable` / `is_fallback_eligible` / `is_circuit_failure`, but `classify_completion_error` never produces it.
- The blast radius is small but real: `ProviderConnectionFailed` and `ProviderUpstreamError` share identical booleans *and* status (both fall to `failure_http_status`'s `_` arm → 502), so only the public `code` string differs. A reqwest-level socket timeout, however, becomes `502 provider_upstream_error` instead of `504 provider_timeout`.
- In practice Moira's own `tokio::time::timeout` (`execution.rs:502`) fires first and yields a correct `ProviderTimeout`, because Rig configures no `reqwest` timeout at all — see the `ProviderRuntimePolicyRecord` pitfall.
- The correct fix, if you need these classes, is **not** more substrings. Match on the error structurally before falling back: `matches!(error, CompletionError::HttpError(_))` plus `std::error::Error::source()` walking, or gate on `reqwest::Error::is_timeout()` / `is_connect()` at a transport layer Moira owns. Any substring you add is hostage to a `reqwest` patch release.

⚠ These are Moira-side or configuration bugs classified as retryable upstream errors. Do not "fix" them by adding more substrings; fix them by failing earlier — `normalize_openai_base_url` already rejects a bad OpenAI-family base URL at build time with `AppError::BadRequest` (`orchestration/resolver.rs:283-292`), which `runtime_handle` then remaps to `ProviderConfigurationInvalid` (`execution.rs:885-890`), so a bad base URL never reaches `classify_completion_error` at all.

Note `ProviderAuthenticationFailed` falls through `failure_http_status`'s `_` arm to **502**, not 401/403 (`application/public.rs:1927-1945`). That is intentional: an upstream credential problem is not the caller's authentication problem.

## Retry, Fallback, and Circuit Derivation

Three independent predicates in `src/orchestration/controls.rs`, all keyed on the class alone:

```rust
impl ExecutionFailure {
    pub fn new(class: ExecutionFailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retryable: is_retryable(class),
            fallback_eligible: is_fallback_eligible(class),
        }
    }
}
```

- `is_retryable` (`controls.rs:628-639`): `ProviderTimeout`, `ProviderConnectionFailed`, `ProviderRateLimited`, `ProviderUnavailable`, `ProviderUpstreamError`, `CircuitOpen`, `CapacityExhausted`.
- `is_fallback_eligible` (`controls.rs:641-653`): the same set **plus** `CredentialNotFound`.
- `is_circuit_failure` (private, `controls.rs:655-665`): `ProviderTimeout`, `ProviderConnectionFailed`, `ProviderUnavailable`, **`ProviderInvalidResponse`**, `ProviderUpstreamError`, `ProviderRateLimited`.

`ProviderInvalidResponse` is the asymmetric one: it trips the breaker but is neither retryable nor fallback-eligible. A provider emitting garbage should stop receiving traffic, not receive more of it. Preserve that asymmetry.

**Committed-output override.** Once any delta or tool-call has reached the consumer, a later failure must not be retried or failed over — the caller has already seen partial output. Two enforcement sites:

- The stream drain sets `committed = true` on the first emitted item (`application/execution.rs:1520-1521`, `:1539-1540`, `:1558-1559`) and then forces `retryable = false; fallback_eligible = false` on both exit paths — the idle-timeout branch (`:1487-1490`) and the mid-stream `Err` branch (`:1502-1506`). It also flips `EventCollector::mark_output_committed` (`:1430-1435`).
- `attempt_timeout_failure(bounded_by_total_deadline, output_committed)` (`execution.rs:1876-1893`), covered by `timeout_after_stream_output_cannot_retry_or_fallback` (`execution.rs:1949-1960`).

Any new failure path that can fire mid-stream must respect it. The override mutates the booleans *after* `ExecutionFailure::new`; do not encode it in `is_retryable`.

## Sanitisation Contract

Three layers, all load-bearing, all covered by tests.

1. **Provider error text never propagates.** `safe_provider_error_message` emits only class + status. `safe_config_error(provider, err)` (`runtime_factory.rs:460-462`) emits `"build Rig {provider} client failed: {err}"` — the `{err}` is Rig's `ClientBuilderError`, which contains no credential. Note this string *does* reach the caller: `runtime_handle` maps **any** factory `AppError` to `ProviderConfigurationInvalid` with `err.to_string()` (`execution.rs:885-890`), and `AppError::Config`'s `Display` is `"configuration error: {0}"` (`src/error.rs:40-41`). That covers `AppError::BadRequest` from `normalize_openai_base_url` too. Anything you put in a factory error message is public.
2. **Handles and credentials have hand-written `Debug`.** `RuntimeModelHandle` prints `RuntimeModelHandle::OpenAi(<redacted>)` (`runtime_factory.rs:52-62`); `ResolvedCredential` redacts `secret` and passes `config` through `redact_credential_config` (`domain/runtime.rs:401-411`, `:613-631`). Rig's own `Client` `Debug` filters `Authorization` and `*api-key*` headers (`src/client/mod.rs:188-219`) but still prints the base URL — that is not sufficient, keep Moira's impls. Never `#[derive(Debug)]` on a type holding a handle or a secret.
3. **`expose_secret()` is called exactly once**, at `runtime_factory.rs:92`, immediately before the provider builder.

Never write `error.provider_response_body()` or `provider_response_json()` into a message, an event payload, a persisted row, or a log line. They exist for interactive debugging only.

Locked by tests:

- `tests/execution_lifecycle.rs:680` — `!malformed_failure.message.contains("{not valid JSON")`.
- `tests/execution_lifecycle.rs:727` — `!failure.message.contains("sk-lifecycle-secret")`.
- `tests/execution_lifecycle.rs:902-904` — the raw HTTP body contains none of the provider payload, the secret, or private diagnostics.
- `tests/execution_lifecycle.rs:901-918` (in `public_provider_failure_retains_keyed_i18n_error_contract`, `:876-921`) — the full public envelope: `code == "provider_invalid_response"`, `message_key == "moira.error.provider_invalid_response"`, `message == "provider request failed (ProviderInvalidResponse)"`, `message_args == {}`, `details == null`, and the body contains neither the raw provider payload nor the secret.

That last assertion means **the sanitised sentence is the literal public message**, and the same `(status, body)` pair is stored for idempotent replay (`application/public.rs:243-254`). Failure classification is part of the idempotency contract, not just the response.

`failure_code` (`application/public.rs:1947-1978`) is an exhaustive 28-arm match with **no** `_` arm. Adding an `ExecutionFailureClass` variant breaks the build there on purpose. Do not add a wildcard.

## Observability

As built, `src/orchestration/**` and `src/application/execution.rs` contain **zero** `tracing::` calls — the only `tracing` users in the crate are `src/infra/db.rs`, `src/infra/workers.rs`, and `src/main.rs`. Observability at the Rig boundary is carried by structured artefacts, not logs:

- `RuntimeEventEnvelope` (`domain/runtime.rs:507-515`) — `request_id`, `execution_id`, monotonic `sequence`, `timestamp`, `event_type`, `payload`.
- Persisted `ProviderAttemptSummary` — `attempt_id`, `attempt_number`, `provider_id`, `provider_model_id`, `credential_id`, `status`, `failure_class`, `latency_ms`, `usage`.
- Prometheus at `/metrics` (`http/observability.rs`), gated on `telemetry.prometheus_enabled`.

If you add `tracing` at this boundary, the allowed field set is exactly the identifier/classification surface above plus `provider_type`, `model_key`, and the upstream HTTP status number. Forbidden as fields or in messages: request or response content, `preamble` / agent-profile instructions, `CompletionRequest.additional_params`, provider response bodies, credential ids' plaintext material, base URLs containing embedded tokens, and anything derived from `provider_response_body()`.

**Rig emits its own spans, and they are a leak vector.** `GenericCompletionModel::completion` opens `info_span!(target: "rig::completions", "chat", …, gen_ai.system_instructions = &completion_request.preamble, …)` (`src/providers/openai/completion/mod.rs:1928-1944`; the Anthropic equivalent is `src/providers/anthropic/completion.rs:2462-2467`), and at `TRACE` it logs the entire pretty-printed request body (`:1966-1972`). Moira **does** populate `CompletionRequest.preamble` from the agent profile (`application/execution.rs:1619`), so that field is an internal protected instruction.

The default filter `"moira=info,tower_http=info"` (`config/settings.rs:674`) is target-scoped and therefore safe — `rig::completions` is not enabled, so the `fmt` layer never records the span at all. The rule follows:

- Never broaden `MOIRA_TELEMETRY__ENV_FILTER` to a bare level such as `debug` or `trace` in any environment that handles real prompts. `config/telemetry.rs:6-8` reads that setting first and only falls back to `EnvFilter::try_from_default_env()` (i.e. `RUST_LOG`) when the configured string **fails to parse** — so a malformed `env_filter` silently hands filtering to the ambient `RUST_LOG`. Treat an unparseable filter as a security bug, not a cosmetic one.
- If Rig diagnostics are needed, add an explicit target directive at a safe level (`rig::completions=warn`), never a global level.
- Do not change `config/telemetry.rs` to attach a subscriber that records span fields without re-auditing this. Both branches use `fmt::layer()` / `fmt::layer().json()` over a shared `EnvFilter` (`telemetry.rs:10-22`); the filter is the only thing standing between the JSON layer and `gen_ai.system_instructions`.

## Testing Strategy

Four levels. Pick the cheapest that actually proves the property.

**L1 — pure classification unit tests.** No client, no network, no feature flags. Construct a `CompletionError` with the public helpers `CompletionError::from_http_response(status, body)` and `CompletionError::from_provider_body(body)`, feed it to `classify_completion_error`, assert class, the three booleans, and that the message excludes the body. This is where the mapping table belongs. Every row you add to the table gets a case here.

**L2 — network-free model and client construction.** Rig ships `rig_core::test_utils` behind the `test-utils` feature (`src/lib.rs:171-173`, `Cargo.toml:75`, a dependency-free feature). Moira does **not** enable it today; enabling it is a deliberate opt-in in `[dev-dependencies]`. Two distinct seams live there — pick by what you are proving.

*L2a — scripted `CompletionModel`, no HTTP at all.* `MockCompletionModel` (`src/test_utils/completion.rs:171`) really does implement `CompletionModel` (`:259-300`, `type Client = ()`), so a Rig completion model is trivially fakeable. Surface:

- `MockCompletionModel::{new, from_turns}(impl IntoIterator<Item = MockTurn>)`, `::text(impl Into<String>)`, `::from_stream_turns(..)` (`:175-213`).
- `::requests() -> Vec<CompletionRequest>` and `::request_count() -> usize` (`:216-223`) — assert what Moira actually sent.
- `MockTurn::{text, tool_call, error, request_error, from_content, from_contents}` with `.with_usage(Usage)`, `.with_message_id(..)`, `.with_call_id(..)` (`:61-145`).
- `MockTurn::error(msg)` yields `CompletionError::ProviderError(msg)`; `MockTurn::request_error(msg)` yields `CompletionError::RequestError(..)` (`:40-45`). Those are the two variants you can inject through this seam — anything status-bearing needs L2b or L1.

This is the cheapest way to prove that `completion_with_model` / `start_stream_with_model` route an error into `classify_completion_error` at all, and that `usage_from_rig` sees the `Usage` you scripted.

*L2b — real provider encoder over a scripted transport.* `src/test_utils/http.rs` exposes `RecordingHttpClient` (unary; `::new(body)`, `::with_error(status, msg)`, `::with_error_response(status, body)`, `::requests() -> Vec<CapturedHttpRequest { uri, headers, body }>`, `Clone` sharing one `Arc` state), `MockStreamingClient { sse_bytes }`, `SequencedStreamingHttpClient::new(Vec<http_client::Result<Bytes>>)`, and `HttpErrorStreamingClient::new(status, body)`. Use this when the property is about Rig's request encoding or response decoding, not about Moira's classification.

Critical typing fact: `openai::completion::CompletionModel<H = reqwest::Client>` is a generic alias (`src/providers/openai/completion/mod.rs:1533`), and `RuntimeModelHandle::OpenAi` stores the `reqwest::Client` instantiation (`runtime_factory.rs:45`). A `RecordingHttpClient`-backed model — and `MockCompletionModel`, which is not a provider type at all — is a **different type** and cannot be placed in `RuntimeModelHandle`. That is fine, because the mapping helpers `completion_with_model` (`runtime_factory.rs:323-335`) and `start_stream_with_model` (`runtime_factory.rs:337-409`) are generic over `M: RigCompletionModel` — an in-file `#[cfg(test)] mod tests` can call them directly. Never widen `RuntimeModelHandle` with a test-only variant.

If you assert on Rig's own spans, hold `rig_core::test_utils::scoped_tracing_subscriber_guard()` for the subscriber's whole lifetime (`src/test_utils/tracing_isolation.rs:18-26`) — `tracing` caches per-callsite interest globally and parallel tests will otherwise poison it.

**L3 — the scripted OpenAI-compatible server.** `tests/support/mock_openai.rs` is an Axum server bound to `127.0.0.1:0` (`:144`) serving `POST /v1/chat/completions` (`:142`), with `base_url()` returning `http://{addr}/v1` (`:164`). Scripts are a `VecDeque<ProviderScript>` popped per request; every request is recorded as `RecordedRequest { authorization: Option<String>, body: Value }` (`:35-38`). Variants (`:86-117`): `Completion`, `HeldCompletion`, `HttpError { status, body }`, `MalformedResponse` (a 200 with `Content-Type: application/json` and the body `{not valid JSON`, `:235-239`), `Stream`, `HeldStream`, `StreamErrorAfterDelta`, `StreamErrorAfterToolCall`, `StalledStream`. An exhausted queue falls back to a 500 (`:220`). Deterministic sequencing uses `ScriptGate` (`wait_arrived` / `release` / `wait_completed` / `wait_connection_closed`, `:60-83`, 5s `WAIT_TIMEOUT` at `:32`); `ConnectionGuard` signals abnormal client disconnects. `call_count()` (`:181`) is the assertion that a failure did *not* reach the provider.

This level is the only thing that proves Rig's encoder and SSE parser still match Moira's expectations over real HTTP. It is where you assert the exact `Authorization` header, the request body shape, and delta timing. Because the queue is popped per request, it is also the only place you can prove retry and fallback *counts*: script two entries and assert both were consumed, or script one and assert `call_count() == 1` after a non-retryable class.

**L4 — the Postgres lifecycle fixture.** `tests/support/mod.rs::LifecycleFixture::new() -> Option<Self>` (`:125`) requires `MOIRA_TEST_DATABASE_URL`; it returns `None` and the test returns early when unset, but **panics when unset under `CI`** (`tests/support/mod.rs:430-437`). Tests serialize on a shared `TEST_SERIAL` mutex (`:40`, `:126`). `add_provider` drives the real admin API end to end (provider → model `test-model` → credential `sk-lifecycle-secret` → runtime policy → routing policy). Use L4 only for properties that need persistence, credential encryption, circuit state, or capacity accounting.

**Do not build a Moira-side mock of `CompletionModel`.** `CompletionModel` is not object-safe, so there is no `dyn` seam, and Moira deliberately has no mock of its own — but do not conclude that no mock exists: Rig ships `MockCompletionModel` (L2a) and you should use it rather than inventing a parallel trait. Anything that must prove Rig's *wire* behaviour still belongs at L2b or L3, driven through real Rig encoder/decoder code with a scripted transport.

**There are currently zero classification unit tests.** `runtime_factory.rs`'s test module (`:528-584`) holds three tests — credential-type error text, the usage zero-sentinel, and stream item ordering — and none of them call `classify_completion_error`. The mapping table above is enforced only indirectly, at L3/L4. The first change you make to classification should also close that gap.

### Test Recipes

L1 — the mapping table, in `runtime_factory.rs`'s own `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn upstream_status_drives_the_class_and_hides_the_provider_body() {
        let error = CompletionError::from_http_response(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"slow down","code":"rate_limit_exceeded"}}"#,
        );
        let failure = classify_completion_error(error);

        assert_eq!(failure.class, ExecutionFailureClass::ProviderRateLimited);
        assert!(failure.retryable);
        assert!(failure.fallback_eligible);
        assert_eq!(
            failure.message,
            "provider request failed with HTTP 429 (ProviderRateLimited)"
        );
        assert!(!failure.message.contains("rate_limit_exceeded"));
    }

    #[test]
    fn provider_error_envelope_on_a_success_status_is_an_upstream_error() {
        let error = CompletionError::from_http_response(
            StatusCode::ACCEPTED,
            r#"{"type":"rate_limit","code":"rate_limit_exceeded"}"#,
        );
        assert_eq!(error.provider_response_status(), Some(StatusCode::ACCEPTED));

        let failure = classify_completion_error(error);
        assert_eq!(failure.class, ExecutionFailureClass::ProviderUpstreamError);
    }

    #[test]
    fn malformed_json_is_not_retryable_but_still_trips_the_breaker_class() {
        // Display is "JsonError: …", which matches the `json` substring.
        let error = CompletionError::JsonError(
            serde_json::from_str::<serde_json::Value>("{not valid JSON").unwrap_err(),
        );
        let failure = classify_completion_error(error);

        assert_eq!(failure.class, ExecutionFailureClass::ProviderInvalidResponse);
        assert!(!failure.retryable);
        assert!(!failure.fallback_eligible);
        assert_eq!(
            failure.message,
            "provider request failed (ProviderInvalidResponse)"
        );
    }

    #[test]
    fn rig_diagnostics_without_a_status_fall_through_to_upstream_error() {
        // Pins the reqwest-Display trap: this is the shape a connect refusal,
        // a DNS failure, and a socket timeout all arrive in.
        let error = CompletionError::ProviderError(
            "Http client error: error sending request for url (http://host:8000/v1/chat/completions)"
                .to_string(),
        );
        let failure = classify_completion_error(error);

        assert_eq!(failure.class, ExecutionFailureClass::ProviderUpstreamError);
        assert!(failure.retryable);
    }
}
```

`axum::http::StatusCode` is the same type as Rig's `http::StatusCode` — `Cargo.lock` resolves a single `http` 1.4.2. `ExecutionFailure` does not derive `PartialEq` (`domain/runtime.rs:435`); assert on `failure.class` (which is `Copy + PartialEq + Eq + Hash`, `:443`) and the individual booleans, never on the whole struct.

Both L2 recipes need the feature turned on first — a deliberate opt-in the repo does not carry today:

```toml
# Cargo.toml
[dev-dependencies]
rig-core = { version = "0.40", features = ["test-utils"] }
```

L2a — drive Moira's generic helper with `MockCompletionModel`, no HTTP stack at all. This is the seam for "did the helper route the error through classification and map usage":

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::{Message, Usage};
    use rig_core::test_utils::{MockCompletionModel, MockTurn};

    fn request_fixture() -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: None,
            chat_history: OneOrMany::one(Message::user("ping")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        }
    }

    #[tokio::test]
    async fn helper_maps_usage_and_records_the_request_it_sent() {
        let mut usage = Usage::new();
        usage.input_tokens = 2;
        usage.total_tokens = 3;
        let model = MockCompletionModel::from_turns([MockTurn::text("hi").with_usage(usage)]);

        let output = completion_with_model(&model, request_fixture())
            .await
            .expect("completion output");

        assert_eq!(output.text, "hi");
        assert_eq!(output.usage.input_tokens, Some(2));
        assert_eq!(output.usage.total_tokens, Some(3));
        assert_eq!(output.usage.output_tokens, None); // zero sentinel -> None
        assert_eq!(model.request_count(), 1);
    }

    #[tokio::test]
    async fn helper_routes_provider_errors_through_classification() {
        let model = MockCompletionModel::from_turns([MockTurn::error("upstream exploded")]);

        let failure = completion_with_model(&model, request_fixture())
            .await
            .expect_err("expected classified failure");

        // MockTurn::error yields CompletionError::ProviderError -> no status -> substring branch.
        assert_eq!(failure.class, ExecutionFailureClass::ProviderUpstreamError);
        assert!(!failure.message.contains("upstream exploded"));
    }
}
```

L2b — a real Rig provider model over a scripted transport, exercising Rig's own encoder and decoder with no socket:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::client::CompletionClient;
    use rig_core::providers::openai::CompletionsClient;
    use rig_core::test_utils::RecordingHttpClient;
    // `request_fixture()` as defined in the L2a block above.

    #[tokio::test]
    async fn completion_output_maps_choice_and_usage_without_a_network() {
        let http = RecordingHttpClient::new(
            r#"{"id":"chatcmpl-1","object":"chat.completion","created":0,"model":"test-model",
                "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},
                            "finish_reason":"stop"}],
                "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
        );
        let client = CompletionsClient::builder()
            .api_key("test-key")
            .http_client(http.clone())
            .build()
            .expect("build completions client");
        let model = client.completion_model("test-model");

        let output = completion_with_model(&model, request_fixture())
            .await
            .expect("completion output");

        assert_eq!(output.text, "hi");
        assert_eq!(output.usage.total_tokens, Some(3));
        assert_eq!(output.usage.input_tokens, Some(2));
        // Chat Completions never populates message_id; only the Responses API does.
        assert_eq!(output.provider_request_id, None);

        let captured = http.requests();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].uri.ends_with("/chat/completions"));
    }
}
```

`CompletionsClient::builder().api_key(..).http_client(..).build()` is the exact chain the vendored crate uses in its own tests (`src/providers/openai/completion/mod.rs:3048-3053`); the `CompletionModel` impl requires `H: Clone + Default + Debug + WasmCompatSend + WasmCompatSync + 'static` and `Client<Ext, H>: HttpClientExt` (`:1887-1899`), all of which `RecordingHttpClient` satisfies (`#[derive(Clone, Debug, Default)]`, `src/test_utils/http.rs:60`). `CompletionsClient` is the Chat Completions client, aliased at `src/providers/openai/client.rs:54` — `openai::Client` is the Responses API and yields a different model type. Moira's factory reaches the same place via `openai::Client::builder()…build()?.completions_api()` (`runtime_factory.rs:99-109`).

To assert a *status-bearing* failure through this seam, use `RecordingHttpClient::with_error_response(status, body)` — that is how the vendored tests produce both the non-2xx `HttpError` path and the 2xx `ProviderResponse` envelope path (`src/providers/openai/completion/mod.rs:3046`, `:3090`).

L3 — assert base-URL normalisation and the credential over real HTTP. `normalize_openai_base_url` has direct unit coverage (`orchestration/resolver.rs:350-360`: `http://192.168.1.13:8000` → `.../v1`, and `http://localhost:8000/v1/` → unchanged, no doubled `/v1`). The end-to-end proof is that the mock server, which only routes `POST /v1/chat/completions`, is reached at all from a `base_url()` of `http://{addr}/v1` — `completion_uses_real_rig_protocol_and_encrypted_credential` (`tests/execution_lifecycle.rs:21`) asserts `Authorization == "Bearer sk-lifecycle-secret"`, `body["model"] == "test-model"`, and `body["stream"] != true`.

For a sanitisation-plus-breaker property, follow `malformed_response_is_sanitized_and_opens_the_provider_circuit` (`tests/execution_lifecycle.rs:652-694`): script one `MalformedResponse` with `circuit_failure_threshold: 1`, assert `ProviderInvalidResponse` and that the message excludes the raw body, then assert the *next* execution is `CircuitOpen` and `provider.call_count().await == 1`. Note how that class is reached: the mock returns **200** with `Content-Type: application/json` and the body `{not valid JSON` (`tests/support/mock_openai.rs:235-239`), so `reqwest` passes it through, Rig's decoder fails, and `CompletionError::JsonError` takes the `json` substring branch.

Every new class or remap needs, at minimum: an L1 case for the class and its booleans, and — if it is reachable over HTTP — an L3 case proving the sanitised message and the breaker/retry consequence. If you cannot construct the `CompletionError` at L1, the class is probably unreachable in production too; prove reachability before shipping it (`ProviderConnectionFailed` is the standing counter-example).

## Clippy and fmt at This Boundary

`cargo clippy --all-targets -- -D warnings` is a hard gate; every lint below is a build failure, not a warning.

- **`#[non_exhaustive]` needs a catch-all.** `CompletionError`, `EmbeddingError`, `ProviderClientError`, `ClientBuilderError`, and `DocumentSourceKind` all require a `_ =>` arm. `clippy::wildcard_enum_match_arm` is allow-by-default, so there is no conflict.
- **`clippy::match_same_arms` fires on duplicated classification bodies.** Merge patterns with `|` (`Some(401 | 403) => …`) instead of writing two arms with the same body.
- **`rustfmt` does not format inside `async_stream::stream! { … }`.** `yield` is not stable expression syntax, so the macro body is skipped and `cargo fmt --check` will happily pass on badly formatted stream code. Hand-format `start_stream_with_model`'s body to match the surrounding style and review it manually.
- **`-D warnings` turns `dead_code` fatal in test support.** Each file under `tests/` is a separate binary that compiles the whole `support` module, so unused helpers break the build. Both `tests/support/mod.rs:1` and `tests/support/mock_openai.rs:1` carry `#![allow(dead_code)]` for this reason. Keep it; do not sprinkle per-item `#[allow]`.
- **`#[async_trait]` vs RPITIT.** Moira's own traits (`RuntimeFactory`, `ExecutionService`) use `#[async_trait]`; Rig's `CompletionModel` is RPITIT and is used generically. Do not `#[async_trait]` anything that must interoperate with Rig's trait, and do not try to make `CompletionModel` object-safe.
- **Keep the stream boxed.** `RuntimeItemStream = Pin<Box<dyn Stream<Item = …> + Send>>`. Returning `impl Stream` from an `#[async_trait]` method does not compile; do not attempt it to avoid an allocation.
- **`clippy::large_enum_variant` on `RuntimeModelHandle`.** Verified not firing today (`cargo clippy --all-targets` is clean on the current tree) because the five provider model structs are similarly sized. If you add a provider whose model type is materially larger, `Box` that variant rather than adding `#[allow]`.
- Async unit tests need `#[tokio::test]`. `.next()` on a stream needs `futures_util::StreamExt` in scope, and `use super::*` in `runtime_factory.rs`'s test module (`:530`) already supplies it from the file's own import at `:4` — the existing `semantic_stream_preserves_item_order_and_in_band_failures` calls `items.next().await` (`:569`) with no explicit `StreamExt` import. Do not add one.
- `unwrap()`/`expect()` are acceptable in tests, never in the classification or factory paths.

## Pitfalls

- **`ProviderConnectionFailed` is dead today.** `reqwest` 0.13.4's `Display` does not walk the source chain, so no real socket failure ever contains `connect` or `dns`. See the ‡ note under the mapping table before writing any test or classification rule that assumes otherwise.
- **A 2xx with an error envelope is not `ProviderInvalidResponse`.** `provider_response_status()` returns the 2xx, so it lands on `ProviderUpstreamError` — retryable and fallback-eligible. If a provider does this routinely, add an explicit `Some(200..=299)` arm rather than letting it fall through `Some(_)`.
- **`ProviderError(String)` deliberately yields no status and no body** — frozen by the vendored test `completion_error_provider_error_is_not_a_provider_response` (`src/completion/request.rs:1477-1491`). It is a Rig diagnostic, not a provider response. It always takes the substring branch.
- **The substring branch is matched against the full `Display`, prefix included.** `CompletionError`'s `#[error(...)]` prefixes (`HttpError:`, `JsonError:`, `ResponseError:`, `ProviderResponseError:`, `UrlError:`, `RequestError:`, `ProviderError:`) are part of the lowercased haystack. That is why `JsonError` matches `json` and `ResponseError`/`ProviderResponse` match `response` regardless of their payload — and why renaming a Rig variant's `#[error]` string in a patch release can silently reclassify Moira's failures.
- **Mid-stream transport failures become `ProviderError`.** `CompletionError::from_stream_transport` (`src/completion/request.rs:133-139`) preserves non-success HTTP as `HttpError` but flattens everything else into `ProviderError(error.to_string())` — and that `to_string()` inherits the reqwest-`Display` truncation described under the mapping table.
- **Stream cancellation looks like EOF, not an error.** `StreamingCompletionResponse`'s `Stream` impl swallows a `ProviderError` whose message contains `"aborted"` and reports normal termination (`src/streaming.rs:459-465`). Do not add a classification rule for it.
- **`Usage` uses `0` as the "not reported" sentinel**, and `usage()` on a stream is zero until the final response arrives (`src/streaming.rs:307-315`). `usage_from_rig` encodes this via `Usage::has_values()` (`src/completion/request.rs:570`) plus the per-field `non_zero` helper (`runtime_factory.rs:430-445`); a failed attempt is always persisted with `UsageSummary::default()` (`execution.rs:597`, `:659`).
- **`CompletionResponse::message_id` is `None`** for OpenAI Chat Completions (`src/providers/openai/completion/mod.rs:1196`), Anthropic, Gemini, DeepSeek, and Azure. `output_from_response` maps it straight to `provider_request_id` (`runtime_factory.rs:411-417`), so do not write a test asserting a non-null `provider_request_id` on those paths.
- **`ProviderRuntimePolicyRecord` is ignored by the factory** (`runtime_factory.rs:90`, `_policy`), and Rig sets **no** `reqwest` timeout of its own. There is therefore no transport-level deadline at all: timeouts are enforced only by Moira's `tokio::time::timeout` wrappers (`execution.rs:502` per attempt, `:1479` stream idle, `:1379` event send). A "connect timeout" test must exercise Moira's wrapper, not a Rig client setting.
- **L4 tests silently pass locally without Postgres.** `LifecycleFixture::new()` returns `None` and the test returns early. If your change is only covered at L4, it is effectively uncovered on a developer machine — add L1/L3 coverage too, and confirm CI has `MOIRA_TEST_DATABASE_URL`.
- **`failure_code` has no wildcard.** Adding an `ExecutionFailureClass` variant breaks `application/public.rs` at compile time. That is the intended forcing function — fill the arm, and add a `failure_http_status` arm if 502 is wrong.
- **The public message string is frozen** by `tests/execution_lifecycle.rs:905-918` and by stored idempotency replays. Reformatting `safe_provider_error_message` is a breaking API change and invalidates persisted replay bodies.
- **`Reasoning`, `ReasoningDelta`, and `Unknown` streamed content are dropped** (`runtime_factory.rs:392-394`) while `reasoning_tokens` is still captured from usage. Do not write a test expecting reasoning deltas to surface. `Unknown` in particular means a new Rig chunk kind is discarded silently rather than failing a test — re-audit that arm on every `rig-core` bump.

## Workflow

1. Read `.agents/skills/moira-rig-integration/SKILL.md`, then this file. Read `.agents/skills/moira-rig-streaming/SKILL.md` too if the failure can occur mid-stream.
2. Reproduce the failure and capture the actual `CompletionError` — its `Debug`, its `provider_response_status()`, and its **exact `Display` string**. Do not guess which variant you have, and never assume a wrapped error's `Display` includes its source; `reqwest`'s does not.
3. Verify the variant and helper behaviour in the vendored crate (`src/completion/request.rs`, `src/provider_response.rs`, `src/http_client/mod.rs`). Record the `path:line`.
4. Decide the class. Prefer a new `Some(status)` arm over a new substring; prefer a structural match (variant + `source()` walk) over any substring at all. If the class does not exist, add it to `ExecutionFailureClass` and fill the resulting compile errors in `failure_code`, `failure_http_status`, `is_retryable`, `is_fallback_eligible`, `is_circuit_failure`.
5. Change only `classify_completion_error`. Do not add a second conversion site, and do not thread the error into `safe_provider_error_message`.
6. Re-check the sanitisation invariants: no provider body, no secret, no preamble, no `provider_response_body()` in any message, event, row, or log.
7. Add L1 cases for every new or changed table row; add an L2a case if the property is about the generic helper rather than the wire; add an L3 case if the failure is reachable over HTTP; add L4 only if persistence, encryption, or circuit state is part of the property. Prove the class is reachable — a class no test can construct is a class production will never emit.
8. If the public code, status, or message changed, follow `.agents/skills/moira-openapi/SKILL.md` and update the error documentation — it is a public contract and an idempotency-replay contract.
9. Update this skill's mapping table in the same change.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

For the lifecycle suite, export `MOIRA_TEST_DATABASE_URL` first — otherwise those tests skip silently outside CI:

```bash
MOIRA_TEST_DATABASE_URL=postgres://... cargo test --test execution_lifecycle
```

The L2 recipes above require `rig-core = { version = "0.40", features = ["test-utils"] }` under `[dev-dependencies]`, which the repo does **not** carry today. Add it in the same change as the tests, or the recipes will not compile.

Tests touching this boundary must prove: every classified status maps to the documented class and booleans; the sanitised message contains neither the provider body nor any secret; `ProviderInvalidResponse` opens the breaker without being retried; a post-delta failure is neither retryable nor fallback-eligible; usage zero-sentinels map to `None`; and the public error code, `message_key`, and message string are unchanged.
