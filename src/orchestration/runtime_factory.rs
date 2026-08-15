use std::pin::Pin;

use async_trait::async_trait;
use axum::http::StatusCode;
use futures_util::{Stream, StreamExt};
use rig_core::{
    OneOrMany,
    client::CompletionClient,
    completion::{
        AssistantContent, CompletionError, CompletionModel as RigCompletionModel,
        CompletionRequest, CompletionResponse, GetTokenUsage, Message, Usage,
        message::{ToolCall, UserContent},
    },
    providers::{anthropic, azure, chatgpt, deepseek, gemini, openai},
    streaming::StreamedAssistantContent,
};
use secrecy::ExposeSecret;
use serde::Serialize;
use serde_json::Value;

use crate::{
    domain::{
        CredentialType, DomainMessage, DomainMessageContent, DomainMessageRole, ExecutionFailure,
        ExecutionFailureClass, ProviderRuntimePolicyRecord, ProviderType, ResolvedCredential,
        ResolvedProviderConfiguration, UsageSummary,
    },
    error::AppError,
    orchestration::normalize_openai_base_url,
};

/// Error code returned when a `chatgpt_oauth` provider is configured or executed without this
/// deployment explicitly accepting the ToS risk. See
/// [`require_chatgpt_subscription_opt_in`].
pub const CHATGPT_SUBSCRIPTION_OPT_IN_REQUIRED: &str = "chatgpt_subscription_opt_in_required";

/// Refuses `ProviderType::ChatgptOauth` — at admin-write time
/// (`application::admin::providers::ProviderAdminService::create_provider`) and at execution
/// time (this file's `build_completion_model` arm alike) — unless this deployment has
/// explicitly opted in via `provider_security.allow_chatgpt_subscription`. A no-op for every
/// other provider type.
///
/// **This is a deliberate ToS risk-acceptance gate, not a capability check.** Issue #216 /
/// `docs/chatgpt-subscription-spike.md` establish that rig-core 0.40 ships a first-party
/// `chatgpt` provider (`rig_core::providers::chatgpt`) targeting
/// `chatgpt.com/backend-api/codex` that is technically wireable through the exact
/// `RuntimeFactory` seam every other provider uses — the blocker was never "no rig-core
/// provider", it is that ChatGPT/Codex subscriptions are personal, single-user under OpenAI's
/// terms, with no carve-out for third-party, multi-tenant use analogous to Anthropic's
/// reinstated third-party agent usage. Wiring this provider into a multi-tenant gateway is this
/// deployment operator's own explicit acceptance of that risk for their own subscription — never
/// a silent default, and never softened in the error this refusal returns.
///
/// Called from two places on purpose, not one: the admin-write-time check gives an operator
/// immediate, actionable feedback before a `chatgpt_oauth` provider row can even be created; the
/// execution-time check is defense in depth against a row that was created while the flag was on
/// and is now stale, or against direct database access that bypassed the admin API entirely — so
/// "never a silent attempt" holds regardless of how the row came to exist.
pub fn require_chatgpt_subscription_opt_in(
    provider_type: ProviderType,
    allow_chatgpt_subscription: bool,
) -> Result<(), AppError> {
    if provider_type != ProviderType::ChatgptOauth || allow_chatgpt_subscription {
        return Ok(());
    }
    // The code argument below is deliberately the literal, not the
    // `CHATGPT_SUBSCRIPTION_OPT_IN_REQUIRED` constant:
    // `i18n::catalog::tests::every_coded_error_literal_in_src_has_a_catalog_entry` scans
    // `AppError::coded(...)` call sites for a literal code argument to prove every code Moira
    // can emit has a catalog entry, and only recognises a bare `&'static str` there — an
    // identifier reads as a dynamic site with no enumerable value set and fails that scan.
    // `EMBEDDING_PROVIDER_UNSUPPORTED` in `orchestration/embedding.rs` follows the same split:
    // the constant exists for callers to assert against without duplicating the string: the
    // throw site spells it out.
    Err(AppError::coded(
        StatusCode::FORBIDDEN,
        "chatgpt_subscription_opt_in_required",
        "the chatgpt_oauth provider is disabled for this deployment; set \
         provider_security.allow_chatgpt_subscription=true to enable it. ChatGPT/Codex \
         subscriptions are personal, single-user under OpenAI's terms, and wiring them into a \
         multi-tenant gateway is this deployment's own explicit ToS risk acceptance, not a \
         sanctioned integration path",
    ))
}

#[derive(Debug, Clone)]
pub struct RigRuntimeFactory {
    /// Mirrors `config::ProviderSecuritySettings::allow_chatgpt_subscription`. Set once, at
    /// construction (`MoiraExecutionService::new`), from resolved `Settings` — not re-read per
    /// request, the same way every other static provider-security posture in this file is
    /// captured once rather than threaded through per call.
    allow_chatgpt_subscription: bool,
}

#[async_trait]
pub trait RuntimeFactory: Send + Sync {
    async fn build_completion_model(
        &self,
        provider: &ResolvedProviderConfiguration,
        model_key: &str,
        credential: &ResolvedCredential,
        policy: &ProviderRuntimePolicyRecord,
    ) -> Result<RuntimeModelHandle, AppError>;
}

#[derive(Clone)]
pub enum RuntimeModelHandle {
    OpenAi(openai::completion::CompletionModel),
    Anthropic(anthropic::completion::CompletionModel),
    Gemini(gemini::completion::CompletionModel),
    DeepSeek(deepseek::CompletionModel),
    AzureOpenAi(azure::CompletionModel),
    /// rig-core 0.40's native ChatGPT-subscription provider (issue #216), reached only when
    /// `require_chatgpt_subscription_opt_in` has already let the build through.
    ChatgptOauth(chatgpt::ResponsesCompletionModel),
}

impl std::fmt::Debug for RuntimeModelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAi(_) => write!(f, "RuntimeModelHandle::OpenAi(<redacted>)"),
            Self::Anthropic(_) => write!(f, "RuntimeModelHandle::Anthropic(<redacted>)"),
            Self::Gemini(_) => write!(f, "RuntimeModelHandle::Gemini(<redacted>)"),
            Self::DeepSeek(_) => write!(f, "RuntimeModelHandle::DeepSeek(<redacted>)"),
            Self::AzureOpenAi(_) => write!(f, "RuntimeModelHandle::AzureOpenAi(<redacted>)"),
            Self::ChatgptOauth(_) => write!(f, "RuntimeModelHandle::ChatgptOauth(<redacted>)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeCompletionOutput {
    pub text: String,
    pub usage: UsageSummary,
    pub provider_request_id: Option<String>,
    /// Every `AssistantContent::ToolCall` in the choice, in the order the provider sent
    /// them (issue #84).
    ///
    /// Until the tool loop landed, `text_from_choice` silently discarded these, so a model
    /// that answered with a tool call produced an empty-string success. That was harmless
    /// only because `CompletionRequest.tools` was hardcoded empty and no model could ever
    /// call one; the moment an agent profile's `skill_refs` put tools on the wire, dropping
    /// them would turn every tool-calling turn into a blank answer.
    pub tool_calls: Vec<rig_core::completion::message::ToolCall>,
}

impl RigRuntimeFactory {
    pub fn new(allow_chatgpt_subscription: bool) -> Self {
        Self {
            allow_chatgpt_subscription,
        }
    }
}

impl Default for RigRuntimeFactory {
    /// `allow_chatgpt_subscription: false` — the ToS-risk opt-in stays off unless a caller
    /// passes `true` to [`RigRuntimeFactory::new`] explicitly. No production code path uses
    /// this impl; `MoiraExecutionService::new` always calls `new` with the resolved setting.
    fn default() -> Self {
        Self::new(false)
    }
}

#[async_trait]
impl RuntimeFactory for RigRuntimeFactory {
    async fn build_completion_model(
        &self,
        provider: &ResolvedProviderConfiguration,
        model_key: &str,
        credential: &ResolvedCredential,
        _policy: &ProviderRuntimePolicyRecord,
    ) -> Result<RuntimeModelHandle, AppError> {
        let secret = credential.secret.expose_secret();
        match provider.provider_type {
            ProviderType::OpenAi | ProviderType::OpenAiCompatible | ProviderType::Local => {
                require_credential_type(
                    credential.credential_type,
                    &[CredentialType::ApiKey, CredentialType::BearerToken],
                )?;
                let mut builder = openai::Client::builder().api_key(secret.as_str());
                if let Some(base_url) = provider.base_url.as_deref() {
                    builder = builder.base_url(normalize_openai_base_url(base_url)?);
                }
                let client = builder
                    .build()
                    .map_err(|err| safe_config_error("openai-compatible", err))?
                    .completions_api();
                Ok(RuntimeModelHandle::OpenAi(
                    client.completion_model(model_key),
                ))
            }
            ProviderType::Anthropic => {
                require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
                let mut builder = anthropic::Client::builder().api_key(secret.as_str());
                if let Some(base_url) = provider.base_url.as_deref() {
                    builder = builder.base_url(base_url);
                }
                let client = builder
                    .build()
                    .map_err(|err| safe_config_error("anthropic", err))?;
                Ok(RuntimeModelHandle::Anthropic(
                    client.completion_model(model_key),
                ))
            }
            ProviderType::Gemini => {
                require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
                let mut builder = gemini::Client::builder().api_key(secret.as_str());
                if let Some(base_url) = provider.base_url.as_deref() {
                    builder = builder.base_url(base_url);
                }
                let client = builder
                    .build()
                    .map_err(|err| safe_config_error("gemini", err))?;
                Ok(RuntimeModelHandle::Gemini(
                    client.completion_model(model_key),
                ))
            }
            ProviderType::DeepSeek => {
                require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
                let mut builder = deepseek::Client::builder().api_key(secret.as_str());
                if let Some(base_url) = provider.base_url.as_deref() {
                    builder = builder.base_url(normalize_openai_base_url(base_url)?);
                }
                let client = builder
                    .build()
                    .map_err(|err| safe_config_error("deepseek", err))?;
                Ok(RuntimeModelHandle::DeepSeek(
                    client.completion_model(model_key),
                ))
            }
            ProviderType::AzureOpenAi => {
                require_credential_type(
                    credential.credential_type,
                    &[CredentialType::AzureOpenAi, CredentialType::ApiKey],
                )?;
                let endpoint = credential
                    .config
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .or(provider.base_url.as_deref())
                    .ok_or_else(|| {
                        AppError::Config(
                            "azure_openai provider requires a configured endpoint".to_string(),
                        )
                    })?;
                let api_version = credential
                    .config
                    .get("api_version")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-10-21");
                let client = azure::Client::builder()
                    .api_key(azure::AzureOpenAIAuth::ApiKey(secret.to_string()))
                    .azure_endpoint(endpoint.to_string())
                    .api_version(api_version)
                    .build()
                    .map_err(|err| safe_config_error("azure_openai", err))?;
                Ok(RuntimeModelHandle::AzureOpenAi(
                    client.completion_model(model_key),
                ))
            }
            ProviderType::ChatgptOauth => {
                require_chatgpt_subscription_opt_in(
                    ProviderType::ChatgptOauth,
                    self.allow_chatgpt_subscription,
                )?;
                require_credential_type(credential.credential_type, &[CredentialType::Oauth2])?;
                // `chatgpt::ChatGPTAuth::AccessToken` is a bring-your-own-token construction —
                // the same shape workstream B's Claude subscription credential storage already
                // uses — and is a clean, thin wrapper: `Authenticator::auth_context()` for this
                // variant is a synchronous clone with no file I/O and no device-code flow
                // (`rig-core-0.40.0/src/providers/chatgpt/auth/mod.rs:106-118`). The alternative
                // `ChatGPTAuth::OAuth` variant drives rig-core's own local-file/device-code login
                // (`chatgpt/auth/native.rs`) and is never constructed here: it is the wrong shape
                // for a multi-tenant server process, exactly as
                // `docs/chatgpt-subscription-spike.md` calls out, and Moira runs its own refresh
                // via the generic `oauth-token-refresh` worker instead.
                let account_id = credential
                    .config
                    .get("account_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let client = chatgpt::Client::builder()
                    .api_key(chatgpt::ChatGPTAuth::AccessToken {
                        access_token: secret.to_string(),
                        account_id,
                    })
                    .build()
                    .map_err(|err| safe_config_error("chatgpt", err))?;
                Ok(RuntimeModelHandle::ChatgptOauth(
                    client.completion_model(model_key),
                ))
            }
            ProviderType::Custom => Err(AppError::Config(
                "custom providers are configurable but not executable in Phase 3".to_string(),
            )),
        }
    }
}

impl RuntimeModelHandle {
    pub async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<RuntimeCompletionOutput, ExecutionFailure> {
        match self {
            Self::OpenAi(model) => completion_with_model(model, request).await,
            Self::Anthropic(model) => completion_with_model(model, request).await,
            Self::Gemini(model) => completion_with_model(model, request).await,
            Self::DeepSeek(model) => completion_with_model(model, request).await,
            Self::AzureOpenAi(model) => completion_with_model(model, request).await,
            Self::ChatgptOauth(model) => completion_with_model(model, request).await,
        }
    }

    pub async fn start_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<RuntimeItemStream, ExecutionFailure> {
        match self {
            Self::OpenAi(model) => start_stream_with_model(model, request).await,
            Self::Anthropic(model) => start_stream_with_model(model, request).await,
            Self::Gemini(model) => start_stream_with_model(model, request).await,
            Self::DeepSeek(model) => start_stream_with_model(model, request).await,
            Self::AzureOpenAi(model) => start_stream_with_model(model, request).await,
            Self::ChatgptOauth(model) => start_stream_with_model(model, request).await,
        }
    }
}

pub type RuntimeItemStream =
    Pin<Box<dyn Stream<Item = Result<RuntimeStreamItem, ExecutionFailure>> + Send>>;

#[derive(Debug, Clone)]
pub enum RuntimeStreamItem {
    TextDelta {
        text: String,
    },
    ToolCallStarted {
        internal_call_id: String,
        name: String,
        arguments: Value,
    },
    ToolCallDelta {
        id: String,
        internal_call_id: String,
        content: Value,
    },
    UsageUpdated {
        usage: UsageSummary,
    },
    FinalMetadata {
        provider_request_id: Option<String>,
    },
}

async fn completion_with_model<M>(
    model: &M,
    request: CompletionRequest,
) -> Result<RuntimeCompletionOutput, ExecutionFailure>
where
    M: RigCompletionModel,
{
    let response = model
        .completion(request)
        .await
        .map_err(classify_completion_error)?;
    Ok(output_from_response(response))
}

async fn start_stream_with_model<M>(
    model: &M,
    request: CompletionRequest,
) -> Result<RuntimeItemStream, ExecutionFailure>
where
    M: RigCompletionModel,
    M::StreamingResponse:
        Clone + Unpin + rig_core::completion::GetTokenUsage + Serialize + Send + 'static,
{
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
                StreamedAssistantContent::Final(response) => {
                    let usage = usage_from_rig(response.token_usage());
                    reported_usage = usage.has_any();
                    yield Ok(RuntimeStreamItem::UsageUpdated { usage });
                }
                StreamedAssistantContent::Reasoning(_)
                | StreamedAssistantContent::ReasoningDelta { .. }
                | StreamedAssistantContent::Unknown(_) => {}
            }
        }

        if !reported_usage {
            let usage = usage_from_rig(stream.usage());
            if usage.has_any() {
                yield Ok(RuntimeStreamItem::UsageUpdated { usage });
            }
        }

        yield Ok(RuntimeStreamItem::FinalMetadata {
            provider_request_id: stream.message_id.clone(),
        });
    }))
}

/// `DomainMessage` -> Rig `Message`. This is the only place either direction is converted; keeping
/// it here is what lets `src/domain` stay free of `rig_core` (plan 06, P2-2).
impl TryFrom<&DomainMessage> for Message {
    type Error = ExecutionFailure;

    fn try_from(message: &DomainMessage) -> Result<Self, Self::Error> {
        match message.role {
            DomainMessageRole::System => Ok(Message::system(text_only_content(message)?)),
            DomainMessageRole::Assistant => Ok(Message::assistant(text_only_content(message)?)),
            DomainMessageRole::User => {
                let parts = message
                    .content
                    .iter()
                    .map(|part| match part {
                        DomainMessageContent::Text { text } => UserContent::text(text.clone()),
                        DomainMessageContent::ImageUrl { url } => {
                            UserContent::image_url(url.clone(), None, None)
                        }
                    })
                    .collect::<Vec<_>>();
                Ok(Message::User {
                    content: OneOrMany::many(parts)
                        .map_err(|_| invalid_execution_request("user message is empty"))?,
                })
            }
            DomainMessageRole::Tool => Err(invalid_execution_request(
                "tool messages require an approved tool registry",
            )),
        }
    }
}

/// Converts an `ExecutionCommand`'s messages into the `chat_history` a `CompletionRequest` needs.
pub fn rig_chat_history(
    messages: &[DomainMessage],
) -> Result<OneOrMany<Message>, ExecutionFailure> {
    let converted = messages
        .iter()
        .map(Message::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    OneOrMany::many(converted).map_err(|_| {
        invalid_execution_request("execution command must contain at least one message")
    })
}

/// Roles that Rig models as a plain `String` carry no image content; joining mirrors how the
/// public boundary flattens multi-part text (`text_only_content` in `src/application/public.rs`).
fn text_only_content(message: &DomainMessage) -> Result<String, ExecutionFailure> {
    let mut text = Vec::with_capacity(message.content.len());
    for part in &message.content {
        match part {
            DomainMessageContent::Text { text: value } => text.push(value.as_str()),
            DomainMessageContent::ImageUrl { .. } => {
                return Err(invalid_execution_request(
                    "this message role only supports text content",
                ));
            }
        }
    }
    Ok(text.join("\n"))
}

fn invalid_execution_request(message: &str) -> ExecutionFailure {
    ExecutionFailure::new(ExecutionFailureClass::InvalidExecutionRequest, message)
}

fn output_from_response<T>(response: CompletionResponse<T>) -> RuntimeCompletionOutput {
    let (text, tool_calls) = split_choice(response.choice);
    RuntimeCompletionOutput {
        text,
        usage: usage_from_rig(response.usage),
        provider_request_id: response.message_id,
        tool_calls,
    }
}

/// Splits an assistant turn into its text and its tool calls.
///
/// One pass rather than two filters so the two halves can never disagree about which
/// content items were seen. Reasoning and any other content kind is still dropped here —
/// that is unchanged and deliberate; only the tool calls stopped being discarded (#84).
fn split_choice(choice: OneOrMany<AssistantContent>) -> (String, Vec<ToolCall>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for content in choice {
        match content {
            AssistantContent::Text(part) => text.push_str(&part.text),
            AssistantContent::ToolCall(tool_call) => tool_calls.push(tool_call),
            _ => {}
        }
    }
    (text, tool_calls)
}

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

fn non_zero(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

fn require_credential_type(
    actual: CredentialType,
    allowed: &[CredentialType],
) -> Result<(), AppError> {
    if allowed.contains(&actual) {
        Ok(())
    } else {
        Err(AppError::Config(format!(
            "credential type {actual:?} is not supported by this provider"
        )))
    }
}

fn safe_config_error(provider: &str, err: impl std::fmt::Display) -> AppError {
    AppError::Config(format!("build Rig {provider} client failed: {err}"))
}

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

trait UsageSummaryExt {
    fn has_any(&self) -> bool;
}

impl UsageSummaryExt for UsageSummary {
    fn has_any(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.total_tokens.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CredentialType;
    use futures_util::stream;

    #[test]
    fn runtime_handle_debug_does_not_expose_secret_shape() {
        let err = require_credential_type(CredentialType::BasicAuth, &[CredentialType::ApiKey])
            .unwrap_err();
        assert!(!err.to_string().contains("password"));
    }

    #[test]
    fn domain_message_round_trips_through_rig_message() {
        let system = Message::try_from(&DomainMessage::system("be terse")).expect("system message");
        assert_eq!(
            system,
            Message::System {
                content: "be terse".to_string()
            }
        );

        let assistant =
            Message::try_from(&DomainMessage::assistant("sure")).expect("assistant message");
        assert_eq!(assistant, Message::assistant("sure"));

        let user = DomainMessage::new(
            DomainMessageRole::User,
            vec![
                DomainMessageContent::Text {
                    text: "what is this".to_string(),
                },
                DomainMessageContent::ImageUrl {
                    url: "https://example.test/a.png".to_string(),
                },
            ],
        );
        let Message::User { content } = Message::try_from(&user).expect("user message") else {
            panic!("user role must convert to Message::User");
        };
        let parts = content.into_iter().collect::<Vec<_>>();
        assert_eq!(
            parts,
            vec![
                UserContent::text("what is this"),
                UserContent::image_url("https://example.test/a.png", None, None),
            ]
        );
    }

    #[test]
    fn multi_part_text_is_joined_for_roles_rig_models_as_a_string() {
        let message = DomainMessage::new(
            DomainMessageRole::System,
            vec![
                DomainMessageContent::Text {
                    text: "first".to_string(),
                },
                DomainMessageContent::Text {
                    text: "second".to_string(),
                },
            ],
        );
        assert_eq!(
            Message::try_from(&message).expect("system message"),
            Message::System {
                content: "first\nsecond".to_string()
            }
        );
    }

    #[test]
    fn tool_and_image_shapes_rig_cannot_carry_fail_as_invalid_execution_requests() {
        let tool = DomainMessage::new(
            DomainMessageRole::Tool,
            vec![DomainMessageContent::Text {
                text: "result".to_string(),
            }],
        );
        let failure = Message::try_from(&tool).expect_err("tool messages are not supported");
        assert_eq!(
            failure.class,
            ExecutionFailureClass::InvalidExecutionRequest
        );

        let system_image = DomainMessage::new(
            DomainMessageRole::System,
            vec![DomainMessageContent::ImageUrl {
                url: "https://example.test/a.png".to_string(),
            }],
        );
        let failure = Message::try_from(&system_image).expect_err("system messages are text only");
        assert_eq!(
            failure.class,
            ExecutionFailureClass::InvalidExecutionRequest
        );

        let empty_user = DomainMessage::new(DomainMessageRole::User, Vec::new());
        let failure = Message::try_from(&empty_user).expect_err("empty user message");
        assert_eq!(
            failure.class,
            ExecutionFailureClass::InvalidExecutionRequest
        );
    }

    #[test]
    fn an_empty_message_list_is_an_invalid_execution_request() {
        let failure = rig_chat_history(&[]).expect_err("empty chat history");
        assert_eq!(
            failure.class,
            ExecutionFailureClass::InvalidExecutionRequest
        );
        assert_eq!(
            failure.message,
            "execution command must contain at least one message"
        );
        assert_eq!(
            rig_chat_history(&[DomainMessage::user("hello")])
                .expect("chat history")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![Message::user("hello")]
        );
    }

    #[test]
    fn usage_zero_sentinel_maps_to_missing_values() {
        assert_eq!(usage_from_rig(Usage::new()).total_tokens, None);
        let mut usage = Usage::new();
        usage.total_tokens = 12;
        assert_eq!(usage_from_rig(usage).total_tokens, Some(12));
    }

    #[tokio::test]
    async fn semantic_stream_preserves_item_order_and_in_band_failures() {
        let failure = ExecutionFailure::new(
            ExecutionFailureClass::ProviderInvalidResponse,
            "provider stream item failed",
        );
        let mut items: RuntimeItemStream = Box::pin(stream::iter(vec![
            Ok(RuntimeStreamItem::TextDelta {
                text: "first".to_string(),
            }),
            Ok(RuntimeStreamItem::UsageUpdated {
                usage: UsageSummary {
                    output_tokens: Some(1),
                    ..UsageSummary::default()
                },
            }),
            Err(failure),
        ]));

        assert!(matches!(
            items.next().await,
            Some(Ok(RuntimeStreamItem::TextDelta { text })) if text == "first"
        ));
        assert!(matches!(
            items.next().await,
            Some(Ok(RuntimeStreamItem::UsageUpdated { usage }))
                if usage.output_tokens == Some(1)
        ));
        assert!(matches!(
            items.next().await,
            Some(Err(error))
                if error.class == ExecutionFailureClass::ProviderInvalidResponse
        ));
        assert!(items.next().await.is_none());
    }

    // -----------------------------------------------------------------------------------
    // Issue #216 — `chatgpt_oauth` provider: opt-in gate + network-free client construction.
    // -----------------------------------------------------------------------------------

    #[test]
    fn chatgpt_subscription_opt_in_gate_refuses_when_off_and_allows_when_on() {
        let refusal = require_chatgpt_subscription_opt_in(ProviderType::ChatgptOauth, false)
            .expect_err("a chatgpt_oauth provider must be refused until the deployment opts in");
        let message = refusal.to_string();
        assert!(
            message.contains(CHATGPT_SUBSCRIPTION_OPT_IN_REQUIRED),
            "refusal must carry the keyed error code, got: {message}"
        );
        assert!(
            message.contains("provider_security.allow_chatgpt_subscription"),
            "refusal must name the flag an operator needs to set, got: {message}"
        );

        require_chatgpt_subscription_opt_in(ProviderType::ChatgptOauth, true)
            .expect("a chatgpt_oauth provider must be allowed once the deployment opts in");
    }

    #[test]
    fn chatgpt_subscription_opt_in_gate_is_a_no_op_for_every_other_provider_type() {
        for provider_type in [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatible,
            ProviderType::Anthropic,
            ProviderType::Gemini,
            ProviderType::DeepSeek,
            ProviderType::AzureOpenAi,
            ProviderType::Local,
            ProviderType::Custom,
        ] {
            require_chatgpt_subscription_opt_in(provider_type, false).unwrap_or_else(|err| {
                panic!("{provider_type:?} must never be gated by the chatgpt opt-in flag: {err}")
            });
        }
    }

    /// L2b (`.agents/skills/moira-rig-errors-testing/SKILL.md`): a real
    /// `chatgpt::ResponsesCompletionModel` built with `ChatGPTAuth::AccessToken` over
    /// rig-core's `RecordingHttpClient` — no socket, no real ChatGPT session, no real token.
    /// Proves two things the spike (`docs/chatgpt-subscription-spike.md`) and the factory arm
    /// both claim: the `AccessToken` construction path is real and reachable through the exact
    /// `Client::builder().api_key(..).build()?.completion_model(..)` chain
    /// `build_completion_model`'s new arm uses, and Moira's own generic `completion_with_model`
    /// helper drives it end to end (choice text + usage mapping) exactly as it does for every
    /// other provider.
    #[tokio::test]
    async fn chatgpt_client_construction_and_completion_output_work_without_a_network() {
        use rig_core::test_utils::RecordingHttpClient;

        // The exact SSE fixture shape rig-core's own vendored test uses
        // (`rig-core-0.40.0/src/providers/chatgpt/mod.rs`,
        // `test_parse_chatgpt_sse_completion`) — `completion()` always reads the ChatGPT
        // backend's response as SSE text, streamed or not.
        let sse_body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\
             data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\
             \"created_at\":1,\"status\":\"completed\",\"error\":null,\"incomplete_details\":null,\
             \"instructions\":null,\"max_output_tokens\":null,\"model\":\"gpt-5.3-codex\",\
             \"usage\":{\"input_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0},\
             \"output_tokens\":1,\"output_tokens_details\":{\"reasoning_tokens\":0},\"total_tokens\":3},\
             \"output\":[{\"type\":\"message\",\"id\":\"msg_1\",\"status\":\"completed\",\"role\":\"assistant\",\
             \"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"hi\"}]}],\"tools\":[]}}\n\
             data: [DONE]";
        let http = RecordingHttpClient::new(sse_body);

        // Mirrors `build_completion_model`'s `ChatgptOauth` arm exactly, with a fake token that
        // is never sent anywhere real: `RecordingHttpClient` never opens a socket.
        let client = chatgpt::Client::builder()
            .api_key(chatgpt::ChatGPTAuth::AccessToken {
                access_token: "test-access-token".to_string(),
                account_id: Some("test-account-id".to_string()),
            })
            .http_client(http.clone())
            .build()
            .expect("chatgpt client must build from a bare AccessToken with no I/O");
        let model = client.completion_model(chatgpt::GPT_5_3_CODEX);

        let request = CompletionRequest {
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
        };

        let output = completion_with_model(&model, request)
            .await
            .expect("completion output");

        assert_eq!(output.text, "hi");
        assert_eq!(output.usage.input_tokens, Some(2));
        assert_eq!(output.usage.output_tokens, Some(1));
        assert_eq!(output.usage.total_tokens, Some(3));

        let captured = http.requests();
        assert_eq!(captured.len(), 1, "exactly one request must have been sent");
        assert_eq!(
            captured[0].headers.get(axum::http::header::AUTHORIZATION),
            Some(&axum::http::HeaderValue::from_static(
                "Bearer test-access-token"
            )),
            "the AccessToken credential must reach the wire as a bearer header, proving the \
             construction path is real rather than a stub"
        );
        assert_eq!(
            captured[0]
                .headers
                .get("ChatGPT-Account-Id")
                .and_then(|value| value.to_str().ok()),
            Some("test-account-id"),
            "credential.config's account_id must reach the wire as ChatGPT-Account-Id"
        );
    }
}
