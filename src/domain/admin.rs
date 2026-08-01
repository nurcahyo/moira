use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ListResponse<T> {
    pub data: Vec<T>,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Pagination {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl<T> ListResponse<T> {
    pub fn new(data: Vec<T>) -> Self {
        Self {
            data,
            pagination: Pagination {
                next_cursor: None,
                has_more: false,
            },
        }
    }
}

/// The single query type shared by all twelve `GET /api/v1/admin/*` list endpoints.
///
/// # Finding P2-9 — what `deny_unknown_fields` does and does not buy (plan 06, module 11)
///
/// `#[serde(deny_unknown_fields)]` rejects only parameters **absent from this struct**:
/// `?not_a_real_field=1` is a `400` on every list route, which
/// `tests/admin_query_contract.rs::each_admin_list_endpoint_rejects_an_unknown_query_field`
/// pins.
///
/// It says nothing about *relevance*. All 26 fields below are accepted by all twelve
/// endpoints, so `GET /api/v1/admin/applications?credential_type=api_key` is a `200` and the
/// filter is silently ignored — `credential_type` exists here only for the credentials list.
/// A caller who mistypes one filter for another therefore gets an unfiltered page rather than
/// an error. `defined_but_unsupported_page_query_field_is_accepted_and_ignored` pins that
/// behaviour deliberately, so changing it is a visible decision rather than a surprise.
///
/// **This is documented, not fixed, in plan 06.** Per-endpoint query types would change the
/// `#[into_params]` expansion and therefore `docs/openapi.json`, which plan 05 froze behind a
/// drift gate; plan 06's premise is that no route or DTO shape moves. Splitting `PageQuery`
/// belongs to whichever plan is willing to regenerate the spec.
///
/// One further consequence of rejecting at the extractor: the `Query` rejection is produced
/// by axum *before* the handler runs, so it precedes `admin_actor` authentication. It is
/// nevertheless served in the standard error envelope — `normalize_infrastructure_error`
/// (`src/lib.rs`) rewrites it, and deliberately discards axum's message, which used to
/// enumerate all 26 field names below to a caller that had presented no credential (F2).
/// See `tests/admin_query_contract.rs` for the guard.
#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub external_application_id: Option<String>,
    pub application_slug: Option<String>,
    pub search: Option<String>,
    pub created_before: Option<DateTime<Utc>>,
    pub created_after: Option<DateTime<Utc>>,
    pub provider_id: Option<Uuid>,
    pub credential_type: Option<CredentialType>,
    pub scope_type: Option<ScopeType>,
    pub application_id: Option<Uuid>,
    pub external_user_id: Option<String>,
    pub external_tenant_id: Option<String>,
    pub expires_before: Option<DateTime<Utc>>,
    pub expires_after: Option<DateTime<Utc>>,
    pub request_id: Option<String>,
    pub actor_type: Option<String>,
    pub actor_subject: Option<String>,
    pub delegated_subject: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub action: Option<String>,
    pub result: Option<AuditResult>,
    pub occurred_before: Option<DateTime<Utc>>,
    pub occurred_after: Option<DateTime<Utc>>,
}

impl PageQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStatus {
    Active,
    Disabled,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    OpenAiCompatible,
    OpenAi,
    Anthropic,
    Gemini,
    DeepSeek,
    AzureOpenAi,
    Local,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeType {
    Global,
    Tenant,
    Application,
    User,
}

fn default_scope_type() -> ScopeType {
    ScopeType::Global
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    ApiKey,
    Oauth2,
    BearerToken,
    BasicAuth,
    CustomHeaders,
    AzureOpenAi,
    ServiceAccount,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    Active,
    Disabled,
    Expired,
    Deleted,
    ValidationFailed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Active,
    Revoked,
    Expired,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditResult {
    Success,
    Denied,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupStatus {
    SetupRequired,
    ConfigurationIncomplete,
    Ready,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupDeploymentEnvironment {
    Development,
    Test,
    Production,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupCheckState {
    Ready,
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupCheckName {
    Database,
    RootSystemKey,
    Application,
    Route,
    Provider,
    ProviderModel,
    ProviderCredential,
    RoutingPolicy,
    ExecutablePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct SetupChecks {
    pub database: SetupCheckState,
    pub root_system_key: SetupCheckState,
    pub application: SetupCheckState,
    pub route: SetupCheckState,
    pub provider: SetupCheckState,
    pub provider_model: SetupCheckState,
    pub provider_credential: SetupCheckState,
    pub routing_policy: SetupCheckState,
    pub executable_path: SetupCheckState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct SetupStatusResponse {
    pub status: SetupStatus,
    pub deployment_environment: SetupDeploymentEnvironment,
    pub checks: SetupChecks,
    pub missing: Vec<SetupCheckName>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplicationRecord {
    pub id: Uuid,
    pub external_application_id: Option<String>,
    pub application_slug: Option<String>,
    pub display_name: String,
    pub status: ResourceStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationCreateRequest {
    pub external_application_id: Option<String>,
    pub application_slug: Option<String>,
    pub display_name: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPatchRequest {
    pub external_application_id: Option<String>,
    pub application_slug: Option<String>,
    pub display_name: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderRecord {
    pub id: Uuid,
    pub provider_type: ProviderType,
    pub display_name: String,
    pub base_url: Option<String>,
    pub status: ResourceStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderCreateRequest {
    pub provider_type: ProviderType,
    pub display_name: String,
    pub base_url: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderPatchRequest {
    pub display_name: Option<String>,
    pub base_url: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderModelRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub model_key: String,
    pub display_name: Option<String>,
    pub capabilities: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelCreateRequest {
    pub model_key: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub capabilities: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelPatchRequest {
    pub display_name: Option<String>,
    pub capabilities: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredentialScope {
    Global,
    Tenant {
        external_tenant_id: String,
    },
    Application {
        application_id: Uuid,
        external_tenant_id: Option<String>,
    },
    User {
        external_user_id: String,
        application_id: Option<Uuid>,
        external_tenant_id: Option<String>,
    },
}

impl CredentialScope {
    pub fn scope_type(&self) -> ScopeType {
        match self {
            Self::Global => ScopeType::Global,
            Self::Tenant { .. } => ScopeType::Tenant,
            Self::Application { .. } => ScopeType::Application,
            Self::User { .. } => ScopeType::User,
        }
    }

    pub fn application_id(&self) -> Option<Uuid> {
        match self {
            Self::Application { application_id, .. } => Some(*application_id),
            Self::User { application_id, .. } => *application_id,
            _ => None,
        }
    }

    pub fn external_user_id(&self) -> Option<&str> {
        match self {
            Self::User {
                external_user_id, ..
            } => Some(external_user_id.as_str()),
            _ => None,
        }
    }

    pub fn external_tenant_id(&self) -> Option<&str> {
        match self {
            Self::Tenant { external_tenant_id } => Some(external_tenant_id.as_str()),
            Self::Application {
                external_tenant_id, ..
            }
            | Self::User {
                external_tenant_id, ..
            } => external_tenant_id.as_deref(),
            Self::Global => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum CredentialSecret {
    ApiKey {
        api_key: String,
    },
    OAuth2 {
        access_token: String,
        refresh_token: Option<String>,
        token_type: Option<String>,
        expires_at: Option<DateTime<Utc>>,
    },
    BearerToken {
        bearer_token: String,
    },
    BasicAuth {
        username: String,
        password: String,
    },
    CustomHeaders {
        headers: Map<String, Value>,
    },
    AzureOpenAi {
        api_key: String,
        endpoint: Option<String>,
    },
    ServiceAccount {
        payload: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub credential_type: CredentialType,
    pub scope: CredentialScope,
    pub display_name: Option<String>,
    #[serde(skip_serializing, default = "default_scope_type")]
    #[schema(ignore)]
    pub scope_type: ScopeType,
    #[serde(skip_serializing, default)]
    #[schema(ignore)]
    pub external_tenant_id: Option<String>,
    #[serde(skip_serializing, default)]
    #[schema(ignore)]
    pub application_id: Option<Uuid>,
    #[serde(skip_serializing, default)]
    #[schema(ignore)]
    pub external_user_id: Option<String>,
    #[serde(skip_serializing, default)]
    #[schema(ignore)]
    pub encryption_algorithm: String,
    #[serde(skip_serializing, default)]
    #[schema(ignore)]
    pub encryption_version: i32,
    pub secret_fingerprint: String,
    pub masked_secret: String,
    pub status: CredentialStatus,
    pub priority: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialCreateRequest {
    pub provider_id: Uuid,
    pub credential_type: CredentialType,
    pub scope: CredentialScope,
    pub secret: CredentialSecret,
    pub display_name: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: i32,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialPatchRequest {
    pub display_name: Option<String>,
    pub priority: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RotateCredentialRequest {
    pub secret: CredentialSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialResolutionInput {
    pub provider_id: Uuid,
    pub credential_id: Option<Uuid>,
    pub external_user_id: Option<String>,
    pub external_tenant_id: Option<String>,
    pub application_id: Option<Uuid>,
    pub credential_type: CredentialType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CredentialResolutionSource {
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub application_id: Option<Uuid>,
    pub display_name: String,
    pub key_prefix: String,
    pub fingerprint: String,
    pub pepper_version: String,
    pub scopes: Vec<String>,
    pub status: KeyStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeySecretResponse {
    pub resource: ApiKeyRecord,
    pub secret: Option<String>,
    pub secret_retrievable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SystemKeyCreateRequest {
    pub display_name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyRotateRequest {
    #[serde(default = "default_revoke_previous_immediately")]
    pub revoke_previous_immediately: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsumerKeyCreateRequest {
    pub application_id: Uuid,
    pub display_name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
pub struct JwtClaimMapping {
    pub subject: Option<String>,
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    pub application_id: Option<String>,
    pub roles: Option<String>,
    pub scopes: Option<String>,
    pub delegated_user: Option<String>,
    pub delegated_tenant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrustedJwtIssuerRecord {
    pub id: Uuid,
    pub issuer: String,
    pub jwks_url: String,
    pub expected_audiences: Vec<String>,
    pub allowed_algorithms: Vec<String>,
    pub subject_claim: String,
    pub user_id_claim: Option<String>,
    pub tenant_id_claim: Option<String>,
    pub application_id_claim: Option<String>,
    pub roles_claim: Option<String>,
    pub scopes_claim: Option<String>,
    pub delegated_user_claim: Option<String>,
    pub delegated_tenant_claim: Option<String>,
    pub clock_skew_seconds: i32,
    pub allow_delegation: bool,
    pub status: ResourceStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustedJwtIssuerCreateRequest {
    pub issuer: String,
    pub jwks_url: String,
    #[serde(default)]
    pub expected_audiences: Vec<String>,
    #[serde(default = "default_allowed_algorithms")]
    pub allowed_algorithms: Vec<String>,
    #[serde(default = "default_subject_claim")]
    pub subject_claim: String,
    pub user_id_claim: Option<String>,
    pub tenant_id_claim: Option<String>,
    pub application_id_claim: Option<String>,
    pub roles_claim: Option<String>,
    pub scopes_claim: Option<String>,
    pub delegated_user_claim: Option<String>,
    pub delegated_tenant_claim: Option<String>,
    #[serde(default = "default_clock_skew_seconds")]
    pub clock_skew_seconds: i32,
    #[serde(default)]
    pub allow_delegation: bool,
    #[serde(default)]
    pub claim_mapping: Option<JwtClaimMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustedJwtIssuerPatchRequest {
    pub jwks_url: Option<String>,
    pub expected_audiences: Option<Vec<String>>,
    pub allowed_algorithms: Option<Vec<String>>,
    pub subject_claim: Option<String>,
    pub user_id_claim: Option<String>,
    pub tenant_id_claim: Option<String>,
    pub application_id_claim: Option<String>,
    pub roles_claim: Option<String>,
    pub scopes_claim: Option<String>,
    pub delegated_user_claim: Option<String>,
    pub delegated_tenant_claim: Option<String>,
    pub clock_skew_seconds: Option<i32>,
    pub allow_delegation: Option<bool>,
    pub claim_mapping: Option<JwtClaimMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditLogRecord {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub request_id: Option<String>,
    pub actor_type: Option<String>,
    pub actor_subject: Option<String>,
    pub delegated_subject: Option<String>,
    pub external_user_id: Option<String>,
    pub external_tenant_id: Option<String>,
    pub application_id: Option<Uuid>,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub action: String,
    pub result: AuditResult,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
    pub metadata: Value,
}

pub type AuditEventRecord = AuditLogRecord;

#[derive(Debug, Clone)]
pub struct AuditLogInsert {
    pub request_id: Option<String>,
    pub actor_type: Option<String>,
    pub actor_subject: Option<String>,
    pub delegated_subject: Option<String>,
    pub external_user_id: Option<String>,
    pub external_tenant_id: Option<String>,
    pub application_id: Option<Uuid>,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub action: String,
    pub result: AuditResult,
    pub source_ip: Option<std::net::IpAddr>,
    pub user_agent: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct IdempotencyRecord {
    pub id: Uuid,
    pub idempotency_key_hash: String,
    pub actor_fingerprint: String,
    pub operation: String,
    pub request_hash: String,
    pub response_status: Option<i32>,
    pub response_body: Option<Value>,
    pub resource_id: Option<String>,
    pub expires_at: DateTime<Utc>,
}

fn default_priority() -> i32 {
    100
}

fn default_revoke_previous_immediately() -> bool {
    true
}

fn default_allowed_algorithms() -> Vec<String> {
    vec!["RS256".to_string()]
}

fn default_subject_claim() -> String {
    "sub".to_string()
}

fn default_clock_skew_seconds() -> i32 {
    60
}
