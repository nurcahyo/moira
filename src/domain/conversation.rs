use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::i18n::ResponseText;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStatus {
    Active,
    Archived,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationMessageRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationMessageType {
    Input,
    Output,
    Summary,
    ToolCall,
    ToolResult,
    Marker,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationContentPersistence {
    None,
    MetadataOnly,
    PlainContent,
    EncryptedContent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryStrategy {
    RecentMessages,
    SummaryPlusRecent,
    FullUntilLimit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConsentMode {
    Disabled,
    ExplicitOnly,
    ApplicationManaged,
    AutomaticWithUserControls,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Preference,
    Fact,
    Goal,
    Constraint,
    Relationship,
    ProjectContext,
    Decision,
    Instruction,
    TemporaryState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Conversation,
    UserApplication,
    TenantApplication,
    Application,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemorySensitivity {
    Normal,
    Personal,
    Restricted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Candidate,
    Active,
    Rejected,
    Superseded,
    Expired,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RagCollectionStatus {
    Active,
    Disabled,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RagCollectionVisibility {
    Application,
    Tenant,
    Restricted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RagDocumentStatus {
    Active,
    Disabled,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RagIngestionStatus {
    Pending,
    Downloading,
    Parsing,
    Chunking,
    Embedding,
    Indexed,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConversationRecord {
    pub id: String,
    pub object: String,
    pub application_id: Uuid,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub title: Option<String>,
    pub status: ConversationStatus,
    pub message_count: i64,
    pub last_message_at: Option<DateTime<Utc>>,
    pub summary_available: bool,
    pub memory_behavior: String,
    pub retention_expires_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationCreateRequest {
    pub title: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationPatchRequest {
    pub title: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ConversationQuery {
    pub status: Option<ConversationStatus>,
    pub created_before: Option<DateTime<Utc>>,
    pub created_after: Option<DateTime<Utc>>,
    pub updated_before: Option<DateTime<Utc>>,
    pub updated_after: Option<DateTime<Utc>>,
    pub search: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl ConversationQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ConversationMessageQuery {
    pub before: Option<String>,
    pub after: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl ConversationMessageQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConversationMessageRecord {
    pub id: String,
    pub object: String,
    pub conversation_id: String,
    pub response_id: Option<String>,
    pub execution_id: Option<String>,
    pub role: ConversationMessageRole,
    pub message_type: ConversationMessageType,
    pub sequence_number: i64,
    pub content: Option<String>,
    /// Keyed integrity fingerprint of the message content, **not a content address**.
    ///
    /// Formatted `"{pepper_version}:{base64url}"`. The digest is an HMAC under a
    /// deployment-held pepper, so identical content hashes differently once that pepper is
    /// rotated: **an operator rotating the idempotency pepper changes this value for a message
    /// whose content never changed.** The version prefix is the signal that the value is scoped
    /// to a pepper. Do not cache, diff, or deduplicate on it across time — use the message `id`.
    pub content_hash: String,
    pub content_size_bytes: i64,
    pub token_count: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationMessageCreateRequest {
    pub role: ConversationMessageRole,
    pub content: String,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

/// One immutable conversation summary version — plan 11 Sub-Phase E.
///
/// `summary_text` is optional and carries the body only when the application's
/// `conversation_content_persistence` admits plaintext. A row with `summary_text: null` is not an
/// error: it records that a summary exists, which version it is and how far it covers, without a
/// body Moira was not permitted to store. Callers must treat "a summary exists" and "the summary
/// text is available" as two separate facts, exactly as `ConversationMessageRecord.content`
/// already requires.
///
/// There is deliberately **no `summary_hash` field**. The hash is a content address over the
/// summary bytes, and publishing it on a caller-visible response would make it an offline oracle
/// over candidate summary plaintexts — the first clause of finding F14's admitting rule, which is
/// exactly why `conversation_messages.content_hash` stayed peppered while
/// `memory_records.content_hash` did not.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConversationSummaryRecord {
    pub id: String,
    pub object: String,
    pub conversation_id: String,
    pub summary_version: i64,
    /// The `sequence_number` of the last message this summary covers.
    pub covers_through_sequence: i64,
    pub summary_text: Option<String>,
    pub token_count: Option<i64>,
    pub created_at: DateTime<Utc>,
    /// Always `null` on a freshly created summary; present on a version a later run replaced.
    pub superseded_at: Option<DateTime<Utc>>,
}

/// Body of `POST /api/v1/conversations/{id}/summarize`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationSummarizeRequest {
    /// Bypass `summary_trigger_tokens` and `minimum_messages_since_summary`.
    ///
    /// Does **not** bypass `summarization_enabled`, and does not make a summary possible when
    /// nothing has been said since the last one — see `decide_summarization` for why each of
    /// those two is not a threshold.
    #[serde(default)]
    pub force: bool,
}

/// Body of the `202` from `POST /api/v1/conversations/{id}/summarize`.
///
/// A summarization for this conversation is already running, so this request did not start a
/// second one. The notice carries the catalog key rather than an English literal, per
/// CONVENTIONS.md §4.2.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConversationSummarizeAccepted {
    pub notice: ResponseText,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseConversationInput {
    pub id: Option<String>,
    #[serde(default)]
    pub create: bool,
    pub title: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicConversationRef {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicCitation {
    pub id: String,
    #[serde(rename = "type")]
    pub citation_type: String,
    pub document_id: Option<String>,
    pub memory_id: Option<String>,
    pub title: Option<String>,
    pub section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConversationPolicyRecord {
    pub id: Uuid,
    pub application_id: Uuid,
    pub conversations_enabled: bool,
    pub conversation_content_persistence: ConversationContentPersistence,
    pub default_retention_days: i32,
    pub maximum_retention_days: i32,
    pub history_strategy: HistoryStrategy,
    pub maximum_recent_messages: i32,
    pub maximum_history_tokens: i32,
    pub summarization_enabled: bool,
    pub summary_trigger_tokens: i32,
    pub summary_target_tokens: i32,
    pub minimum_messages_since_summary: i32,
    pub memory_enabled: bool,
    pub memory_extraction_enabled: bool,
    pub memory_retrieval_enabled: bool,
    pub memory_consent_mode: MemoryConsentMode,
    pub rag_enabled: bool,
    pub default_collection_ids: Vec<Uuid>,
    pub caller_can_create_conversations: bool,
    pub caller_can_delete_conversations: bool,
    pub caller_can_export_conversations: bool,
    pub protected_instruction_policy: String,
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationPolicyPutRequest {
    pub conversations_enabled: Option<bool>,
    pub conversation_content_persistence: Option<ConversationContentPersistence>,
    pub default_retention_days: Option<i32>,
    pub maximum_retention_days: Option<i32>,
    pub history_strategy: Option<HistoryStrategy>,
    pub maximum_recent_messages: Option<i32>,
    pub maximum_history_tokens: Option<i32>,
    pub summarization_enabled: Option<bool>,
    pub summary_trigger_tokens: Option<i32>,
    pub summary_target_tokens: Option<i32>,
    pub minimum_messages_since_summary: Option<i32>,
    pub memory_enabled: Option<bool>,
    pub memory_extraction_enabled: Option<bool>,
    pub memory_retrieval_enabled: Option<bool>,
    pub memory_consent_mode: Option<MemoryConsentMode>,
    pub rag_enabled: Option<bool>,
    pub default_collection_ids: Option<Vec<Uuid>>,
    pub caller_can_create_conversations: Option<bool>,
    pub caller_can_delete_conversations: Option<bool>,
    pub caller_can_export_conversations: Option<bool>,
    pub protected_instruction_policy: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MemoryPolicyRecord {
    pub id: Uuid,
    pub application_id: Uuid,
    pub enabled: bool,
    pub consent_mode: MemoryConsentMode,
    pub allowed_memory_types: Vec<MemoryType>,
    pub allowed_sensitivity_levels: Vec<MemorySensitivity>,
    pub automatic_extraction_enabled: bool,
    pub automatic_retrieval_enabled: bool,
    pub manual_memory_enabled: bool,
    pub minimum_extraction_confidence: f64,
    pub minimum_retrieval_confidence: f64,
    pub maximum_memory_count_per_user: i32,
    pub maximum_memory_tokens_per_request: i32,
    pub default_ttl_days: Option<i32>,
    pub maximum_ttl_days: Option<i32>,
    pub user_can_list: bool,
    pub user_can_edit: bool,
    pub user_can_delete: bool,
    pub user_can_disable: bool,
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryPolicyPutRequest {
    pub enabled: Option<bool>,
    pub consent_mode: Option<MemoryConsentMode>,
    pub allowed_memory_types: Option<Vec<MemoryType>>,
    pub allowed_sensitivity_levels: Option<Vec<MemorySensitivity>>,
    pub automatic_extraction_enabled: Option<bool>,
    pub automatic_retrieval_enabled: Option<bool>,
    pub manual_memory_enabled: Option<bool>,
    pub minimum_extraction_confidence: Option<f64>,
    pub minimum_retrieval_confidence: Option<f64>,
    pub maximum_memory_count_per_user: Option<i32>,
    pub maximum_memory_tokens_per_request: Option<i32>,
    pub default_ttl_days: Option<i32>,
    pub maximum_ttl_days: Option<i32>,
    pub user_can_list: Option<bool>,
    pub user_can_edit: Option<bool>,
    pub user_can_delete: Option<bool>,
    pub user_can_disable: Option<bool>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetrievalPolicyRecord {
    pub id: Uuid,
    pub application_id: Uuid,
    pub enabled: bool,
    pub memory_retrieval_enabled: bool,
    pub rag_retrieval_enabled: bool,
    pub allowed_collection_ids: Vec<Uuid>,
    pub default_collection_ids: Vec<Uuid>,
    pub maximum_memory_results: i32,
    pub maximum_chunk_results: i32,
    pub maximum_memory_tokens: i32,
    pub maximum_rag_tokens: i32,
    pub semantic_weight: f64,
    pub keyword_weight: f64,
    pub recency_weight: f64,
    pub importance_weight: f64,
    pub minimum_memory_score: f64,
    pub minimum_chunk_score: f64,
    pub maximum_chunks_per_document: i32,
    pub diversity_enabled: bool,
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrievalPolicyPutRequest {
    pub enabled: Option<bool>,
    pub memory_retrieval_enabled: Option<bool>,
    pub rag_retrieval_enabled: Option<bool>,
    pub allowed_collection_ids: Option<Vec<Uuid>>,
    pub default_collection_ids: Option<Vec<Uuid>>,
    pub maximum_memory_results: Option<i32>,
    pub maximum_chunk_results: Option<i32>,
    pub maximum_memory_tokens: Option<i32>,
    pub maximum_rag_tokens: Option<i32>,
    pub semantic_weight: Option<f64>,
    pub keyword_weight: Option<f64>,
    pub recency_weight: Option<f64>,
    pub importance_weight: Option<f64>,
    pub minimum_memory_score: Option<f64>,
    pub minimum_chunk_score: Option<f64>,
    pub maximum_chunks_per_document: Option<i32>,
    pub diversity_enabled: Option<bool>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddingPolicyRecord {
    pub id: Uuid,
    pub application_id: Uuid,
    pub embedding_provider_id: Option<Uuid>,
    pub embedding_model_id: Option<Uuid>,
    pub embedding_dimension: Option<i32>,
    pub batch_size: i32,
    pub maximum_input_tokens: i32,
    pub timeout_ms: i32,
    pub memory_embeddings_enabled: bool,
    pub rag_embeddings_enabled: bool,
    pub failure_behavior: String,
    pub metadata: Value,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingPolicyPutRequest {
    pub embedding_provider_id: Option<Uuid>,
    pub embedding_model_id: Option<Uuid>,
    pub embedding_dimension: Option<i32>,
    pub batch_size: Option<i32>,
    pub maximum_input_tokens: Option<i32>,
    pub timeout_ms: Option<i32>,
    pub memory_embeddings_enabled: Option<bool>,
    pub rag_embeddings_enabled: Option<bool>,
    pub failure_behavior: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MemoryRecord {
    pub id: String,
    pub object: String,
    pub memory_type: MemoryType,
    pub scope: MemoryScope,
    pub content: Option<String>,
    pub importance: f64,
    pub confidence: f64,
    pub sensitivity: MemorySensitivity,
    pub status: MemoryStatus,
    pub conversation_id: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub last_confirmed_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub use_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryCreateRequest {
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub content: String,
    pub importance: Option<f64>,
    pub confidence: Option<f64>,
    pub valid_until: Option<DateTime<Utc>>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryPatchRequest {
    pub content: Option<String>,
    pub importance: Option<f64>,
    pub valid_until: Option<DateTime<Utc>>,
    pub status: Option<MemoryStatus>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct MemoryQuery {
    pub memory_type: Option<MemoryType>,
    pub status: Option<MemoryStatus>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl MemoryQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RagCollectionRecord {
    pub id: String,
    pub object: String,
    pub application_id: Uuid,
    pub external_tenant_id: Option<String>,
    pub collection_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub status: RagCollectionStatus,
    pub visibility: RagCollectionVisibility,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RagCollectionCreateRequest {
    pub application_id: Uuid,
    pub external_tenant_id: Option<String>,
    pub collection_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub visibility: RagCollectionVisibility,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RagCollectionPatchRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<RagCollectionVisibility>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct RagCollectionQuery {
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub status: Option<RagCollectionStatus>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl RagCollectionQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RagDocumentRecord {
    pub id: String,
    pub object: String,
    pub collection_id: String,
    pub external_document_id: Option<String>,
    pub title: String,
    pub source_type: String,
    pub source_uri: Option<String>,
    pub mime_type: String,
    pub status: RagDocumentStatus,
    pub current_version_id: Option<Uuid>,
    pub ingestion_status: Option<RagIngestionStatus>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RagDocumentCreateRequest {
    pub external_document_id: Option<String>,
    pub title: String,
    pub source_type: String,
    pub source_uri: Option<String>,
    pub mime_type: String,
    pub content: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RagDocumentIngestRequest {
    pub content: Option<String>,
    pub source_etag: Option<String>,
    pub source_last_modified: Option<DateTime<Utc>>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

pub fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rag_document_record(
        ingestion_status: Option<RagIngestionStatus>,
    ) -> RagDocumentRecord {
        RagDocumentRecord {
            id: "doc_123".to_string(),
            object: "rag.document".to_string(),
            collection_id: "col_123".to_string(),
            external_document_id: Some("ext-1".to_string()),
            title: "Title".to_string(),
            source_type: "text".to_string(),
            source_uri: None,
            mime_type: "text/plain".to_string(),
            status: RagDocumentStatus::Active,
            current_version_id: Some(Uuid::nil()),
            ingestion_status,
            metadata: empty_object(),
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            updated_at: DateTime::<Utc>::UNIX_EPOCH,
            deleted_at: None,
            version: 1,
        }
    }

    #[test]
    fn rag_document_record_serializes_ingestion_status_as_snake_case() {
        let record = sample_rag_document_record(Some(RagIngestionStatus::Pending));
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["ingestion_status"], "pending");

        let record = sample_rag_document_record(None);
        let value = serde_json::to_value(&record).unwrap();
        // The key must be present as an explicit JSON null, not omitted: a
        // `skip_serializing_if` regression on this field must fail this test.
        assert!(
            value.as_object().unwrap().contains_key("ingestion_status"),
            "ingestion_status key must be present even when None"
        );
        assert_eq!(value["ingestion_status"], Value::Null);
    }

    #[test]
    fn rag_document_record_round_trips_through_serde() {
        for ingestion_status in [
            None,
            Some(RagIngestionStatus::Pending),
            Some(RagIngestionStatus::Downloading),
            Some(RagIngestionStatus::Parsing),
            Some(RagIngestionStatus::Chunking),
            Some(RagIngestionStatus::Embedding),
            Some(RagIngestionStatus::Indexed),
            Some(RagIngestionStatus::Failed),
            Some(RagIngestionStatus::Superseded),
        ] {
            let record = sample_rag_document_record(ingestion_status);
            let json = serde_json::to_string(&record).unwrap();
            let round_tripped: RagDocumentRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(round_tripped.ingestion_status, ingestion_status);
            assert_eq!(round_tripped.id, record.id);
            assert_eq!(round_tripped.collection_id, record.collection_id);
            assert_eq!(round_tripped.current_version_id, record.current_version_id);
            assert_eq!(round_tripped.status, record.status);
            assert_eq!(round_tripped.version, record.version);
        }
    }
}
