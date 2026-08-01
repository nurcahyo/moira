use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandIdempotency, AdminCommandMutation, AdminCommandRunner, AdminCommandSpec,
        ContextSections, EXTRACTION_TRANSCRIPT_MESSAGES, ExecutionService, ExtractionPolicy,
        FAILURE_EXTRACTION_CALL_FAILED, FAILURE_SUMMARIZATION_CALL_FAILED,
        MAXIMUM_CANDIDATES_PER_RUN, MoiraExecutionService, NEAR_DUPLICATE_MAX_DISTANCE,
        RejectionReason, RequestContext, SECRET_NEEDLES, SUMMARY_TRANSCRIPT_MESSAGES,
        SummarizationBacklog, SummarizationPolicy, SummarizationSkip, assemble_context,
        budget_tokens, classify_candidate, decide_summarization, effective_extraction_status,
        extraction_messages, extraction_output_schema, is_near_duplicate, parse_candidates,
        parse_summary, render_transcript, summarization_messages,
    },
    domain::{
        AuditLogInsert, AuditResult, CallerRuntimeIdentity, ConversationCreateRequest,
        ConversationMessageCreateRequest, ConversationMessageQuery, ConversationMessageRecord,
        ConversationMessageRole, ConversationMessageType, ConversationPatchRequest,
        ConversationPolicyPutRequest, ConversationPolicyRecord, ConversationQuery,
        ConversationRecord, ConversationStatus, ConversationSummaryRecord, CursorScope,
        DomainMessage, EmbeddingPolicyPutRequest, EmbeddingPolicyRecord, ExecutionCommand,
        ExecutionOptions, HistoryStrategy, ListCursor, ListResponse, MemoryConsentMode,
        MemoryCreateRequest, MemoryPatchRequest, MemoryPolicyPutRequest, MemoryPolicyRecord,
        MemoryQuery, MemoryRecord, MemoryScope, MemoryStatus, Pagination, PublicCitation,
        PublicContentPart, PublicInputMessage, RagCollectionCreateRequest,
        RagCollectionPatchRequest, RagCollectionQuery, RagCollectionRecord, RagCollectionStatus,
        RagDocumentCreateRequest, RagDocumentIngestRequest, RagDocumentRecord,
        ResponseConversationInput, RetrievalPolicyPutRequest, RetrievalPolicyRecord, SeqCursor,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, ContextPlanInsert, ConversationAccess, ConversationInsert,
        ConversationMessageInsert, ConversationRepository, ConversationSummaryInsert,
        ConversationSummaryRow, ExtractedMemoryInsert, MemoryExtractionRunInsert,
        MemoryExtractionRunOutcome, MemoryInsert, PgAdminRepository, PgConversationRepository,
        RagIngestionContext, RetrievalRunInsert, RetrievalScope, SummarizationLock,
        complete_memory_extraction_run, confirm_memory, count_messages_after_sequence,
        create_rag_collection_with_connection, create_rag_document_with_connection,
        find_active_conversation_summary, find_application_embedding_target,
        find_collection_ingestion_context, find_conversation_context_anchor,
        find_conversation_route_hint, find_document_ingestion_context, find_embedding_model_target,
        find_memory_by_content_hash, find_memory_by_key, find_memory_candidates,
        find_messages_after_sequence, find_nearest_memory, find_rag_chunk_candidates,
        find_recent_messages, ingest_rag_document_with_connection, insert_context_plan,
        insert_conversation_summary, insert_extracted_memory, insert_memory_embedding,
        insert_memory_extraction_run, insert_retrieval_run, resolve_embedding_credential,
    },
    orchestration::{
        CANDIDATE_OVERFETCH, ChunkStrategy, ChunkingLimits, EmbeddingBatchPlan, EmbeddingFactory,
        FAILURE_EMBEDDING_DIMENSION_UNSUPPORTED, FAILURE_EMBEDDING_FAILED,
        FAILURE_EMBEDDING_NOT_CONFIGURED, MAX_CANDIDATE_ROWS, MemoryCandidate, RagChunkCandidate,
        RagIngestionPlan, RetrievalLimits, RetrievalWeights, RigEmbeddingFactory,
        SUPPORTED_EMBEDDING_DIMENSION, Scored, embed_texts, encode_vector_literal, prepare_chunks,
        provider_type_supports_embeddings, rank_chunks, rank_memories,
    },
    security::{Actor, ActorType, request_hash},
};

/// What `POST /api/v1/conversations/{id}/summarize` did — plan 11 Sub-Phase E.
///
/// An enum rather than an `Option<ConversationSummaryRecord>` because the two arms mean different
/// things to a caller and map to different status codes: `AlreadyRunning` is a `202` and says
/// *somebody else is doing this right now*, not *there was nothing to do*, which is the `409`
/// `summarization_not_needed` refusal instead.
#[derive(Debug, Clone)]
pub enum SummarizationOutcome {
    Summarized(ConversationSummaryRecord),
    AlreadyRunning,
}

#[derive(Debug, Clone)]
pub struct ConversationExecutionLink {
    pub conversation_id: String,
    pub user_message_id: String,
    /// The route the caller's own turn was issued against, verbatim.
    ///
    /// Carried so memory extraction (Sub-Phase F) can issue its second completion through the
    /// **same** route. Extraction has no configuration surface of its own — there is no
    /// `extraction_route_key` policy column — and reusing the caller's route is the only choice
    /// that cannot fail on an application where responses themselves work: same provider, same
    /// credential, same model policy, already proven reachable one call ago.
    ///
    /// `None` reproduces the caller's own "no hint" case and lands on the default route, which
    /// is what the caller got too. Extraction is therefore never routed somewhere the caller's
    /// own turn would not have gone.
    ///
    /// **Reversal condition:** give extraction its own route the moment an operator needs it on
    /// a cheaper model than the response path — a policy column, a DTO field, an OpenAPI
    /// regeneration, and a test that the two routes really differ.
    pub route_hint: Option<String>,
    /// What the planner assembled for this turn (plan 11 Sub-Phases D and G).
    ///
    /// Empty when the application has retrieval disabled, which is the default — the field is
    /// always present so no call site has to branch on whether planning ran.
    pub context: PlannedContext,
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

// ---------------------------------------------------------------------------
// Retrieval failure classes (plan 11, Sub-Phase C).
//
// These are `retrieval_runs.failure_class` values, not error codes: a retrieval failure is
// recorded on the run row whether or not it becomes visible to the caller, and under the
// default `failure_behavior` it never does. Keeping them distinct from the `AppError` codes is
// what stops "the vector backend was down" from being reported to a caller as an execution
// failure it can do nothing about.
// ---------------------------------------------------------------------------

/// The database or connection pool refused the candidate query.
const FAILURE_RETRIEVAL_BACKEND: &str = "retrieval_backend_failed";

/// `memory_records.resolution_status` for a contradiction nobody has adjudicated.
///
/// The column has no check constraint (`migrations/0007…:255`), so this is a constant rather
/// than a literal at the write site: two spellings would make "how many contradictions are
/// outstanding" unanswerable by query.
const CONTRADICTION_UNRESOLVED: &str = "unresolved";

/// A run that produced nothing, with the reason on the row.
///
/// `candidate_count` is zero rather than "however many we had parsed": a failure before the
/// parse means no candidate was ever proposed, and a failure at the parse means none was
/// proposed *usably*. Reporting a non-zero candidate count with zero accepted and zero rejected
/// would not add up.
fn failed_extraction(failure_class: &'static str) -> MemoryExtractionRunOutcome {
    MemoryExtractionRunOutcome {
        candidate_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        status: "failed",
        failure_class: Some(failure_class),
        metadata: json!({}),
    }
}

/// The only `application_embedding_policies.failure_behavior` value that makes retrieval a hard
/// requirement.
///
/// The column has **no check constraint** (`migrations/0007…:109`), so any string can be
/// stored. Every value other than this one — including typos — is treated as the documented
/// default `'continue_without_semantic_retrieval'`. Failing open is the right default here: an
/// operator fat-fingering a policy field must not start returning `503` on the response path.
pub const FAILURE_BEHAVIOR_FAIL_REQUEST: &str = "fail_request";

/// A query embedding plus the model that produced it.
struct QueryEmbedding {
    vector: Vec<f32>,
    model_id: Option<Uuid>,
}

/// What one retrieval pass produced, before budgeting.
#[derive(Default)]
struct RetrievalOutcome {
    embedding_model_id: Option<Uuid>,
    memory_candidate_count: i32,
    chunk_candidate_count: i32,
    memories: Vec<Scored<MemoryCandidate>>,
    chunks: Vec<Scored<RagChunkCandidate>>,
}

/// Rows to ask the candidate query for, given how many the policy will keep.
///
/// Over-fetched so the re-rank can actually change the answer, and hard-capped so a
/// mis-configured `maximum_*_results` cannot ask PostgreSQL to sort the whole table inside a
/// request.
fn candidate_limit(maximum_results: i32) -> i64 {
    (i64::from(maximum_results.max(1)) * CANDIDATE_OVERFETCH).min(MAX_CANDIDATE_ROWS)
}

/// How many prior messages to read.
///
/// `full_until_limit` still has a ceiling — the token budget is the real bound, and reading an
/// unbounded conversation to then discard most of it is a denial-of-service shape, not a
/// feature. The multiplier gives the budget something to trim.
fn history_message_limit(policy: &ConversationPolicyRecord) -> i64 {
    let recent = i64::from(policy.maximum_recent_messages.max(1));
    match policy.history_strategy {
        HistoryStrategy::FullUntilLimit => (recent * 4).min(MAX_HISTORY_MESSAGES),
        HistoryStrategy::RecentMessages | HistoryStrategy::SummaryPlusRecent => recent,
    }
}

/// Hard ceiling on messages read for one plan, whatever the policy says.
const MAX_HISTORY_MESSAGES: i64 = 200;

fn history_strategy_label(strategy: HistoryStrategy) -> &'static str {
    match strategy {
        HistoryStrategy::RecentMessages => "recent_messages",
        HistoryStrategy::SummaryPlusRecent => "summary_plus_recent",
        HistoryStrategy::FullUntilLimit => "full_until_limit",
    }
}

/// What the context planner decided, threaded from [`ConversationService::prepare_response_conversation`]
/// through the execution command and out into the response's citations.
///
/// Carried on [`ConversationExecutionLink`] rather than re-queried later: the plan was already
/// computed in this request, and re-reading `context_plans` to build citations would open a
/// window where a concurrent write could put a different plan's ids on this response.
#[derive(Debug, Clone, Default)]
pub struct PlannedContext {
    /// Prepended to the caller's own messages before the command executes.
    pub messages: Vec<DomainMessage>,
    /// Exactly the memories and chunks that made it into [`Self::messages`].
    pub citations: Vec<PublicCitation>,
    /// The `context_plans` row id, `None` when nothing was planned.
    pub context_plan_id: Option<Uuid>,
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
    /// Because the sort key is the mutable `updated_at`, and the version trigger only ever
    /// moves it forward, a conversation touched mid-sweep is lifted **above** the cursor: an
    /// unreached row is silently skipped, and an already-returned row cannot come back. Pages
    /// are disjoint; callers needing an exactly-once sweep need a completeness check, not a
    /// de-duplication pass. Documented in full on
    /// `crate::infra::repositories::conversation`.
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

    /// Persists the user's turn, then plans the context that turn executes against.
    ///
    /// The comment this replaced said the opposite — "does not load history, summaries,
    /// memories, or RAG content into the prompt sent to the provider" — which stopped being
    /// true when plan 11 added the [`Self::plan_context`] call below, on the same function.
    /// The returned [`ConversationExecutionLink`] carries both the assembled messages and
    /// the citations for them.
    pub async fn prepare_response_conversation(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        execution_id: Uuid,
        route_hint: Option<String>,
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
        // Planning runs *after* the user's turn is persisted, so `message.sequence_number` is
        // the cutoff that keeps the current turn out of the replayed history block. It runs
        // before the command executes, which is what lets the assembled context reach the
        // provider on this same request.
        let context = self
            .plan_context(
                actor,
                execution_id,
                &conversation.id,
                message.sequence_number,
                &content,
            )
            .await?;
        Ok(Some(ConversationExecutionLink {
            conversation_id: conversation.id,
            user_message_id: message.id,
            route_hint,
            context,
        }))
    }

    // -----------------------------------------------------------------------------------
    // Context planning and retrieval (plan 11, Sub-Phases C, D and G).
    // -----------------------------------------------------------------------------------

    /// Retrieves, budgets and records the context for one turn.
    ///
    /// # Failure behaviour
    ///
    /// `application_embedding_policies.failure_behavior` decides what a retrieval failure does.
    /// The default, `'continue_without_semantic_retrieval'`, degrades: the turn proceeds with
    /// whatever context was assembled without retrieval, and the caller gets a `200` with empty
    /// citations. *A broken vector index must never take down the execution path.*
    ///
    /// The one non-default value, [`FAILURE_BEHAVIOR_FAIL_REQUEST`], surfaces
    /// `retrieval_unavailable` instead. Any other string is treated as the default — an
    /// unrecognised operator setting must not silently become fail-closed on the response path.
    ///
    /// `context_length_exceeded` is **not** subject to this: it is a caller-input problem, not a
    /// retrieval outage, and degrading it would mean silently truncating the caller's own turn.
    async fn plan_context(
        &self,
        actor: &Actor,
        execution_id: Uuid,
        conversation_public_id: &str,
        current_sequence: i64,
        current_input: &str,
    ) -> Result<PlannedContext, AppError> {
        // `internal_application_id` is the row id; `application_id` is the caller-facing
        // string. Retrieval scopes on the row id, so an actor without one — a system key
        // acting outside any application — plans no context rather than planning across
        // every application.
        let Some(application_id) = actor.internal_application_id else {
            return Ok(PlannedContext::default());
        };
        let pool = self.state.pool()?;
        let conversation_policy = self
            .repo
            .get_or_create_conversation_policy(application_id)
            .await?;
        let retrieval_policy = self
            .repo
            .get_or_create_retrieval_policy(application_id)
            .await?;

        let Some((conversation_uuid, summary_id, summary_text, _)) =
            find_conversation_context_anchor(pool, conversation_public_id).await?
        else {
            return Ok(PlannedContext::default());
        };

        let history = find_recent_messages(
            pool,
            conversation_public_id,
            current_sequence,
            history_message_limit(&conversation_policy),
        )
        .await?;

        let mut sections = ContextSections {
            summary: match (summary_id, summary_text) {
                // `history_strategy = 'recent_messages'` means exactly that: no summary block,
                // even when one exists.
                (Some(id), Some(text))
                    if conversation_policy.history_strategy != HistoryStrategy::RecentMessages =>
                {
                    Some((id, text))
                }
                _ => None,
            },
            history,
            memories: Vec::new(),
            chunks: Vec::new(),
        };

        let want_memory = retrieval_policy.enabled
            && retrieval_policy.memory_retrieval_enabled
            && conversation_policy.memory_retrieval_enabled;
        let want_rag = retrieval_policy.enabled && retrieval_policy.rag_retrieval_enabled;

        let mut retrieval_failure: Option<&'static str> = None;
        if want_memory || want_rag {
            let started = std::time::Instant::now();
            match self
                .retrieve(
                    application_id,
                    actor,
                    current_input,
                    &retrieval_policy,
                    want_memory,
                    want_rag,
                )
                .await
            {
                Ok(outcome) => {
                    let embedding_model_id = outcome.embedding_model_id;
                    sections.memories = outcome.memories;
                    sections.chunks = outcome.chunks;
                    self.state
                        .metrics
                        .record_retrieval_run(started.elapsed(), true);
                    insert_retrieval_run(
                        pool,
                        &RetrievalRunInsert {
                            execution_id,
                            conversation_uuid: Some(conversation_uuid),
                            query_hash: request_hash(current_input.as_bytes()),
                            embedding_model_id,
                            memory_candidate_count: outcome.memory_candidate_count,
                            memory_returned_count: sections.memories.len() as i32,
                            chunk_candidate_count: outcome.chunk_candidate_count,
                            chunk_returned_count: sections.chunks.len() as i32,
                            latency_ms: started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                            status: "completed",
                            failure_class: None,
                        },
                    )
                    .await?;
                }
                Err(class) => {
                    retrieval_failure = Some(class);
                    self.state
                        .metrics
                        .record_retrieval_run(started.elapsed(), false);
                    insert_retrieval_run(
                        pool,
                        &RetrievalRunInsert {
                            execution_id,
                            conversation_uuid: Some(conversation_uuid),
                            query_hash: request_hash(current_input.as_bytes()),
                            embedding_model_id: None,
                            memory_candidate_count: 0,
                            memory_returned_count: 0,
                            chunk_candidate_count: 0,
                            chunk_returned_count: 0,
                            latency_ms: started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                            status: "failed",
                            failure_class: Some(class),
                        },
                    )
                    .await?;
                }
            }
        }

        if retrieval_failure.is_some() && self.retrieval_is_required(application_id).await? {
            return Err(AppError::coded(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "retrieval_unavailable",
                "retrieval is required for this application but could not be served",
            ));
        }

        let assembled = assemble_context(
            sections,
            budget_tokens(current_input),
            i64::from(conversation_policy.maximum_history_tokens),
        )?;

        if assembled.messages.is_empty() && assembled.citations.is_empty() {
            // Nothing was planned, so there is no plan to record. Writing a `context_plans`
            // row of all-empty arrays on every unretrieved turn would bloat the table and make
            // the diagnostic surface useless.
            return Ok(PlannedContext::default());
        }

        let context_plan_id = insert_context_plan(
            pool,
            &ContextPlanInsert {
                execution_id,
                conversation_uuid: Some(conversation_uuid),
                strategy: history_strategy_label(conversation_policy.history_strategy).to_string(),
                estimated_input_tokens: assembled.estimated_input_tokens,
                included_message_ids: assembled.included_message_ids.clone(),
                included_summary_id: assembled.included_summary_id,
                included_memory_ids: assembled.included_memory_ids.clone(),
                included_chunk_ids: assembled.included_chunk_ids.clone(),
                excluded_counts: assembled.excluded_counts.clone(),
                truncation_reason: assembled.truncation_reason.clone(),
            },
        )
        .await?;

        Ok(PlannedContext {
            messages: assembled.messages,
            citations: assembled.citations,
            context_plan_id: Some(context_plan_id),
        })
    }

    /// Embeds the query and runs both retrieval arms.
    ///
    /// `Err` carries a `retrieval_runs.failure_class`, not an `AppError`: every failure here is
    /// a *retrieval* failure, and whether it becomes a caller-visible error is the
    /// `failure_behavior` decision made by the caller of this function, not by this function.
    async fn retrieve(
        &self,
        application_id: Uuid,
        actor: &Actor,
        query: &str,
        policy: &RetrievalPolicyRecord,
        want_memory: bool,
        want_rag: bool,
    ) -> Result<RetrievalOutcome, &'static str> {
        let pool = self.state.pool().map_err(|_| FAILURE_RETRIEVAL_BACKEND)?;
        let vector = self.embed_query(application_id, query).await?;
        let encoded = encode_vector_literal(&vector.vector);
        let scope = RetrievalScope {
            application_id,
            external_tenant_id: effective_tenant(actor),
            external_user_id: effective_user(actor),
        };
        let weights = RetrievalWeights {
            semantic: policy.semantic_weight,
            keyword: policy.keyword_weight,
            recency: policy.recency_weight,
            importance: policy.importance_weight,
        };

        let mut outcome = RetrievalOutcome {
            embedding_model_id: vector.model_id,
            ..RetrievalOutcome::default()
        };

        if want_memory {
            let limit = candidate_limit(policy.maximum_memory_results);
            let candidates = find_memory_candidates(pool, &scope, &encoded, limit)
                .await
                .map_err(|_| FAILURE_RETRIEVAL_BACKEND)?;
            outcome.memory_candidate_count = candidates.len() as i32;
            outcome.memories = rank_memories(
                query,
                candidates,
                weights,
                RetrievalLimits {
                    maximum_results: policy.maximum_memory_results.max(0) as usize,
                    minimum_score: policy.minimum_memory_score,
                    maximum_per_group: 0,
                    diversity_enabled: false,
                },
            );
        }

        if want_rag {
            let limit = candidate_limit(policy.maximum_chunk_results);
            let candidates = find_rag_chunk_candidates(
                pool,
                &scope,
                &encoded,
                &policy.allowed_collection_ids,
                limit,
            )
            .await
            .map_err(|_| FAILURE_RETRIEVAL_BACKEND)?;
            outcome.chunk_candidate_count = candidates.len() as i32;
            outcome.chunks = rank_chunks(
                query,
                candidates,
                weights,
                RetrievalLimits {
                    maximum_results: policy.maximum_chunk_results.max(0) as usize,
                    minimum_score: policy.minimum_chunk_score,
                    maximum_per_group: policy.maximum_chunks_per_document.max(0) as usize,
                    diversity_enabled: policy.diversity_enabled,
                },
            );
        }

        Ok(outcome)
    }

    /// Embeds the current turn with the application's configured embedding model.
    ///
    /// The same model ingestion used, reached from the application rather than the document —
    /// a query embedded by a different model than the corpus would produce distances that are
    /// arithmetically valid and semantically meaningless.
    async fn embed_query(
        &self,
        application_id: Uuid,
        query: &str,
    ) -> Result<QueryEmbedding, &'static str> {
        let pool = self.state.pool().map_err(|_| FAILURE_RETRIEVAL_BACKEND)?;
        let target = find_application_embedding_target(pool, application_id)
            .await
            .map_err(|_| FAILURE_RETRIEVAL_BACKEND)?
            .ok_or(FAILURE_EMBEDDING_NOT_CONFIGURED)?;
        let (Some(provider_id), Some(model_id)) = (target.provider_id, target.model_id) else {
            return Err(FAILURE_EMBEDDING_NOT_CONFIGURED);
        };
        if let Some(declared) = target.declared_dimension
            && declared != SUPPORTED_EMBEDDING_DIMENSION as i32
        {
            return Err(FAILURE_EMBEDDING_DIMENSION_UNSUPPORTED);
        }
        let (provider, model_key) = find_embedding_model_target(pool, provider_id, model_id)
            .await
            .map_err(|_| FAILURE_RETRIEVAL_BACKEND)?
            .ok_or(FAILURE_EMBEDDING_NOT_CONFIGURED)?;
        if !provider_type_supports_embeddings(provider.provider_type) {
            return Err(FAILURE_EMBEDDING_NOT_CONFIGURED);
        }
        let credential = resolve_embedding_credential(
            pool,
            &self.state.cipher,
            provider_id,
            provider.provider_type,
            application_id,
        )
        .await
        .map_err(|_| FAILURE_RETRIEVAL_BACKEND)?
        .ok_or(FAILURE_EMBEDDING_NOT_CONFIGURED)?;
        let handle = RigEmbeddingFactory::new()
            .build_embedding_model(
                &provider,
                &model_key,
                &credential,
                SUPPORTED_EMBEDDING_DIMENSION,
            )
            .await
            .map_err(|_| FAILURE_EMBEDDING_NOT_CONFIGURED)?;
        let started = std::time::Instant::now();
        let vectors = embed_texts(
            &handle,
            std::slice::from_ref(&query.to_string()),
            EmbeddingBatchPlan {
                batch_size: 1,
                deadline: std::time::Duration::from_millis(target.timeout_ms.max(1) as u64),
                dimension: SUPPORTED_EMBEDDING_DIMENSION,
            },
        )
        .await;
        self.state
            .metrics
            .record_embedding_batch_latency(started.elapsed());
        let mut vectors = vectors.map_err(|_| FAILURE_EMBEDDING_FAILED)?;
        if vectors.len() != 1 {
            return Err(FAILURE_EMBEDDING_FAILED);
        }
        Ok(QueryEmbedding {
            vector: vectors.remove(0),
            model_id: Some(model_id),
        })
    }

    /// Whether this application's `failure_behavior` makes retrieval a hard requirement.
    async fn retrieval_is_required(&self, application_id: Uuid) -> Result<bool, AppError> {
        let policy = self
            .repo
            .get_or_create_embedding_policy(application_id)
            .await?;
        Ok(policy.failure_behavior == FAILURE_BEHAVIOR_FAIL_REQUEST)
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
        // Automatic memory extraction (plan 11 Sub-Phase F). Deliberately after the assistant
        // message is persisted and audited, and deliberately infallible: the caller's response
        // has already been produced, and an extraction problem must not turn a successful
        // response into an error. Every failure is recorded on `memory_extraction_runs`.
        self.extract_memories(actor, ctx, link, response_id).await;
        // Automatic summarization (plan 11 Sub-Phase E). After the assistant message is
        // persisted, deliberately: the summary's `covers_through_sequence` must include the turn
        // that just completed, or the next run would re-read it and the boundary would lag one
        // turn behind forever. Infallible for the same reason extraction is.
        self.maybe_summarize_after_turn(actor, ctx, link).await;
        Ok(Some(message))
    }

    /// Extracts durable memories from the turn that just completed — plan 11 Sub-Phase F.
    ///
    /// # Why this is inline rather than enqueued
    ///
    /// Sub-Phase E's summarization is specified as *enqueued*, and the same reasoning would
    /// apply here — extraction is a second completion call, so it roughly doubles the latency
    /// of a turn on an application that enables it. It is nevertheless synchronous, because the
    /// alternatives available in this tree are worse:
    ///
    /// * **The queue does not execute job bodies yet.** `run_supervisor` wires
    ///   `queue::StubJobDispatcher` (`src/infra/workers.rs`), so an enqueued extraction would
    ///   be claimed and dropped. Shipping a feature whose work never runs, behind a flag that
    ///   says it does, is exactly the P0-1 shape this plan exists to remove.
    /// * **A detached `tokio::spawn` would outlive the request** and its pool guarantees, and
    ///   could not be asserted on deterministically without a `sleep` — which
    ///   `CONVENTIONS.md` §3 forbids and finding P2-12 is about.
    ///
    /// The cost is bounded by the flag: `automatic_extraction_enabled` defaults to `false`, so
    /// no existing application pays anything, and an operator who turns it on is opting into a
    /// second model call per turn — which is documented in `docs/memory-extraction.md`.
    ///
    /// **Reversal condition:** move the body behind `memory-extraction-retry` the moment a real
    /// `JobDispatcher` replaces the stub. This function is already shaped for it — it takes
    /// only ids and reads everything else from the database.
    ///
    /// # Failure policy
    ///
    /// Returns `()`. Every early return is a *decision*, not an error, and every genuine
    /// failure is written to `memory_extraction_runs.failure_class`. Nothing here can produce
    /// an `AppError`, so there is no path by which extraction changes the caller's status code.
    async fn extract_memories(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        link: &ConversationExecutionLink,
        response_id: Uuid,
    ) {
        let Some(application_id) = actor.internal_application_id else {
            return;
        };
        let Ok(pool) = self.state.pool() else {
            return;
        };
        let (Ok(memory_policy), Ok(conversation_policy)) = (
            self.repo.get_or_create_memory_policy(application_id).await,
            self.repo
                .get_or_create_conversation_policy(application_id)
                .await,
        ) else {
            return;
        };
        // Three independent switches, all of which must be on. `enabled` gates the memory
        // subsystem, `automatic_extraction_enabled` gates this feature, and the conversation
        // policy's own `memory_extraction_enabled` gates it again per the belt-and-braces rule
        // `plan_context` already applies to retrieval.
        if !memory_policy.enabled
            || !memory_policy.automatic_extraction_enabled
            || !conversation_policy.memory_enabled
            || !conversation_policy.memory_extraction_enabled
        {
            return;
        }
        // **The consent branch.** `None` means consent was withheld by at least one of the two
        // consent columns, and nothing at all is written — not a memory, not a run row. A run
        // row would itself be a record that Moira read the conversation for extraction
        // purposes, which is the thing consent was withheld for.
        let Some(status) = effective_extraction_status(
            conversation_policy.memory_consent_mode,
            memory_policy.consent_mode,
        ) else {
            return;
        };

        let Ok(Some((conversation_uuid, _, _, _))) =
            find_conversation_context_anchor(pool, &link.conversation_id).await
        else {
            return;
        };
        // `i64::MAX` rather than the current sequence: unlike the planner, extraction *wants*
        // the turn that just happened — it is the whole subject of the run.
        let Ok(history) = find_recent_messages(
            pool,
            &link.conversation_id,
            i64::MAX,
            EXTRACTION_TRANSCRIPT_MESSAGES,
        )
        .await
        else {
            return;
        };
        let turns: Vec<(String, String)> = history
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .clone()
                    .map(|content| (message.role.clone(), content))
            })
            .collect();
        if turns.is_empty() {
            // A conversation persisting no plaintext (`conversation_content_persistence` of
            // `'none'`/`'metadata_only'`/`'encrypted_content'`) has nothing to extract from.
            // Returning here rather than calling a model with an empty transcript is both the
            // cheaper and the more honest answer.
            return;
        }
        let input_message_ids: Vec<Uuid> =
            history.iter().map(|message| message.message_uuid).collect();

        let run_id = match insert_memory_extraction_run(
            pool,
            &MemoryExtractionRunInsert {
                conversation_uuid: Some(conversation_uuid),
                response_id: Some(response_id),
                input_message_ids,
                provider_model_id: None,
            },
        )
        .await
        {
            Ok(id) => id,
            Err(_) => return,
        };

        let outcome = self
            .run_extraction(
                actor,
                application_id,
                conversation_uuid,
                response_id,
                run_id,
                status,
                &memory_policy,
                &turns,
                link.route_hint.clone(),
            )
            .await;

        self.state.metrics.record_memory_extraction_run(
            outcome.accepted_count.max(0) as u64,
            outcome.rejected_count.max(0) as u64,
            outcome.status == "completed",
        );
        let _ = complete_memory_extraction_run(pool, run_id, &outcome).await;
        // Audited by count, never by content. `memory_extraction_runs` carries the same counts
        // and `audit_logs` is the operator-facing record that extraction ran at all — neither
        // holds a byte of the transcript or of the extracted memories.
        let _ = self
            .audit(
                actor,
                ctx,
                "memory.extraction.completed",
                "memory_extraction_run",
                Some(run_id.to_string()),
                json!({
                    "status": outcome.status,
                    "candidate_count": outcome.candidate_count,
                    "accepted_count": outcome.accepted_count,
                    "rejected_count": outcome.rejected_count,
                    "failure_class": outcome.failure_class,
                }),
            )
            .await;
    }

    /// The extraction call and the per-candidate loop.
    ///
    /// Split out so [`Self::extract_memories`] is the policy/consent gate and this is the work:
    /// every path here returns a [`MemoryExtractionRunOutcome`], so there is no way to leave
    /// the run row in `'running'` except by the process dying, which is the one case the row is
    /// there to make visible.
    #[allow(clippy::too_many_arguments)]
    async fn run_extraction(
        &self,
        actor: &Actor,
        application_id: Uuid,
        conversation_uuid: Uuid,
        response_id: Uuid,
        run_id: Uuid,
        status: MemoryStatus,
        policy: &MemoryPolicyRecord,
        turns: &[(String, String)],
        route_hint: Option<String>,
    ) -> MemoryExtractionRunOutcome {
        let Ok(pool) = self.state.pool() else {
            return failed_extraction(FAILURE_EXTRACTION_CALL_FAILED);
        };
        let command = ExecutionCommand {
            request_id: format!("memory-extraction-{run_id}"),
            execution_id: Uuid::now_v7(),
            identity: CallerRuntimeIdentity {
                actor_type: format!("{:?}", actor.actor_type),
                subject: actor.subject.clone(),
                external_user_id: actor.external_user_id.clone(),
                external_tenant_id: actor.external_tenant_id.clone(),
                application_id: Some(application_id),
                scopes: actor.scopes.clone(),
            },
            application_id: Some(application_id),
            external_tenant_id: effective_tenant(actor),
            external_user_id: effective_user(actor),
            messages: extraction_messages(&render_transcript(turns)),
            route_hint,
            provider_hint: None,
            model_hint: None,
            credential_hint: None,
            options: ExecutionOptions {
                // Zero temperature: the same transcript must produce the same candidates, or
                // the dedupe below is testing a moving target.
                temperature: Some(0.0),
                output_schema: Some(extraction_output_schema()),
                stream: false,
                ..ExecutionOptions::default()
            },
            metadata: json!({ "moira": { "purpose": "memory_extraction" } }),
        };

        let Ok(service) = MoiraExecutionService::new(self.state.clone()) else {
            return failed_extraction(FAILURE_EXTRACTION_CALL_FAILED);
        };
        let Ok(execution) = service.execute(command).await else {
            return failed_extraction(FAILURE_EXTRACTION_CALL_FAILED);
        };
        // `structured_output` is not populated by the execution kernel today — it is `None` on
        // both the streaming and non-streaming paths — so the schema-constrained reply arrives
        // as `output_text` and is parsed here. Preferring `structured_output` when it is
        // present means this call site needs no edit the day the kernel starts filling it.
        let raw = match execution
            .structured_output
            .as_ref()
            .map(|value| value.to_string())
            .or(execution.output_text)
        {
            Some(raw) => raw,
            None => return failed_extraction(FAILURE_EXTRACTION_CALL_FAILED),
        };
        let candidates = match parse_candidates(&raw) {
            Ok(candidates) => candidates,
            Err(class) => return failed_extraction(class),
        };

        let scope = RetrievalScope {
            application_id,
            external_tenant_id: effective_tenant(actor),
            external_user_id: effective_user(actor),
        };
        let extraction_policy = ExtractionPolicy {
            allowed_memory_types: policy.allowed_memory_types.clone(),
            allowed_sensitivity_levels: policy.allowed_sensitivity_levels.clone(),
            minimum_extraction_confidence: policy.minimum_extraction_confidence,
        };
        // The scope a new memory is written at: the narrowest one the actor can represent.
        // `user_application` needs an external user id and `conversation` needs a conversation
        // id, per `memory_records_scope_valid` — so an actor with no user id gets a
        // conversation-scoped memory rather than a broader tenant- or application-scoped one.
        // Widening on missing identity is how a memory ends up readable by callers who never
        // said it.
        let memory_scope = if effective_user(actor).is_some() {
            MemoryScope::UserApplication
        } else {
            MemoryScope::Conversation
        };

        let mut rejections: std::collections::BTreeMap<&'static str, i64> = Default::default();
        let mut accepted = 0i32;
        let mut rejected = 0i32;
        let mut duplicates = 0i64;
        let mut contradictions = 0i64;
        let candidate_count = candidates.len() as i32;

        for (index, proposed) in candidates.iter().enumerate() {
            if index >= MAXIMUM_CANDIDATES_PER_RUN {
                rejected += 1;
                *rejections
                    .entry(RejectionReason::RunCandidateLimit.label())
                    .or_default() += 1;
                continue;
            }
            let candidate = match classify_candidate(proposed, &extraction_policy) {
                Ok(candidate) => candidate,
                Err(reason) => {
                    rejected += 1;
                    *rejections.entry(reason.label()).or_default() += 1;
                    continue;
                }
            };
            let content_hash = memory_content_hash(&candidate.content);

            // Exact dedupe first: it needs no embedding, so an application with embeddings off
            // still gets duplicate suppression.
            match find_memory_by_content_hash(pool, &scope, &content_hash).await {
                Ok(Some(existing)) => {
                    let _ = confirm_memory(pool, existing).await;
                    duplicates += 1;
                    continue;
                }
                Ok(None) => {}
                // A failed lookup must not become a silent insert: an unavailable dedupe is a
                // reason to skip the candidate, not to write a row the check would have refused.
                Err(_) => {
                    rejected += 1;
                    *rejections.entry("dedupe_unavailable").or_default() += 1;
                    continue;
                }
            }

            let embedding = self
                .embed_memory_content(application_id, &candidate.content)
                .await;
            if let Some((encoded, _)) = embedding.as_ref()
                && let Ok(Some((existing, distance))) =
                    find_nearest_memory(pool, &scope, encoded).await
                && is_near_duplicate(distance, NEAR_DUPLICATE_MAX_DISTANCE)
            {
                let _ = confirm_memory(pool, existing).await;
                duplicates += 1;
                continue;
            }

            // Contradiction: the same subject, a different thing said about it. Recorded on the
            // new row rather than resolved — overwriting the old memory would destroy the
            // evidence that the caller changed their mind, which is the only thing that makes
            // the conflict reviewable.
            let mut contradicts = None;
            if let Some(key) = candidate.memory_key.as_deref()
                && let Ok(Some((existing, existing_hash))) =
                    find_memory_by_key(pool, &scope, key).await
                && existing_hash != content_hash
            {
                contradicts = Some(existing);
                contradictions += 1;
            }

            let id = Uuid::now_v7();
            let public_id = format!("mem_{id}");
            let tenant = effective_tenant(actor);
            let user = effective_user(actor);
            let insert = ExtractedMemoryInsert {
                id,
                public_id: &public_id,
                application_id,
                external_tenant_id: tenant.as_deref(),
                external_user_id: user.as_deref(),
                conversation_uuid: Some(conversation_uuid),
                scope: memory_scope,
                memory_type: candidate.memory_type,
                memory_key: candidate.memory_key.as_deref(),
                content: &candidate.content,
                content_hash: &content_hash,
                confidence: candidate.confidence,
                sensitivity: candidate.sensitivity,
                status,
                source_message_ids: Vec::new(),
                source_response_id: Some(response_id),
                source_extraction_run_id: run_id,
                contradicts_memory_id: contradicts,
                resolution_status: contradicts.map(|_| CONTRADICTION_UNRESOLVED),
            };
            if insert_extracted_memory(pool, &insert).await.is_err() {
                rejected += 1;
                *rejections.entry("insert_failed").or_default() += 1;
                continue;
            }
            accepted += 1;
            if let Some((encoded, model_id)) = embedding {
                let _ = insert_memory_embedding(pool, id, model_id, &encoded).await;
            }
        }

        MemoryExtractionRunOutcome {
            candidate_count,
            accepted_count: accepted,
            rejected_count: rejected,
            status: "completed",
            failure_class: None,
            metadata: json!({
                "rejections": rejections,
                "duplicates": duplicates,
                "contradictions": contradictions,
            }),
        }
    }

    /// Embeds one memory body, returning the pgvector literal and the model that produced it.
    ///
    /// `None` when the application has memory embeddings off or the embedding call failed. The
    /// caller degrades to exact-hash dedupe only — a missing embedding must not become a
    /// missing memory, and it must not become a *duplicate* memory either, which is why the
    /// exact check runs first and unconditionally.
    async fn embed_memory_content(
        &self,
        application_id: Uuid,
        content: &str,
    ) -> Option<(String, Option<Uuid>)> {
        let policy = self
            .repo
            .get_or_create_embedding_policy(application_id)
            .await
            .ok()?;
        if !policy.memory_embeddings_enabled {
            return None;
        }
        let embedding = self.embed_query(application_id, content).await.ok()?;
        Some((encode_vector_literal(&embedding.vector), embedding.model_id))
    }

    // -----------------------------------------------------------------------------------
    // Conversation summarization (plan 11, Sub-Phase E).
    // -----------------------------------------------------------------------------------

    /// Produces a new immutable summary version for a conversation.
    ///
    /// Backs `POST /api/v1/conversations/{id}/summarize` and, when
    /// [`Self::maybe_summarize_after_turn`] calls it, the automatic path.
    ///
    /// # What fills the slot Sub-Phase D left
    ///
    /// `find_conversation_context_anchor` has read the active summary since Sub-Phase D and has
    /// reliably returned `None`, because `conversation_summaries` had two readers and no writer.
    /// This is the writer. Nothing in the planner changes.
    ///
    /// # Ordering, and why every step is where it is
    ///
    /// 1. **Authorization and access before anything else**, through the same
    ///    `find_conversation_authorized` predicate every other conversation write uses — a
    ///    caller who cannot see the conversation must not be able to learn whether it has a
    ///    backlog, which a differently-ordered policy check would leak.
    /// 2. **The trigger decision before the lock**, so a request that was never going to
    ///    summarise does not open a connection or contend with a run that is doing real work.
    /// 3. **The lock before the model call**, which is the whole point of it.
    /// 4. **`covers_through_sequence` from the messages actually read**, not from the
    ///    conversation's current tail. When the backlog exceeds
    ///    [`SUMMARY_TRANSCRIPT_MESSAGES`] the run summarises the *oldest* uncovered messages and
    ///    advances the boundary only that far, so the next run continues contiguously. Setting
    ///    the boundary from the tail would silently claim coverage of messages no summary saw.
    pub async fn summarize_conversation(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        conversation_id: &str,
        force: bool,
    ) -> Result<SummarizationOutcome, AppError> {
        self.state
            .authz
            .require(actor, "moira:conversations:write")?;
        self.summarize_conversation_unscoped(actor, ctx, conversation_id, force)
            .await
    }

    /// The summarization operation without the endpoint's scope check.
    ///
    /// # Why the scope check is not in here
    ///
    /// `moira:conversations:write` is the **endpoint's** requirement, not the operation's. The
    /// automatic path runs on the response path, where the acting key is whatever issued
    /// `POST /api/v1/responses`. Leaving the scope check in the shared body would make automatic
    /// summarization silently never run for the ordinary caller while every flag said it was on:
    /// a feature that is enabled, configured, metric-seeded and dead. That is P0-1's shape — the
    /// finding this whole plan exists to remove — and it is the exact failure this split prevents.
    ///
    /// **Do not merge these two functions.** That instruction is worth nothing on its own, so
    /// here is the evidence, checkable in ten seconds:
    ///
    /// * `LifecycleFixture::enable_public_streaming` (`tests/support/mod.rs`) mints the consumer
    ///   key a caller actually uses: `moira:responses:create`, `moira:responses:stream`,
    ///   `moira:responses:read`, `moira:execution:override-route`, `moira:conversations:create`
    ///   and `moira:conversations:read` — and deliberately **not** `moira:conversations:write`.
    ///   That list predates this wave; it is what a response-plane key looks like, not a fixture
    ///   tuned to make this argument.
    /// * Move the `require` call into this function and
    ///   `automatic_summarization_runs_for_a_key_that_cannot_call_the_endpoint`
    ///   (`tests/conversation_summarization.rs`) fails: no summary is written at all.
    ///
    ///   **That test exists because this claim was false when it was first written here.** The
    ///   comment originally named a different test, and running the mutation showed the whole
    ///   suite stayed green — every case drove its turns with a key that happened to hold
    ///   `moira:conversations:write`, so nothing exercised the split. An earlier *fixture* fix
    ///   had removed the only scope-less turn. A claim in a comment is not a guard; this one is
    ///   now backed by a test that has been seen to fail.
    /// * The endpoint still enforces the scope, and
    ///   `the_summarize_endpoint_refuses_a_key_without_the_write_scope` is what proves it — so
    ///   the split widens nothing. Deleting *that* test is the other way to break this safely
    ///   looking change.
    ///
    /// Everything that is a *property of the data* stays here — the access predicate, the
    /// archived check, the policy, the trigger. Only the caller-plane scope moves out.
    async fn summarize_conversation_unscoped(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        conversation_id: &str,
        force: bool,
    ) -> Result<SummarizationOutcome, AppError> {
        let conversation = self
            .repo
            .find_conversation_authorized(conversation_id, &conversation_access(actor, false)?)
            .await?;
        if conversation.status == ConversationStatus::Archived {
            return Err(AppError::coded(
                axum::http::StatusCode::CONFLICT,
                "conversation_archived",
                "conversation is archived",
            ));
        }
        let application_id = required_application_id(actor)?;
        let pool = self.state.pool()?;
        let policy = self
            .repo
            .get_or_create_conversation_policy(application_id)
            .await?;

        let Some((conversation_uuid, _, _, _)) =
            find_conversation_context_anchor(pool, &conversation.id).await?
        else {
            return Err(AppError::coded(
                axum::http::StatusCode::NOT_FOUND,
                "conversation_not_found",
                "conversation not found",
            ));
        };

        let plan = self
            .plan_summarization(pool, conversation_uuid, &policy, force)
            .await?;

        let Some(lock) = SummarizationLock::try_acquire(pool, conversation_uuid).await? else {
            // Another run holds this conversation's lock. Not an error: the caller's intent is
            // already being satisfied, so nothing is started and nothing is counted.
            return Ok(SummarizationOutcome::AlreadyRunning);
        };
        let outcome = self
            .run_summarization(actor, pool, conversation_uuid, &policy, &plan)
            .await;
        lock.release().await;

        self.state.metrics.record_summarization_run(outcome.is_ok());
        let row = outcome?;
        // Audited by shape, never by content: the version, the coverage boundary and the token
        // count. `audit_logs` records that a summary was produced and how far it reaches; the
        // summary body itself lives only in `conversation_summaries`.
        self.audit(
            actor,
            ctx,
            "conversation.summary.created",
            "conversation_summary",
            Some(row.id.to_string()),
            json!({
                "conversation_id": conversation.id,
                "summary_version": row.summary_version,
                "covers_through_sequence": row.covers_through_sequence,
                "token_count": row.token_count,
                "message_count": plan.turns.len(),
                "forced": force,
            }),
        )
        .await?;
        Ok(SummarizationOutcome::Summarized(summary_record_from_row(
            &conversation.id,
            &row,
        )))
    }

    /// Reads the backlog and applies the trigger decision.
    ///
    /// Split out so [`Self::summarize_conversation`] reads as gate-then-work, and so every
    /// refusal is one `Err` with a named reason rather than a chain of early returns whose
    /// ordering is implicit.
    async fn plan_summarization(
        &self,
        pool: &sqlx::PgPool,
        conversation_uuid: Uuid,
        policy: &ConversationPolicyRecord,
        force: bool,
    ) -> Result<SummarizationPlan, AppError> {
        let previous = find_active_conversation_summary(pool, conversation_uuid).await?;
        let boundary = previous
            .as_ref()
            .map(|row| row.covers_through_sequence)
            .unwrap_or(0);
        // The uncapped count, deliberately: see `count_messages_after_sequence`. The token
        // estimate below is over the *capped* fetch instead, which is conservative in the safe
        // direction — it can only under-report the backlog, and under-reporting delays a
        // summarization rather than triggering one over content the run would not have read.
        let messages_since_summary =
            count_messages_after_sequence(pool, conversation_uuid, boundary).await?;
        let messages = find_messages_after_sequence(
            pool,
            conversation_uuid,
            boundary,
            SUMMARY_TRANSCRIPT_MESSAGES,
        )
        .await?;
        let tokens_since_summary = messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .map(budget_tokens)
            .sum();

        let summarization_policy = SummarizationPolicy {
            enabled: policy.summarization_enabled,
            trigger_tokens: policy.summary_trigger_tokens,
            minimum_messages_since_summary: policy.minimum_messages_since_summary,
            target_tokens: policy.summary_target_tokens,
        };
        let backlog = SummarizationBacklog {
            messages_since_summary,
            tokens_since_summary,
        };
        decide_summarization(&summarization_policy, backlog, force)
            .map_err(summarization_skip_error)?;

        let turns: Vec<(String, String)> = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .clone()
                    .map(|content| (message.role.clone(), content))
            })
            .collect();
        if turns.is_empty() {
            // The backlog is real but carries no plaintext — an application persisting
            // `none`/`metadata_only`/`encrypted_content`. Refused rather than sent to a model
            // with an empty transcript, which would bill for a summary of nothing and then
            // advance the coverage boundary over messages that were never summarised.
            return Err(AppError::coded_with_details(
                axum::http::StatusCode::CONFLICT,
                "summarization_not_needed",
                "the messages awaiting summarization carry no stored content",
                json!({ "reason": "no_persisted_content" }),
            ));
        }
        let covers_through_sequence = messages
            .iter()
            .map(|message| message.sequence_number)
            .max()
            .unwrap_or(boundary);

        Ok(SummarizationPlan {
            previous_summary: previous.and_then(|row| row.summary_text),
            turns,
            covers_through_sequence,
            target_tokens: policy.summary_target_tokens,
        })
    }

    /// The completion call and the write.
    ///
    /// `Err` is always a coded `summarization_failed`; every path returns one rather than
    /// leaking a provider error, so a failing model cannot put a provider message on a
    /// caller-visible response.
    async fn run_summarization(
        &self,
        actor: &Actor,
        pool: &sqlx::PgPool,
        conversation_uuid: Uuid,
        policy: &ConversationPolicyRecord,
        plan: &SummarizationPlan,
    ) -> Result<ConversationSummaryRow, AppError> {
        let _ = policy;
        // The route the conversation's own turns went to. `route_hint: None` would land on
        // `get_default_route()`, which is decision D-F3's exact trap — a route the caller never
        // used, and `NoEligibleModel` on a deployment with no active default.
        let route_hint = find_conversation_route_hint(pool, conversation_uuid)
            .await
            .unwrap_or(None);
        let command = ExecutionCommand {
            request_id: format!("summarization-{conversation_uuid}"),
            execution_id: Uuid::now_v7(),
            identity: CallerRuntimeIdentity {
                actor_type: format!("{:?}", actor.actor_type),
                subject: actor.subject.clone(),
                external_user_id: actor.external_user_id.clone(),
                external_tenant_id: actor.external_tenant_id.clone(),
                application_id: actor.internal_application_id,
                scopes: actor.scopes.clone(),
            },
            application_id: actor.internal_application_id,
            external_tenant_id: effective_tenant(actor),
            external_user_id: effective_user(actor),
            messages: summarization_messages(
                plan.previous_summary.as_deref(),
                &render_transcript(&plan.turns),
                plan.target_tokens,
            ),
            route_hint,
            provider_hint: None,
            model_hint: None,
            credential_hint: None,
            options: ExecutionOptions {
                // Zero temperature: two runs over the same backlog must produce the same
                // summary, or `summary_hash` stops being a content address of anything.
                temperature: Some(0.0),
                stream: false,
                ..ExecutionOptions::default()
            },
            metadata: json!({ "moira": { "purpose": "conversation_summarization" } }),
        };

        let service = MoiraExecutionService::new(self.state.clone())
            .map_err(|_| summarization_failed(FAILURE_SUMMARIZATION_CALL_FAILED))?;
        let execution = service
            .execute(command)
            .await
            .map_err(|_| summarization_failed(FAILURE_SUMMARIZATION_CALL_FAILED))?;
        // `structured_output` is not populated by the execution kernel today (finding F29) and
        // summarization asks for prose anyway, so the reply arrives as `output_text`. Preferring
        // `structured_output` when present costs nothing and means this call site needs no edit
        // the day the kernel starts filling it.
        let raw = execution
            .structured_output
            .as_ref()
            .map(|value| value.to_string())
            .or(execution.output_text)
            .ok_or_else(|| summarization_failed(FAILURE_SUMMARIZATION_CALL_FAILED))?;
        let summary = parse_summary(&raw).map_err(summarization_failed)?;

        let summary_hash = request_hash(summary.text.as_bytes());
        let token_count = budget_tokens(&summary.text);
        insert_conversation_summary(
            pool,
            &ConversationSummaryInsert {
                conversation_uuid,
                covers_through_sequence: plan.covers_through_sequence,
                summary_text: Some(summary.text.as_str()),
                summary_hash: &summary_hash,
                token_count: Some(token_count),
                provider_model_id: None,
            },
        )
        .await
    }

    /// Summarises after an assistant turn, if the policy says to — plan 11 Sub-Phase E.
    ///
    /// # Why this is inline rather than enqueued, again
    ///
    /// The plan body specifies summarization as *enqueued*, precisely so it does not add latency
    /// to the response path, and that is the right design. It is nevertheless synchronous here
    /// for the same reason decision D-F2 gave for extraction: `run_supervisor` wires
    /// `queue::StubJobDispatcher`, so an enqueued summarization would be **claimed and dropped**
    /// — a feature whose work never runs, behind a flag that says it does, which is P0-1's exact
    /// shape and the thing this plan exists to remove. A detached `tokio::spawn` would outlive
    /// the request and could not be asserted on without a `sleep`, which CONVENTIONS.md §3
    /// forbids.
    ///
    /// The cost is bounded twice over: `summarization_enabled` defaults to `false`, and even
    /// when on, a run happens only once the backlog crosses both thresholds — so the amortised
    /// cost is one extra completion per `summary_trigger_tokens` of conversation, not one per
    /// turn.
    ///
    /// **Reversal condition:** move the body behind `conversation-summarization-retry` the moment
    /// a real `JobDispatcher` replaces the stub. [`Self::summarize_conversation`] already takes
    /// only ids and reads everything else from the database.
    ///
    /// # Failure policy
    ///
    /// Returns `()`. Every outcome — refused by the trigger, already locked, the model failing —
    /// is a decision, not an error. Nothing here can change the caller's status code, which is
    /// the same fail-open rule retrieval and extraction follow: the caller's response has
    /// already been produced.
    async fn maybe_summarize_after_turn(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        link: &ConversationExecutionLink,
    ) {
        let _ = self
            .summarize_conversation_unscoped(actor, ctx, &link.conversation_id, false)
            .await;
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
        let content_hash = memory_content_hash(&request.content);
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
        // Embed the memory so it can actually be retrieved.
        //
        // Wave 1 wired `rag_chunk_embeddings` and left `memory_embeddings` with no writer at
        // all, which would have made `find_memory_candidates` unreachable in production and
        // testable only by fabricating rows — a suite that asserts nothing. Written after the
        // record, not inside its transaction: an embedding failure must not lose a memory the
        // caller successfully stored, and an unembedded memory is still a valid memory. It is
        // simply not semantically retrievable, which `memory_embeddings` being empty says
        // honestly.
        self.embed_new_memory(application_id, id, &request.content)
            .await;
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

    /// Writes a `memory_embeddings` row, best-effort.
    ///
    /// Silent on failure by design — see the call site. The absence of the row is the record of
    /// the failure; there is no status column on `memory_records` that could claim otherwise,
    /// and inventing one would repeat P0-1.
    async fn embed_new_memory(&self, application_id: Uuid, memory_id: Uuid, content: &str) {
        let Ok(pool) = self.state.pool() else {
            return;
        };
        let Some((encoded, model_id)) = self.embed_memory_content(application_id, content).await
        else {
            return;
        };
        let _ = insert_memory_embedding(pool, memory_id, model_id, &encoded).await;
    }

    /// Lists memories, paging by `(updated_at, id)`.
    ///
    /// Same mutable-sort-key caveat as [`Self::list_conversations`]: a memory whose
    /// `updated_at` moves during a sweep is lifted above the cursor, so it is **missed**, not
    /// re-seen.
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
        // Same content address `create_memory` writes — see `memory_content_hash`. A patch that
        // wrote a different *kind* of digest would leave one table holding two incomparable
        // formats, which is F14's silent-mismatch failure re-created from a format split
        // instead of a pepper rotation.
        let hash = request.content.as_deref().map(memory_content_hash);
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
        // Same pre-transaction pipeline as `/ingest`: a document created with inline content is
        // an ingestion entry point too, and a version written by this path with no chunks would
        // be as dishonest as one written by `/ingest`.
        let context = find_collection_ingestion_context(self.state.pool()?, collection_id).await?;
        let plan = self
            .plan_rag_ingestion(
                context.as_ref(),
                context
                    .as_ref()
                    .map(|context| context.application_id)
                    .unwrap_or_default(),
                request.content.as_deref(),
                Some(request.mime_type.as_str()),
            )
            .await?;
        self.state.metrics.record_rag_ingestion_run(
            plan.chunk_count() as u64,
            plan.embedded_chunk_count() as u64,
            plan.failure_class.is_none(),
        );
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
                        &plan,
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
    /// This is the **only** entry point, deliberately. It used to have a `list_rag_documents`
    /// sibling that took a bare `limit` and passed `None` for the cursor, and the handler
    /// called that one: the route advertised no `cursor` parameter, hard-coded `limit = 50`,
    /// and still returned a genuine `next_cursor` with `has_more: true` that had nowhere to go.
    /// Every document past the fiftieth was unreachable over HTTP. Deleting the convenience
    /// overload is what stops that from being re-introduced — a caller must now decide what to
    /// do about the cursor in order to compile.
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
        // Chunk and embed before the runner opens its transaction; see `plan_rag_ingestion`.
        let context = find_document_ingestion_context(self.state.pool()?, document_id).await?;
        let plan = self
            .plan_rag_ingestion(
                context.as_ref(),
                context
                    .as_ref()
                    .map(|context| context.application_id)
                    .unwrap_or_default(),
                Some(content),
                None,
            )
            .await?;
        self.state.metrics.record_rag_ingestion_run(
            plan.chunk_count() as u64,
            plan.embedded_chunk_count() as u64,
            plan.failure_class.is_none(),
        );
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
                        &plan,
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

    // -----------------------------------------------------------------------------------
    // RAG ingestion pipeline (plan 11, Sub-Phases A and B).
    // -----------------------------------------------------------------------------------

    /// Builds the ingestion plan for one document version — chunks, hashes and embeddings —
    /// **before** the command transaction opens.
    ///
    /// Ordering is the whole point. `ingest_rag_document_with_connection` runs inside the
    /// `AdminCommandRunner` transaction, holding `select … for update` on the document row and
    /// the idempotency advisory lock. Embedding is a network call, so doing it there would pin
    /// a pooled connection and both locks across an unbounded await, once per batch. Everything
    /// expensive therefore happens here, and the transaction only writes.
    ///
    /// Two consequences, both accepted deliberately:
    ///
    /// * Two concurrent requests carrying the same `Idempotency-Key` both embed, and only one
    ///   of them wins the envelope. That wastes provider spend; it does not corrupt anything,
    ///   because the loser's chunks are never written.
    /// * A `422 rag_document_too_large` is raised before the key is claimed, which matches how
    ///   every other validation on this surface already behaves — a rejected request must never
    ///   occupy an idempotency key.
    async fn plan_rag_ingestion(
        &self,
        context: Option<&RagIngestionContext>,
        application_id: Uuid,
        content: Option<&str>,
        mime_type_hint: Option<&str>,
    ) -> Result<RagIngestionPlan, AppError> {
        let Some(content) = content.filter(|value| !value.trim().is_empty()) else {
            return Ok(RagIngestionPlan::empty());
        };
        // A missing context means the document or collection does not exist. The 404 is left to
        // the transaction, which is where it has always been raised — moving it here would
        // change whether a not-found ingest claims its idempotency key.
        let Some(context) = context else {
            return Ok(RagIngestionPlan::empty());
        };

        let mime_type = context.mime_type.as_deref().or(mime_type_hint);
        let strategy = ChunkStrategy::for_mime_type(mime_type);
        let limits = ChunkingLimits {
            max_chunk_chars: self.state.settings.rag.max_chunk_chars,
            max_chunks_per_document: self.state.settings.rag.max_chunks_per_document,
        };
        let chunks = prepare_chunks(content, strategy, limits)?;
        let strategy_label = strategy.as_str();
        if chunks.is_empty() {
            return Ok(RagIngestionPlan::empty());
        }

        // No embedding policy, or one with `rag_embeddings_enabled = false`. Not a failure:
        // the application has not asked for semantic indexing, and the chunks it did ask for
        // are stored. `embedded_chunk_count = 0` on the run row is how that stays visible.
        let Some(target) = context.embedding.as_ref() else {
            return Ok(RagIngestionPlan {
                chunks,
                strategy: strategy_label,
                embedding_model_id: None,
                embedding_dimension: None,
                failure_class: None,
            });
        };

        let (Some(provider_id), Some(model_id)) = (target.provider_id, target.model_id) else {
            return Ok(RagIngestionPlan::failed(
                chunks,
                strategy_label,
                FAILURE_EMBEDDING_NOT_CONFIGURED,
            ));
        };
        if let Some(declared) = target.declared_dimension
            && declared != SUPPORTED_EMBEDDING_DIMENSION as i32
        {
            return Ok(RagIngestionPlan::failed(
                chunks,
                strategy_label,
                FAILURE_EMBEDDING_DIMENSION_UNSUPPORTED,
            ));
        }

        let pool = self.state.pool()?;
        let Some((provider, model_key)) =
            find_embedding_model_target(pool, provider_id, model_id).await?
        else {
            return Ok(RagIngestionPlan::failed(
                chunks,
                strategy_label,
                FAILURE_EMBEDDING_NOT_CONFIGURED,
            ));
        };
        if !provider_type_supports_embeddings(provider.provider_type) {
            return Ok(RagIngestionPlan::failed(
                chunks,
                strategy_label,
                FAILURE_EMBEDDING_NOT_CONFIGURED,
            ));
        }
        let Some(credential) = resolve_embedding_credential(
            pool,
            &self.state.cipher,
            provider_id,
            provider.provider_type,
            application_id,
        )
        .await?
        else {
            return Ok(RagIngestionPlan::failed(
                chunks,
                strategy_label,
                FAILURE_EMBEDDING_NOT_CONFIGURED,
            ));
        };

        let handle = match RigEmbeddingFactory::new()
            .build_embedding_model(
                &provider,
                &model_key,
                &credential,
                SUPPORTED_EMBEDDING_DIMENSION,
            )
            .await
        {
            Ok(handle) => handle,
            // A provider that cannot embed, or a client that will not build, is a
            // configuration problem — recorded on the run row, not raised at the caller, so
            // the chunks that did succeed are still stored and the version is honestly
            // `'failed'` rather than silently `'indexed'`.
            Err(_) => {
                return Ok(RagIngestionPlan::failed(
                    chunks,
                    strategy_label,
                    FAILURE_EMBEDDING_NOT_CONFIGURED,
                ));
            }
        };

        let texts: Vec<String> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
        let plan = EmbeddingBatchPlan {
            batch_size: target.batch_size.max(1) as usize,
            deadline: std::time::Duration::from_millis(target.timeout_ms.max(1) as u64),
            dimension: SUPPORTED_EMBEDDING_DIMENSION,
        };
        let started = std::time::Instant::now();
        let vectors = match embed_texts(&handle, &texts, plan).await {
            Ok(vectors) => vectors,
            Err(_) => {
                self.state
                    .metrics
                    .record_embedding_batch_latency(started.elapsed());
                return Ok(RagIngestionPlan::failed(
                    chunks,
                    strategy_label,
                    FAILURE_EMBEDDING_FAILED,
                ));
            }
        };
        self.state
            .metrics
            .record_embedding_batch_latency(started.elapsed());

        // `embed_texts` guarantees one vector per input and the right width, so this zip
        // cannot silently drop a chunk. Asserted rather than assumed: a mismatch here would
        // store an embedding against the wrong chunk, which is undetectable downstream.
        if vectors.len() != chunks.len() {
            return Ok(RagIngestionPlan::failed(
                chunks,
                strategy_label,
                FAILURE_EMBEDDING_FAILED,
            ));
        }
        let mut chunks = chunks;
        for (chunk, vector) in chunks.iter_mut().zip(vectors) {
            chunk.embedding = Some(vector);
        }
        Ok(RagIngestionPlan {
            chunks,
            strategy: strategy_label,
            embedding_model_id: Some(model_id),
            embedding_dimension: Some(SUPPORTED_EMBEDDING_DIMENSION as i32),
            failure_class: None,
        })
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
/// The actor fingerprint comes from `admin::actor_fingerprint`, which since plan 06
/// (Module 16 / P2-15) is the only formula in the crate: `runtime_admin` and `public` no
/// longer keep their own weaker copies, so there is nothing left to pick wrongly here.
/// Reusing it is still load-bearing rather than stylistic — the fingerprint is a column of
/// the `idempotency_records` unique index and an input to the advisory-lock key, so a
/// divergent copy would silently un-scope replay for these four routes.
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

/// The content address stored in `memory_records.content_hash`.
///
/// **Decision (finding F14).** This is [`crate::security::request_hash`] — a plain, unkeyed
/// SHA-256 aliasing `secret_fingerprint` — and deliberately **not**
/// `IdempotencyHasher::hash`, which is what it used to be and what the neighbouring
/// `conversation_messages.content_hash` still is.
///
/// # Why the two tables diverge
///
/// `IdempotencyHasher`'s rotation contract (`src/security/idempotency.rs`) accepts only the
/// *active* pepper, and justifies that narrowness with a retention argument: every
/// `idempotency_records` row expires within 24 hours, so old-pepper rows age out on their own.
/// **`memory_records` has no such retention.** Its rows are long-lived by design — a nullable
/// `valid_until` and a `status` that stays `'active'` indefinitely — so a pepper rotation would
/// not produce a bounded window, it would permanently orphan every stored hash. Exact-match
/// memory dedupe would then stop matching, silently, with no error and no log line. The hasher
/// is right for its namesake table and was reused for one with a fundamentally different
/// lifetime.
///
/// The same admitting rule plan 11 wrote for `rag_chunks.chunk_hash`
/// (`src/orchestration/ingestion.rs`) is applied here per *table*, and `memory_records` passes
/// all three clauses where `conversation_messages` fails the first:
///
/// * **(a) not caller-visible.** [`MemoryRecord`] has no `content_hash` field and no schema in
///   `docs/openapi.json` carries one for a memory. `ConversationMessageRecord` does, which is
///   why *that* column stays peppered: an unkeyed digest of message content, handed to the
///   caller, is an offline verifier for content the schema otherwise expects to hold encrypted.
/// * **(b) never a caller-supplied lookup key.** `MemoryCreateRequest`, `MemoryPatchRequest`
///   and `MemoryQuery` all carry `deny_unknown_fields` and none of them has a hash field; the
///   only caller-supplied memory lookup key is the `mem_…` public id.
/// * **(c) never a cross-application comparison.** Every `memory_records` read is bound by
///   `application_id` — `find_memory_authorized`, `list_memories_authorized` and
///   `find_memory_candidates` all require it in every arm — so a dedupe built on this value
///   cannot become an existence oracle over another application's memories.
///
///   **Plan 11 Sub-Phase F built that dedupe, so clause (c) now has a second set of call
///   sites.** `find_memory_by_content_hash` compares this exact value across rows, and
///   `find_nearest_memory`/`find_memory_by_key` compare content by other means; all three go
///   through `MEMORY_SCOPE_PREDICATE` in `src/infra/repositories/conversation.rs`, which binds
///   `application_id` in every arm, and `every_memory_read_shares_the_isolation_predicate`
///   asserts it against the emitted SQL rather than against behaviour.
///
/// # Reversal condition
///
/// Go back to a keyed hash — and pair it with a re-hash-on-rotation procedure, because the
/// lifetime problem above does not go away — the moment any one of those three clauses stops
/// holding: `content_hash` appears on `MemoryRecord` or any other caller-visible DTO, a filter
/// or lookup accepts a caller-supplied hash, or a dedupe/similarity query drops the
/// `application_id` predicate.
fn memory_content_hash(content: &str) -> String {
    request_hash(content.as_bytes())
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

/// What one summarization run will feed the model, once the trigger has said yes.
#[derive(Debug)]
struct SummarizationPlan {
    /// The active summary's text, when there is one and it was persisted in plaintext.
    previous_summary: Option<String>,
    /// `(role, text)` for the messages this run covers, oldest first.
    turns: Vec<(String, String)>,
    /// The `sequence_number` of the newest message this run read — never the conversation's
    /// current tail. See `summarize_conversation`'s step 4.
    covers_through_sequence: i64,
    target_tokens: i32,
}

/// Maps a refusal from `decide_summarization` onto the caller-visible error.
///
/// Two codes, not four. `summarization_disabled` is a *policy* condition an operator can change;
/// the other three are *state* conditions about this conversation right now, and they share
/// `summarization_not_needed` with the specific reason in `details`. Minting a code per skip
/// reason would put three unreachable-in-practice codes in the catalog for one distinction a
/// caller acts on identically.
fn summarization_skip_error(skip: SummarizationSkip) -> AppError {
    match skip {
        SummarizationSkip::Disabled => AppError::coded(
            axum::http::StatusCode::FORBIDDEN,
            "summarization_disabled",
            "conversation summarization is disabled for this application",
        ),
        other => AppError::coded_with_details(
            axum::http::StatusCode::CONFLICT,
            "summarization_not_needed",
            "there is nothing new to summarize in this conversation",
            json!({ "reason": other.label() }),
        ),
    }
}

/// A summarization run that reached the model and produced nothing storable.
///
/// `reason` is one of the module-level failure classes in
/// [`crate::application::summarization`], never a provider message: a provider body must not
/// reach a caller-visible envelope, which is the sanitisation contract the Rig boundary already
/// holds everywhere else.
fn summarization_failed(reason: &'static str) -> AppError {
    AppError::coded_with_details(
        axum::http::StatusCode::BAD_GATEWAY,
        "summarization_failed",
        "the conversation could not be summarized",
        json!({ "reason": reason }),
    )
}

/// Maps a stored summary row onto the caller-facing DTO.
///
/// The public id is derived — `conversation_summaries` has no `public_id` column, and adding one
/// would be a migration for an identifier no endpoint accepts as input. The prefix matches the
/// convention every other public id here follows so a caller reading a log can tell what it is.
///
/// **`summary_hash` is deliberately not mapped.** It is an unkeyed content address over the
/// summary bytes, and putting it on a caller-visible response would make it an offline oracle
/// over candidate plaintexts — the first clause of finding F14's admitting rule, which is why
/// `conversation_messages.content_hash` stayed peppered while `memory_records.content_hash`
/// became a content address.
fn summary_record_from_row(
    conversation_public_id: &str,
    row: &ConversationSummaryRow,
) -> ConversationSummaryRecord {
    ConversationSummaryRecord {
        id: format!("csum_{}", row.id),
        object: "conversation.summary".to_string(),
        conversation_id: conversation_public_id.to_string(),
        summary_version: row.summary_version,
        covers_through_sequence: row.covers_through_sequence,
        summary_text: row.summary_text.clone(),
        token_count: row.token_count,
        created_at: row.created_at,
        superseded_at: row.superseded_at,
    }
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

/// Whether caller-supplied memory content looks like credential material.
///
/// Shares [`SECRET_NEEDLES`] with the extraction path rather than keeping its own copy. The two
/// *functions* stay separate — this one raises a caller-facing 422, the other records a
/// rejection reason on a run row nobody sees — but a second copy of the list is how the
/// caller-supplied and model-supplied paths drift apart, and the model-supplied one is the
/// worse of the two to be lax about.
fn contains_secret_like_text(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    SECRET_NEEDLES.iter().any(|needle| lower.contains(needle))
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
        let order = crate::application::ContextPlanner::deterministic_phase_five_order();
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
