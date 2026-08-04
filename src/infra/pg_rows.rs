use serde_json::Value;
use sqlx::Row;

use crate::{
    domain::{
        ApiKeyRecord, ApplicationExecutionPolicyRecord, ApplicationRecord, AuditEvent,
        AuditLogRecord, AuditResult, ConversationContentPersistence, ConversationMessageRecord,
        ConversationMessageRole, ConversationMessageType, ConversationPolicyRecord,
        ConversationRecord, ConversationStatus, CredentialRecord, CredentialScope,
        CredentialStatus, CredentialSummary, CredentialType, EmbeddingPolicyRecord,
        ExecutionFailureClass, HistoryStrategy, KeyStatus, MemoryConsentMode, MemoryPolicyRecord,
        MemoryRecord, MemoryScope, MemorySensitivity, MemoryStatus, MemoryType, OwnerScope,
        ProviderConfig, ProviderKind, ProviderModelRecord, ProviderModelRuntimeConfig,
        ProviderRecord, ProviderRuntimePolicyRecord, ProviderType, PublicResponseRecord,
        PublicResponseStatus, PublicUsageSummary, RagCollectionRecord, RagCollectionStatus,
        RagCollectionVisibility, RagDocumentRecord, RagDocumentStatus, RagIngestionStatus,
        ResourceStatus, ResponsePersistenceMode, RetrievalPolicyRecord, RouteDefinitionRecord,
        RouteSelectionStrategy, RoutingPolicyRecord, RuntimePolicyStatus, ScopeType,
        TrustedJwtIssuerRecord,
    },
    error::AppError,
};

pub fn provider_from_row(row: &sqlx::postgres::PgRow) -> Result<ProviderConfig, AppError> {
    Ok(ProviderConfig {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        application_id: row.try_get("application_id")?,
        name: row.try_get("name")?,
        kind: provider_kind_from_db(row.try_get::<String, _>("kind")?)?,
        base_url: row.try_get("base_url")?,
        default_model: row.try_get("default_model")?,
        enabled: row.try_get("enabled")?,
        timeout_ms: row.try_get("timeout_ms")?,
        metadata: row.try_get::<Value, _>("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

pub fn credential_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CredentialSummary, AppError> {
    Ok(CredentialSummary {
        id: row.try_get("id")?,
        provider_id: row.try_get("provider_id")?,
        owner_scope: owner_scope_from_db(row.try_get::<String, _>("owner_scope")?)?,
        owner_id: row.try_get("owner_id")?,
        key_id: row.try_get("key_id")?,
        enabled: row.try_get("enabled")?,
        expires_at: row.try_get("expires_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

pub fn audit_from_row(row: &sqlx::postgres::PgRow) -> Result<AuditEvent, AppError> {
    Ok(AuditEvent {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        application_id: row.try_get("application_id")?,
        actor_subject: row.try_get("actor_subject")?,
        action: row.try_get("action")?,
        resource_type: row.try_get("resource_type")?,
        resource_id: row.try_get("resource_id")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
    })
}

pub fn application_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ApplicationRecord, AppError> {
    Ok(ApplicationRecord {
        id: row.try_get("id")?,
        external_application_id: row.try_get("external_application_id")?,
        application_slug: row.try_get("application_slug")?,
        display_name: row.try_get("display_name")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version").unwrap_or(1),
    })
}

pub fn provider_record_from_row(row: &sqlx::postgres::PgRow) -> Result<ProviderRecord, AppError> {
    Ok(ProviderRecord {
        id: row.try_get("id")?,
        provider_type: provider_type_from_db(row.try_get::<String, _>("provider_type")?)?,
        display_name: row.try_get("display_name")?,
        base_url: row.try_get("base_url")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version").unwrap_or(1),
    })
}

pub fn provider_model_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ProviderModelRecord, AppError> {
    Ok(ProviderModelRecord {
        id: row.try_get("id")?,
        provider_id: row.try_get("provider_id")?,
        model_key: row.try_get("model_key")?,
        display_name: row.try_get("display_name")?,
        capabilities: row.try_get("capabilities")?,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version").unwrap_or(1),
    })
}

pub fn credential_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CredentialRecord, AppError> {
    let scope_type = scope_type_from_db(row.try_get::<String, _>("scope_type")?)?;
    let external_tenant_id: Option<String> = row.try_get("external_tenant_id")?;
    let application_id: Option<uuid::Uuid> = row.try_get("application_id")?;
    let external_user_id: Option<String> = row.try_get("external_user_id")?;
    let scope = credential_scope_from_parts(
        scope_type,
        external_tenant_id.clone(),
        application_id,
        external_user_id.clone(),
    )?;
    Ok(CredentialRecord {
        id: row.try_get("id")?,
        provider_id: row.try_get("provider_id")?,
        credential_type: credential_type_from_db(row.try_get::<String, _>("credential_type")?)?,
        scope,
        display_name: row.try_get("display_name").unwrap_or(None),
        scope_type,
        external_tenant_id,
        application_id,
        external_user_id,
        encryption_algorithm: row.try_get("encryption_algorithm")?,
        encryption_version: row.try_get("encryption_version")?,
        secret_fingerprint: row.try_get("secret_fingerprint")?,
        masked_secret: row.try_get("masked_secret")?,
        status: credential_status_from_db(row.try_get::<String, _>("status")?)?,
        priority: row.try_get("priority")?,
        expires_at: row.try_get("expires_at")?,
        last_validated_at: row.try_get("last_validated_at")?,
        last_used_at: row.try_get("last_used_at")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version").unwrap_or(1),
    })
}

pub fn api_key_record_from_row(row: &sqlx::postgres::PgRow) -> Result<ApiKeyRecord, AppError> {
    Ok(ApiKeyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        display_name: row.try_get("display_name")?,
        key_prefix: row.try_get("key_prefix")?,
        fingerprint: row.try_get("fingerprint")?,
        pepper_version: row.try_get("pepper_version")?,
        scopes: row.try_get("scopes")?,
        status: key_status_from_db(row.try_get::<String, _>("status")?)?,
        expires_at: row.try_get("expires_at")?,
        last_used_at: row.try_get("last_used_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

pub fn trusted_jwt_issuer_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<TrustedJwtIssuerRecord, AppError> {
    Ok(TrustedJwtIssuerRecord {
        id: row.try_get("id")?,
        issuer: row.try_get("issuer")?,
        jwks_url: row.try_get("jwks_url")?,
        expected_audiences: row.try_get("expected_audiences")?,
        allowed_algorithms: row.try_get("allowed_algorithms")?,
        subject_claim: row.try_get("subject_claim")?,
        user_id_claim: row.try_get("user_id_claim")?,
        tenant_id_claim: row.try_get("tenant_id_claim")?,
        application_id_claim: row.try_get("application_id_claim")?,
        roles_claim: row.try_get("roles_claim")?,
        scopes_claim: row.try_get("scopes_claim")?,
        delegated_user_claim: row.try_get("delegated_user_claim")?,
        delegated_tenant_claim: row.try_get("delegated_tenant_claim")?,
        clock_skew_seconds: row.try_get("clock_skew_seconds")?,
        allow_delegation: row.try_get("allow_delegation")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version").unwrap_or(1),
    })
}

pub fn audit_log_record_from_row(row: &sqlx::postgres::PgRow) -> Result<AuditLogRecord, AppError> {
    Ok(AuditLogRecord {
        id: row.try_get("id")?,
        occurred_at: row.try_get("occurred_at")?,
        request_id: row.try_get("request_id")?,
        actor_type: row.try_get("actor_type")?,
        actor_subject: row.try_get("actor_subject")?,
        delegated_subject: row.try_get("delegated_subject")?,
        external_user_id: row.try_get("external_user_id")?,
        external_tenant_id: row.try_get("external_tenant_id")?,
        application_id: row.try_get("application_id")?,
        resource_type: row.try_get("resource_type")?,
        resource_id: row.try_get("resource_id")?,
        action: row.try_get("action")?,
        result: audit_result_from_db(row.try_get::<String, _>("result")?)?,
        source_ip: row.try_get("source_ip")?,
        user_agent: row.try_get("user_agent")?,
        metadata: row.try_get("metadata")?,
    })
}

pub fn route_definition_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RouteDefinitionRecord, AppError> {
    Ok(RouteDefinitionRecord {
        id: row.try_get("id")?,
        route_key: row.try_get("route_key")?,
        display_name: row.try_get("display_name")?,
        description: row.try_get("description")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        selection_strategy: route_selection_strategy_from_db(
            row.try_get::<String, _>("selection_strategy")?,
        )?,
        agent_profile_id: row.try_get("agent_profile_id")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
    })
}

pub fn routing_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RoutingPolicyRecord, AppError> {
    Ok(RoutingPolicyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        external_tenant_id: row.try_get("external_tenant_id")?,
        route_id: row.try_get("route_id")?,
        provider_id: row.try_get("provider_id")?,
        provider_model_id: row.try_get("provider_model_id")?,
        priority: row.try_get("priority")?,
        weight: row.try_get("weight")?,
        cost_weight: row.try_get("cost_weight")?,
        latency_weight: row.try_get("latency_weight")?,
        quality_weight: row.try_get("quality_weight")?,
        privacy_class: row.try_get("privacy_class")?,
        required_capabilities: row.try_get("required_capabilities")?,
        maximum_cost_per_request: row.try_get("maximum_cost_per_request")?,
        maximum_input_tokens: row.try_get("maximum_input_tokens")?,
        maximum_output_tokens: row.try_get("maximum_output_tokens")?,
        timeout_ms: row.try_get("timeout_ms")?,
        retry_policy: row.try_get("retry_policy")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
    })
}

pub fn agent_profile_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<crate::domain::AgentProfileRecord, AppError> {
    Ok(crate::domain::AgentProfileRecord {
        id: row.try_get("id")?,
        profile_key: row.try_get("profile_key")?,
        display_name: row.try_get("display_name")?,
        preamble: row.try_get("preamble")?,
        temperature: row.try_get("temperature")?,
        max_tokens: row.try_get("max_tokens")?,
        tool_policy: row.try_get("tool_policy")?,
        context_policy: row.try_get("context_policy")?,
        memory_policy: row.try_get("memory_policy")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
    })
}

pub fn provider_runtime_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ProviderRuntimePolicyRecord, AppError> {
    Ok(ProviderRuntimePolicyRecord {
        id: row.try_get("id")?,
        provider_id: row.try_get("provider_id")?,
        connect_timeout_ms: row.try_get("connect_timeout_ms")?,
        request_timeout_ms: row.try_get("request_timeout_ms")?,
        stream_idle_timeout_ms: row.try_get("stream_idle_timeout_ms")?,
        max_concurrent_requests: row.try_get("max_concurrent_requests")?,
        max_concurrent_streams: row.try_get("max_concurrent_streams")?,
        retry_limit: row.try_get("retry_limit")?,
        retry_base_delay_ms: row.try_get("retry_base_delay_ms")?,
        retry_max_delay_ms: row.try_get("retry_max_delay_ms")?,
        circuit_failure_threshold: row.try_get("circuit_failure_threshold")?,
        circuit_open_duration_ms: row.try_get("circuit_open_duration_ms")?,
        status: runtime_policy_status_from_db(row.try_get::<String, _>("status")?)?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

pub fn application_execution_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ApplicationExecutionPolicyRecord, AppError> {
    Ok(ApplicationExecutionPolicyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        responses_enabled: row.try_get("responses_enabled")?,
        streaming_enabled: row.try_get("streaming_enabled")?,
        tools_enabled: row.try_get("tools_enabled")?,
        vision_enabled: row.try_get("vision_enabled")?,
        structured_output_enabled: row.try_get("structured_output_enabled")?,
        caller_system_instructions_allowed: row.try_get("caller_system_instructions_allowed")?,
        model_overrides_allowed: row.try_get("model_overrides_allowed")?,
        route_overrides_allowed: row.try_get("route_overrides_allowed")?,
        provider_overrides_allowed: row.try_get("provider_overrides_allowed")?,
        credential_overrides_allowed: row.try_get("credential_overrides_allowed")?,
        timeout_overrides_allowed: row.try_get("timeout_overrides_allowed")?,
        persistence_mode: response_persistence_mode_from_db(
            row.try_get::<String, _>("persistence_mode")?,
        )?,
        response_retention_seconds: row.try_get("response_retention_seconds")?,
        maximum_request_bytes: row.try_get("maximum_request_bytes")?,
        maximum_input_items: row.try_get("maximum_input_items")?,
        maximum_output_tokens: row.try_get("maximum_output_tokens")?,
        maximum_timeout_ms: row.try_get("maximum_timeout_ms")?,
        rate_limit_requests_per_minute: row.try_get("rate_limit_requests_per_minute")?,
        rate_limit_streams_per_minute: row.try_get("rate_limit_streams_per_minute")?,
        metadata: row.try_get("metadata")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

pub fn public_response_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PublicResponseRecord, AppError> {
    let provider_type = row
        .try_get::<Option<String>, _>("provider_type")?
        .map(provider_type_from_db)
        .transpose()?;
    let usage = row
        .try_get::<Value, _>("usage_summary")
        .ok()
        .and_then(|value| serde_json::from_value::<PublicUsageSummary>(value).ok())
        .unwrap_or_default();
    let failure_class = row
        .try_get::<Option<String>, _>("failure_class")?
        .map(execution_failure_class_from_db)
        .transpose()?;
    Ok(PublicResponseRecord {
        id: row.try_get("id")?,
        execution_id: row.try_get("execution_id")?,
        request_id: row.try_get("request_id")?,
        application_id: row.try_get("application_id")?,
        external_tenant_id: row.try_get("external_tenant_id")?,
        external_user_id: row.try_get("external_user_id")?,
        conversation_id: row.try_get("conversation_id").unwrap_or(None),
        conversation_public_id: row.try_get("conversation_public_id").unwrap_or(None),
        status: public_response_status_from_db(row.try_get::<String, _>("status")?)?,
        route_id: row.try_get("route_id")?,
        route_key: row.try_get("route_key").unwrap_or(None),
        provider_id: row.try_get("provider_id")?,
        provider_type,
        provider_model_id: row.try_get("provider_model_id")?,
        model_key: row.try_get("model_key").unwrap_or(None),
        output_summary: row.try_get("output_summary")?,
        usage,
        metadata: row.try_get("metadata")?,
        failure_class,
        failure_message: row.try_get("failure_message")?,
        output_persisted: row.try_get("output_persisted")?,
        created_at: row.try_get("created_at")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
        failed_at: row.try_get("failed_at")?,
        cancelled_at: row.try_get("cancelled_at")?,
        expires_at: row.try_get("expires_at")?,
        version: row.try_get("version")?,
    })
}

/// `ConversationRecord.memory_behavior` — the consent mode actually in force, from **both**
/// columns.
///
/// # F30
///
/// The value used to be `coalesce(mp.consent_mode, 'explicit_only')`, computed in
/// `conversation_select`. That reported the memory policy alone, so an application that had
/// tightened `application_conversation_policies.memory_consent_mode` was told its memory behaviour
/// was the looser of its two settings — while extraction obeyed the stricter one. The two agree in
/// every default deployment and disagree exactly when an operator has deliberately tightened one,
/// which is why nothing noticed.
///
/// `MemoryConsentMode::stricter_of` is the same rule `effective_extraction_status` applies, and it
/// is now applied here rather than restated, so the reported value and the enforced value cannot
/// drift.
///
/// **`policy_controlled` when either column is missing, never a half-answer.** The sentinel is the
/// pre-existing behaviour for the single-row call sites that do not go through
/// `conversation_select` (find/patch/insert … returning), and extending it to "one column present,
/// one absent" is deliberate: reporting the one column we happen to have is exactly the defect
/// this function exists to remove. Every row from `conversation_select` carries both.
fn effective_memory_behavior(row: &sqlx::postgres::PgRow) -> String {
    let mode = |column: &str| -> Option<MemoryConsentMode> {
        row.try_get::<String, _>(column)
            .ok()
            .and_then(|value| memory_consent_mode_from_db(value).ok())
    };
    match (
        mode("conversation_consent_mode"),
        mode("memory_policy_consent_mode"),
    ) {
        (Some(conversation), Some(memory)) => {
            memory_consent_mode_to_db(MemoryConsentMode::stricter_of(conversation, memory))
                .to_string()
        }
        _ => "policy_controlled".to_string(),
    }
}

pub fn conversation_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ConversationRecord, AppError> {
    Ok(ConversationRecord {
        id: row.try_get("public_id")?,
        object: "conversation".to_string(),
        application_id: row.try_get("application_id")?,
        external_tenant_id: row.try_get("external_tenant_id")?,
        external_user_id: row.try_get("external_user_id")?,
        title: row.try_get("title")?,
        status: conversation_status_from_db(row.try_get::<String, _>("status")?)?,
        message_count: row.try_get("message_count")?,
        last_message_at: row.try_get("last_message_at")?,
        summary_available: row.try_get("summary_available").unwrap_or(false),
        memory_behavior: effective_memory_behavior(row),
        retention_expires_at: row.try_get("retention_expires_at")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        archived_at: row.try_get("archived_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
    })
}

pub fn conversation_message_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ConversationMessageRecord, AppError> {
    Ok(ConversationMessageRecord {
        id: row.try_get("public_id")?,
        object: "conversation.message".to_string(),
        conversation_id: row.try_get("conversation_public_id")?,
        response_id: row
            .try_get::<Option<uuid::Uuid>, _>("response_id")?
            .map(|id| format!("resp_{id}")),
        execution_id: row
            .try_get::<Option<uuid::Uuid>, _>("execution_id")?
            .map(|id| format!("exec_{id}")),
        role: conversation_message_role_from_db(row.try_get::<String, _>("role")?)?,
        message_type: conversation_message_type_from_db(row.try_get::<String, _>("message_type")?)?,
        sequence_number: row.try_get("sequence_number")?,
        content: row.try_get("content_plain")?,
        content_hash: row.try_get("content_hash")?,
        content_size_bytes: row.try_get("content_size_bytes")?,
        token_count: row.try_get("token_count")?,
        created_at: row.try_get("created_at")?,
        deleted_at: row.try_get("deleted_at")?,
        metadata: row.try_get("metadata")?,
    })
}

pub fn conversation_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ConversationPolicyRecord, AppError> {
    Ok(ConversationPolicyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        conversations_enabled: row.try_get("conversations_enabled")?,
        conversation_content_persistence: conversation_content_persistence_from_db(
            row.try_get::<String, _>("conversation_content_persistence")?,
        )?,
        default_retention_days: row.try_get("default_retention_days")?,
        maximum_retention_days: row.try_get("maximum_retention_days")?,
        history_strategy: history_strategy_from_db(row.try_get::<String, _>("history_strategy")?)?,
        maximum_recent_messages: row.try_get("maximum_recent_messages")?,
        maximum_history_tokens: row.try_get("maximum_history_tokens")?,
        summarization_enabled: row.try_get("summarization_enabled")?,
        summary_trigger_tokens: row.try_get("summary_trigger_tokens")?,
        summary_target_tokens: row.try_get("summary_target_tokens")?,
        minimum_messages_since_summary: row.try_get("minimum_messages_since_summary")?,
        memory_enabled: row.try_get("memory_enabled")?,
        memory_extraction_enabled: row.try_get("memory_extraction_enabled")?,
        memory_retrieval_enabled: row.try_get("memory_retrieval_enabled")?,
        memory_consent_mode: memory_consent_mode_from_db(
            row.try_get::<String, _>("memory_consent_mode")?,
        )?,
        rag_enabled: row.try_get("rag_enabled")?,
        default_collection_ids: row.try_get("default_collection_ids")?,
        caller_can_create_conversations: row.try_get("caller_can_create_conversations")?,
        caller_can_delete_conversations: row.try_get("caller_can_delete_conversations")?,
        caller_can_export_conversations: row.try_get("caller_can_export_conversations")?,
        protected_instruction_policy: row.try_get("protected_instruction_policy")?,
        metadata: row.try_get("metadata")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

pub fn memory_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<MemoryPolicyRecord, AppError> {
    Ok(MemoryPolicyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        enabled: row.try_get("enabled")?,
        consent_mode: memory_consent_mode_from_db(row.try_get::<String, _>("consent_mode")?)?,
        allowed_memory_types: row
            .try_get::<Vec<String>, _>("allowed_memory_types")?
            .into_iter()
            .map(memory_type_from_db)
            .collect::<Result<Vec<_>, _>>()?,
        allowed_sensitivity_levels: row
            .try_get::<Vec<String>, _>("allowed_sensitivity_levels")?
            .into_iter()
            .map(memory_sensitivity_from_db)
            .collect::<Result<Vec<_>, _>>()?,
        automatic_extraction_enabled: row.try_get("automatic_extraction_enabled")?,
        automatic_retrieval_enabled: row.try_get("automatic_retrieval_enabled")?,
        manual_memory_enabled: row.try_get("manual_memory_enabled")?,
        minimum_extraction_confidence: row.try_get("minimum_extraction_confidence")?,
        minimum_retrieval_confidence: row.try_get("minimum_retrieval_confidence")?,
        maximum_memory_count_per_user: row.try_get("maximum_memory_count_per_user")?,
        maximum_memory_tokens_per_request: row.try_get("maximum_memory_tokens_per_request")?,
        default_ttl_days: row.try_get("default_ttl_days")?,
        maximum_ttl_days: row.try_get("maximum_ttl_days")?,
        user_can_list: row.try_get("user_can_list")?,
        user_can_edit: row.try_get("user_can_edit")?,
        user_can_delete: row.try_get("user_can_delete")?,
        user_can_disable: row.try_get("user_can_disable")?,
        metadata: row.try_get("metadata")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

pub fn retrieval_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RetrievalPolicyRecord, AppError> {
    Ok(RetrievalPolicyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        enabled: row.try_get("enabled")?,
        memory_retrieval_enabled: row.try_get("memory_retrieval_enabled")?,
        rag_retrieval_enabled: row.try_get("rag_retrieval_enabled")?,
        allowed_collection_ids: row.try_get("allowed_collection_ids")?,
        default_collection_ids: row.try_get("default_collection_ids")?,
        maximum_memory_results: row.try_get("maximum_memory_results")?,
        maximum_chunk_results: row.try_get("maximum_chunk_results")?,
        maximum_memory_tokens: row.try_get("maximum_memory_tokens")?,
        maximum_rag_tokens: row.try_get("maximum_rag_tokens")?,
        semantic_weight: row.try_get("semantic_weight")?,
        keyword_weight: row.try_get("keyword_weight")?,
        recency_weight: row.try_get("recency_weight")?,
        importance_weight: row.try_get("importance_weight")?,
        minimum_memory_score: row.try_get("minimum_memory_score")?,
        minimum_chunk_score: row.try_get("minimum_chunk_score")?,
        maximum_chunks_per_document: row.try_get("maximum_chunks_per_document")?,
        diversity_enabled: row.try_get("diversity_enabled")?,
        metadata: row.try_get("metadata")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

pub fn embedding_policy_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EmbeddingPolicyRecord, AppError> {
    Ok(EmbeddingPolicyRecord {
        id: row.try_get("id")?,
        application_id: row.try_get("application_id")?,
        embedding_provider_id: row.try_get("embedding_provider_id")?,
        embedding_model_id: row.try_get("embedding_model_id")?,
        embedding_dimension: row.try_get("embedding_dimension")?,
        batch_size: row.try_get("batch_size")?,
        maximum_input_tokens: row.try_get("maximum_input_tokens")?,
        timeout_ms: row.try_get("timeout_ms")?,
        memory_embeddings_enabled: row.try_get("memory_embeddings_enabled")?,
        rag_embeddings_enabled: row.try_get("rag_embeddings_enabled")?,
        failure_behavior: row.try_get("failure_behavior")?,
        metadata: row.try_get("metadata")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

pub fn memory_record_from_row(row: &sqlx::postgres::PgRow) -> Result<MemoryRecord, AppError> {
    Ok(MemoryRecord {
        id: row.try_get("public_id")?,
        object: "memory".to_string(),
        memory_type: memory_type_from_db(row.try_get::<String, _>("memory_type")?)?,
        scope: memory_scope_from_db(row.try_get::<String, _>("memory_scope")?)?,
        content: row.try_get("content_plain")?,
        importance: row.try_get("importance")?,
        confidence: row.try_get("confidence")?,
        sensitivity: memory_sensitivity_from_db(row.try_get::<String, _>("sensitivity")?)?,
        status: memory_status_from_db(row.try_get::<String, _>("status")?)?,
        conversation_id: row.try_get("conversation_public_id").unwrap_or(None),
        valid_from: row.try_get("valid_from")?,
        valid_until: row.try_get("valid_until")?,
        last_confirmed_at: row.try_get("last_confirmed_at")?,
        last_used_at: row.try_get("last_used_at")?,
        use_count: row.try_get("use_count")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
        metadata: row.try_get("metadata")?,
    })
}

pub fn rag_collection_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RagCollectionRecord, AppError> {
    Ok(RagCollectionRecord {
        id: row.try_get("public_id")?,
        object: "rag.collection".to_string(),
        application_id: row.try_get("application_id")?,
        external_tenant_id: row.try_get("external_tenant_id")?,
        collection_key: row.try_get("collection_key")?,
        display_name: row.try_get("display_name")?,
        description: row.try_get("description")?,
        status: rag_collection_status_from_db(row.try_get::<String, _>("status")?)?,
        visibility: rag_collection_visibility_from_db(row.try_get::<String, _>("visibility")?)?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
    })
}

pub fn rag_document_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RagDocumentRecord, AppError> {
    Ok(RagDocumentRecord {
        id: row.try_get("public_id")?,
        object: "rag.document".to_string(),
        collection_id: row.try_get("collection_public_id")?,
        external_document_id: row.try_get("external_document_id")?,
        title: row.try_get("title")?,
        source_type: row.try_get("source_type")?,
        source_uri: row.try_get("source_uri")?,
        mime_type: row.try_get("mime_type")?,
        status: rag_document_status_from_db(row.try_get::<String, _>("status")?)?,
        current_version_id: row.try_get("current_version_id")?,
        ingestion_status: row
            .try_get::<Option<String>, _>("ingestion_status")?
            .map(rag_ingestion_status_from_db)
            .transpose()?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
        version: row.try_get("version")?,
    })
}

pub fn provider_model_runtime_config_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ProviderModelRuntimeConfig, AppError> {
    Ok(ProviderModelRuntimeConfig {
        provider_model_id: row.try_get("provider_model_id")?,
        model_version: row.try_get("model_version")?,
        model_key: row.try_get("model_key")?,
        capabilities: row.try_get("capabilities")?,
    })
}

pub fn provider_kind_from_db(value: String) -> Result<ProviderKind, AppError> {
    match value.as_str() {
        "openai_compatible" => Ok(ProviderKind::OpenAiCompatible),
        "openai" => Ok(ProviderKind::OpenAi),
        "anthropic" => Ok(ProviderKind::Anthropic),
        "gemini" => Ok(ProviderKind::Gemini),
        "deepseek" => Ok(ProviderKind::DeepSeek),
        "azure_openai" => Ok(ProviderKind::AzureOpenAi),
        "local" => Ok(ProviderKind::Local),
        "custom" => Ok(ProviderKind::Custom),
        _ => Err(AppError::Internal(format!("unknown provider kind {value}"))),
    }
}

pub fn provider_kind_to_db(kind: &ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAiCompatible => "openai_compatible",
        ProviderKind::OpenAi => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Gemini => "gemini",
        ProviderKind::DeepSeek => "deepseek",
        ProviderKind::AzureOpenAi => "azure_openai",
        ProviderKind::Local => "local",
        ProviderKind::Custom => "custom",
    }
}

pub fn owner_scope_from_db(value: String) -> Result<OwnerScope, AppError> {
    match value.as_str() {
        "global" => Ok(OwnerScope::Global),
        "tenant" => Ok(OwnerScope::Tenant),
        "application" => Ok(OwnerScope::Application),
        "user" => Ok(OwnerScope::User),
        _ => Err(AppError::Internal(format!("unknown owner scope {value}"))),
    }
}

pub fn owner_scope_to_db(scope: &OwnerScope) -> &'static str {
    match scope {
        OwnerScope::Global => "global",
        OwnerScope::Tenant => "tenant",
        OwnerScope::Application => "application",
        OwnerScope::User => "user",
    }
}

pub fn resource_status_from_db(value: String) -> Result<ResourceStatus, AppError> {
    match value.as_str() {
        "active" => Ok(ResourceStatus::Active),
        "disabled" => Ok(ResourceStatus::Disabled),
        "deleted" => Ok(ResourceStatus::Deleted),
        _ => Err(AppError::Internal(format!(
            "unknown resource status {value}"
        ))),
    }
}

pub fn resource_status_to_db(status: &ResourceStatus) -> &'static str {
    match status {
        ResourceStatus::Active => "active",
        ResourceStatus::Disabled => "disabled",
        ResourceStatus::Deleted => "deleted",
    }
}

pub fn route_selection_strategy_from_db(value: String) -> Result<RouteSelectionStrategy, AppError> {
    match value.as_str() {
        "explicit" => Ok(RouteSelectionStrategy::Explicit),
        "rules" => Ok(RouteSelectionStrategy::Rules),
        "default" => Ok(RouteSelectionStrategy::Default),
        _ => Err(AppError::Internal(format!(
            "unknown route selection strategy {value}"
        ))),
    }
}

pub fn route_selection_strategy_to_db(strategy: &RouteSelectionStrategy) -> &'static str {
    match strategy {
        RouteSelectionStrategy::Explicit => "explicit",
        RouteSelectionStrategy::Rules => "rules",
        RouteSelectionStrategy::Default => "default",
    }
}

pub fn runtime_policy_status_from_db(value: String) -> Result<RuntimePolicyStatus, AppError> {
    match value.as_str() {
        "active" => Ok(RuntimePolicyStatus::Active),
        "disabled" => Ok(RuntimePolicyStatus::Disabled),
        _ => Err(AppError::Internal(format!(
            "unknown runtime policy status {value}"
        ))),
    }
}

pub fn runtime_policy_status_to_db(status: &RuntimePolicyStatus) -> &'static str {
    match status {
        RuntimePolicyStatus::Active => "active",
        RuntimePolicyStatus::Disabled => "disabled",
    }
}

pub fn public_response_status_from_db(value: String) -> Result<PublicResponseStatus, AppError> {
    match value.as_str() {
        "queued" => Ok(PublicResponseStatus::Queued),
        "in_progress" => Ok(PublicResponseStatus::InProgress),
        "completed" => Ok(PublicResponseStatus::Completed),
        "failed" => Ok(PublicResponseStatus::Failed),
        "cancelled" => Ok(PublicResponseStatus::Cancelled),
        _ => Err(AppError::Internal(format!(
            "unknown public response status {value}"
        ))),
    }
}

pub fn public_response_status_to_db(status: PublicResponseStatus) -> &'static str {
    match status {
        PublicResponseStatus::Queued => "queued",
        PublicResponseStatus::InProgress => "in_progress",
        PublicResponseStatus::Completed => "completed",
        PublicResponseStatus::Failed => "failed",
        PublicResponseStatus::Cancelled => "cancelled",
    }
}

pub fn response_persistence_mode_from_db(
    value: String,
) -> Result<ResponsePersistenceMode, AppError> {
    match value.as_str() {
        "none" => Ok(ResponsePersistenceMode::None),
        "metadata_only" => Ok(ResponsePersistenceMode::MetadataOnly),
        "encrypted_content" => Ok(ResponsePersistenceMode::EncryptedContent),
        "plain_content" => Ok(ResponsePersistenceMode::PlainContent),
        _ => Err(AppError::Internal(format!(
            "unknown response persistence mode {value}"
        ))),
    }
}

pub fn response_persistence_mode_to_db(mode: ResponsePersistenceMode) -> &'static str {
    match mode {
        ResponsePersistenceMode::None => "none",
        ResponsePersistenceMode::MetadataOnly => "metadata_only",
        ResponsePersistenceMode::EncryptedContent => "encrypted_content",
        ResponsePersistenceMode::PlainContent => "plain_content",
    }
}

pub fn conversation_status_from_db(value: String) -> Result<ConversationStatus, AppError> {
    match value.as_str() {
        "active" => Ok(ConversationStatus::Active),
        "archived" => Ok(ConversationStatus::Archived),
        "deleted" => Ok(ConversationStatus::Deleted),
        _ => Err(AppError::Internal(format!(
            "unknown conversation status {value}"
        ))),
    }
}

pub fn conversation_status_to_db(status: ConversationStatus) -> &'static str {
    match status {
        ConversationStatus::Active => "active",
        ConversationStatus::Archived => "archived",
        ConversationStatus::Deleted => "deleted",
    }
}

pub fn conversation_message_role_from_db(
    value: String,
) -> Result<ConversationMessageRole, AppError> {
    match value.as_str() {
        "system" => Ok(ConversationMessageRole::System),
        "developer" => Ok(ConversationMessageRole::Developer),
        "user" => Ok(ConversationMessageRole::User),
        "assistant" => Ok(ConversationMessageRole::Assistant),
        "tool" => Ok(ConversationMessageRole::Tool),
        _ => Err(AppError::Internal(format!(
            "unknown conversation message role {value}"
        ))),
    }
}

pub fn conversation_message_role_to_db(role: ConversationMessageRole) -> &'static str {
    match role {
        ConversationMessageRole::System => "system",
        ConversationMessageRole::Developer => "developer",
        ConversationMessageRole::User => "user",
        ConversationMessageRole::Assistant => "assistant",
        ConversationMessageRole::Tool => "tool",
    }
}

pub fn conversation_message_type_from_db(
    value: String,
) -> Result<ConversationMessageType, AppError> {
    match value.as_str() {
        "input" => Ok(ConversationMessageType::Input),
        "output" => Ok(ConversationMessageType::Output),
        "summary" => Ok(ConversationMessageType::Summary),
        "tool_call" => Ok(ConversationMessageType::ToolCall),
        "tool_result" => Ok(ConversationMessageType::ToolResult),
        "marker" => Ok(ConversationMessageType::Marker),
        _ => Err(AppError::Internal(format!(
            "unknown conversation message type {value}"
        ))),
    }
}

pub fn conversation_message_type_to_db(message_type: ConversationMessageType) -> &'static str {
    match message_type {
        ConversationMessageType::Input => "input",
        ConversationMessageType::Output => "output",
        ConversationMessageType::Summary => "summary",
        ConversationMessageType::ToolCall => "tool_call",
        ConversationMessageType::ToolResult => "tool_result",
        ConversationMessageType::Marker => "marker",
    }
}

pub fn conversation_content_persistence_from_db(
    value: String,
) -> Result<ConversationContentPersistence, AppError> {
    match value.as_str() {
        "none" => Ok(ConversationContentPersistence::None),
        "metadata_only" => Ok(ConversationContentPersistence::MetadataOnly),
        "plain_content" => Ok(ConversationContentPersistence::PlainContent),
        "encrypted_content" => Ok(ConversationContentPersistence::EncryptedContent),
        _ => Err(AppError::Internal(format!(
            "unknown conversation content persistence {value}"
        ))),
    }
}

pub fn conversation_content_persistence_to_db(
    value: ConversationContentPersistence,
) -> &'static str {
    match value {
        ConversationContentPersistence::None => "none",
        ConversationContentPersistence::MetadataOnly => "metadata_only",
        ConversationContentPersistence::PlainContent => "plain_content",
        ConversationContentPersistence::EncryptedContent => "encrypted_content",
    }
}

pub fn history_strategy_from_db(value: String) -> Result<HistoryStrategy, AppError> {
    match value.as_str() {
        "recent_messages" => Ok(HistoryStrategy::RecentMessages),
        "summary_plus_recent" => Ok(HistoryStrategy::SummaryPlusRecent),
        "full_until_limit" => Ok(HistoryStrategy::FullUntilLimit),
        _ => Err(AppError::Internal(format!(
            "unknown history strategy {value}"
        ))),
    }
}

pub fn history_strategy_to_db(value: HistoryStrategy) -> &'static str {
    match value {
        HistoryStrategy::RecentMessages => "recent_messages",
        HistoryStrategy::SummaryPlusRecent => "summary_plus_recent",
        HistoryStrategy::FullUntilLimit => "full_until_limit",
    }
}

pub fn memory_consent_mode_from_db(value: String) -> Result<MemoryConsentMode, AppError> {
    match value.as_str() {
        "disabled" => Ok(MemoryConsentMode::Disabled),
        "explicit_only" => Ok(MemoryConsentMode::ExplicitOnly),
        "application_managed" => Ok(MemoryConsentMode::ApplicationManaged),
        "automatic_with_user_controls" => Ok(MemoryConsentMode::AutomaticWithUserControls),
        _ => Err(AppError::Internal(format!(
            "unknown memory consent mode {value}"
        ))),
    }
}

pub fn memory_consent_mode_to_db(value: MemoryConsentMode) -> &'static str {
    match value {
        MemoryConsentMode::Disabled => "disabled",
        MemoryConsentMode::ExplicitOnly => "explicit_only",
        MemoryConsentMode::ApplicationManaged => "application_managed",
        MemoryConsentMode::AutomaticWithUserControls => "automatic_with_user_controls",
    }
}

pub fn memory_type_from_db(value: String) -> Result<MemoryType, AppError> {
    match value.as_str() {
        "preference" => Ok(MemoryType::Preference),
        "fact" => Ok(MemoryType::Fact),
        "goal" => Ok(MemoryType::Goal),
        "constraint" => Ok(MemoryType::Constraint),
        "relationship" => Ok(MemoryType::Relationship),
        "project_context" => Ok(MemoryType::ProjectContext),
        "decision" => Ok(MemoryType::Decision),
        "instruction" => Ok(MemoryType::Instruction),
        "temporary_state" => Ok(MemoryType::TemporaryState),
        _ => Err(AppError::Internal(format!("unknown memory type {value}"))),
    }
}

pub fn memory_type_to_db(value: MemoryType) -> &'static str {
    match value {
        MemoryType::Preference => "preference",
        MemoryType::Fact => "fact",
        MemoryType::Goal => "goal",
        MemoryType::Constraint => "constraint",
        MemoryType::Relationship => "relationship",
        MemoryType::ProjectContext => "project_context",
        MemoryType::Decision => "decision",
        MemoryType::Instruction => "instruction",
        MemoryType::TemporaryState => "temporary_state",
    }
}

pub fn memory_scope_from_db(value: String) -> Result<MemoryScope, AppError> {
    match value.as_str() {
        "conversation" => Ok(MemoryScope::Conversation),
        "user_application" => Ok(MemoryScope::UserApplication),
        "tenant_application" => Ok(MemoryScope::TenantApplication),
        "application" => Ok(MemoryScope::Application),
        _ => Err(AppError::Internal(format!("unknown memory scope {value}"))),
    }
}

pub fn memory_scope_to_db(value: MemoryScope) -> &'static str {
    match value {
        MemoryScope::Conversation => "conversation",
        MemoryScope::UserApplication => "user_application",
        MemoryScope::TenantApplication => "tenant_application",
        MemoryScope::Application => "application",
    }
}

pub fn memory_sensitivity_from_db(value: String) -> Result<MemorySensitivity, AppError> {
    match value.as_str() {
        "normal" => Ok(MemorySensitivity::Normal),
        "personal" => Ok(MemorySensitivity::Personal),
        "restricted" => Ok(MemorySensitivity::Restricted),
        _ => Err(AppError::Internal(format!(
            "unknown memory sensitivity {value}"
        ))),
    }
}

pub fn memory_sensitivity_to_db(value: MemorySensitivity) -> &'static str {
    match value {
        MemorySensitivity::Normal => "normal",
        MemorySensitivity::Personal => "personal",
        MemorySensitivity::Restricted => "restricted",
    }
}

pub fn memory_status_from_db(value: String) -> Result<MemoryStatus, AppError> {
    match value.as_str() {
        "candidate" => Ok(MemoryStatus::Candidate),
        "active" => Ok(MemoryStatus::Active),
        "rejected" => Ok(MemoryStatus::Rejected),
        "superseded" => Ok(MemoryStatus::Superseded),
        "expired" => Ok(MemoryStatus::Expired),
        "deleted" => Ok(MemoryStatus::Deleted),
        _ => Err(AppError::Internal(format!("unknown memory status {value}"))),
    }
}

pub fn memory_status_to_db(value: MemoryStatus) -> &'static str {
    match value {
        MemoryStatus::Candidate => "candidate",
        MemoryStatus::Active => "active",
        MemoryStatus::Rejected => "rejected",
        MemoryStatus::Superseded => "superseded",
        MemoryStatus::Expired => "expired",
        MemoryStatus::Deleted => "deleted",
    }
}

pub fn rag_collection_status_from_db(value: String) -> Result<RagCollectionStatus, AppError> {
    match value.as_str() {
        "active" => Ok(RagCollectionStatus::Active),
        "disabled" => Ok(RagCollectionStatus::Disabled),
        "deleted" => Ok(RagCollectionStatus::Deleted),
        _ => Err(AppError::Internal(format!(
            "unknown rag collection status {value}"
        ))),
    }
}

pub fn rag_collection_status_to_db(value: RagCollectionStatus) -> &'static str {
    match value {
        RagCollectionStatus::Active => "active",
        RagCollectionStatus::Disabled => "disabled",
        RagCollectionStatus::Deleted => "deleted",
    }
}

pub fn rag_collection_visibility_from_db(
    value: String,
) -> Result<RagCollectionVisibility, AppError> {
    match value.as_str() {
        "application" => Ok(RagCollectionVisibility::Application),
        "tenant" => Ok(RagCollectionVisibility::Tenant),
        "restricted" => Ok(RagCollectionVisibility::Restricted),
        _ => Err(AppError::Internal(format!(
            "unknown rag collection visibility {value}"
        ))),
    }
}

pub fn rag_collection_visibility_to_db(value: RagCollectionVisibility) -> &'static str {
    match value {
        RagCollectionVisibility::Application => "application",
        RagCollectionVisibility::Tenant => "tenant",
        RagCollectionVisibility::Restricted => "restricted",
    }
}

pub fn rag_document_status_from_db(value: String) -> Result<RagDocumentStatus, AppError> {
    match value.as_str() {
        "active" => Ok(RagDocumentStatus::Active),
        "disabled" => Ok(RagDocumentStatus::Disabled),
        "deleted" => Ok(RagDocumentStatus::Deleted),
        _ => Err(AppError::Internal(format!(
            "unknown rag document status {value}"
        ))),
    }
}

pub fn rag_document_status_to_db(value: RagDocumentStatus) -> &'static str {
    match value {
        RagDocumentStatus::Active => "active",
        RagDocumentStatus::Disabled => "disabled",
        RagDocumentStatus::Deleted => "deleted",
    }
}

pub fn rag_ingestion_status_from_db(value: String) -> Result<RagIngestionStatus, AppError> {
    match value.as_str() {
        "pending" => Ok(RagIngestionStatus::Pending),
        "downloading" => Ok(RagIngestionStatus::Downloading),
        "parsing" => Ok(RagIngestionStatus::Parsing),
        "chunking" => Ok(RagIngestionStatus::Chunking),
        "embedding" => Ok(RagIngestionStatus::Embedding),
        "indexed" => Ok(RagIngestionStatus::Indexed),
        "failed" => Ok(RagIngestionStatus::Failed),
        "superseded" => Ok(RagIngestionStatus::Superseded),
        _ => Err(AppError::Internal(format!(
            "unknown rag ingestion status {value}"
        ))),
    }
}

pub fn rag_ingestion_status_to_db(value: RagIngestionStatus) -> &'static str {
    match value {
        RagIngestionStatus::Pending => "pending",
        RagIngestionStatus::Downloading => "downloading",
        RagIngestionStatus::Parsing => "parsing",
        RagIngestionStatus::Chunking => "chunking",
        RagIngestionStatus::Embedding => "embedding",
        RagIngestionStatus::Indexed => "indexed",
        RagIngestionStatus::Failed => "failed",
        RagIngestionStatus::Superseded => "superseded",
    }
}

pub fn execution_failure_class_from_db(value: String) -> Result<ExecutionFailureClass, AppError> {
    match value.as_str() {
        "invalid_execution_request" => Ok(ExecutionFailureClass::InvalidExecutionRequest),
        "application_unavailable" => Ok(ExecutionFailureClass::ApplicationUnavailable),
        "route_not_found" => Ok(ExecutionFailureClass::RouteNotFound),
        "route_forbidden" => Ok(ExecutionFailureClass::RouteForbidden),
        "model_not_found" => Ok(ExecutionFailureClass::ModelNotFound),
        "model_forbidden" => Ok(ExecutionFailureClass::ModelForbidden),
        "model_capability_mismatch" => Ok(ExecutionFailureClass::ModelCapabilityMismatch),
        "no_eligible_model" => Ok(ExecutionFailureClass::NoEligibleModel),
        "credential_not_found" => Ok(ExecutionFailureClass::CredentialNotFound),
        "credential_forbidden" => Ok(ExecutionFailureClass::CredentialForbidden),
        "credential_expired" => Ok(ExecutionFailureClass::CredentialExpired),
        "credential_disabled" => Ok(ExecutionFailureClass::CredentialDisabled),
        "credential_decryption_failed" => Ok(ExecutionFailureClass::CredentialDecryptionFailed),
        "provider_configuration_invalid" => Ok(ExecutionFailureClass::ProviderConfigurationInvalid),
        "provider_unavailable" => Ok(ExecutionFailureClass::ProviderUnavailable),
        "provider_rate_limited" => Ok(ExecutionFailureClass::ProviderRateLimited),
        "provider_timeout" => Ok(ExecutionFailureClass::ProviderTimeout),
        "provider_connection_failed" => Ok(ExecutionFailureClass::ProviderConnectionFailed),
        "provider_authentication_failed" => Ok(ExecutionFailureClass::ProviderAuthenticationFailed),
        "provider_invalid_response" => Ok(ExecutionFailureClass::ProviderInvalidResponse),
        "provider_upstream_error" => Ok(ExecutionFailureClass::ProviderUpstreamError),
        "circuit_open" => Ok(ExecutionFailureClass::CircuitOpen),
        "capacity_exhausted" => Ok(ExecutionFailureClass::CapacityExhausted),
        "request_cancelled" => Ok(ExecutionFailureClass::RequestCancelled),
        "deadline_exceeded" => Ok(ExecutionFailureClass::DeadlineExceeded),
        "structured_output_invalid" => Ok(ExecutionFailureClass::StructuredOutputInvalid),
        "stream_backpressure_exceeded" => Ok(ExecutionFailureClass::StreamBackpressureExceeded),
        "internal_error" => Ok(ExecutionFailureClass::InternalError),
        _ => Err(AppError::Internal(format!(
            "unknown execution failure class {value}"
        ))),
    }
}

pub fn provider_type_from_db(value: String) -> Result<ProviderType, AppError> {
    match value.as_str() {
        "openai_compatible" => Ok(ProviderType::OpenAiCompatible),
        "openai" => Ok(ProviderType::OpenAi),
        "anthropic" => Ok(ProviderType::Anthropic),
        "gemini" => Ok(ProviderType::Gemini),
        "deepseek" => Ok(ProviderType::DeepSeek),
        "azure_openai" => Ok(ProviderType::AzureOpenAi),
        "local" => Ok(ProviderType::Local),
        "custom" => Ok(ProviderType::Custom),
        _ => Err(AppError::Internal(format!("unknown provider type {value}"))),
    }
}

pub fn provider_type_to_db(provider_type: &ProviderType) -> &'static str {
    match provider_type {
        ProviderType::OpenAiCompatible => "openai_compatible",
        ProviderType::OpenAi => "openai",
        ProviderType::Anthropic => "anthropic",
        ProviderType::Gemini => "gemini",
        ProviderType::DeepSeek => "deepseek",
        ProviderType::AzureOpenAi => "azure_openai",
        ProviderType::Local => "local",
        ProviderType::Custom => "custom",
    }
}

pub fn scope_type_from_db(value: String) -> Result<ScopeType, AppError> {
    match value.as_str() {
        "global" => Ok(ScopeType::Global),
        "tenant" => Ok(ScopeType::Tenant),
        "application" => Ok(ScopeType::Application),
        "user" => Ok(ScopeType::User),
        _ => Err(AppError::Internal(format!("unknown scope type {value}"))),
    }
}

pub fn scope_type_to_db(scope_type: &ScopeType) -> &'static str {
    match scope_type {
        ScopeType::Global => "global",
        ScopeType::Tenant => "tenant",
        ScopeType::Application => "application",
        ScopeType::User => "user",
    }
}

pub fn credential_type_from_db(value: String) -> Result<CredentialType, AppError> {
    match value.as_str() {
        "api_key" => Ok(CredentialType::ApiKey),
        "oauth2" => Ok(CredentialType::Oauth2),
        "bearer_token" => Ok(CredentialType::BearerToken),
        "basic_auth" => Ok(CredentialType::BasicAuth),
        "custom_headers" => Ok(CredentialType::CustomHeaders),
        "azure_openai" => Ok(CredentialType::AzureOpenAi),
        "service_account" => Ok(CredentialType::ServiceAccount),
        _ => Err(AppError::Internal(format!(
            "unknown credential type {value}"
        ))),
    }
}

pub fn credential_type_to_db(credential_type: &CredentialType) -> &'static str {
    match credential_type {
        CredentialType::ApiKey => "api_key",
        CredentialType::Oauth2 => "oauth2",
        CredentialType::BearerToken => "bearer_token",
        CredentialType::BasicAuth => "basic_auth",
        CredentialType::CustomHeaders => "custom_headers",
        CredentialType::AzureOpenAi => "azure_openai",
        CredentialType::ServiceAccount => "service_account",
    }
}

pub fn credential_status_from_db(value: String) -> Result<CredentialStatus, AppError> {
    match value.as_str() {
        "active" => Ok(CredentialStatus::Active),
        "disabled" => Ok(CredentialStatus::Disabled),
        "expired" => Ok(CredentialStatus::Expired),
        "deleted" => Ok(CredentialStatus::Deleted),
        "validation_failed" => Ok(CredentialStatus::ValidationFailed),
        _ => Err(AppError::Internal(format!(
            "unknown credential status {value}"
        ))),
    }
}

pub fn credential_status_to_db(status: &CredentialStatus) -> &'static str {
    match status {
        CredentialStatus::Active => "active",
        CredentialStatus::Disabled => "disabled",
        CredentialStatus::Expired => "expired",
        CredentialStatus::Deleted => "deleted",
        CredentialStatus::ValidationFailed => "validation_failed",
    }
}

pub fn key_status_from_db(value: String) -> Result<KeyStatus, AppError> {
    match value.as_str() {
        "active" => Ok(KeyStatus::Active),
        "revoked" => Ok(KeyStatus::Revoked),
        "expired" => Ok(KeyStatus::Expired),
        "deleted" => Ok(KeyStatus::Deleted),
        _ => Err(AppError::Internal(format!("unknown key status {value}"))),
    }
}

pub fn audit_result_from_db(value: String) -> Result<AuditResult, AppError> {
    match value.as_str() {
        "success" => Ok(AuditResult::Success),
        "denied" => Ok(AuditResult::Denied),
        "failed" => Ok(AuditResult::Failed),
        _ => Err(AppError::Internal(format!("unknown audit result {value}"))),
    }
}

pub fn audit_result_to_db(result: &AuditResult) -> &'static str {
    match result {
        AuditResult::Success => "success",
        AuditResult::Denied => "denied",
        AuditResult::Failed => "failed",
    }
}

fn credential_scope_from_parts(
    scope_type: ScopeType,
    external_tenant_id: Option<String>,
    application_id: Option<uuid::Uuid>,
    external_user_id: Option<String>,
) -> Result<CredentialScope, AppError> {
    match scope_type {
        ScopeType::Global => Ok(CredentialScope::Global),
        ScopeType::Tenant => Ok(CredentialScope::Tenant {
            external_tenant_id: external_tenant_id.ok_or_else(|| {
                AppError::Internal("tenant credential missing external_tenant_id".to_string())
            })?,
        }),
        ScopeType::Application => Ok(CredentialScope::Application {
            application_id: application_id.ok_or_else(|| {
                AppError::Internal("application credential missing application_id".to_string())
            })?,
            external_tenant_id,
        }),
        ScopeType::User => Ok(CredentialScope::User {
            external_user_id: external_user_id.ok_or_else(|| {
                AppError::Internal("user credential missing external_user_id".to_string())
            })?,
            application_id,
            external_tenant_id,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rag_ingestion_status_round_trips_all_eight_variants() {
        // These strings must match the DB CHECK constraint on
        // rag_document_versions.ingestion_status exactly (verified against
        // migrations/0007_conversations_memory_rag.sql:382-383):
        // 'pending','downloading','parsing','chunking','embedding','indexed',
        // 'failed','superseded'.
        let cases = [
            (RagIngestionStatus::Pending, "pending"),
            (RagIngestionStatus::Downloading, "downloading"),
            (RagIngestionStatus::Parsing, "parsing"),
            (RagIngestionStatus::Chunking, "chunking"),
            (RagIngestionStatus::Embedding, "embedding"),
            (RagIngestionStatus::Indexed, "indexed"),
            (RagIngestionStatus::Failed, "failed"),
            (RagIngestionStatus::Superseded, "superseded"),
        ];

        for (variant, db_value) in cases {
            assert_eq!(
                rag_ingestion_status_to_db(variant),
                db_value,
                "unexpected DB literal for {variant:?}"
            );
            assert_eq!(
                rag_ingestion_status_from_db(db_value.to_string()).unwrap(),
                variant,
                "unexpected variant parsed from {db_value:?}"
            );
        }
    }

    #[test]
    fn rag_ingestion_status_from_db_rejects_unknown_value() {
        let result = rag_ingestion_status_from_db("indexing-in-progress".to_string());
        assert!(
            result.is_err(),
            "an out-of-vocabulary DB string must error, not silently default"
        );
    }
}
