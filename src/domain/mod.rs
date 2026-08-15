mod admin;
mod agent_platform;
mod auth_settings;
mod conversation;
mod graph;
mod i18n;
mod identity;
mod ids;
mod message;
mod models;
mod pagination;
mod public;
mod runtime;

pub use admin::{
    ApiKeyRecord, ApiKeyRotateRequest, ApiKeySecretResponse, ApplicationCreateRequest,
    ApplicationPatchRequest, ApplicationRecord, AuditEventRecord, AuditLogInsert, AuditLogRecord,
    AuditResult, ConsumerKeyCreateRequest, CredentialCreateRequest, CredentialPatchRequest,
    CredentialRecord, CredentialResolutionInput, CredentialResolutionSource, CredentialScope,
    CredentialSecret, CredentialStatus, CredentialType, IdempotencyRecord, JwtClaimMapping,
    KeyStatus, ListResponse, PageQuery, Pagination, ProviderCreateRequest, ProviderHealthEntry,
    ProviderHealthResponse, ProviderHealthStatus, ProviderModelCreateRequest,
    ProviderModelPatchRequest, ProviderModelRecord, ProviderPatchRequest, ProviderRecord,
    ProviderType, ResourceStatus, RotateCredentialRequest, ScopeType, SetupCheckName,
    SetupCheckState, SetupChecks, SetupDeploymentEnvironment, SetupStatus, SetupStatusResponse,
    SystemKeyCreateRequest, TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest,
    TrustedJwtIssuerRecord,
};
pub use agent_platform::{
    AgentFlowCreateRequest, AgentFlowPatchRequest, AgentFlowRecord, AgentFlowRunRecord,
    AgentFlowStepCreateRequest, AgentFlowStepRecord, AgentSkillBinding, EvalCaseCreateRequest,
    EvalCaseRecord, EvalRunRecord, EvalRunStatus, EvalSuiteCreateRequest, EvalSuitePatchRequest,
    EvalSuiteRecord, EvalTriggerKind, FlowRunStatus, FlowStepOnFailure, FlowStepRunStatus,
    GradingKind, GuardContext, GuardDenialReason, GuardPolicy, GuardPolicyError, GuardVerdict,
    HttpMethod, SkillBulkEnableRequest, SkillBulkEnableResponse, SkillCreateRequest,
    SkillCredentialOutcome, SkillGuard, SkillHttpExecutorPatchRequest, SkillHttpExecutorRecord,
    SkillImportRequest, SkillImportResponse, SkillKind, SkillPatchRequest, SkillRecord,
    SkillResolution, SkillStatus, SkillUnusableReason, credential_binding_permits_host,
    evaluate_guards,
};
pub use auth_settings::{
    AuthMethod, AuthProviderSettingsCreateRequest, AuthProviderSettingsPatchRequest,
    AuthProviderSettingsRecord, PublicAuthMethod, PublicSignInMethod, SetupAuthMethodsResponse,
    SetupSignInMethodsResponse,
};
pub use conversation::{
    ContentWrite, ConversationContentPersistence, ConversationCreateRequest,
    ConversationMessageCreateRequest, ConversationMessageQuery, ConversationMessageRecord,
    ConversationMessageRole, ConversationMessageType, ConversationPatchRequest,
    ConversationPolicyPutRequest, ConversationPolicyRecord, ConversationQuery, ConversationRecord,
    ConversationStatus, ConversationSummarizeAccepted, ConversationSummarizeRequest,
    ConversationSummaryRecord, EmbeddingPolicyPutRequest, EmbeddingPolicyRecord, HistoryStrategy,
    MemoryConsentMode, MemoryCreateRequest, MemoryPatchRequest, MemoryPolicyPutRequest,
    MemoryPolicyRecord, MemoryQuery, MemoryRecord, MemoryScope, MemorySensitivity, MemoryStatus,
    MemoryType, PublicCitation, PublicConversationRef, RagCollectionCreateRequest,
    RagCollectionPatchRequest, RagCollectionQuery, RagCollectionRecord, RagCollectionStatus,
    RagCollectionVisibility, RagDocumentCreateRequest, RagDocumentIngestRequest, RagDocumentRecord,
    RagDocumentStatus, RagIngestionStatus, ResponseConversationInput, RetrievalPolicyPutRequest,
    RetrievalPolicyRecord,
};
// Issue #234 (plan 12 §4). `assemble_graph` and the raw row types are exported alongside the
// wire types (`GraphNode`/`GraphEdge`/`GraphResponse`) so `infra::repositories::graph` can
// decode into them and `application::graph` can call the pure assembler — both need more than
// the OpenAPI-facing shapes.
pub use graph::{
    AgentProfileGraphRow, AgentRouteEdgeRow, FlowStepGraphRow, GraphEdge, GraphEdgeKind, GraphNode,
    GraphNodeType, GraphRawData, GraphResponse, NamedStatusGraphRow, assemble_graph,
};
pub use i18n::{ResponseText, ResponseTextArgs};
pub use identity::{
    AdminIdentityPatchRequest, AdminIdentityRecord, AdminIdentityStatus, AdminInviteConstraint,
    AdminInviteCreateRequest, AdminInvitePreviewRequest, AdminInvitePreviewResponse,
    AdminInviteRecord, AdminInviteRedeemRequest, AdminInviteSecretResponse, AdminInviteStatus,
    ClaimAdminIdentityRequest, MAX_INVITE_EXPIRY_SECONDS, MIN_INVITE_EXPIRY_SECONDS,
    SetupClaimStatusResponse,
};
pub use ids::{
    AgentProfileId, ApplicationId, ApplicationSlug, AttemptId, AuditEventId, ConsumerKeyId,
    ExecutionId, ExternalApplicationId, ExternalTenantId, ExternalUserId, ProviderCredentialId,
    ProviderId, ProviderModelId, RequestId, RouteId, RoutingPolicyId, SystemKeyId,
    TrustedJwtIssuerId,
};
pub use message::{DomainMessage, DomainMessageContent, DomainMessageRole};
pub use models::{
    AuditEvent, CreateProviderRequest, CredentialSummary, HealthResponse, OwnerScope,
    ProviderConfig, ProviderKind, SetRoutingDefaultRequest, UpdateProviderRequest,
    UpsertCredentialRequest,
};
pub use pagination::{CursorScope, Keyed, ListCursor, SeqCursor};
pub use public::{
    ApplicationExecutionPolicyPutRequest, ApplicationExecutionPolicyRecord, ExecutionQuery,
    OpenAiCompatTextFormat, OpenAiCompatTextOptions, OpenAiResponseCompatRequest,
    PublicCapabilities, PublicContentPart, PublicExecutionSummary, PublicInputMessage,
    PublicListQuery, PublicMessageRole, PublicModelCapabilities, PublicModelRef,
    PublicModelResource, PublicOutputContentPart, PublicOutputItem, PublicResponse,
    PublicResponseFormat, PublicResponseRecord, PublicResponseRequest, PublicResponseStatus,
    PublicRouteRef, PublicRouteResource, PublicSseEnvelope, PublicToolDeclaration,
    PublicUsageRecord, PublicUsageSummary, ResponsePersistenceMode, UsageQuery,
};
pub use runtime::{
    AgentProfileCreateRequest, AgentProfilePatchRequest, AgentProfileRecord,
    AgentProfileResolution, ApplicationRoutingDefaultsPutRequest, ApplicationRoutingDefaultsRecord,
    AttemptSelectionReason, AttemptStatus, CallerRuntimeIdentity, ComplexityTier,
    CredentialDecision, CredentialDecisionSource, DiagnosticExecutionRequest,
    DiagnosticExecutionResponse, EffectiveExecutionPolicy, ExecutionCommand, ExecutionFailure,
    ExecutionFailureClass, ExecutionOptions, ExecutionOutcome, ExecutionStatus,
    ExecutionStreamHandle, ModelCandidate, ModelDecision, ModelSelectionReason,
    ProviderAttemptSummary, ProviderModelRuntimeConfig, ProviderRuntimePolicyPutRequest,
    ProviderRuntimePolicyRecord, ResolvedCredential, ResolvedProviderConfiguration, RouteDecision,
    RouteDefinitionCreateRequest, RouteDefinitionPatchRequest, RouteDefinitionRecord,
    RouteSelectionReason, RouteSelectionStrategy, RoutingPolicyCreateRequest,
    RoutingPolicyPatchRequest, RoutingPolicyRecord, RuntimeEventEnvelope, RuntimeEventType,
    RuntimePolicyStatus, UsageSummary,
};
