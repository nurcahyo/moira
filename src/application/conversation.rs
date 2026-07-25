use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandIdempotency, AdminCommandMutation, AdminCommandRunner, AdminCommandSpec,
        RequestContext,
    },
    domain::{
        AuditLogInsert, AuditResult, ConversationCreateRequest, ConversationMessageCreateRequest,
        ConversationMessageQuery, ConversationMessageRecord, ConversationMessageRole,
        ConversationMessageType, ConversationPatchRequest, ConversationPolicyPutRequest,
        ConversationPolicyRecord, ConversationQuery, ConversationRecord, ConversationStatus,
        CursorScope, EmbeddingPolicyPutRequest, EmbeddingPolicyRecord, ListCursor, ListResponse,
        MemoryConsentMode, MemoryCreateRequest, MemoryPatchRequest, MemoryPolicyPutRequest,
        MemoryPolicyRecord, MemoryQuery, MemoryRecord, MemoryScope, MemoryStatus, Pagination,
        PublicContentPart, PublicInputMessage, RagCollectionCreateRequest,
        RagCollectionPatchRequest, RagCollectionQuery, RagCollectionRecord, RagCollectionStatus,
        RagDocumentCreateRequest, RagDocumentIngestRequest, RagDocumentRecord,
        ResponseConversationInput, RetrievalPolicyPutRequest, RetrievalPolicyRecord, SeqCursor,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, ConversationAccess, ConversationInsert, ConversationMessageInsert,
        MemoryInsert, PgAdminRepository, PgConversationRepository,
        create_rag_collection_with_connection, create_rag_document_with_connection,
        ingest_rag_document_with_connection,
    },
    security::{Actor, ActorType},
};

#[derive(Debug, Clone)]
pub struct ConversationExecutionLink {
    pub conversation_id: String,
    pub user_message_id: String,
}

// ---------------------------------------------------------------------------
// Keyset pagination (plan 04, finding P1-4).
//
// One scope per list endpoint, used for BOTH encode and decode. A cursor minted for one list
// therefore fails closed with `400 invalid_cursor` if replayed against another, instead of
// paging through an unrelated table's key space — see `crate::domain::pagination`.
//
// The labels are wire-visible only through the opaque tag, never as text, so they are free to
// be descriptive. They must not be edited casually: changing one invalidates every
// outstanding cursor for that endpoint.
// ---------------------------------------------------------------------------

const CONVERSATIONS_CURSOR: CursorScope = CursorScope::new("conversations.list");
const CONVERSATION_MESSAGES_CURSOR: CursorScope = CursorScope::new("conversations.messages");
const MEMORIES_CURSOR: CursorScope = CursorScope::new("memories.list");
const RAG_COLLECTIONS_CURSOR: CursorScope = CursorScope::new("rag.collections");
const RAG_DOCUMENTS_CURSOR: CursorScope = CursorScope::new("rag.documents");

/// The keyset key a listed row is paginated by.
///
/// Exists so [`paginate`] can serve both cursor shapes — `(timestamp, id)` for the four
/// timestamp-ordered lists and a bare sequence number for the message list — without the
/// page-assembly arithmetic being written out twice and drifting.
trait PageKey: Copy {
    fn encode_for(self, scope: CursorScope) -> String;
}

impl PageKey for ListCursor {
    fn encode_for(self, scope: CursorScope) -> String {
        self.encode(scope)
    }
}

impl PageKey for SeqCursor {
    fn encode_for(self, scope: CursorScope) -> String {
        self.encode(scope)
    }
}

/// Rows to ask the repository for when the caller wants `limit` of them.
///
/// Exactly one extra row, which is the cheapest way to learn `has_more` — a second
/// `count(*)` over the same predicate would double the work and still be racy. The extra row
/// is discarded by [`paginate`] and never reaches the caller.
///
/// `saturating_add` rather than `+`: `limit` is clamped to `1..=200` by every caller's
/// `limit()` helper, but an overflow panic here would be a silly way to find out that
/// stopped being true.
fn over_fetch(limit: i64) -> i64 {
    limit.saturating_add(1)
}

/// Trims an over-fetched page, computes `has_more`, and encodes `next_cursor`.
///
/// `next_cursor` is the key of the **last row actually returned**, not of the over-fetched
/// row that proved there is more. Encoding the over-fetched row's key instead is the classic
/// off-by-one that silently drops exactly one row per page boundary.
fn paginate<T, K: PageKey>(
    mut rows: Vec<(T, K)>,
    limit: i64,
    scope: CursorScope,
) -> ListResponse<T> {
    let limit = usize::try_from(limit).unwrap_or(0);
    let has_more = rows.len() > limit;
    rows.truncate(limit);

    let next_cursor = if has_more {
        rows.last().map(|(_, key)| key.encode_for(scope))
    } else {
        None
    };

    ListResponse {
        data: rows.into_iter().map(|(record, _)| record).collect(),
        pagination: Pagination {
            next_cursor,
            has_more,
        },
    }
}

// NOTE: this ordering is a design placeholder for the future context-assembly pipeline (plans/11-rag-memory-intelligence.md). It is not currently consumed by prepare_response_conversation and does not affect what is sent to the provider.
pub struct ContextPlanner;

impl ContextPlanner {
    pub fn deterministic_phase_five_order() -> [&'static str; 8] {
        [
            "protected_instructions",
            "current_input",
            "tool_state",
            "recent_messages",
            "conversation_summary",
            "retrieved_memory",
            "retrieved_rag",
            "older_history",
        ]
    }
}

#[derive(Clone)]
pub struct ConversationService {
    state: AppState,
    repo: PgConversationRepository,
    admin_repo: PgAdminRepository,
}

impl ConversationService {
    pub fn new(state: &AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state: state.clone(),
            repo: PgConversationRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool),
        })
    }

    /// The keyed hasher every conversation/RAG ledger and content hash goes through
    /// (plan 03, P1-1).
    fn command_hasher(&self) -> crate::security::IdempotencyHasher {
        self.state.idempotency_hasher.clone()
    }

    pub async fn create_conversation(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ConversationCreateRequest,
    ) -> Result<ConversationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:create")?;
        let application_id = required_application_id(actor)?;
        validate_title(request.title.as_deref())?;
        validate_metadata(&request.metadata)?;
        let policy = self
            .repo
            .get_or_create_conversation_policy(application_id)
            .await?;
        if !policy.conversations_enabled || !policy.caller_can_create_conversations {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "conversation_policy_disabled",
                "conversation creation is disabled",
            ));
        }
        let id = Uuid::now_v7();
        let public_id = format!("conv_{id}");
        let external_tenant_id = effective_tenant(actor);
        let external_user_id = effective_user(actor);
        let record = self
            .repo
            .create_conversation(&ConversationInsert {
                id,
                public_id: &public_id,
                application_id,
                external_tenant_id: external_tenant_id.as_deref(),
                external_user_id: external_user_id.as_deref(),
                request: &request,
                retention_days: policy.default_retention_days,
            })
            .await?;
        self.audit(
            actor,
            ctx,
            "conversation.created",
            "conversation",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    /// Lists conversations, paging by `(updated_at, id)`.
    ///
    /// The cursor is decoded **before** the query runs, so a tampered or foreign cursor costs
    /// a `400 invalid_cursor` and no database round trip.
    ///
    /// Because the sort key is the mutable `updated_at`, a conversation touched mid-sweep can
    /// be re-seen on a later page or missed entirely — accepted keyset semantics, documented
    /// in full on `crate::infra::repositories::conversation`.
    pub async fn list_conversations(
        &self,
        actor: &Actor,
        query: &ConversationQuery,
    ) -> Result<ListResponse<ConversationRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:read")?;
        let cursor = ListCursor::decode_optional(query.cursor.as_deref(), CONVERSATIONS_CURSOR)?;
        let limit = query.limit();
        let rows = self
            .repo
            .list_conversations_authorized(
                &conversation_access(
                    actor,
                    can_read_all(actor, "moira:conversations:read", &self.state),
                )?,
                query,
                cursor,
                over_fetch(limit),
            )
            .await?;
        Ok(paginate(rows, limit, CONVERSATIONS_CURSOR))
    }

    pub async fn get_conversation(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        conversation_id: &str,
    ) -> Result<ConversationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:read")?;
        let record = self
            .repo
            .find_conversation_authorized(
                conversation_id,
                &conversation_access(
                    actor,
                    can_read_all(actor, "moira:conversations:read", &self.state),
                )?,
            )
            .await?;
        self.audit(
            actor,
            ctx,
            "conversation.read",
            "conversation",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn patch_conversation(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        conversation_id: &str,
        request: ConversationPatchRequest,
    ) -> Result<ConversationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:write")?;
        validate_title(request.title.as_deref())?;
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        self.ensure_conversation_write(actor, conversation_id)
            .await?;
        let record = self
            .repo
            .patch_conversation(conversation_id, &request)
            .await?;
        self.audit(
            actor,
            ctx,
            "conversation.updated",
            "conversation",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn set_conversation_status(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        conversation_id: &str,
        status: ConversationStatus,
    ) -> Result<ConversationRecord, AppError> {
        let scope = if status == ConversationStatus::Deleted {
            "moira:conversations:delete"
        } else {
            "moira:conversations:write"
        };
        self.state.authz.require(actor, scope)?;
        self.ensure_conversation_write(actor, conversation_id)
            .await?;
        let record = self
            .repo
            .set_conversation_status(conversation_id, status)
            .await?;
        let action = match status {
            ConversationStatus::Active => "conversation.restored",
            ConversationStatus::Archived => "conversation.archived",
            ConversationStatus::Deleted => "conversation.deleted",
        };
        self.audit(
            actor,
            ctx,
            action,
            "conversation",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    /// Lists a conversation's messages, paging by ascending `sequence_number`.
    ///
    /// The one ascending list on this surface, so it uses [`SeqCursor`] and a `>` predicate.
    /// Unlike the four timestamp-ordered lists this sweep is exactly-once: `sequence_number`
    /// is assigned once at insert and never changes.
    pub async fn list_messages(
        &self,
        actor: &Actor,
        conversation_id: &str,
        query: &ConversationMessageQuery,
    ) -> Result<ListResponse<ConversationMessageRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:read")?;
        let cursor =
            SeqCursor::decode_optional(query.cursor.as_deref(), CONVERSATION_MESSAGES_CURSOR)?;
        let limit = query.limit();
        self.repo
            .find_conversation_authorized(
                conversation_id,
                &conversation_access(
                    actor,
                    can_read_all(actor, "moira:conversations:read", &self.state),
                )?,
            )
            .await?;
        let rows = self
            .repo
            .list_messages(conversation_id, query, cursor, over_fetch(limit))
            .await?;
        Ok(paginate(rows, limit, CONVERSATION_MESSAGES_CURSOR))
    }

    pub async fn create_message(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        conversation_id: &str,
        request: ConversationMessageCreateRequest,
    ) -> Result<ConversationMessageRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:write")?;
        self.ensure_conversation_write(actor, conversation_id)
            .await?;
        if request.role != ConversationMessageRole::User
            && (actor.actor_type == ActorType::ConsumerKey
                || !self.state.authz.has_scope(actor, "moira:admin"))
        {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "message_role_invalid",
                "ordinary callers may only create user messages",
            ));
        }
        validate_metadata(&request.metadata)?;
        validate_content(&request.content)?;
        let content_hash = self
            .state
            .idempotency_hasher
            .hash(request.content.as_bytes());
        let record = self
            .repo
            .add_message(&ConversationMessageInsert {
                conversation_public_id: conversation_id.to_string(),
                response_id: None,
                execution_id: None,
                role: request.role,
                message_type: ConversationMessageType::Input,
                content_plain: Some(request.content.clone()),
                content_hash,
                content_size_bytes: request.content.len() as i64,
                token_count: Some(estimate_tokens(&request.content)),
                metadata: request.metadata,
            })
            .await?;
        self.audit(
            actor,
            ctx,
            "conversation.message.created",
            "conversation_message",
            Some(record.id.clone()),
            json!({ "conversation_id": conversation_id, "role": record.role }),
        )
        .await?;
        Ok(record)
    }

    // Persists the user's message for later retrieval by GET endpoints; does not load history, summaries, memories, or RAG content into the prompt sent to the provider. See docs/conversation-memory-rag-api.md for the MVP boundary.
    pub async fn prepare_response_conversation(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        input: Option<&ResponseConversationInput>,
        messages: &[PublicInputMessage],
    ) -> Result<Option<ConversationExecutionLink>, AppError> {
        let Some(input) = input else {
            return Ok(None);
        };
        let conversation = if input.create {
            self.create_conversation(
                actor,
                ctx,
                ConversationCreateRequest {
                    title: input.title.clone(),
                    metadata: input.metadata.clone(),
                },
            )
            .await?
        } else {
            let id = input.id.as_deref().ok_or_else(|| {
                AppError::unprocessable(
                    "conversation_not_found",
                    "conversation.id is required unless conversation.create is true",
                )
            })?;
            self.get_conversation(actor, ctx, id).await?
        };
        if conversation.status == ConversationStatus::Archived {
            return Err(AppError::coded(
                axum::http::StatusCode::CONFLICT,
                "conversation_archived",
                "conversation is archived",
            ));
        }
        let content = user_text_from_public_input(messages);
        validate_content(&content)?;
        let content_hash = self.state.idempotency_hasher.hash(content.as_bytes());
        let message = self
            .repo
            .add_message(&ConversationMessageInsert {
                conversation_public_id: conversation.id.clone(),
                response_id: None,
                execution_id: None,
                role: ConversationMessageRole::User,
                message_type: ConversationMessageType::Input,
                content_plain: Some(content.clone()),
                content_hash,
                content_size_bytes: content.len() as i64,
                token_count: Some(estimate_tokens(&content)),
                metadata: json!({ "source": "response_request" }),
            })
            .await?;
        self.audit(
            actor,
            ctx,
            "conversation.message.created",
            "conversation_message",
            Some(message.id.clone()),
            json!({ "conversation_id": conversation.id, "source": "response_request" }),
        )
        .await?;
        Ok(Some(ConversationExecutionLink {
            conversation_id: conversation.id,
            user_message_id: message.id,
        }))
    }

    pub async fn record_assistant_response(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        link: &ConversationExecutionLink,
        response_id: Uuid,
        execution_id: Uuid,
        output_text: Option<&str>,
    ) -> Result<Option<ConversationMessageRecord>, AppError> {
        let Some(output) = output_text else {
            return Ok(None);
        };
        let content_hash = self.state.idempotency_hasher.hash(output.as_bytes());
        let message = self
            .repo
            .add_message(&ConversationMessageInsert {
                conversation_public_id: link.conversation_id.clone(),
                response_id: Some(response_id),
                execution_id: Some(execution_id),
                role: ConversationMessageRole::Assistant,
                message_type: ConversationMessageType::Output,
                content_plain: Some(output.to_string()),
                content_hash,
                content_size_bytes: output.len() as i64,
                token_count: Some(estimate_tokens(output)),
                metadata: json!({ "source": "response_completion", "user_message_id": link.user_message_id }),
            })
            .await?;
        self.audit(
            actor,
            ctx,
            "conversation.message.created",
            "conversation_message",
            Some(message.id.clone()),
            json!({ "conversation_id": link.conversation_id, "source": "response_completion" }),
        )
        .await?;
        Ok(Some(message))
    }

    pub async fn create_memory(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: MemoryCreateRequest,
    ) -> Result<MemoryRecord, AppError> {
        self.state.authz.require(actor, "moira:memories:create")?;
        let application_id = required_application_id(actor)?;
        let policy = self
            .repo
            .get_or_create_memory_policy(application_id)
            .await?;
        if !policy.enabled || !policy.manual_memory_enabled {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "memory_disabled",
                "manual memory is disabled for this application",
            ));
        }
        if matches!(policy.consent_mode, MemoryConsentMode::Disabled) {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "memory_consent_required",
                "memory consent is disabled",
            ));
        }
        validate_content(&request.content)?;
        validate_metadata(&request.metadata)?;
        let id = Uuid::now_v7();
        let public_id = format!("mem_{id}");
        let external_tenant_id = effective_tenant(actor);
        let external_user_id = effective_user(actor);
        let content_hash = self
            .state
            .idempotency_hasher
            .hash(request.content.as_bytes());
        let record = self
            .repo
            .create_memory(&MemoryInsert {
                id,
                public_id: &public_id,
                application_id,
                external_tenant_id: external_tenant_id.as_deref(),
                external_user_id: external_user_id.as_deref(),
                scope: MemoryScope::UserApplication,
                request: &request,
                content_hash: &content_hash,
            })
            .await?;
        self.audit(
            actor,
            ctx,
            "memory.created",
            "memory",
            Some(record.id.clone()),
            json!({ "type": record.memory_type, "scope": record.scope }),
        )
        .await?;
        Ok(record)
    }

    /// Lists memories, paging by `(updated_at, id)`.
    ///
    /// Same mutable-sort-key caveat as [`Self::list_conversations`]: a memory whose
    /// `updated_at` moves during a sweep can be re-seen or missed.
    pub async fn list_memories(
        &self,
        actor: &Actor,
        query: &MemoryQuery,
    ) -> Result<ListResponse<MemoryRecord>, AppError> {
        self.state.authz.require(actor, "moira:memories:read")?;
        let application_id = required_application_id(actor)?;
        let policy = self
            .repo
            .get_or_create_memory_policy(application_id)
            .await?;
        if !policy.enabled || !policy.user_can_list {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "memory_disabled",
                "memory listing is disabled",
            ));
        }
        let cursor = ListCursor::decode_optional(query.cursor.as_deref(), MEMORIES_CURSOR)?;
        let limit = query.limit();
        let rows = self
            .repo
            .list_memories_authorized(
                &conversation_access(
                    actor,
                    can_read_all(actor, "moira:memories:read", &self.state),
                )?,
                query,
                cursor,
                over_fetch(limit),
            )
            .await?;
        Ok(paginate(rows, limit, MEMORIES_CURSOR))
    }

    pub async fn get_memory(
        &self,
        actor: &Actor,
        memory_id: &str,
    ) -> Result<MemoryRecord, AppError> {
        self.state.authz.require(actor, "moira:memories:read")?;
        self.repo
            .find_memory_authorized(
                memory_id,
                &conversation_access(
                    actor,
                    can_read_all(actor, "moira:memories:read", &self.state),
                )?,
            )
            .await
    }

    pub async fn patch_memory(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        memory_id: &str,
        request: MemoryPatchRequest,
    ) -> Result<MemoryRecord, AppError> {
        self.state.authz.require(actor, "moira:memories:write")?;
        self.ensure_memory_write(actor, memory_id).await?;
        if let Some(content) = &request.content {
            validate_content(content)?;
        }
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        let hash = request
            .content
            .as_ref()
            .map(|content| self.state.idempotency_hasher.hash(content.as_bytes()));
        let record = self
            .repo
            .patch_memory(memory_id, &request, hash.as_deref())
            .await?;
        self.audit(
            actor,
            ctx,
            if request.status == Some(MemoryStatus::Deleted) {
                "memory.deleted"
            } else {
                "memory.updated"
            },
            "memory",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn delete_memory(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        memory_id: &str,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:memories:delete")?;
        self.ensure_memory_write(actor, memory_id).await?;
        self.repo.delete_memory(memory_id).await?;
        self.audit(
            actor,
            ctx,
            "memory.deleted",
            "memory",
            Some(memory_id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn get_conversation_policy(
        &self,
        actor: &Actor,
        application_id: Uuid,
    ) -> Result<ConversationPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversation-policies:read")?;
        self.repo
            .get_or_create_conversation_policy(application_id)
            .await
    }

    pub async fn put_conversation_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        application_id: Uuid,
        request: ConversationPolicyPutRequest,
    ) -> Result<ConversationPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversation-policies:write")?;
        validate_metadata_option(&request.metadata)?;
        let record = self
            .repo
            .put_conversation_policy(application_id, &request)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit(
            actor,
            ctx,
            "conversation_policy.upsert",
            "conversation_policy",
            Some(application_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn get_memory_policy(
        &self,
        actor: &Actor,
        application_id: Uuid,
    ) -> Result<MemoryPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:memory-policies:read")?;
        self.repo.get_or_create_memory_policy(application_id).await
    }

    pub async fn put_memory_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        application_id: Uuid,
        request: MemoryPolicyPutRequest,
    ) -> Result<MemoryPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:memory-policies:write")?;
        validate_metadata_option(&request.metadata)?;
        let record = self
            .repo
            .put_memory_policy(application_id, &request)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit(
            actor,
            ctx,
            "memory_policy.upsert",
            "memory_policy",
            Some(application_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn get_retrieval_policy(
        &self,
        actor: &Actor,
        application_id: Uuid,
    ) -> Result<RetrievalPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:retrieval-policies:read")?;
        self.repo
            .get_or_create_retrieval_policy(application_id)
            .await
    }

    pub async fn put_retrieval_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        application_id: Uuid,
        request: RetrievalPolicyPutRequest,
    ) -> Result<RetrievalPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:retrieval-policies:write")?;
        validate_metadata_option(&request.metadata)?;
        let record = self
            .repo
            .put_retrieval_policy(application_id, &request)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit(
            actor,
            ctx,
            "retrieval_policy.upsert",
            "retrieval_policy",
            Some(application_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn get_embedding_policy(
        &self,
        actor: &Actor,
        application_id: Uuid,
    ) -> Result<EmbeddingPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:embedding-policies:read")?;
        self.repo
            .get_or_create_embedding_policy(application_id)
            .await
    }

    pub async fn put_embedding_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        application_id: Uuid,
        request: EmbeddingPolicyPutRequest,
    ) -> Result<EmbeddingPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:embedding-policies:write")?;
        validate_metadata_option(&request.metadata)?;
        let record = self
            .repo
            .put_embedding_policy(application_id, &request)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit(
            actor,
            ctx,
            "embedding_policy.upsert",
            "embedding_policy",
            Some(application_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn create_rag_collection(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: RagCollectionCreateRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-collections:write")?;
        // Authorization and validation stay outside the runner: they are cheap and
        // deterministic, and a rejected request must never occupy an idempotency key.
        validate_metadata(&request.metadata)?;
        let spec = conversation_command_spec(
            ctx,
            actor,
            RAG_COLLECTION_CREATE_OPERATION,
            json!({}),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.admin_repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    // Inside the closure so a replayed request never burns an identifier.
                    let id = Uuid::now_v7();
                    let record = create_rag_collection_with_connection(
                        transaction.connection(),
                        id,
                        &format!("collection_{id}"),
                        &request,
                    )
                    .await?;
                    transaction
                        .insert_audit(conversation_audit(
                            &actor,
                            &ctx,
                            "rag.collection.created",
                            "rag_collection",
                            Some(record.id.clone()),
                            json!({ "application_id": record.application_id }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.clone()))
                })
            })
            .await?;
        Ok(outcome.response)
    }

    /// Lists RAG collections, paging by `(created_at, id)`.
    ///
    /// Immutable sort key, so unlike the conversation and memory lists this sweep is
    /// exactly-once under concurrent updates.
    pub async fn list_rag_collections(
        &self,
        actor: &Actor,
        query: &RagCollectionQuery,
    ) -> Result<ListResponse<RagCollectionRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-collections:read")?;
        let cursor = ListCursor::decode_optional(query.cursor.as_deref(), RAG_COLLECTIONS_CURSOR)?;
        let limit = query.limit();
        let rows = self
            .repo
            .list_rag_collections(query, cursor, over_fetch(limit))
            .await?;
        Ok(paginate(rows, limit, RAG_COLLECTIONS_CURSOR))
    }

    pub async fn get_rag_collection(
        &self,
        actor: &Actor,
        collection_id: &str,
    ) -> Result<RagCollectionRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-collections:read")?;
        self.repo.get_rag_collection(collection_id).await
    }

    pub async fn patch_rag_collection(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        collection_id: &str,
        request: RagCollectionPatchRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-collections:write")?;
        validate_metadata_option(&request.metadata)?;
        let record = self
            .repo
            .patch_rag_collection(collection_id, &request)
            .await?;
        self.audit(
            actor,
            ctx,
            "rag.collection.updated",
            "rag_collection",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn set_rag_collection_status(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        collection_id: &str,
        status: RagCollectionStatus,
    ) -> Result<RagCollectionRecord, AppError> {
        let scope = if status == RagCollectionStatus::Deleted {
            "moira:rag-collections:delete"
        } else {
            "moira:rag-collections:write"
        };
        self.state.authz.require(actor, scope)?;
        let record = self
            .repo
            .set_rag_collection_status(collection_id, status)
            .await?;
        self.audit(
            actor,
            ctx,
            match status {
                RagCollectionStatus::Active => "rag.collection.updated",
                RagCollectionStatus::Disabled => "rag.collection.disabled",
                RagCollectionStatus::Deleted => "rag.collection.deleted",
            },
            "rag_collection",
            Some(record.id.clone()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn create_rag_document(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        collection_id: &str,
        request: RagDocumentCreateRequest,
    ) -> Result<RagDocumentRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-documents:write")?;
        // Authorization and validation stay outside the runner: they are cheap and
        // deterministic, and a rejected request must never occupy an idempotency key.
        validate_metadata(&request.metadata)?;
        validate_document(&request)?;
        let spec = conversation_command_spec(
            ctx,
            actor,
            RAG_DOCUMENT_CREATE_OPERATION,
            json!({ "collection_id": collection_id }),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let collection_id = collection_id.to_string();
        // Moved out of the closure only because `self` cannot cross the `move` boundary;
        // the hash itself is still computed inside the transaction, as the comment below says.
        let content_hasher = self.command_hasher();
        let outcome = AdminCommandRunner::new(self.admin_repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    // The content hash is an input to the mutation, not to the idempotency
                    // envelope, so it is computed inside the transaction.
                    let content_hash = request
                        .content
                        .as_ref()
                        .map(|content| content_hasher.hash(content.as_bytes()));
                    // Inside the closure so a replayed request never burns an identifier.
                    let id = Uuid::now_v7();
                    let record = create_rag_document_with_connection(
                        transaction.connection(),
                        id,
                        &format!("doc_{id}"),
                        &collection_id,
                        &request,
                        content_hash.as_deref(),
                    )
                    .await?;
                    transaction
                        .insert_audit(conversation_audit(
                            &actor,
                            &ctx,
                            "rag.document.created",
                            "rag_document",
                            Some(record.id.clone()),
                            json!({
                                "collection_id": collection_id,
                                "has_content": request.content.is_some(),
                            }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.clone()))
                })
            })
            .await?;
        Ok(outcome.response)
    }

    /// Lists a collection's documents, paging by `(created_at, id)`.
    ///
    /// # Why this one has no `cursor` argument
    ///
    /// `GET /api/v1/admin/rag-collections/{collection_id}/documents`
    /// (`src/http/conversation.rs`) is the only list route on this surface that declares
    /// **no query parameters at all** — not `cursor`, not even `limit`; its handler passes a
    /// hard-coded `50`. There is consequently no cursor for this signature to accept yet.
    ///
    /// What this method does still deliver is a correct `has_more`/`next_cursor` on the first
    /// page, so a caller can already tell that a collection has more than `limit` documents.
    /// Consuming that cursor requires adding a query extractor and an OpenAPI `params(..)`
    /// entry to the handler; until then use [`Self::list_rag_documents_page`], which is the
    /// real implementation and is fully wired.
    pub async fn list_rag_documents(
        &self,
        actor: &Actor,
        collection_id: &str,
        limit: i64,
    ) -> Result<ListResponse<RagDocumentRecord>, AppError> {
        self.list_rag_documents_page(actor, collection_id, None, limit)
            .await
    }

    /// Cursor-aware RAG document listing — see [`Self::list_rag_documents`] for why the two
    /// exist.
    ///
    /// `limit` is clamped here rather than in the repository, so the over-fetch row cannot be
    /// eaten by a repository-side ceiling at `limit == 200`. The bounds match every other
    /// `limit()` helper on this surface.
    pub async fn list_rag_documents_page(
        &self,
        actor: &Actor,
        collection_id: &str,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<RagDocumentRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-documents:read")?;
        let cursor = ListCursor::decode_optional(cursor, RAG_DOCUMENTS_CURSOR)?;
        let limit = limit.clamp(1, 200);
        let rows = self
            .repo
            .list_rag_documents(collection_id, cursor, over_fetch(limit))
            .await?;
        Ok(paginate(rows, limit, RAG_DOCUMENTS_CURSOR))
    }

    pub async fn get_rag_document(
        &self,
        actor: &Actor,
        document_id: &str,
    ) -> Result<RagDocumentRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-documents:read")?;
        self.repo.get_rag_document(document_id).await
    }

    pub async fn delete_rag_document(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        document_id: &str,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-documents:delete")?;
        self.repo.delete_rag_document(document_id).await?;
        self.audit(
            actor,
            ctx,
            "rag.document.deleted",
            "rag_document",
            Some(document_id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn ingest_rag_document(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        document_id: &str,
        request: RagDocumentIngestRequest,
    ) -> Result<RagDocumentRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:rag-documents:ingest")?;
        // Authorization and validation stay outside the runner: they are cheap and
        // deterministic, and a rejected request must never occupy an idempotency key.
        let content = request.content.as_deref().ok_or_else(|| {
            AppError::unprocessable(
                "rag_document_parse_failed",
                "direct text content is required for synchronous ingestion",
            )
        })?;
        validate_content(content)?;
        validate_metadata(&request.metadata)?;
        let spec = conversation_command_spec(
            ctx,
            actor,
            RAG_DOCUMENT_INGEST_OPERATION,
            json!({ "document_id": document_id }),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let document_id = document_id.to_string();
        // Moved out of the closure only because `self` cannot cross the `move` boundary;
        // the hash itself is still computed inside the transaction, as the comment below says.
        let content_hasher = self.command_hasher();
        let outcome = AdminCommandRunner::new(self.admin_repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    // The content hash is an input to the mutation, not to the idempotency
                    // envelope, so it is computed inside the transaction. `content` is
                    // known to be present: the check above already ran.
                    let content = request.content.as_deref().unwrap_or_default();
                    let content_hash = content_hasher.hash(content.as_bytes());
                    let record = ingest_rag_document_with_connection(
                        transaction.connection(),
                        &document_id,
                        &request,
                        &content_hash,
                    )
                    .await?;
                    transaction
                        .insert_audit(conversation_audit(
                            &actor,
                            &ctx,
                            "rag.document.ingested",
                            "rag_document",
                            Some(record.id.clone()),
                            json!({}),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 200, Some(record.id.clone()))
                })
            })
            .await?;
        Ok(outcome.response)
    }

    async fn ensure_conversation_write(
        &self,
        actor: &Actor,
        conversation_id: &str,
    ) -> Result<(), AppError> {
        self.repo
            .find_conversation_authorized(conversation_id, &conversation_access(actor, false)?)
            .await
            .map(|_| ())
    }

    async fn ensure_memory_write(&self, actor: &Actor, memory_id: &str) -> Result<(), AppError> {
        let privileged = matches!(actor.actor_type, ActorType::SystemKey | ActorType::DevAdmin);
        self.repo
            .find_memory_authorized(memory_id, &conversation_access(actor, privileged)?)
            .await
            .map(|_| ())
    }

    async fn audit(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        action: &str,
        resource_type: &str,
        resource_id: Option<String>,
        metadata: Value,
    ) -> Result<(), AppError> {
        self.admin_repo
            .insert_audit(conversation_audit(
                actor,
                ctx,
                action,
                resource_type,
                resource_id,
                metadata,
            ))
            .await
    }
}

/// Operation identity for `POST /api/v1/admin/rag-collections`.
pub(crate) const RAG_COLLECTION_CREATE_OPERATION: &str = "rag.collection.create";
/// Operation identity for `POST /api/v1/admin/rag-collections/{collection_id}/documents`.
pub(crate) const RAG_DOCUMENT_CREATE_OPERATION: &str = "rag.document.create";
/// Operation identity shared by `POST /api/v1/admin/rag-documents/{id}/ingest` **and**
/// `POST /api/v1/admin/rag-documents/{id}/reindex`.
///
/// `reindex_rag_document` is a literal call-through to `ingest_rag_document`
/// (`src/http/conversation.rs`) and performs an identical mutation, so the two aliases share
/// one operation identity and one `path` envelope. Consequence, decided deliberately in
/// `plans/02b-idempotency-replay.md` (Architecture -> "Operation identities"): the same key
/// and body sent to `/reindex` after `/ingest` replays the ingest response instead of
/// creating a second version. Discriminating the two routes inside the `path` envelope
/// would instead yield `409 idempotency_conflict`, which is worse UX for no correctness
/// gain.
pub(crate) const RAG_DOCUMENT_INGEST_OPERATION: &str = "rag.document.ingest";

/// Builds the idempotency envelope for a conversation-surface write command.
///
/// Mirrors `crate::application::admin::admin_command_spec`. `expected_version` deliberately
/// stays `None`: these routes accept no `If-Match` today, and adding optimistic concurrency
/// is a separate contract change (`plans/02b-idempotency-replay.md`, Excluded scope).
///
/// The actor fingerprint comes from the single crate-wide `admin::actor_fingerprint` — these
/// four routes authenticate through `admin_actor`, not `public_actor`, so
/// `public_actor_fingerprint` must not be used here.
pub(crate) fn conversation_command_spec<T: Serialize>(
    ctx: &RequestContext,
    actor: &Actor,
    operation: &str,
    path: Value,
    request: &T,
) -> Result<AdminCommandSpec, AppError> {
    AdminCommandSpec::new(operation, path, request).map(|spec| {
        spec.with_idempotency(
            ctx.idempotency_key
                .as_ref()
                .map(|key| AdminCommandIdempotency {
                    key: key.clone(),
                    actor_fingerprint: crate::application::admin::actor_fingerprint(actor),
                }),
        )
    })
}

/// The conversation surface's audit-row builder.
///
/// Deliberately **not** `crate::application::admin::success_audit`: that one lowercases
/// `actor_type`, whereas this surface has always written the `Debug` casing verbatim.
/// Reusing it would silently rewrite the recorded `actor_type` for every RAG and
/// conversation audit row. The casing divergence is pre-existing debt tracked for plan 06;
/// this builder reproduces today's mapping exactly so moving the write inside the
/// transaction changes atomicity and nothing else.
pub(crate) fn conversation_audit(
    actor: &Actor,
    ctx: &RequestContext,
    action: &str,
    resource_type: &str,
    resource_id: Option<String>,
    metadata: Value,
) -> AuditLogInsert {
    AuditLogInsert {
        request_id: Some(ctx.request_id.clone()),
        actor_type: Some(format!("{:?}", actor.actor_type)),
        actor_subject: actor.subject.clone(),
        delegated_subject: actor.delegated_subject.clone(),
        external_user_id: actor.external_user_id.clone(),
        external_tenant_id: actor.external_tenant_id.clone(),
        application_id: actor.internal_application_id,
        resource_type: resource_type.to_string(),
        resource_id,
        action: action.to_string(),
        result: AuditResult::Success,
        source_ip: ctx.source_ip,
        user_agent: ctx.user_agent.clone(),
        metadata,
    }
}

fn required_application_id(actor: &Actor) -> Result<Uuid, AppError> {
    actor.internal_application_id.ok_or_else(|| {
        AppError::coded(
            axum::http::StatusCode::FORBIDDEN,
            "conversation_forbidden",
            "application-bound identity is required",
        )
    })
}

pub fn conversation_access(
    actor: &Actor,
    privileged: bool,
) -> Result<ConversationAccess, AppError> {
    if matches!(
        actor.actor_type,
        ActorType::ConsumerKey | ActorType::TrustedJwt
    ) && actor.internal_application_id.is_none()
    {
        return Err(AppError::Forbidden(
            "application-bound caller identity is required for context access".to_string(),
        ));
    }

    Ok(ConversationAccess {
        privileged,
        application_id: actor.internal_application_id,
        external_tenant_id: effective_tenant(actor),
        external_user_id: effective_user(actor),
    })
}

fn can_read_all(actor: &Actor, scope: &str, state: &AppState) -> bool {
    matches!(actor.actor_type, ActorType::SystemKey | ActorType::DevAdmin)
        && state.authz.has_scope(actor, scope)
}

fn effective_tenant(actor: &Actor) -> Option<String> {
    actor
        .external_tenant_id
        .clone()
        .or_else(|| actor.tenant_id.clone())
}

fn effective_user(actor: &Actor) -> Option<String> {
    actor
        .external_user_id
        .clone()
        .or_else(|| actor.subject.clone())
}

fn validate_title(title: Option<&str>) -> Result<(), AppError> {
    if let Some(title) = title
        && (title.len() > 512 || title.chars().any(char::is_control))
    {
        return Err(AppError::BadRequest(
            "conversation title is invalid".to_string(),
        ));
    }
    Ok(())
}

fn validate_content(content: &str) -> Result<(), AppError> {
    if content.is_empty() || content.len() > 262_144 {
        return Err(AppError::unprocessable(
            "context_required_content_too_large",
            "content must be non-empty and within the configured limit",
        ));
    }
    if contains_secret_like_text(content) {
        return Err(AppError::unprocessable(
            "memory_sensitivity_forbidden",
            "content appears to contain secret material",
        ));
    }
    Ok(())
}

fn validate_document(request: &RagDocumentCreateRequest) -> Result<(), AppError> {
    if request.title.is_empty() || request.title.len() > 512 {
        return Err(AppError::unprocessable(
            "rag_document_type_unsupported",
            "document title is invalid",
        ));
    }
    if !matches!(
        request.mime_type.as_str(),
        "text/plain" | "text/markdown" | "application/json"
    ) {
        return Err(AppError::unprocessable(
            "rag_document_type_unsupported",
            "only bounded text, markdown, and JSON documents are supported",
        ));
    }
    if request.source_type != "direct_text" && request.source_type != "metadata_only" {
        return Err(AppError::unprocessable(
            "rag_document_type_unsupported",
            "only direct_text and metadata_only sources are supported in this phase",
        ));
    }
    if let Some(content) = &request.content {
        validate_content(content)?;
    }
    Ok(())
}

fn validate_metadata_option(metadata: &Option<Value>) -> Result<(), AppError> {
    if let Some(metadata) = metadata {
        validate_metadata(metadata)?;
    }
    Ok(())
}

fn validate_metadata(metadata: &Value) -> Result<(), AppError> {
    let Some(map) = metadata.as_object() else {
        return Err(AppError::unprocessable(
            "invalid_metadata",
            "metadata must be a JSON object",
        ));
    };
    if map.len() > 64 {
        return Err(AppError::unprocessable(
            "invalid_metadata",
            "metadata has too many keys",
        ));
    }
    for key in map.keys() {
        let lower = key.to_ascii_lowercase();
        if key.len() > 128
            || matches!(
                lower.as_str(),
                "api_key"
                    | "authorization"
                    | "password"
                    | "secret"
                    | "token"
                    | "access_token"
                    | "refresh_token"
                    | "private_key"
                    | "cookie"
            )
        {
            return Err(AppError::unprocessable(
                "invalid_metadata",
                "metadata contains a disallowed key",
            ));
        }
    }
    Ok(())
}

fn user_text_from_public_input(messages: &[PublicInputMessage]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        for part in &message.content {
            match part {
                PublicContentPart::InputText { text } => lines.push(text.clone()),
                PublicContentPart::InputImage { image_url } => {
                    lines.push(format!("[image: {image_url}]"));
                }
            }
        }
    }
    lines.join("\n")
}

fn estimate_tokens(content: &str) -> i64 {
    content.split_whitespace().count().max(1) as i64
}

fn contains_secret_like_text(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "api_key=",
        "authorization:",
        "bearer ",
        "sk-",
        "private key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCOPE: CursorScope = CursorScope::new("test.pagination");

    fn list_keys(count: usize) -> Vec<(String, ListCursor)> {
        (0..count)
            .map(|index| {
                let ts =
                    chrono::DateTime::from_timestamp_micros(1_700_000_000_000_000 - index as i64)
                        .expect("in-range timestamp");
                let id = Uuid::from_u128(index as u128 + 1);
                (format!("row-{index}"), ListCursor::new(ts, id))
            })
            .collect()
    }

    #[test]
    fn over_fetch_asks_for_exactly_one_extra_row() {
        assert_eq!(over_fetch(1), 2);
        assert_eq!(over_fetch(50), 51);
        assert_eq!(over_fetch(200), 201);
        // Saturating rather than panicking, even though no caller can reach this.
        assert_eq!(over_fetch(i64::MAX), i64::MAX);
    }

    #[test]
    fn has_more_is_false_when_exactly_limit_rows_are_available() {
        let page = paginate(list_keys(5), 5, TEST_SCOPE);

        assert_eq!(page.data.len(), 5);
        assert!(!page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, None);
    }

    #[test]
    fn has_more_is_false_for_a_short_page() {
        let page = paginate(list_keys(2), 5, TEST_SCOPE);

        assert_eq!(page.data.len(), 2);
        assert!(!page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, None);
    }

    #[test]
    fn has_more_is_true_and_the_page_is_trimmed_when_limit_plus_one_rows_are_fetched() {
        let page = paginate(list_keys(6), 5, TEST_SCOPE);

        assert_eq!(
            page.data.len(),
            5,
            "the over-fetched row must not be served"
        );
        assert_eq!(page.data.last().unwrap(), "row-4");
        assert!(page.pagination.has_more);
        assert!(page.pagination.next_cursor.is_some());
    }

    #[test]
    fn next_cursor_encodes_the_last_returned_row_not_the_over_fetched_row() {
        let rows = list_keys(6);
        let last_returned = rows[4].1;
        let over_fetched = rows[5].1;

        let page = paginate(rows, 5, TEST_SCOPE);
        let next_cursor = page
            .pagination
            .next_cursor
            .expect("has_more implies a cursor");

        assert_eq!(
            next_cursor,
            last_returned.encode(TEST_SCOPE),
            "next_cursor must resume from the last row the caller SAW; using the over-fetched \
             row's key silently drops exactly one row per page boundary"
        );
        assert_ne!(next_cursor, over_fetched.encode(TEST_SCOPE));
    }

    #[test]
    fn next_cursor_round_trips_under_the_scope_it_was_minted_for() {
        let page = paginate(list_keys(6), 5, TEST_SCOPE);
        let encoded = page
            .pagination
            .next_cursor
            .expect("has_more implies a cursor");

        assert!(ListCursor::decode(&encoded, TEST_SCOPE).is_ok());
        assert!(
            ListCursor::decode(&encoded, CursorScope::new("test.other")).is_err(),
            "a cursor must not page through another endpoint's key space"
        );
    }

    #[test]
    fn an_empty_result_reports_no_further_pages() {
        let page = paginate(Vec::<(String, ListCursor)>::new(), 50, TEST_SCOPE);

        assert!(page.data.is_empty());
        assert!(!page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, None);
    }

    #[test]
    fn sequence_keyed_pages_use_the_sequence_cursor_shape() {
        let rows: Vec<(String, SeqCursor)> = (1..=4)
            .map(|sequence| (format!("msg-{sequence}"), SeqCursor::new(sequence)))
            .collect();

        let page = paginate(rows, 3, TEST_SCOPE);
        let encoded = page
            .pagination
            .next_cursor
            .expect("has_more implies a cursor");

        assert_eq!(page.data, vec!["msg-1", "msg-2", "msg-3"]);
        assert!(page.pagination.has_more);
        assert_eq!(
            SeqCursor::decode(&encoded, TEST_SCOPE).unwrap(),
            SeqCursor::new(3),
            "the message list resumes from the last sequence number returned"
        );
        assert!(
            ListCursor::decode(&encoded, TEST_SCOPE).is_err(),
            "a sequence cursor must not decode as a (timestamp, id) cursor"
        );
    }

    /// Every list endpoint on this surface must use a distinct scope.
    ///
    /// Two endpoints sharing a scope is invisible in normal use — both cursors decode — and
    /// only shows up as one list silently paging through another's key space.
    #[test]
    fn every_list_endpoint_has_its_own_cursor_scope() {
        let scopes = [
            CONVERSATIONS_CURSOR,
            CONVERSATION_MESSAGES_CURSOR,
            MEMORIES_CURSOR,
            RAG_COLLECTIONS_CURSOR,
            RAG_DOCUMENTS_CURSOR,
        ];
        let mut labels: Vec<&str> = scopes.iter().map(|scope| scope.label()).collect();
        labels.sort_unstable();
        let unique = labels.len();
        labels.dedup();

        assert_eq!(
            labels.len(),
            unique,
            "cursor scopes must be distinct: {labels:?}"
        );
        assert!(labels.iter().all(|label| !label.is_empty()));
    }

    #[test]
    fn a_malformed_cursor_is_rejected_before_any_query_runs() {
        let error = ListCursor::decode_optional(Some("not-a-cursor"), CONVERSATIONS_CURSOR)
            .expect_err("a garbage cursor must not reach the database");
        let response = error.error_response(Some("req_test".to_string()));

        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(response.error.code, "invalid_cursor");
        assert_eq!(response.error.message_key, "moira.error.invalid_cursor");
        assert!(!response.error.message.is_empty());

        // Absent and empty both mean "first page", not "malformed".
        assert!(
            ListCursor::decode_optional(None, CONVERSATIONS_CURSOR)
                .unwrap()
                .is_none()
        );
        assert!(
            ListCursor::decode_optional(Some(""), CONVERSATIONS_CURSOR)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn context_planner_order_keeps_required_content_first() {
        let order = ContextPlanner::deterministic_phase_five_order();
        assert_eq!(order[0], "protected_instructions");
        assert_eq!(order[1], "current_input");
        assert!(order.contains(&"retrieved_memory"));
        assert!(order.contains(&"retrieved_rag"));
    }

    #[test]
    fn metadata_rejects_secret_like_keys() {
        assert!(validate_metadata(&json!({ "token": "hidden" })).is_err());
        assert!(validate_metadata(&json!({ "ticket": "MOIRA-5" })).is_ok());
    }

    #[test]
    fn only_system_and_development_admin_actors_can_read_all_context() {
        let state = AppState::new(crate::config::Settings::default(), None).unwrap();
        for actor_type in [ActorType::SystemKey, ActorType::DevAdmin] {
            let actor = Actor {
                actor_type,
                scopes: vec!["moira:conversations:read".to_string()],
                ..Actor::default()
            };
            assert!(can_read_all(&actor, "moira:conversations:read", &state));
        }

        let trusted_jwt = Actor {
            actor_type: ActorType::TrustedJwt,
            scopes: vec![
                "moira:conversations:read".to_string(),
                "moira:memories:read".to_string(),
            ],
            ..Actor::default()
        };
        assert!(!can_read_all(
            &trusted_jwt,
            "moira:conversations:read",
            &state
        ));
        assert!(!can_read_all(&trusted_jwt, "moira:memories:read", &state));
    }

    fn command_hasher() -> crate::security::IdempotencyHasher {
        crate::security::IdempotencyHasher::new(b"conversation-pepper".to_vec(), "v1")
    }

    fn test_context(idempotency_key: Option<String>) -> RequestContext {
        RequestContext {
            request_id: "req-test".to_string(),
            source_ip: None,
            user_agent: None,
            idempotency_key,
        }
    }

    #[test]
    fn conversation_command_hash_is_stable_across_object_key_order() {
        let ctx = test_context(None);
        let actor = Actor::default();
        let left = conversation_command_spec(
            &ctx,
            &actor,
            RAG_DOCUMENT_CREATE_OPERATION,
            json!({ "collection_id": "collection_1" }),
            &json!({"title": "doc", "metadata": {"b": 2, "a": 1}}),
        )
        .unwrap();
        let right = conversation_command_spec(
            &ctx,
            &actor,
            RAG_DOCUMENT_CREATE_OPERATION,
            json!({ "collection_id": "collection_1" }),
            &json!({"metadata": {"a": 1, "b": 2}, "title": "doc"}),
        )
        .unwrap();

        let hasher = command_hasher();
        assert_eq!(
            left.request_hash(&hasher).unwrap(),
            right.request_hash(&hasher).unwrap()
        );
    }

    #[test]
    fn conversation_command_hash_covers_operation_and_path() {
        let ctx = test_context(None);
        let actor = Actor::default();
        let body = json!({ "content": "hello" });

        let document_a = conversation_command_spec(
            &ctx,
            &actor,
            RAG_DOCUMENT_INGEST_OPERATION,
            json!({ "document_id": "doc_a" }),
            &body,
        )
        .unwrap();
        let document_b = conversation_command_spec(
            &ctx,
            &actor,
            RAG_DOCUMENT_INGEST_OPERATION,
            json!({ "document_id": "doc_b" }),
            &body,
        )
        .unwrap();
        let hasher = command_hasher();
        assert_ne!(
            document_a.request_hash(&hasher).unwrap(),
            document_b.request_hash(&hasher).unwrap(),
            "the document id must be inside the hash envelope"
        );

        let collection_create = conversation_command_spec(
            &ctx,
            &actor,
            RAG_COLLECTION_CREATE_OPERATION,
            json!({}),
            &body,
        )
        .unwrap();
        let document_create = conversation_command_spec(
            &ctx,
            &actor,
            RAG_DOCUMENT_CREATE_OPERATION,
            json!({}),
            &body,
        )
        .unwrap();
        assert_ne!(
            collection_create.request_hash(&hasher).unwrap(),
            document_create.request_hash(&hasher).unwrap(),
            "the operation identity must be inside the hash envelope"
        );
    }

    #[test]
    fn ingest_and_reindex_share_one_operation_and_request_envelope() {
        // DOCUMENTS the `/reindex` decision; it does not guard it. `POST .../reindex` is a
        // literal call-through to `ingest_rag_document` (src/http/conversation.rs), so both
        // routes reach this one method and build their spec from this one
        // `RAG_DOCUMENT_INGEST_OPERATION` constant with one path envelope. Because there is
        // only one construction site, this test necessarily builds both specs from the same
        // constant, the same path and the same body — it reduces to `f(x) == f(x)` and is
        // structurally incapable of failing. Keep it as executable documentation of the
        // shared identity, but do not count it as coverage.
        //
        // The real guard is the e2e test
        // `reindex_replays_an_ingest_performed_under_the_same_key` in
        // tests/rag_idempotency_replay.rs, which drives both HTTP routes for real and
        // asserts the second one replays the first's response instead of creating a new
        // version row. That test is load-bearing (mutation testing killed it three ways);
        // this one is not.
        let ctx = test_context(None);
        let actor = Actor::default();
        let body = json!({ "content": "hello", "metadata": {} });
        let path = json!({ "document_id": "doc_shared" });

        let ingest_spec = conversation_command_spec(
            &ctx,
            &actor,
            RAG_DOCUMENT_INGEST_OPERATION,
            path.clone(),
            &body,
        )
        .unwrap();
        let reindex_spec =
            conversation_command_spec(&ctx, &actor, RAG_DOCUMENT_INGEST_OPERATION, path, &body)
                .unwrap();

        let hasher = command_hasher();
        assert_eq!(
            ingest_spec.request_hash(&hasher).unwrap(),
            reindex_spec.request_hash(&hasher).unwrap()
        );
    }

    #[test]
    fn conversation_command_spec_omits_idempotency_when_no_key_is_present() {
        let ctx = test_context(None);
        let actor = Actor::default();
        let spec = conversation_command_spec(
            &ctx,
            &actor,
            RAG_COLLECTION_CREATE_OPERATION,
            json!({}),
            &json!({}),
        )
        .unwrap();
        assert!(
            format!("{spec:?}").contains("idempotency: None"),
            "a spec built without ctx.idempotency_key must carry no AdminCommandIdempotency"
        );

        let ctx_with_key = test_context(Some("replay-key".to_string()));
        let spec_with_key = conversation_command_spec(
            &ctx_with_key,
            &actor,
            RAG_COLLECTION_CREATE_OPERATION,
            json!({}),
            &json!({}),
        )
        .unwrap();
        assert!(
            format!("{spec_with_key:?}").contains("idempotency: Some"),
            "a spec built with ctx.idempotency_key must carry an AdminCommandIdempotency"
        );
    }

    #[test]
    fn conversation_audit_preserves_the_existing_actor_type_casing() {
        let actor = Actor {
            actor_type: ActorType::SystemKey,
            ..Actor::default()
        };
        let ctx = test_context(None);
        let insert = conversation_audit(
            &actor,
            &ctx,
            "rag.document.ingested",
            "rag_document",
            Some("doc_1".to_string()),
            json!({}),
        );
        assert_eq!(
            insert.actor_type,
            Some("SystemKey".to_string()),
            "conversation_audit must not lowercase actor_type, unlike admin::success_audit"
        );
    }

    #[test]
    fn context_access_requires_consumer_and_trusted_jwt_application_binding() {
        for actor_type in [ActorType::ConsumerKey, ActorType::TrustedJwt] {
            let actor = Actor {
                actor_type,
                ..Actor::default()
            };
            assert!(matches!(
                conversation_access(&actor, false),
                Err(AppError::Forbidden(_))
            ));
        }

        let application_id = Uuid::now_v7();
        let trusted_jwt = Actor {
            actor_type: ActorType::TrustedJwt,
            internal_application_id: Some(application_id),
            ..Actor::default()
        };
        let access = conversation_access(&trusted_jwt, false).unwrap();
        assert_eq!(access.application_id, Some(application_id));
        assert!(!access.privileged);

        let system = Actor {
            actor_type: ActorType::SystemKey,
            ..Actor::default()
        };
        let access = conversation_access(&system, true).unwrap();
        assert_eq!(access.application_id, None);
        assert!(access.privileged);
    }
}
