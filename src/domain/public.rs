use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::domain::{
    ExecutionFailureClass, ProviderType, PublicCitation, PublicConversationRef,
    ResponseConversationInput, UsageSummary,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicResponseStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponsePersistenceMode {
    None,
    MetadataOnly,
    EncryptedContent,
    PlainContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplicationExecutionPolicyRecord {
    pub id: Uuid,
    pub application_id: Uuid,
    pub responses_enabled: bool,
    pub streaming_enabled: bool,
    pub tools_enabled: bool,
    pub vision_enabled: bool,
    pub structured_output_enabled: bool,
    pub caller_system_instructions_allowed: bool,
    pub model_overrides_allowed: bool,
    pub route_overrides_allowed: bool,
    pub provider_overrides_allowed: bool,
    pub credential_overrides_allowed: bool,
    pub timeout_overrides_allowed: bool,
    pub persistence_mode: ResponsePersistenceMode,
    pub response_retention_seconds: i64,
    pub maximum_request_bytes: i64,
    pub maximum_input_items: i32,
    pub maximum_output_tokens: i64,
    pub maximum_timeout_ms: i64,
    pub rate_limit_requests_per_minute: i32,
    pub rate_limit_streams_per_minute: i32,
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationExecutionPolicyPutRequest {
    pub responses_enabled: Option<bool>,
    pub streaming_enabled: Option<bool>,
    pub tools_enabled: Option<bool>,
    pub vision_enabled: Option<bool>,
    pub structured_output_enabled: Option<bool>,
    pub caller_system_instructions_allowed: Option<bool>,
    pub model_overrides_allowed: Option<bool>,
    pub route_overrides_allowed: Option<bool>,
    pub provider_overrides_allowed: Option<bool>,
    pub credential_overrides_allowed: Option<bool>,
    pub timeout_overrides_allowed: Option<bool>,
    pub persistence_mode: Option<ResponsePersistenceMode>,
    pub response_retention_seconds: Option<i64>,
    pub maximum_request_bytes: Option<i64>,
    pub maximum_input_items: Option<i32>,
    pub maximum_output_tokens: Option<i64>,
    pub maximum_timeout_ms: Option<i64>,
    pub rate_limit_requests_per_minute: Option<i32>,
    pub rate_limit_streams_per_minute: Option<i32>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicResponseRequest {
    pub input: Vec<PublicInputMessage>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub provider: Option<Uuid>,
    pub credential_id: Option<Uuid>,
    pub conversation: Option<ResponseConversationInput>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens: Option<u64>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub response_format: PublicResponseFormat,
    #[serde(default)]
    pub tools: Vec<PublicToolDeclaration>,
    pub tool_choice: Option<Value>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
    pub seed: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicInputMessage {
    pub role: PublicMessageRole,
    pub content: Vec<PublicContentPart>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicMessageRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicContentPart {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicResponseFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: Value,
        #[serde(default)]
        strict: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicToolDeclaration {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    pub description: Option<String>,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicResponse {
    pub id: String,
    pub object: String,
    pub created_at: DateTime<Utc>,
    pub status: PublicResponseStatus,
    pub execution_id: String,
    pub request_id: String,
    pub route: Option<PublicRouteRef>,
    pub model: Option<PublicModelRef>,
    pub conversation: Option<PublicConversationRef>,
    pub output: Vec<PublicOutputItem>,
    /// Provenance for the retrieved context that reached the model on this request: one
    /// entry per memory or RAG chunk that was actually included in the assembled prompt,
    /// never per candidate the token budget dropped.
    ///
    /// Populated on `POST /api/v1/responses` and the OpenAI-compatible `POST /v1/responses`
    /// when the request carries a `conversation`, the caller's application has retrieval
    /// enabled with an embedding model configured, and at least one memory or chunk scores
    /// above the configured threshold. An empty array is the normal result whenever any of
    /// those does not hold: no conversation on the request, retrieval disabled for the
    /// application, nothing matched, or a retrieval failure absorbed by the default
    /// `continue_without_semantic_retrieval` policy.
    ///
    /// Only retrieved memories and RAG chunks are cited. Replayed conversation history and
    /// the conversation summary are injected into the prompt without citations, so an empty
    /// array does not mean the model saw no conversation context.
    ///
    /// `GET /api/v1/responses/{response_id}` returns an empty array for every response:
    /// citations come from the context plan computed during the originating request and are
    /// deliberately not re-resolved afterwards. Serialises as `[]`, never `null`. See
    /// docs/retrieval-citations.md.
    pub citations: Vec<PublicCitation>,
    pub usage: PublicUsageSummary,
    pub metadata: Value,
    pub output_persisted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicRouteRef {
    pub id: Uuid,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicModelRef {
    pub id: Uuid,
    pub provider: ProviderType,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicOutputItem {
    Message {
        role: String,
        content: Vec<PublicOutputContentPart>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicOutputContentPart {
    OutputText { text: String },
    OutputUnavailable { reason: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct PublicUsageSummary {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost: Option<f64>,
    pub currency: Option<String>,
}

impl From<UsageSummary> for PublicUsageSummary {
    fn from(value: UsageSummary) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cached_input_tokens: value.cached_input_tokens,
            reasoning_tokens: value.reasoning_tokens,
            total_tokens: value.total_tokens,
            estimated_cost: None,
            currency: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicSseEnvelope {
    pub response_id: String,
    pub execution_id: String,
    pub request_id: String,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    /// Event name. A stream ends with exactly one `response.completed`,
    /// `response.failed`, or `response.cancelled` event.
    #[serde(rename = "type")]
    #[schema(example = "response.cancelled")]
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicResponseRecord {
    pub id: Uuid,
    pub execution_id: Uuid,
    pub request_id: String,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub conversation_id: Option<Uuid>,
    pub conversation_public_id: Option<String>,
    pub status: PublicResponseStatus,
    pub route_id: Option<Uuid>,
    pub route_key: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_type: Option<ProviderType>,
    pub provider_model_id: Option<Uuid>,
    pub model_key: Option<String>,
    pub output_summary: Value,
    pub usage: PublicUsageSummary,
    pub metadata: Value,
    pub failure_class: Option<ExecutionFailureClass>,
    pub failure_message: Option<String>,
    pub output_persisted: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicExecutionSummary {
    pub execution_id: String,
    pub response_id: String,
    pub request_id: String,
    pub status: PublicResponseStatus,
    pub route: Option<PublicRouteRef>,
    pub model: Option<PublicModelRef>,
    pub attempt_count: i64,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub latency_ms: Option<i64>,
    pub usage: PublicUsageSummary,
    pub failure_class: Option<ExecutionFailureClass>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicUsageRecord {
    pub execution_id: String,
    pub provider: Option<ProviderType>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicModelResource {
    pub id: Uuid,
    pub key: String,
    pub provider: ProviderType,
    pub display_name: Option<String>,
    pub capabilities: PublicModelCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct PublicModelCapabilities {
    pub text: bool,
    pub vision: bool,
    pub tools: bool,
    pub streaming: bool,
    pub structured_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicRouteResource {
    pub id: Uuid,
    pub key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicCapabilities {
    pub streaming: bool,
    pub vision: bool,
    pub tools: bool,
    pub structured_output: bool,
    pub reasoning: bool,
    pub response_persistence: ResponsePersistenceMode,
    pub max_input_items: i32,
    pub max_request_bytes: i64,
    pub max_output_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct UsageQuery {
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_model_id: Option<Uuid>,
    pub route_id: Option<Uuid>,
    pub occurred_after: Option<DateTime<Utc>>,
    pub occurred_before: Option<DateTime<Utc>>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl UsageQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ExecutionQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl ExecutionQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAiResponseCompatRequest {
    pub model: Option<String>,
    pub input: Value,
    #[serde(default)]
    pub stream: bool,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub text: Option<OpenAiCompatTextOptions>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

/// OpenAI's `text` object on the Responses API, narrowed to what Moira actually honours.
///
/// It is typed rather than `Value` on purpose. `OpenAiResponseCompatRequest` carries
/// `deny_unknown_fields`, so an *undeclared* `text` would have been an honest 422; declaring
/// it as a free-form `Value` and reading nothing turned that refusal into a silent no-op
/// (F35), and published `"text": {}` in `docs/openapi.json` as if any shape were supported.
/// Every key Moira cannot honour is therefore absent from this type, so serde refuses it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCompatTextOptions {
    #[serde(default)]
    pub format: Option<OpenAiCompatTextFormat>,
}

/// The `text.format` discriminated union.
///
/// `json_object` is declared so it can be *named* in the refusal rather than rejected as an
/// unknown variant; the translation itself refuses it. See
/// `application::public::openai_compat_to_public_request`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenAiCompatTextFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: Value,
        #[serde(default)]
        strict: Option<bool>,
    },
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}
