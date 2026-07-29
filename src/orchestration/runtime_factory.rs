use std::pin::Pin;

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use rig_core::{
    OneOrMany,
    client::CompletionClient,
    completion::{
        AssistantContent, CompletionError, CompletionModel as RigCompletionModel,
        CompletionRequest, CompletionResponse, GetTokenUsage, Message, Usage, message::UserContent,
    },
    providers::{anthropic, azure, deepseek, gemini, openai},
    streaming::StreamedAssistantContent,
};
use secrecy::ExposeSecret;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    domain::{
        CredentialType, DomainMessage, DomainMessageContent, DomainMessageRole, ExecutionFailure,
        ExecutionFailureClass, ProviderRuntimePolicyRecord, ProviderType, ResolvedCredential,
        ResolvedProviderConfiguration, RuntimeEventEnvelope, RuntimeEventType, UsageSummary,
    },
    error::AppError,
    orchestration::normalize_openai_base_url,
};

#[derive(Debug, Clone)]
pub struct RigRuntimeFactory;

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
}

impl std::fmt::Debug for RuntimeModelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAi(_) => write!(f, "RuntimeModelHandle::OpenAi(<redacted>)"),
            Self::Anthropic(_) => write!(f, "RuntimeModelHandle::Anthropic(<redacted>)"),
            Self::Gemini(_) => write!(f, "RuntimeModelHandle::Gemini(<redacted>)"),
            Self::DeepSeek(_) => write!(f, "RuntimeModelHandle::DeepSeek(<redacted>)"),
            Self::AzureOpenAi(_) => write!(f, "RuntimeModelHandle::AzureOpenAi(<redacted>)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeCompletionOutput {
    pub text: String,
    pub usage: UsageSummary,
    pub provider_request_id: Option<String>,
}

impl RigRuntimeFactory {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RigRuntimeFactory {
    fn default() -> Self {
        Self::new()
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
        }
    }

    pub async fn stream(
        &self,
        request: CompletionRequest,
        base_event: RuntimeEventSeed,
    ) -> Result<RuntimeStreamOutput, ExecutionFailure> {
        let mut stream = self.start_stream(request).await?;
        let mut seed = base_event;
        let mut text = String::new();
        let mut usage = UsageSummary::default();
        let mut provider_request_id = None;
        let mut events = Vec::new();

        while let Some(item) = stream.next().await {
            match item? {
                RuntimeStreamItem::TextDelta { text: delta } => {
                    text.push_str(&delta);
                    events.push(next_event(
                        &mut seed,
                        RuntimeEventType::OutputTextDelta,
                        json!({ "text": delta }),
                    ));
                }
                RuntimeStreamItem::ToolCallStarted {
                    internal_call_id,
                    name,
                    ..
                } => events.push(next_event(
                    &mut seed,
                    RuntimeEventType::ToolCallStarted,
                    json!({
                        "internal_call_id": internal_call_id,
                        "name": name
                    }),
                )),
                RuntimeStreamItem::ToolCallDelta {
                    id,
                    internal_call_id,
                    ..
                } => events.push(next_event(
                    &mut seed,
                    RuntimeEventType::ToolCallDelta,
                    json!({ "id": id, "internal_call_id": internal_call_id }),
                )),
                RuntimeStreamItem::UsageUpdated {
                    usage: updated_usage,
                } => {
                    usage = updated_usage;
                    events.push(next_event(
                        &mut seed,
                        RuntimeEventType::UsageUpdated,
                        json!({ "usage": usage }),
                    ));
                }
                RuntimeStreamItem::FinalMetadata {
                    provider_request_id: request_id,
                } => provider_request_id = request_id,
            }
        }

        Ok(RuntimeStreamOutput {
            text,
            usage,
            provider_request_id,
            events,
        })
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

#[derive(Debug, Clone)]
pub struct RuntimeEventSeed {
    pub request_id: String,
    pub execution_id: uuid::Uuid,
    pub next_sequence: u64,
}

#[derive(Debug, Clone)]
pub struct RuntimeStreamOutput {
    pub text: String,
    pub usage: UsageSummary,
    pub provider_request_id: Option<String>,
    pub events: Vec<RuntimeEventEnvelope>,
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
    RuntimeCompletionOutput {
        text: text_from_choice(response.choice),
        usage: usage_from_rig(response.usage),
        provider_request_id: response.message_id,
    }
}

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

fn next_event(
    seed: &mut RuntimeEventSeed,
    event_type: RuntimeEventType,
    payload: Value,
) -> RuntimeEventEnvelope {
    let event = RuntimeEventEnvelope {
        request_id: seed.request_id.clone(),
        execution_id: seed.execution_id,
        sequence: seed.next_sequence,
        timestamp: chrono::Utc::now(),
        event_type,
        payload,
    };
    seed.next_sequence += 1;
    event
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
}
