use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        ConversationCreateRequest, ConversationMessageQuery, ConversationMessageRecord,
        ConversationMessageRole, ConversationMessageType, ConversationPatchRequest,
        ConversationPolicyPutRequest, ConversationPolicyRecord, ConversationQuery,
        ConversationRecord, ConversationStatus, EmbeddingPolicyPutRequest, EmbeddingPolicyRecord,
        MemoryCreateRequest, MemoryPatchRequest, MemoryPolicyPutRequest, MemoryPolicyRecord,
        MemoryQuery, MemoryRecord, MemoryScope, RagCollectionCreateRequest,
        RagCollectionPatchRequest, RagCollectionQuery, RagCollectionRecord, RagCollectionStatus,
        RagDocumentCreateRequest, RagDocumentIngestRequest, RagDocumentRecord,
        RetrievalPolicyPutRequest, RetrievalPolicyRecord,
    },
    error::AppError,
    infra::pg_rows::{
        conversation_content_persistence_to_db, conversation_message_record_from_row,
        conversation_message_role_to_db, conversation_message_type_to_db,
        conversation_policy_record_from_row, conversation_record_from_row,
        conversation_status_to_db, embedding_policy_record_from_row, history_strategy_to_db,
        memory_consent_mode_to_db, memory_policy_record_from_row, memory_record_from_row,
        memory_scope_to_db, memory_sensitivity_to_db, memory_status_to_db, memory_type_to_db,
        rag_collection_record_from_row, rag_collection_status_to_db,
        rag_collection_visibility_to_db, rag_document_record_from_row,
        retrieval_policy_record_from_row,
    },
};

#[derive(Debug, Clone)]
pub struct PgConversationRepository {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct ConversationAccess {
    pub privileged: bool,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConversationMessageInsert {
    pub conversation_public_id: String,
    pub response_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub role: ConversationMessageRole,
    pub message_type: ConversationMessageType,
    pub content_plain: Option<String>,
    pub content_hash: String,
    pub content_size_bytes: i64,
    pub token_count: Option<i64>,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct ConversationInsert<'a> {
    pub id: Uuid,
    pub public_id: &'a str,
    pub application_id: Uuid,
    pub external_tenant_id: Option<&'a str>,
    pub external_user_id: Option<&'a str>,
    pub request: &'a ConversationCreateRequest,
    pub retention_days: i32,
}

#[derive(Debug, Clone)]
pub struct MemoryInsert<'a> {
    pub id: Uuid,
    pub public_id: &'a str,
    pub application_id: Uuid,
    pub external_tenant_id: Option<&'a str>,
    pub external_user_id: Option<&'a str>,
    pub scope: MemoryScope,
    pub request: &'a MemoryCreateRequest,
    pub content_hash: &'a str,
}

impl PgConversationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_or_create_conversation_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ConversationPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_conversation_policies (application_id)
            values ($1)
            on conflict (application_id) do update set application_id = excluded.application_id
            returning *
            "#,
        )
        .bind(application_id)
        .fetch_one(&self.pool)
        .await?;
        conversation_policy_record_from_row(&row)
    }

    pub async fn put_conversation_policy(
        &self,
        application_id: Uuid,
        request: &ConversationPolicyPutRequest,
    ) -> Result<ConversationPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_conversation_policies (
                application_id, conversations_enabled, conversation_content_persistence,
                default_retention_days, maximum_retention_days, history_strategy,
                maximum_recent_messages, maximum_history_tokens, summarization_enabled,
                summary_trigger_tokens, summary_target_tokens, minimum_messages_since_summary,
                memory_enabled, memory_extraction_enabled, memory_retrieval_enabled,
                memory_consent_mode, rag_enabled, default_collection_ids,
                caller_can_create_conversations, caller_can_delete_conversations,
                caller_can_export_conversations, protected_instruction_policy, metadata
            )
            values (
                $1, coalesce($2, true), coalesce($3, 'plain_content'),
                coalesce($4, 90), coalesce($5, 365), coalesce($6, 'summary_plus_recent'),
                coalesce($7, 24), coalesce($8, 12000), coalesce($9, false),
                coalesce($10, 8000), coalesce($11, 1000), coalesce($12, 8),
                coalesce($13, false), coalesce($14, false), coalesce($15, false),
                coalesce($16, 'explicit_only'), coalesce($17, false), coalesce($18, array[]::uuid[]),
                coalesce($19, true), coalesce($20, true),
                coalesce($21, false), coalesce($22, 'exclude_from_exports'), coalesce($23, '{}'::jsonb)
            )
            on conflict (application_id) do update set
                conversations_enabled = coalesce($2, application_conversation_policies.conversations_enabled),
                conversation_content_persistence = coalesce($3, application_conversation_policies.conversation_content_persistence),
                default_retention_days = coalesce($4, application_conversation_policies.default_retention_days),
                maximum_retention_days = coalesce($5, application_conversation_policies.maximum_retention_days),
                history_strategy = coalesce($6, application_conversation_policies.history_strategy),
                maximum_recent_messages = coalesce($7, application_conversation_policies.maximum_recent_messages),
                maximum_history_tokens = coalesce($8, application_conversation_policies.maximum_history_tokens),
                summarization_enabled = coalesce($9, application_conversation_policies.summarization_enabled),
                summary_trigger_tokens = coalesce($10, application_conversation_policies.summary_trigger_tokens),
                summary_target_tokens = coalesce($11, application_conversation_policies.summary_target_tokens),
                minimum_messages_since_summary = coalesce($12, application_conversation_policies.minimum_messages_since_summary),
                memory_enabled = coalesce($13, application_conversation_policies.memory_enabled),
                memory_extraction_enabled = coalesce($14, application_conversation_policies.memory_extraction_enabled),
                memory_retrieval_enabled = coalesce($15, application_conversation_policies.memory_retrieval_enabled),
                memory_consent_mode = coalesce($16, application_conversation_policies.memory_consent_mode),
                rag_enabled = coalesce($17, application_conversation_policies.rag_enabled),
                default_collection_ids = coalesce($18, application_conversation_policies.default_collection_ids),
                caller_can_create_conversations = coalesce($19, application_conversation_policies.caller_can_create_conversations),
                caller_can_delete_conversations = coalesce($20, application_conversation_policies.caller_can_delete_conversations),
                caller_can_export_conversations = coalesce($21, application_conversation_policies.caller_can_export_conversations),
                protected_instruction_policy = coalesce($22, application_conversation_policies.protected_instruction_policy),
                metadata = coalesce($23, application_conversation_policies.metadata),
                updated_at = now()
            returning *
            "#,
        )
        .bind(application_id)
        .bind(request.conversations_enabled)
        .bind(
            request
                .conversation_content_persistence
                .map(conversation_content_persistence_to_db),
        )
        .bind(request.default_retention_days)
        .bind(request.maximum_retention_days)
        .bind(request.history_strategy.map(history_strategy_to_db))
        .bind(request.maximum_recent_messages)
        .bind(request.maximum_history_tokens)
        .bind(request.summarization_enabled)
        .bind(request.summary_trigger_tokens)
        .bind(request.summary_target_tokens)
        .bind(request.minimum_messages_since_summary)
        .bind(request.memory_enabled)
        .bind(request.memory_extraction_enabled)
        .bind(request.memory_retrieval_enabled)
        .bind(request.memory_consent_mode.map(memory_consent_mode_to_db))
        .bind(request.rag_enabled)
        .bind(&request.default_collection_ids)
        .bind(request.caller_can_create_conversations)
        .bind(request.caller_can_delete_conversations)
        .bind(request.caller_can_export_conversations)
        .bind(&request.protected_instruction_policy)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        conversation_policy_record_from_row(&row)
    }

    pub async fn get_or_create_memory_policy(
        &self,
        application_id: Uuid,
    ) -> Result<MemoryPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_memory_policies (application_id)
            values ($1)
            on conflict (application_id) do update set application_id = excluded.application_id
            returning *
            "#,
        )
        .bind(application_id)
        .fetch_one(&self.pool)
        .await?;
        memory_policy_record_from_row(&row)
    }

    pub async fn put_memory_policy(
        &self,
        application_id: Uuid,
        request: &MemoryPolicyPutRequest,
    ) -> Result<MemoryPolicyRecord, AppError> {
        let allowed_types = request.allowed_memory_types.as_ref().map(|items| {
            items
                .iter()
                .copied()
                .map(memory_type_to_db)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        let allowed_sensitivity = request.allowed_sensitivity_levels.as_ref().map(|items| {
            items
                .iter()
                .copied()
                .map(memory_sensitivity_to_db)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        let row = sqlx::query(
            r#"
            insert into application_memory_policies (
                application_id, enabled, consent_mode, allowed_memory_types,
                allowed_sensitivity_levels, automatic_extraction_enabled,
                automatic_retrieval_enabled, manual_memory_enabled,
                minimum_extraction_confidence, minimum_retrieval_confidence,
                maximum_memory_count_per_user, maximum_memory_tokens_per_request,
                default_ttl_days, maximum_ttl_days, user_can_list, user_can_edit,
                user_can_delete, user_can_disable, metadata
            )
            values (
                $1, coalesce($2, false), coalesce($3, 'explicit_only'),
                coalesce($4, array['preference','fact','goal','constraint','relationship','project_context','decision','instruction','temporary_state']::text[]),
                coalesce($5, array['normal']::text[]), coalesce($6, false),
                coalesce($7, false), coalesce($8, false),
                coalesce($9, 0.8), coalesce($10, 0.6),
                coalesce($11, 1000), coalesce($12, 1500),
                $13, $14, coalesce($15, true), coalesce($16, true),
                coalesce($17, true), coalesce($18, true), coalesce($19, '{}'::jsonb)
            )
            on conflict (application_id) do update set
                enabled = coalesce($2, application_memory_policies.enabled),
                consent_mode = coalesce($3, application_memory_policies.consent_mode),
                allowed_memory_types = coalesce($4, application_memory_policies.allowed_memory_types),
                allowed_sensitivity_levels = coalesce($5, application_memory_policies.allowed_sensitivity_levels),
                automatic_extraction_enabled = coalesce($6, application_memory_policies.automatic_extraction_enabled),
                automatic_retrieval_enabled = coalesce($7, application_memory_policies.automatic_retrieval_enabled),
                manual_memory_enabled = coalesce($8, application_memory_policies.manual_memory_enabled),
                minimum_extraction_confidence = coalesce($9, application_memory_policies.minimum_extraction_confidence),
                minimum_retrieval_confidence = coalesce($10, application_memory_policies.minimum_retrieval_confidence),
                maximum_memory_count_per_user = coalesce($11, application_memory_policies.maximum_memory_count_per_user),
                maximum_memory_tokens_per_request = coalesce($12, application_memory_policies.maximum_memory_tokens_per_request),
                default_ttl_days = coalesce($13, application_memory_policies.default_ttl_days),
                maximum_ttl_days = coalesce($14, application_memory_policies.maximum_ttl_days),
                user_can_list = coalesce($15, application_memory_policies.user_can_list),
                user_can_edit = coalesce($16, application_memory_policies.user_can_edit),
                user_can_delete = coalesce($17, application_memory_policies.user_can_delete),
                user_can_disable = coalesce($18, application_memory_policies.user_can_disable),
                metadata = coalesce($19, application_memory_policies.metadata),
                updated_at = now()
            returning *
            "#,
        )
        .bind(application_id)
        .bind(request.enabled)
        .bind(request.consent_mode.map(memory_consent_mode_to_db))
        .bind(allowed_types)
        .bind(allowed_sensitivity)
        .bind(request.automatic_extraction_enabled)
        .bind(request.automatic_retrieval_enabled)
        .bind(request.manual_memory_enabled)
        .bind(request.minimum_extraction_confidence)
        .bind(request.minimum_retrieval_confidence)
        .bind(request.maximum_memory_count_per_user)
        .bind(request.maximum_memory_tokens_per_request)
        .bind(request.default_ttl_days)
        .bind(request.maximum_ttl_days)
        .bind(request.user_can_list)
        .bind(request.user_can_edit)
        .bind(request.user_can_delete)
        .bind(request.user_can_disable)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        memory_policy_record_from_row(&row)
    }

    pub async fn get_or_create_retrieval_policy(
        &self,
        application_id: Uuid,
    ) -> Result<RetrievalPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_retrieval_policies (application_id)
            values ($1)
            on conflict (application_id) do update set application_id = excluded.application_id
            returning *
            "#,
        )
        .bind(application_id)
        .fetch_one(&self.pool)
        .await?;
        retrieval_policy_record_from_row(&row)
    }

    pub async fn put_retrieval_policy(
        &self,
        application_id: Uuid,
        request: &RetrievalPolicyPutRequest,
    ) -> Result<RetrievalPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_retrieval_policies (
                application_id, enabled, memory_retrieval_enabled, rag_retrieval_enabled,
                allowed_collection_ids, default_collection_ids, maximum_memory_results,
                maximum_chunk_results, maximum_memory_tokens, maximum_rag_tokens,
                semantic_weight, keyword_weight, recency_weight, importance_weight,
                minimum_memory_score, minimum_chunk_score, maximum_chunks_per_document,
                diversity_enabled, metadata
            )
            values (
                $1, coalesce($2, false), coalesce($3, false), coalesce($4, false),
                coalesce($5, array[]::uuid[]), coalesce($6, array[]::uuid[]), coalesce($7, 10),
                coalesce($8, 8), coalesce($9, 1500), coalesce($10, 4000),
                coalesce($11, 0.7), coalesce($12, 0.2), coalesce($13, 0.05), coalesce($14, 0.05),
                coalesce($15, 0.5), coalesce($16, 0.5), coalesce($17, 3),
                coalesce($18, true), coalesce($19, '{}'::jsonb)
            )
            on conflict (application_id) do update set
                enabled = coalesce($2, application_retrieval_policies.enabled),
                memory_retrieval_enabled = coalesce($3, application_retrieval_policies.memory_retrieval_enabled),
                rag_retrieval_enabled = coalesce($4, application_retrieval_policies.rag_retrieval_enabled),
                allowed_collection_ids = coalesce($5, application_retrieval_policies.allowed_collection_ids),
                default_collection_ids = coalesce($6, application_retrieval_policies.default_collection_ids),
                maximum_memory_results = coalesce($7, application_retrieval_policies.maximum_memory_results),
                maximum_chunk_results = coalesce($8, application_retrieval_policies.maximum_chunk_results),
                maximum_memory_tokens = coalesce($9, application_retrieval_policies.maximum_memory_tokens),
                maximum_rag_tokens = coalesce($10, application_retrieval_policies.maximum_rag_tokens),
                semantic_weight = coalesce($11, application_retrieval_policies.semantic_weight),
                keyword_weight = coalesce($12, application_retrieval_policies.keyword_weight),
                recency_weight = coalesce($13, application_retrieval_policies.recency_weight),
                importance_weight = coalesce($14, application_retrieval_policies.importance_weight),
                minimum_memory_score = coalesce($15, application_retrieval_policies.minimum_memory_score),
                minimum_chunk_score = coalesce($16, application_retrieval_policies.minimum_chunk_score),
                maximum_chunks_per_document = coalesce($17, application_retrieval_policies.maximum_chunks_per_document),
                diversity_enabled = coalesce($18, application_retrieval_policies.diversity_enabled),
                metadata = coalesce($19, application_retrieval_policies.metadata),
                updated_at = now()
            returning *
            "#,
        )
        .bind(application_id)
        .bind(request.enabled)
        .bind(request.memory_retrieval_enabled)
        .bind(request.rag_retrieval_enabled)
        .bind(&request.allowed_collection_ids)
        .bind(&request.default_collection_ids)
        .bind(request.maximum_memory_results)
        .bind(request.maximum_chunk_results)
        .bind(request.maximum_memory_tokens)
        .bind(request.maximum_rag_tokens)
        .bind(request.semantic_weight)
        .bind(request.keyword_weight)
        .bind(request.recency_weight)
        .bind(request.importance_weight)
        .bind(request.minimum_memory_score)
        .bind(request.minimum_chunk_score)
        .bind(request.maximum_chunks_per_document)
        .bind(request.diversity_enabled)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        retrieval_policy_record_from_row(&row)
    }

    pub async fn get_or_create_embedding_policy(
        &self,
        application_id: Uuid,
    ) -> Result<EmbeddingPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_embedding_policies (application_id)
            values ($1)
            on conflict (application_id) do update set application_id = excluded.application_id
            returning *
            "#,
        )
        .bind(application_id)
        .fetch_one(&self.pool)
        .await?;
        embedding_policy_record_from_row(&row)
    }

    pub async fn put_embedding_policy(
        &self,
        application_id: Uuid,
        request: &EmbeddingPolicyPutRequest,
    ) -> Result<EmbeddingPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into application_embedding_policies (
                application_id, embedding_provider_id, embedding_model_id, embedding_dimension,
                batch_size, maximum_input_tokens, timeout_ms, memory_embeddings_enabled,
                rag_embeddings_enabled, failure_behavior, metadata
            )
            values (
                $1, $2, $3, $4, coalesce($5, 32), coalesce($6, 8192), coalesce($7, 60000),
                coalesce($8, false), coalesce($9, false),
                coalesce($10, 'continue_without_semantic_retrieval'), coalesce($11, '{}'::jsonb)
            )
            on conflict (application_id) do update set
                embedding_provider_id = coalesce($2, application_embedding_policies.embedding_provider_id),
                embedding_model_id = coalesce($3, application_embedding_policies.embedding_model_id),
                embedding_dimension = coalesce($4, application_embedding_policies.embedding_dimension),
                batch_size = coalesce($5, application_embedding_policies.batch_size),
                maximum_input_tokens = coalesce($6, application_embedding_policies.maximum_input_tokens),
                timeout_ms = coalesce($7, application_embedding_policies.timeout_ms),
                memory_embeddings_enabled = coalesce($8, application_embedding_policies.memory_embeddings_enabled),
                rag_embeddings_enabled = coalesce($9, application_embedding_policies.rag_embeddings_enabled),
                failure_behavior = coalesce($10, application_embedding_policies.failure_behavior),
                metadata = coalesce($11, application_embedding_policies.metadata),
                updated_at = now()
            returning *
            "#,
        )
        .bind(application_id)
        .bind(request.embedding_provider_id)
        .bind(request.embedding_model_id)
        .bind(request.embedding_dimension)
        .bind(request.batch_size)
        .bind(request.maximum_input_tokens)
        .bind(request.timeout_ms)
        .bind(request.memory_embeddings_enabled)
        .bind(request.rag_embeddings_enabled)
        .bind(&request.failure_behavior)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        embedding_policy_record_from_row(&row)
    }

    pub async fn create_conversation(
        &self,
        insert: &ConversationInsert<'_>,
    ) -> Result<ConversationRecord, AppError> {
        let retention_expires_at = if insert.retention_days == 0 {
            None
        } else {
            Some(Utc::now() + Duration::days(insert.retention_days as i64))
        };
        let row = sqlx::query(&conversation_select(
            r#"
            insert into conversations (
                id, public_id, application_id, external_tenant_id, external_user_id,
                title, metadata, retention_expires_at
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8)
            returning *
            "#,
        ))
        .bind(insert.id)
        .bind(insert.public_id)
        .bind(insert.application_id)
        .bind(insert.external_tenant_id)
        .bind(insert.external_user_id)
        .bind(&insert.request.title)
        .bind(&insert.request.metadata)
        .bind(retention_expires_at)
        .fetch_one(&self.pool)
        .await?;
        conversation_record_from_row(&row)
    }

    pub async fn find_conversation_authorized(
        &self,
        public_id: &str,
        access: &ConversationAccess,
    ) -> Result<ConversationRecord, AppError> {
        let row = sqlx::query(&conversation_select(
            r#"
            select c.*
            from conversations c
            where c.public_id = $1
              and c.deleted_at is null
              and ($2::boolean
                   or (c.application_id = $3
                       and ($4::text is null or c.external_tenant_id = $4)
                       and ($5::text is null or c.external_user_id = $5)))
            "#,
        ))
        .bind(public_id)
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "conversation_not_found",
                "conversation not found",
            )
        })?;
        conversation_record_from_row(&row)
    }

    pub async fn list_conversations_authorized(
        &self,
        access: &ConversationAccess,
        query: &ConversationQuery,
    ) -> Result<Vec<ConversationRecord>, AppError> {
        let rows = sqlx::query(&conversation_select(
            r#"
            select c.*
            from conversations c
            where c.deleted_at is null
              and ($1::boolean
                   or (c.application_id = $2
                       and ($3::text is null or c.external_tenant_id = $3)
                       and ($4::text is null or c.external_user_id = $4)))
              and ($5::text is null or c.status = $5)
              and ($6::timestamptz is null or c.created_at < $6)
              and ($7::timestamptz is null or c.created_at >= $7)
              and ($8::timestamptz is null or c.updated_at < $8)
              and ($9::timestamptz is null or c.updated_at >= $9)
              and ($10::text is null or c.title ilike '%' || $10 || '%')
            order by c.updated_at desc, c.id desc
            limit $11
            "#,
        ))
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .bind(query.status.map(conversation_status_to_db))
        .bind(query.created_before)
        .bind(query.created_after)
        .bind(query.updated_before)
        .bind(query.updated_after)
        .bind(&query.search)
        .bind(query.limit())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(conversation_record_from_row).collect()
    }

    pub async fn patch_conversation(
        &self,
        public_id: &str,
        request: &ConversationPatchRequest,
    ) -> Result<ConversationRecord, AppError> {
        let row = sqlx::query(&conversation_select(
            r#"
            update conversations
            set title = coalesce($2, title),
                metadata = coalesce($3, metadata)
            where public_id = $1 and deleted_at is null
            returning *
            "#,
        ))
        .bind(public_id)
        .bind(&request.title)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "conversation_not_found",
                "conversation not found",
            )
        })?;
        conversation_record_from_row(&row)
    }

    pub async fn set_conversation_status(
        &self,
        public_id: &str,
        status: ConversationStatus,
    ) -> Result<ConversationRecord, AppError> {
        let archived_at = if status == ConversationStatus::Archived {
            Some(Utc::now())
        } else {
            None
        };
        let deleted_at = if status == ConversationStatus::Deleted {
            Some(Utc::now())
        } else {
            None
        };
        let row = sqlx::query(&conversation_select(
            r#"
            update conversations
            set status = $2,
                archived_at = case when $2 = 'archived' then coalesce(archived_at, $3) else null end,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, $4) else null end
            where public_id = $1
            returning *
            "#,
        ))
        .bind(public_id)
        .bind(conversation_status_to_db(status))
        .bind(archived_at)
        .bind(deleted_at)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::coded(axum::http::StatusCode::NOT_FOUND, "conversation_not_found", "conversation not found"))?;
        conversation_record_from_row(&row)
    }

    pub async fn add_message(
        &self,
        insert: &ConversationMessageInsert,
    ) -> Result<ConversationMessageRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let conversation = sqlx::query(
            r#"
            select id
            from conversations
            where public_id = $1 and deleted_at is null
            for update
            "#,
        )
        .bind(&insert.conversation_public_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "conversation_not_found",
                "conversation not found",
            )
        })?;
        let conversation_id: Uuid = conversation.try_get("id")?;
        let sequence: i64 = sqlx::query_scalar(
            r#"
            select coalesce(max(sequence_number), 0) + 1
            from conversation_messages
            where conversation_id = $1
            "#,
        )
        .bind(conversation_id)
        .fetch_one(&mut *tx)
        .await?;
        let message_id = Uuid::now_v7();
        let public_id = format!("msg_{message_id}");
        let row = sqlx::query(&conversation_message_select(
            r#"
            insert into conversation_messages (
                id, public_id, conversation_id, response_id, execution_id, role,
                message_type, sequence_number, content_plain, content_hash,
                content_size_bytes, token_count, metadata
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            returning *
            "#,
        ))
        .bind(message_id)
        .bind(&public_id)
        .bind(conversation_id)
        .bind(insert.response_id)
        .bind(insert.execution_id)
        .bind(conversation_message_role_to_db(insert.role))
        .bind(conversation_message_type_to_db(insert.message_type))
        .bind(sequence)
        .bind(&insert.content_plain)
        .bind(&insert.content_hash)
        .bind(insert.content_size_bytes)
        .bind(insert.token_count)
        .bind(&insert.metadata)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            update conversations
            set message_count = message_count + 1,
                last_message_at = now()
            where id = $1
            "#,
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        conversation_message_record_from_row(&row)
    }

    pub async fn list_messages(
        &self,
        conversation_public_id: &str,
        query: &ConversationMessageQuery,
    ) -> Result<Vec<ConversationMessageRecord>, AppError> {
        let rows = sqlx::query(&conversation_message_select(
            r#"
            select m.*
            from conversation_messages m
            join conversations c on c.id = m.conversation_id
            where c.public_id = $1
              and m.deleted_at is null
              and ($2::bigint is null or m.sequence_number < $2)
              and ($3::bigint is null or m.sequence_number > $3)
            order by m.sequence_number asc
            limit $4
            "#,
        ))
        .bind(conversation_public_id)
        .bind(message_sequence(query.before.as_deref()))
        .bind(message_sequence(query.after.as_deref()))
        .bind(query.limit())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(conversation_message_record_from_row)
            .collect()
    }

    pub async fn create_memory(&self, insert: &MemoryInsert<'_>) -> Result<MemoryRecord, AppError> {
        let row = sqlx::query(&memory_select(
            r#"
            insert into memory_records (
                id, public_id, application_id, external_tenant_id, external_user_id,
                memory_scope, memory_type, content_plain, content_hash, importance,
                confidence, sensitivity, status, valid_until, metadata
            )
            values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                coalesce($10, 0.5), coalesce($11, 1.0), 'normal', 'active', $12, $13
            )
            returning *
            "#,
        ))
        .bind(insert.id)
        .bind(insert.public_id)
        .bind(insert.application_id)
        .bind(insert.external_tenant_id)
        .bind(insert.external_user_id)
        .bind(memory_scope_to_db(insert.scope))
        .bind(memory_type_to_db(insert.request.memory_type))
        .bind(&insert.request.content)
        .bind(insert.content_hash)
        .bind(insert.request.importance)
        .bind(insert.request.confidence)
        .bind(insert.request.valid_until)
        .bind(&insert.request.metadata)
        .fetch_one(&self.pool)
        .await?;
        memory_record_from_row(&row)
    }

    pub async fn find_memory_authorized(
        &self,
        public_id: &str,
        access: &ConversationAccess,
    ) -> Result<MemoryRecord, AppError> {
        let row = sqlx::query(&memory_select(
            r#"
            select m.*
            from memory_records m
            where m.public_id = $1
              and m.deleted_at is null
              and ($2::boolean
                   or (m.application_id = $3
                       and ($4::text is null or m.external_tenant_id = $4)
                       and ($5::text is null or m.external_user_id = $5)))
            "#,
        ))
        .bind(public_id)
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "memory_not_found",
                "memory not found",
            )
        })?;
        memory_record_from_row(&row)
    }

    pub async fn list_memories_authorized(
        &self,
        access: &ConversationAccess,
        query: &MemoryQuery,
    ) -> Result<Vec<MemoryRecord>, AppError> {
        let rows = sqlx::query(&memory_select(
            r#"
            select m.*
            from memory_records m
            where m.deleted_at is null
              and ($1::boolean
                   or (m.application_id = $2
                       and ($3::text is null or m.external_tenant_id = $3)
                       and ($4::text is null or m.external_user_id = $4)))
              and ($5::text is null or m.memory_type = $5)
              and ($6::text is null or m.status = $6)
            order by m.updated_at desc, m.id desc
            limit $7
            "#,
        ))
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .bind(query.memory_type.map(memory_type_to_db))
        .bind(query.status.map(memory_status_to_db))
        .bind(query.limit())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(memory_record_from_row).collect()
    }

    pub async fn patch_memory(
        &self,
        public_id: &str,
        request: &MemoryPatchRequest,
        content_hash: Option<&str>,
    ) -> Result<MemoryRecord, AppError> {
        let row = sqlx::query(&memory_select(
            r#"
            update memory_records
            set content_plain = coalesce($2, content_plain),
                content_hash = coalesce($3, content_hash),
                importance = coalesce($4, importance),
                valid_until = coalesce($5, valid_until),
                status = coalesce($6, status),
                metadata = coalesce($7, metadata)
            where public_id = $1 and deleted_at is null
            returning *
            "#,
        ))
        .bind(public_id)
        .bind(&request.content)
        .bind(content_hash)
        .bind(request.importance)
        .bind(request.valid_until)
        .bind(request.status.map(memory_status_to_db))
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "memory_not_found",
                "memory not found",
            )
        })?;
        memory_record_from_row(&row)
    }

    pub async fn delete_memory(&self, public_id: &str) -> Result<(), AppError> {
        sqlx::query(
            r#"
            update memory_records
            set status = 'deleted', deleted_at = coalesce(deleted_at, now())
            where public_id = $1 and deleted_at is null
            "#,
        )
        .bind(public_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_rag_collection(
        &self,
        id: Uuid,
        public_id: &str,
        request: &RagCollectionCreateRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into rag_collections (
                id, public_id, application_id, external_tenant_id, collection_key,
                display_name, description, visibility, metadata
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            returning *
            "#,
        )
        .bind(id)
        .bind(public_id)
        .bind(request.application_id)
        .bind(&request.external_tenant_id)
        .bind(&request.collection_key)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(rag_collection_visibility_to_db(request.visibility))
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        rag_collection_record_from_row(&row)
    }

    pub async fn list_rag_collections(
        &self,
        query: &RagCollectionQuery,
    ) -> Result<Vec<RagCollectionRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select *
            from rag_collections
            where deleted_at is null
              and ($1::uuid is null or application_id = $1)
              and ($2::text is null or external_tenant_id = $2)
              and ($3::text is null or status = $3)
            order by created_at desc, id desc
            limit $4
            "#,
        )
        .bind(query.application_id)
        .bind(&query.external_tenant_id)
        .bind(query.status.map(rag_collection_status_to_db))
        .bind(query.limit())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(rag_collection_record_from_row).collect()
    }

    pub async fn get_rag_collection(
        &self,
        public_id: &str,
    ) -> Result<RagCollectionRecord, AppError> {
        let row = sqlx::query(
            r#"
            select *
            from rag_collections
            where public_id = $1 and deleted_at is null
            "#,
        )
        .bind(public_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "rag_collection_not_found",
                "RAG collection not found",
            )
        })?;
        rag_collection_record_from_row(&row)
    }

    pub async fn patch_rag_collection(
        &self,
        public_id: &str,
        request: &RagCollectionPatchRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        let row = sqlx::query(
            r#"
            update rag_collections
            set display_name = coalesce($2, display_name),
                description = coalesce($3, description),
                visibility = coalesce($4, visibility),
                metadata = coalesce($5, metadata)
            where public_id = $1 and deleted_at is null
            returning *
            "#,
        )
        .bind(public_id)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(request.visibility.map(rag_collection_visibility_to_db))
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "rag_collection_not_found",
                "RAG collection not found",
            )
        })?;
        rag_collection_record_from_row(&row)
    }

    pub async fn set_rag_collection_status(
        &self,
        public_id: &str,
        status: RagCollectionStatus,
    ) -> Result<RagCollectionRecord, AppError> {
        let row = sqlx::query(
            r#"
            update rag_collections
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end
            where public_id = $1
            returning *
            "#,
        )
        .bind(public_id)
        .bind(rag_collection_status_to_db(status))
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::coded(axum::http::StatusCode::NOT_FOUND, "rag_collection_not_found", "RAG collection not found"))?;
        rag_collection_record_from_row(&row)
    }

    pub async fn create_rag_document(
        &self,
        id: Uuid,
        public_id: &str,
        collection_public_id: &str,
        request: &RagDocumentCreateRequest,
        content_hash: Option<&str>,
    ) -> Result<RagDocumentRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let collection_id: Uuid = sqlx::query_scalar(
            "select id from rag_collections where public_id = $1 and deleted_at is null",
        )
        .bind(collection_public_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "rag_collection_not_found",
                "RAG collection not found",
            )
        })?;
        let row = sqlx::query(&rag_document_select(
            r#"
            insert into rag_documents (
                id, public_id, collection_id, external_document_id, title,
                source_type, source_uri, mime_type, metadata
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            returning *
            "#,
        ))
        .bind(id)
        .bind(public_id)
        .bind(collection_id)
        .bind(&request.external_document_id)
        .bind(&request.title)
        .bind(&request.source_type)
        .bind(&request.source_uri)
        .bind(&request.mime_type)
        .bind(&request.metadata)
        .fetch_one(&mut *tx)
        .await?;
        if let (Some(content), Some(hash)) = (&request.content, content_hash) {
            let version_id = Uuid::now_v7();
            sqlx::query(
                r#"
                insert into rag_document_versions (
                    id, document_id, version_number, content_plain, content_hash,
                    content_size_bytes, ingestion_status, metadata
                )
                values ($1, $2, 1, $3, $4, $5, 'indexed', $6)
                "#,
            )
            .bind(version_id)
            .bind(id)
            .bind(content)
            .bind(hash)
            .bind(content.len() as i64)
            .bind(&request.metadata)
            .execute(&mut *tx)
            .await?;
            sqlx::query("update rag_documents set current_version_id = $2 where id = $1")
                .bind(id)
                .bind(version_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        rag_document_record_from_row(&row)
    }

    pub async fn list_rag_documents(
        &self,
        collection_public_id: &str,
        limit: i64,
    ) -> Result<Vec<RagDocumentRecord>, AppError> {
        let rows = sqlx::query(&rag_document_select(
            r#"
            select d.*
            from rag_documents d
            join rag_collections c on c.id = d.collection_id
            where c.public_id = $1 and d.deleted_at is null
            order by d.created_at desc, d.id desc
            limit $2
            "#,
        ))
        .bind(collection_public_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(rag_document_record_from_row).collect()
    }

    pub async fn get_rag_document(&self, public_id: &str) -> Result<RagDocumentRecord, AppError> {
        let row = sqlx::query(&rag_document_select(
            r#"
            select d.*
            from rag_documents d
            where d.public_id = $1 and d.deleted_at is null
            "#,
        ))
        .bind(public_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "rag_document_not_found",
                "RAG document not found",
            )
        })?;
        rag_document_record_from_row(&row)
    }

    pub async fn delete_rag_document(&self, public_id: &str) -> Result<(), AppError> {
        sqlx::query(
            "update rag_documents set status = 'deleted', deleted_at = coalesce(deleted_at, now()) where public_id = $1",
        )
        .bind(public_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn ingest_rag_document(
        &self,
        public_id: &str,
        request: &RagDocumentIngestRequest,
        content_hash: &str,
    ) -> Result<RagDocumentRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let document_id: Uuid = sqlx::query_scalar(
            "select id from rag_documents where public_id = $1 and deleted_at is null for update",
        )
        .bind(public_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "rag_document_not_found",
                "RAG document not found",
            )
        })?;
        let version_number: i32 = sqlx::query_scalar(
            "select coalesce(max(version_number), 0) + 1 from rag_document_versions where document_id = $1",
        )
        .bind(document_id)
        .fetch_one(&mut *tx)
        .await?;
        let version_id = Uuid::now_v7();
        let content = request.content.as_deref().unwrap_or("");
        sqlx::query(
            r#"
            update rag_document_versions
            set superseded_at = coalesce(superseded_at, now()),
                ingestion_status = case when ingestion_status = 'indexed' then 'superseded' else ingestion_status end
            where document_id = $1 and superseded_at is null
            "#,
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            insert into rag_document_versions (
                id, document_id, version_number, content_plain, content_hash,
                content_size_bytes, source_etag, source_last_modified,
                ingestion_status, metadata
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, 'indexed', $9)
            "#,
        )
        .bind(version_id)
        .bind(document_id)
        .bind(version_number)
        .bind(content)
        .bind(content_hash)
        .bind(content.len() as i64)
        .bind(&request.source_etag)
        .bind(request.source_last_modified)
        .bind(&request.metadata)
        .execute(&mut *tx)
        .await?;
        sqlx::query("update rag_documents set current_version_id = $2 where id = $1")
            .bind(document_id)
            .bind(version_id)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(&rag_document_select(
            "select d.* from rag_documents d where d.id = $1",
        ))
        .bind(document_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        rag_document_record_from_row(&row)
    }
}

fn conversation_select(inner: &str) -> String {
    format!(
        r#"
        with conversation_rows as ({inner})
        select conversation_rows.*,
               exists (
                   select 1 from conversation_summaries s
                   where s.conversation_id = conversation_rows.id
                     and s.superseded_at is null
               ) as summary_available,
               coalesce(mp.consent_mode, 'explicit_only') as memory_behavior
        from conversation_rows
        left join application_memory_policies mp on mp.application_id = conversation_rows.application_id
        "#
    )
}

fn conversation_message_select(inner: &str) -> String {
    format!(
        r#"
        with message_rows as ({inner})
        select message_rows.*, c.public_id as conversation_public_id
        from message_rows
        join conversations c on c.id = message_rows.conversation_id
        "#
    )
}

fn memory_select(inner: &str) -> String {
    format!(
        r#"
        with memory_rows as ({inner})
        select memory_rows.*, c.public_id as conversation_public_id
        from memory_rows
        left join conversations c on c.id = memory_rows.conversation_id
        "#
    )
}

fn rag_document_select(inner: &str) -> String {
    format!(
        r#"
        with document_rows as ({inner})
        select document_rows.*, c.public_id as collection_public_id
        from document_rows
        join rag_collections c on c.id = document_rows.collection_id
        "#
    )
}

fn message_sequence(value: Option<&str>) -> Option<i64> {
    value.and_then(|value| value.strip_prefix("msgseq_").unwrap_or(value).parse().ok())
}
