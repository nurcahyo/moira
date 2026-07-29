use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::{CredentialType, DomainMessage, ProviderType, ResourceStatus};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteDefinitionRecord {
    pub id: Uuid,
    pub route_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub status: ResourceStatus,
    pub selection_strategy: RouteSelectionStrategy,
    pub agent_profile_id: Option<Uuid>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteSelectionStrategy {
    Explicit,
    Rules,
    Default,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteDefinitionCreateRequest {
    pub route_key: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default = "default_route_selection_strategy")]
    pub selection_strategy: RouteSelectionStrategy,
    pub agent_profile_id: Option<Uuid>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteDefinitionPatchRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub selection_strategy: Option<RouteSelectionStrategy>,
    pub agent_profile_id: Option<Uuid>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RoutingPolicyRecord {
    pub id: Uuid,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub route_id: Uuid,
    pub provider_id: Uuid,
    pub provider_model_id: Uuid,
    pub priority: i32,
    pub weight: i32,
    pub cost_weight: f64,
    pub latency_weight: f64,
    pub quality_weight: f64,
    pub privacy_class: Option<String>,
    pub required_capabilities: Vec<String>,
    pub maximum_cost_per_request: Option<f64>,
    pub maximum_input_tokens: Option<i64>,
    pub maximum_output_tokens: Option<i64>,
    pub timeout_ms: Option<i32>,
    pub retry_policy: Value,
    pub status: ResourceStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicyCreateRequest {
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub route_id: Uuid,
    pub provider_id: Uuid,
    pub provider_model_id: Uuid,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_weight")]
    pub weight: i32,
    #[serde(default)]
    pub cost_weight: f64,
    #[serde(default)]
    pub latency_weight: f64,
    #[serde(default)]
    pub quality_weight: f64,
    pub privacy_class: Option<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    pub maximum_cost_per_request: Option<f64>,
    pub maximum_input_tokens: Option<i64>,
    pub maximum_output_tokens: Option<i64>,
    pub timeout_ms: Option<i32>,
    #[serde(default)]
    pub retry_policy: Value,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicyPatchRequest {
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub route_id: Option<Uuid>,
    pub provider_id: Option<Uuid>,
    pub provider_model_id: Option<Uuid>,
    pub priority: Option<i32>,
    pub weight: Option<i32>,
    pub cost_weight: Option<f64>,
    pub latency_weight: Option<f64>,
    pub quality_weight: Option<f64>,
    pub privacy_class: Option<String>,
    pub required_capabilities: Option<Vec<String>>,
    pub maximum_cost_per_request: Option<f64>,
    pub maximum_input_tokens: Option<i64>,
    pub maximum_output_tokens: Option<i64>,
    pub timeout_ms: Option<i32>,
    pub retry_policy: Option<Value>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentProfileRecord {
    pub id: Uuid,
    pub profile_key: String,
    pub display_name: String,
    pub preamble: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub tool_policy: Value,
    pub context_policy: Value,
    pub memory_policy: Value,
    pub status: ResourceStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileCreateRequest {
    pub profile_key: String,
    pub display_name: String,
    pub preamble: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    #[serde(default)]
    pub tool_policy: Value,
    #[serde(default)]
    pub context_policy: Value,
    #[serde(default)]
    pub memory_policy: Value,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentProfilePatchRequest {
    pub display_name: Option<String>,
    pub preamble: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub tool_policy: Option<Value>,
    pub context_policy: Option<Value>,
    pub memory_policy: Option<Value>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderRuntimePolicyRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub connect_timeout_ms: i32,
    pub request_timeout_ms: i32,
    pub stream_idle_timeout_ms: i32,
    pub max_concurrent_requests: i32,
    pub max_concurrent_streams: i32,
    pub retry_limit: i32,
    pub retry_base_delay_ms: i32,
    pub retry_max_delay_ms: i32,
    pub circuit_failure_threshold: i32,
    pub circuit_open_duration_ms: i32,
    pub status: RuntimePolicyStatus,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePolicyStatus {
    Active,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderRuntimePolicyPutRequest {
    pub connect_timeout_ms: Option<i32>,
    pub request_timeout_ms: Option<i32>,
    pub stream_idle_timeout_ms: Option<i32>,
    pub max_concurrent_requests: Option<i32>,
    pub max_concurrent_streams: Option<i32>,
    pub retry_limit: Option<i32>,
    pub retry_base_delay_ms: Option<i32>,
    pub retry_max_delay_ms: Option<i32>,
    pub circuit_failure_threshold: Option<i32>,
    pub circuit_open_duration_ms: Option<i32>,
    pub status: Option<RuntimePolicyStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerRuntimeIdentity {
    pub actor_type: String,
    pub subject: Option<String>,
    pub external_user_id: Option<String>,
    pub external_tenant_id: Option<String>,
    pub application_id: Option<Uuid>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionCommand {
    pub request_id: String,
    pub execution_id: Uuid,
    pub identity: CallerRuntimeIdentity,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub messages: Vec<DomainMessage>,
    pub route_hint: Option<String>,
    pub provider_hint: Option<Uuid>,
    pub model_hint: Option<Uuid>,
    pub credential_hint: Option<Uuid>,
    pub options: ExecutionOptions,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutionOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default = "default_allow_fallback")]
    pub allow_fallback: bool,
    pub max_fallbacks: Option<usize>,
    pub max_retries: Option<usize>,
    pub output_schema: Option<Value>,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
            stream: false,
            required_capabilities: Vec::new(),
            allow_fallback: true,
            max_fallbacks: None,
            max_retries: None,
            output_schema: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveExecutionPolicy {
    pub timeout_ms: u64,
    pub max_retries: usize,
    pub max_fallbacks: usize,
    pub required_capabilities: Vec<String>,
    pub allow_fallback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteDecision {
    pub route_id: Uuid,
    pub route_key: String,
    pub reason: RouteSelectionReason,
    pub agent_profile_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteSelectionReason {
    ExplicitHint,
    RuleMatch,
    ApplicationDefault,
    GlobalDefault,
}

#[derive(Debug, Clone)]
pub struct ModelCandidate {
    pub policy_id: Uuid,
    pub provider_id: Uuid,
    pub provider_version: i64,
    pub provider_type: ProviderType,
    pub provider_display_name: String,
    pub base_url: Option<String>,
    pub provider_model_id: Uuid,
    pub model_version: i64,
    pub model_key: String,
    pub capabilities: Value,
    pub policy_priority: i32,
    pub weight: i32,
    pub timeout_ms: Option<i32>,
    pub retry_policy: Value,
    pub runtime_policy: ProviderRuntimePolicyRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelDecision {
    pub policy_id: Uuid,
    pub provider_id: Uuid,
    pub provider_model_id: Uuid,
    pub model_key: String,
    pub provider_type: ProviderType,
    pub reason: ModelSelectionReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelectionReason {
    ExplicitHint,
    Priority,
    Weighted,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialDecision {
    pub credential_id: Uuid,
    pub credential_type: CredentialType,
    pub source: CredentialDecisionSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialDecisionSource {
    Explicit,
    UserApplicationTenant,
    UserApplication,
    UserTenant,
    User,
    ApplicationTenant,
    Application,
    Tenant,
    Global,
}

#[derive(Debug, Clone)]
pub struct ResolvedProviderConfiguration {
    pub provider_id: Uuid,
    pub provider_version: i64,
    pub provider_type: ProviderType,
    pub display_name: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderModelRuntimeConfig {
    pub provider_model_id: Uuid,
    pub model_version: i64,
    pub model_key: String,
    pub capabilities: Value,
}

#[derive(Clone)]
pub struct ResolvedCredential {
    pub credential_id: Uuid,
    pub credential_version: i64,
    pub credential_type: CredentialType,
    pub secret: secrecy::SecretString,
    pub config: Value,
}

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedCredential")
            .field("credential_id", &self.credential_id)
            .field("credential_version", &self.credential_version)
            .field("credential_type", &self.credential_type)
            .field("secret", &"<redacted>")
            .field("config", &redact_credential_config(&self.config))
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutionOutcome {
    pub request_id: String,
    pub execution_id: Uuid,
    pub status: ExecutionStatus,
    pub output_text: Option<String>,
    pub structured_output: Option<Value>,
    pub usage: UsageSummary,
    pub route: Option<RouteDecision>,
    pub model: Option<ModelDecision>,
    pub attempts: Vec<ProviderAttemptSummary>,
    pub failure: Option<ExecutionFailure>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutionFailure {
    pub class: ExecutionFailureClass,
    pub message: String,
    pub retryable: bool,
    pub fallback_eligible: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionFailureClass {
    InvalidExecutionRequest,
    ApplicationUnavailable,
    RouteNotFound,
    RouteForbidden,
    ModelNotFound,
    ModelForbidden,
    ModelCapabilityMismatch,
    NoEligibleModel,
    CredentialNotFound,
    CredentialForbidden,
    CredentialExpired,
    CredentialDisabled,
    CredentialDecryptionFailed,
    ProviderConfigurationInvalid,
    ProviderUnavailable,
    ProviderRateLimited,
    ProviderTimeout,
    ProviderConnectionFailed,
    ProviderAuthenticationFailed,
    ProviderInvalidResponse,
    ProviderUpstreamError,
    CircuitOpen,
    CapacityExhausted,
    RequestCancelled,
    DeadlineExceeded,
    StructuredOutputInvalid,
    StreamBackpressureExceeded,
    InternalError,
}

impl ExecutionFailureClass {
    /// Every variant, so the i18n catalog gate can walk them.
    ///
    /// This exists because the gate that checks "every emitted error code has a catalog entry"
    /// scans for *literal* code arguments, and these codes are computed by [`Self::code`] — so 23
    /// of them shipped to clients as `moira.error.<code>` message keys with no catalog entry, and
    /// no test noticed. Walking the enum instead of the call sites is what makes that class of gap
    /// impossible rather than merely absent today.
    ///
    /// The exhaustive `match` in [`Self::code`] means adding a variant fails to compile until it
    /// has a code; being listed here means it then fails the catalog test until it has a string.
    /// **Add new variants to this array** — a variant omitted here is invisible to the gate, which
    /// is the one way this can still rot.
    pub const ALL: [Self; 28] = [
        Self::InvalidExecutionRequest,
        Self::ApplicationUnavailable,
        Self::RouteNotFound,
        Self::RouteForbidden,
        Self::ModelNotFound,
        Self::ModelForbidden,
        Self::ModelCapabilityMismatch,
        Self::NoEligibleModel,
        Self::CredentialNotFound,
        Self::CredentialForbidden,
        Self::CredentialExpired,
        Self::CredentialDisabled,
        Self::CredentialDecryptionFailed,
        Self::ProviderConfigurationInvalid,
        Self::ProviderUnavailable,
        Self::ProviderRateLimited,
        Self::ProviderTimeout,
        Self::ProviderConnectionFailed,
        Self::ProviderAuthenticationFailed,
        Self::ProviderInvalidResponse,
        Self::ProviderUpstreamError,
        Self::CircuitOpen,
        Self::CapacityExhausted,
        Self::RequestCancelled,
        Self::DeadlineExceeded,
        Self::StructuredOutputInvalid,
        Self::StreamBackpressureExceeded,
        Self::InternalError,
    ];

    /// The public error code for this class, rendered to callers as `moira.error.<code>`.
    ///
    /// Lives on the domain type rather than in `application::public` so the catalog gate can reach
    /// it without depending on the application layer. `const` because the catalog gate evaluates it
    /// at compile time — a missing entry is then a build failure rather than a test failure.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidExecutionRequest => "invalid_execution_request",
            Self::ApplicationUnavailable => "application_unavailable",
            Self::RouteNotFound => "route_not_found",
            Self::RouteForbidden => "route_forbidden",
            Self::ModelNotFound => "model_not_found",
            Self::ModelForbidden => "model_forbidden",
            Self::ModelCapabilityMismatch => "model_capability_mismatch",
            Self::NoEligibleModel => "no_eligible_model",
            Self::CredentialNotFound => "credential_not_found",
            Self::CredentialForbidden => "credential_forbidden",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialDisabled => "credential_disabled",
            Self::CredentialDecryptionFailed => "credential_decryption_failed",
            Self::ProviderConfigurationInvalid => "provider_configuration_invalid",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderRateLimited => "provider_rate_limited",
            Self::ProviderTimeout => "provider_timeout",
            Self::ProviderConnectionFailed => "provider_connection_failed",
            Self::ProviderAuthenticationFailed => "provider_authentication_failed",
            Self::ProviderInvalidResponse => "provider_invalid_response",
            Self::ProviderUpstreamError => "provider_upstream_error",
            Self::CircuitOpen => "circuit_open",
            Self::CapacityExhausted => "capacity_exhausted",
            Self::RequestCancelled => "request_cancelled",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::StructuredOutputInvalid => "structured_output_invalid",
            Self::StreamBackpressureExceeded => "stream_backpressure_exceeded",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UsageSummary {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderAttemptSummary {
    pub attempt_id: Uuid,
    pub attempt_number: i32,
    pub provider_id: Uuid,
    pub provider_model_id: Uuid,
    pub credential_id: Uuid,
    pub status: AttemptStatus,
    pub failure_class: Option<ExecutionFailureClass>,
    pub latency_ms: Option<i64>,
    pub usage: UsageSummary,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Started,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RuntimeEventEnvelope {
    pub request_id: String,
    pub execution_id: Uuid,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub event_type: RuntimeEventType,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventType {
    ExecutionStarted,
    RoutingStarted,
    RouteSelected,
    ModelSelected,
    ProviderAttemptStarted,
    OutputTextDelta,
    ToolCallStarted,
    ToolCallDelta,
    ToolCallCompleted,
    ToolResult,
    UsageUpdated,
    ProviderAttemptFailed,
    FallbackSelected,
    ExecutionCompleted,
    ExecutionFailed,
}

#[derive(Debug)]
pub struct ExecutionStreamHandle {
    pub events: tokio::sync::mpsc::Receiver<Result<RuntimeEventEnvelope, ExecutionFailure>>,
    pub outcome: tokio::sync::oneshot::Receiver<Result<ExecutionOutcome, ExecutionFailure>>,
    cancellation: tokio_util::sync::CancellationToken,
}

impl ExecutionStreamHandle {
    pub fn new(
        events: tokio::sync::mpsc::Receiver<Result<RuntimeEventEnvelope, ExecutionFailure>>,
        outcome: tokio::sync::oneshot::Receiver<Result<ExecutionOutcome, ExecutionFailure>>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            events,
            outcome,
            cancellation,
        }
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }
}

impl Drop for ExecutionStreamHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticExecutionRequest {
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub route: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_model_id: Option<Uuid>,
    pub credential_id: Option<Uuid>,
    pub prompt: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub options: ExecutionOptions,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiagnosticExecutionResponse {
    pub outcome: ExecutionOutcome,
    pub events: Vec<RuntimeEventEnvelope>,
}

fn default_priority() -> i32 {
    100
}

fn default_weight() -> i32 {
    1
}

fn default_allow_fallback() -> bool {
    true
}

fn default_route_selection_strategy() -> RouteSelectionStrategy {
    RouteSelectionStrategy::Default
}

fn redact_credential_config(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if key.to_ascii_lowercase().contains("secret")
                        || key.to_ascii_lowercase().contains("key")
                        || key.to_ascii_lowercase().contains("token")
                    {
                        (key.clone(), Value::String("<redacted>".to_string()))
                    } else {
                        (key.clone(), value.clone())
                    }
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_credential_debug_redacts_secret() {
        let credential = ResolvedCredential {
            credential_id: Uuid::now_v7(),
            credential_version: 3,
            credential_type: CredentialType::ApiKey,
            secret: secrecy::SecretString::new("sk-test-secret".to_string()),
            config: serde_json::json!({"api_key": "sk-test-secret", "endpoint": "https://example.test"}),
        };
        let debug = format!("{credential:?}");
        assert!(!debug.contains("sk-test-secret"));
        assert!(debug.contains("<redacted>"));
    }
}
