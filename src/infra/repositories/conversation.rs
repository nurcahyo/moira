//! Conversation-, memory- and RAG-domain persistence.
//!
//! # Keyset pagination contract (plan 04, finding P1-4)
//!
//! The five `list_*` methods below take an optional cursor and return each record paired
//! with **the cursor for its own row**, in the query's existing sort order. Three rules hold
//! across all five and must not be relaxed:
//!
//! 1. **The sort key is not uniform, and no query's sort key may be changed to make it so.**
//!    Conversations and memories order by `(updated_at desc, id desc)`; RAG collections and
//!    documents by `(created_at desc, id desc)`; messages by `sequence_number asc`. The
//!    keyset predicate is therefore `<` for the four descending lists and `>` for the one
//!    ascending list. Changing a sort key to unify them would silently reorder every
//!    existing caller's results.
//! 2. **Cursor values are bound parameters, never interpolated.** They arrive here already
//!    parsed into `DateTime<Utc>`/`Uuid`/`i64` by [`crate::domain::ListCursor`], so a
//!    client-supplied cursor can never reach the query text.
//! 3. **`limit` is the exact row count to fetch**, including the caller's over-fetch row.
//!    The repository neither adds one nor clamps; `crate::application::conversation` owns
//!    both, so the trim/`has_more` arithmetic lives in exactly one place.
//!
//! The pairing exists because the public DTOs expose `id` as the caller-facing `public_id`
//! string (`"conv_…"`), never the table's `uuid` primary key that the sort key and the
//! cursor are built from. Re-deriving the uuid by parsing the public id would work today and
//! break silently the day a public id stops being `prefix_{uuid}`. The repository owns the
//! sort key, so it also mints the cursor; the application layer only trims, counts and
//! encodes.
//!
//! ## Consequence of keying conversations and memories on `updated_at`
//!
//! Those two lists sort by a **mutable** column, so a row updated part-way through a
//! pagination sweep changes position mid-sweep. The direction of that failure is not
//! symmetric, and getting it backwards sends a caller's reconciliation logic the wrong way:
//!
//! `updated_at` is set by the `moira_bump_resource_version` trigger
//! (`migrations/0004_admin_api_contract.sql`) to `now()` on every update, so it only ever
//! moves **forward**. Under `order by updated_at desc, id desc` with the keyset predicate
//! `(updated_at, id) < cursor`, "forward in time" means "further above the cursor":
//!
//! * A row that has **already been returned** sits above the cursor. Bumping it pushes it
//!   further above, where the predicate still excludes it. It cannot come back.
//!   **Duplicates do not happen.**
//! * A row that has **not been reached yet** sits below the cursor. Bumping it lifts it
//!   above, where the predicate now excludes it. The sweep never sees it.
//!   **Rows are silently skipped.**
//!
//! Measured on Postgres with four rows and `limit 2`: after bumping an unreached row, page 2
//! returned one row instead of two and the whole sweep saw three of the four; after bumping
//! an already-returned row, page 2 was byte-identical and contained no duplicate.
//!
//! So the pages of a sweep **are disjoint**, and de-duplicating by `id` protects against
//! nothing. What a caller who needs an exactly-once sweep actually needs is a
//! **completeness** check — a total count, a follow-up pass, or a sweep keyed on an
//! immutable column — because the rows it is missing are the ones that were busiest.
//!
//! The one way a row can move *backwards* is a transaction that started before the previous
//! page was served and commits after it: `now()` is transaction-start time, so its
//! `updated_at` can land below the cursor and the row can be returned a second time. That
//! window is bounded by the length of the overlapping write transaction, not by the length
//! of the sweep, and it is the only source of duplicates here.
//!
//! All of this is standard keyset behaviour for a mutable sort key and is accepted
//! deliberately — the alternative is changing the sort key, which rule 1 forbids.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        ConversationCreateRequest, ConversationMessageQuery, ConversationMessageRecord,
        ConversationMessageRole, ConversationMessageType, ConversationPatchRequest,
        ConversationPolicyPutRequest, ConversationPolicyRecord, ConversationQuery,
        ConversationRecord, ConversationStatus, EmbeddingPolicyPutRequest, EmbeddingPolicyRecord,
        ListCursor, MemoryCreateRequest, MemoryPatchRequest, MemoryPolicyPutRequest,
        MemoryPolicyRecord, MemoryQuery, MemoryRecord, MemoryScope, RagCollectionCreateRequest,
        RagCollectionPatchRequest, RagCollectionQuery, RagCollectionRecord, RagCollectionStatus,
        RagDocumentCreateRequest, RagDocumentIngestRequest, RagDocumentRecord,
        RetrievalPolicyPutRequest, RetrievalPolicyRecord, SeqCursor,
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
        rag_collection_visibility_to_db, rag_document_record_from_row, rag_ingestion_status_to_db,
        retrieval_policy_record_from_row,
    },
    orchestration::{RagIngestionPlan, encode_vector_literal},
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

// ---------------------------------------------------------------------------
// Keyset list queries.
//
// These five live as `const`s rather than inline literals for one reason: being compile-time
// constants is itself the proof that no cursor value can ever reach the query text. A `const`
// cannot be built by `format!`, so the SQL-injection question is settled by the type system
// rather than by review, and the unit tests below can assert the shape of each predicate
// (`<` for the four descending lists, `>` for the one ascending list, `$N` placeholders only)
// without a database.
// ---------------------------------------------------------------------------

const LIST_CONVERSATIONS_SQL: &str = r#"
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
              and ($11::timestamptz is null or (c.updated_at, c.id) < ($11, $12::uuid))
            order by c.updated_at desc, c.id desc
            limit $13
            "#;

// The conversation is resolved by a scalar subquery rather than a join, and that is a
// performance decision, not a style one. Measured on PostgreSQL 18.3 against 60k messages in
// one conversation, paging from sequence 30000 with `limit 51`:
//
//   join form:     Hash Join over a Seq Scan of conversation_messages, keyset applied as a
//                  post-fetch Filter (30,000 rows discarded), top-N heapsort,
//                  1,468 buffers, 28.5 ms
//   subquery form: InitPlan on conversations_public_id_key, then a plain Index Scan on
//                  conversation_messages_sequence_unique with the whole predicate as an
//                  Index Cond and NO sort node, 10 buffers, 0.10 ms
//
// The join form cannot be fixed with an index. `conversation_id` arrives from the join, so it
// is not available as an index-scan constant; forcing a nested loop (`enable_hashjoin=off`)
// still produced a full bitmap scan and a sort. The subquery makes it an InitPlan constant,
// which is what lets the existing `(conversation_id, sequence_number)` unique index supply
// both the filter and the ordering. Reverting this to a join silently reintroduces a scan
// that grows with the conversation and gets *slower* the deeper the caller pages — the exact
// failure keyset pagination exists to avoid.
//
// Semantics are unchanged: `conversation_messages.conversation_id` is `not null` with an FK to
// `conversations(id)`, and `public_id` is unique, so the subquery yields exactly the row the
// join matched — and yields no rows (not an error) for an unknown `public_id`, as before.
const LIST_MESSAGES_SQL: &str = r#"
            select m.*
            from conversation_messages m
            where m.conversation_id = (
                    select c.id from conversations c where c.public_id = $1
                  )
              and m.deleted_at is null
              and ($2::bigint is null or m.sequence_number < $2)
              and ($3::bigint is null or m.sequence_number > $3)
              and ($4::bigint is null or m.sequence_number > $4)
            order by m.sequence_number asc
            limit $5
            "#;

const LIST_MEMORIES_SQL: &str = r#"
            select m.*
            from memory_records m
            where m.deleted_at is null
              and ($1::boolean
                   or (m.application_id = $2
                       and ($3::text is null or m.external_tenant_id = $3)
                       and ($4::text is null or m.external_user_id = $4)))
              and ($5::text is null or m.memory_type = $5)
              and ($6::text is null or m.status = $6)
              and ($7::timestamptz is null or (m.updated_at, m.id) < ($7, $8::uuid))
            order by m.updated_at desc, m.id desc
            limit $9
            "#;

const LIST_RAG_COLLECTIONS_SQL: &str = r#"
            select *
            from rag_collections
            where deleted_at is null
              and ($1::uuid is null or application_id = $1)
              and ($2::text is null or external_tenant_id = $2)
              and ($3::text is null or status = $3)
              and ($4::timestamptz is null or (created_at, id) < ($4, $5::uuid))
            order by created_at desc, id desc
            limit $6
            "#;

// Same scalar-subquery rewrite as `LIST_MESSAGES_SQL`, for the same measured reason, and here
// it is only half the fix. Against 60k documents in one collection, paging from the midpoint
// with `limit 51`:
//
//   join, no index:       Seq Scan + top-N heapsort, 2,900 buffers, 21.4 ms
//   join, WITH the index: still Seq Scan + top-N heapsort, 2,906 buffers, 31.4 ms
//   subquery, no index:   still Seq Scan + top-N heapsort, 2,900 buffers, 11.7 ms
//   subquery + index:     Index Scan, whole keyset as an Index Cond, no sort,
//                         36 buffers, 0.07 ms
//
// So neither change works alone: the index is unusable while `collection_id` comes from a
// join, and the subquery alone leaves the planner with no ordered index to walk. The index is
// `rag_documents_collection_created_cursor_idx` in `migrations/0010_list_cursor_indexes.sql`;
// it exists because the pre-existing `rag_documents_collection_cursor_idx` puts `status`
// between `collection_id` and `created_at`, and this query does not constrain `status`.
const LIST_RAG_DOCUMENTS_SQL: &str = r#"
            select d.*
            from rag_documents d
            where d.collection_id = (
                    select c.id from rag_collections c where c.public_id = $1
                  )
              and d.deleted_at is null
              and ($2::timestamptz is null or (d.created_at, d.id) < ($2, $3::uuid))
            order by d.created_at desc, d.id desc
            limit $4
            "#;

/// The conversation/memory/RAG persistence surface: the four per-application policy documents,
/// conversation and message rows, extracted memories, and the RAG collection/document tables.
///
/// Extracted as a trait (plan 06, Module 8 / P2-3) so `ConversationService` can be unit-tested
/// against a fake instead of a live Postgres. Mirrors [`AdminRepository`](super::AdminRepository):
/// the trait carries the documentation, the `#[async_trait] impl` below carries only SQL.
///
/// The `*_authorized` methods apply the caller's [`ConversationAccess`] scoping **in SQL**, not in
/// the application layer. An implementation that returned rows outside the supplied access scope
/// would be a cross-tenant disclosure, so that filtering is part of this contract, not an
/// optimisation. The three `*_with_connection` free functions below are deliberately **not** on
/// this trait: they run inside the admin-command transaction and carry no authorization of their
/// own — see the `pub(crate)` note in `repositories/mod.rs`.
#[async_trait]
pub trait ConversationRepository: Send + Sync {
    async fn get_or_create_conversation_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ConversationPolicyRecord, AppError>;

    async fn put_conversation_policy(
        &self,
        application_id: Uuid,
        request: &ConversationPolicyPutRequest,
    ) -> Result<ConversationPolicyRecord, AppError>;

    async fn get_or_create_memory_policy(
        &self,
        application_id: Uuid,
    ) -> Result<MemoryPolicyRecord, AppError>;

    async fn put_memory_policy(
        &self,
        application_id: Uuid,
        request: &MemoryPolicyPutRequest,
    ) -> Result<MemoryPolicyRecord, AppError>;

    async fn get_or_create_retrieval_policy(
        &self,
        application_id: Uuid,
    ) -> Result<RetrievalPolicyRecord, AppError>;

    async fn put_retrieval_policy(
        &self,
        application_id: Uuid,
        request: &RetrievalPolicyPutRequest,
    ) -> Result<RetrievalPolicyRecord, AppError>;

    async fn get_or_create_embedding_policy(
        &self,
        application_id: Uuid,
    ) -> Result<EmbeddingPolicyRecord, AppError>;

    async fn put_embedding_policy(
        &self,
        application_id: Uuid,
        request: &EmbeddingPolicyPutRequest,
    ) -> Result<EmbeddingPolicyRecord, AppError>;

    async fn create_conversation(
        &self,
        insert: &ConversationInsert<'_>,
    ) -> Result<ConversationRecord, AppError>;

    async fn find_conversation_authorized(
        &self,
        public_id: &str,
        access: &ConversationAccess,
    ) -> Result<ConversationRecord, AppError>;

    /// Lists conversations newest-updated-first, resuming after `cursor` when supplied.
    ///
    /// See the module docs for the keyset contract. The sort key here is
    /// `(updated_at, id)` — a **mutable** timestamp that the version trigger only ever moves
    /// forward, so a conversation updated mid-sweep is **skipped**, not duplicated. The full
    /// argument, and what callers must check instead of de-duplicating, is in the module docs.
    async fn list_conversations_authorized(
        &self,
        access: &ConversationAccess,
        query: &ConversationQuery,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(ConversationRecord, ListCursor)>, AppError>;

    async fn patch_conversation(
        &self,
        public_id: &str,
        request: &ConversationPatchRequest,
    ) -> Result<ConversationRecord, AppError>;

    async fn set_conversation_status(
        &self,
        public_id: &str,
        status: ConversationStatus,
    ) -> Result<ConversationRecord, AppError>;

    async fn add_message(
        &self,
        insert: &ConversationMessageInsert,
    ) -> Result<ConversationMessageRecord, AppError>;

    /// Lists a conversation's messages in ascending sequence order, resuming after `cursor`.
    ///
    /// The only **ascending** list in this file, so its keyset predicate is
    /// `sequence_number > $n`, not `<`. `sequence_number` is unique per conversation
    /// (`conversation_messages_sequence_unique`, `migrations/0007_*.sql`), so the ordering is
    /// already total and no `id` tiebreaker is needed — hence a bare [`SeqCursor`] rather
    /// than a `(timestamp, id)` pair.
    ///
    /// The cursor composes with, and does not replace, the pre-existing `before`/`after`
    /// filters: all three are separate `and`-ed predicates over the same column.
    async fn list_messages(
        &self,
        conversation_public_id: &str,
        query: &ConversationMessageQuery,
        cursor: Option<SeqCursor>,
        limit: i64,
    ) -> Result<Vec<(ConversationMessageRecord, SeqCursor)>, AppError>;

    async fn create_memory(&self, insert: &MemoryInsert<'_>) -> Result<MemoryRecord, AppError>;

    async fn find_memory_authorized(
        &self,
        public_id: &str,
        access: &ConversationAccess,
    ) -> Result<MemoryRecord, AppError>;

    /// Lists memories newest-updated-first, resuming after `cursor` when supplied.
    ///
    /// Same `(updated_at, id)` mutable sort key as
    /// [`Self::list_conversations_authorized`], so a memory updated mid-sweep is **skipped**
    /// rather than re-seen, for the reason set out in the module docs.
    async fn list_memories_authorized(
        &self,
        access: &ConversationAccess,
        query: &MemoryQuery,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(MemoryRecord, ListCursor)>, AppError>;

    async fn patch_memory(
        &self,
        public_id: &str,
        request: &MemoryPatchRequest,
        content_hash: Option<&str>,
    ) -> Result<MemoryRecord, AppError>;

    async fn delete_memory(&self, public_id: &str) -> Result<(), AppError>;

    /// Pooled wrapper over [`create_rag_collection_with_connection`].
    ///
    /// The SQL body lives in the free function so it can also run inside a caller-supplied
    /// transaction (the `AdminCommandRunner` idempotency envelope). This method owns the
    /// transaction lifecycle; the free function must never open one.
    async fn create_rag_collection(
        &self,
        id: Uuid,
        public_id: &str,
        request: &RagCollectionCreateRequest,
    ) -> Result<RagCollectionRecord, AppError>;

    /// Lists RAG collections newest-created-first, resuming after `cursor` when supplied.
    ///
    /// Unlike conversations and memories the sort key here is `(created_at, id)` — immutable
    /// once written, so a sweep over it is exactly-once even under concurrent updates.
    async fn list_rag_collections(
        &self,
        query: &RagCollectionQuery,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(RagCollectionRecord, ListCursor)>, AppError>;

    async fn get_rag_collection(&self, public_id: &str) -> Result<RagCollectionRecord, AppError>;

    async fn patch_rag_collection(
        &self,
        public_id: &str,
        request: &RagCollectionPatchRequest,
    ) -> Result<RagCollectionRecord, AppError>;

    async fn set_rag_collection_status(
        &self,
        public_id: &str,
        status: RagCollectionStatus,
    ) -> Result<RagCollectionRecord, AppError>;

    /// Pooled wrapper over [`create_rag_document_with_connection`].
    ///
    /// The SQL body lives in the free function so it can also run inside a caller-supplied
    /// transaction (the `AdminCommandRunner` idempotency envelope). This method owns the
    /// transaction lifecycle; the free function must never open one.
    /// See [`ConversationRepository::ingest_rag_document`] on why `plan` is not optional.
    async fn create_rag_document(
        &self,
        id: Uuid,
        public_id: &str,
        collection_public_id: &str,
        request: &RagDocumentCreateRequest,
        content_hash: Option<&str>,
        plan: &RagIngestionPlan,
    ) -> Result<RagDocumentRecord, AppError>;

    /// Lists a collection's documents newest-created-first, resuming after `cursor`.
    ///
    /// The keyset predicate goes **inside** the CTE, where the `limit` is: it selects which
    /// rows survive into the page. The newest-first ordering of the emitted page is a
    /// property of [`rag_document_select`]'s outer `order by`, which must not be removed —
    /// see the comment there and the unit test that guards it.
    ///
    /// `limit` is no longer clamped here; `crate::application::conversation` clamps it, so
    /// the over-fetch row cannot be silently eaten by a repository-side ceiling.
    async fn list_rag_documents(
        &self,
        collection_public_id: &str,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(RagDocumentRecord, ListCursor)>, AppError>;

    async fn get_rag_document(&self, public_id: &str) -> Result<RagDocumentRecord, AppError>;

    async fn delete_rag_document(&self, public_id: &str) -> Result<(), AppError>;

    /// Pooled wrapper over [`ingest_rag_document_with_connection`].
    ///
    /// The SQL body lives in the free function so it can also run inside a caller-supplied
    /// transaction (the `AdminCommandRunner` idempotency envelope). This method owns the
    /// transaction lifecycle; the free function must never open one — the `for update` row
    /// lock and the version supersession must belong to whatever transaction the caller
    /// already holds.
    ///
    /// `plan` is mandatory rather than optional so no implementation and no caller can write a
    /// version without deciding what the pipeline produced. Passing
    /// [`RagIngestionPlan::empty`] is a valid answer — it means "no content, nothing to
    /// index", and it yields `'pending'`. What is not possible is forgetting.
    async fn ingest_rag_document(
        &self,
        public_id: &str,
        request: &RagDocumentIngestRequest,
        content_hash: &str,
        plan: &RagIngestionPlan,
    ) -> Result<RagDocumentRecord, AppError>;
}

impl PgConversationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ConversationRepository for PgConversationRepository {
    async fn get_or_create_conversation_policy(
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

    async fn put_conversation_policy(
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

    async fn get_or_create_memory_policy(
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

    async fn put_memory_policy(
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

    async fn get_or_create_retrieval_policy(
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

    async fn put_retrieval_policy(
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

    async fn get_or_create_embedding_policy(
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

    async fn put_embedding_policy(
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

    async fn create_conversation(
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

    async fn find_conversation_authorized(
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

    async fn list_conversations_authorized(
        &self,
        access: &ConversationAccess,
        query: &ConversationQuery,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(ConversationRecord, ListCursor)>, AppError> {
        let rows = sqlx::query(&conversation_select(LIST_CONVERSATIONS_SQL))
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
            .bind(cursor.map(|cursor| cursor.ts))
            .bind(cursor.map(|cursor| cursor.id))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let record = conversation_record_from_row(row)?;
                let key = ListCursor::new(record.updated_at, row.try_get("id")?);
                Ok((record, key))
            })
            .collect()
    }

    async fn patch_conversation(
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

    async fn set_conversation_status(
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

    async fn add_message(
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

    async fn list_messages(
        &self,
        conversation_public_id: &str,
        query: &ConversationMessageQuery,
        cursor: Option<SeqCursor>,
        limit: i64,
    ) -> Result<Vec<(ConversationMessageRecord, SeqCursor)>, AppError> {
        let rows = sqlx::query(&conversation_message_select(LIST_MESSAGES_SQL))
            .bind(conversation_public_id)
            .bind(message_sequence(query.before.as_deref()))
            .bind(message_sequence(query.after.as_deref()))
            .bind(cursor.map(SeqCursor::sequence_number))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let record = conversation_message_record_from_row(row)?;
                let key = SeqCursor::new(record.sequence_number);
                Ok((record, key))
            })
            .collect()
    }

    async fn create_memory(&self, insert: &MemoryInsert<'_>) -> Result<MemoryRecord, AppError> {
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

    async fn find_memory_authorized(
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

    async fn list_memories_authorized(
        &self,
        access: &ConversationAccess,
        query: &MemoryQuery,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(MemoryRecord, ListCursor)>, AppError> {
        let rows = sqlx::query(&memory_select(LIST_MEMORIES_SQL))
            .bind(access.privileged)
            .bind(access.application_id)
            .bind(&access.external_tenant_id)
            .bind(&access.external_user_id)
            .bind(query.memory_type.map(memory_type_to_db))
            .bind(query.status.map(memory_status_to_db))
            .bind(cursor.map(|cursor| cursor.ts))
            .bind(cursor.map(|cursor| cursor.id))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let record = memory_record_from_row(row)?;
                let key = ListCursor::new(record.updated_at, row.try_get("id")?);
                Ok((record, key))
            })
            .collect()
    }

    async fn patch_memory(
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

    async fn delete_memory(&self, public_id: &str) -> Result<(), AppError> {
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

    async fn create_rag_collection(
        &self,
        id: Uuid,
        public_id: &str,
        request: &RagCollectionCreateRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let record = create_rag_collection_with_connection(&mut tx, id, public_id, request).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_rag_collections(
        &self,
        query: &RagCollectionQuery,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(RagCollectionRecord, ListCursor)>, AppError> {
        let rows = sqlx::query(LIST_RAG_COLLECTIONS_SQL)
            .bind(query.application_id)
            .bind(&query.external_tenant_id)
            .bind(query.status.map(rag_collection_status_to_db))
            .bind(cursor.map(|cursor| cursor.ts))
            .bind(cursor.map(|cursor| cursor.id))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let record = rag_collection_record_from_row(row)?;
                let key = ListCursor::new(record.created_at, row.try_get("id")?);
                Ok((record, key))
            })
            .collect()
    }

    async fn get_rag_collection(&self, public_id: &str) -> Result<RagCollectionRecord, AppError> {
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

    async fn patch_rag_collection(
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

    async fn set_rag_collection_status(
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

    async fn create_rag_document(
        &self,
        id: Uuid,
        public_id: &str,
        collection_public_id: &str,
        request: &RagDocumentCreateRequest,
        content_hash: Option<&str>,
        plan: &RagIngestionPlan,
    ) -> Result<RagDocumentRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let record = create_rag_document_with_connection(
            &mut tx,
            id,
            public_id,
            collection_public_id,
            request,
            content_hash,
            plan,
        )
        .await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_rag_documents(
        &self,
        collection_public_id: &str,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<(RagDocumentRecord, ListCursor)>, AppError> {
        let rows = sqlx::query(&rag_document_select(LIST_RAG_DOCUMENTS_SQL))
            .bind(collection_public_id)
            .bind(cursor.map(|cursor| cursor.ts))
            .bind(cursor.map(|cursor| cursor.id))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let record = rag_document_record_from_row(row)?;
                let key = ListCursor::new(record.created_at, row.try_get("id")?);
                Ok((record, key))
            })
            .collect()
    }

    async fn get_rag_document(&self, public_id: &str) -> Result<RagDocumentRecord, AppError> {
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

    async fn delete_rag_document(&self, public_id: &str) -> Result<(), AppError> {
        sqlx::query(
            "update rag_documents set status = 'deleted', deleted_at = coalesce(deleted_at, now()) where public_id = $1",
        )
        .bind(public_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn ingest_rag_document(
        &self,
        public_id: &str,
        request: &RagDocumentIngestRequest,
        content_hash: &str,
        plan: &RagIngestionPlan,
    ) -> Result<RagDocumentRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let record =
            ingest_rag_document_with_connection(&mut tx, public_id, request, content_hash, plan)
                .await?;
        tx.commit().await?;
        Ok(record)
    }
}

// ---------------------------------------------------------------------------
// Connection-taking RAG write bodies.
//
// These mirror the `insert_audit_with_connection` precedent in
// `src/infra/repositories/admin.rs`: the SQL lives in a free function that takes a
// `&mut PgConnection`, so the same body can run either on a pooled connection (through the
// `PgConversationRepository` wrappers above) or inside a caller-supplied transaction — in
// particular `PgAdminCommandTransaction::connection()`, which is how the idempotency
// envelope in `plans/02b-idempotency-replay.md` reaches them.
//
// INVARIANT: none of these functions may call `begin()`, `commit()`, or `rollback()`.
// The caller owns the transaction. An inner `begin()` here would nest a transaction inside
// the admin command runner's `savepoint admin_command_mutation` and silently break the
// rollback semantics that `begin_command_savepoint`/`rollback_command_savepoint` depend on.
//
// INVARIANT: these stay `pub(crate)` and must never be re-exported as `pub` from
// `src/infra/repositories/mod.rs`. They perform no authorization check, write no audit row
// and claim no idempotency record; all three are the responsibility of their only
// legitimate caller, `crate::application::conversation`. A `pub` export would hand external
// code an unauthenticated, unaudited RAG write path that bypasses the envelope entirely.
// ---------------------------------------------------------------------------

pub(crate) async fn create_rag_collection_with_connection(
    connection: &mut PgConnection,
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
    .fetch_one(connection)
    .await?;
    rag_collection_record_from_row(&row)
}

pub(crate) async fn create_rag_document_with_connection(
    connection: &mut PgConnection,
    id: Uuid,
    public_id: &str,
    collection_public_id: &str,
    request: &RagDocumentCreateRequest,
    content_hash: Option<&str>,
    plan: &RagIngestionPlan,
) -> Result<RagDocumentRecord, AppError> {
    let collection_id: Uuid = sqlx::query_scalar(
        "select id from rag_collections where public_id = $1 and deleted_at is null",
    )
    .bind(collection_public_id)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(|| {
        AppError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "rag_collection_not_found",
            "RAG collection not found",
        )
    })?;
    let mut row = sqlx::query(&rag_document_select(
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
    .fetch_one(&mut *connection)
    .await?;
    if let (Some(content), Some(hash)) = (&request.content, content_hash) {
        let version_id = Uuid::now_v7();
        // The status is *derived* from what the pipeline actually produced
        // (`RagIngestionPlan::terminal_status`) and can never be passed in as a literal. That
        // is the structural fix for P0-1: the old code bound `'indexed'` unconditionally, and
        // its 02a replacement bound `'pending'` unconditionally. Both were constants; only one
        // of them was honest, and neither was derived from work.
        sqlx::query(
            r#"
                insert into rag_document_versions (
                    id, document_id, version_number, content_plain, content_hash,
                    content_size_bytes, ingestion_status, metadata
                )
                values ($1, $2, 1, $3, $4, $5, $6, $7)
                "#,
        )
        .bind(version_id)
        .bind(id)
        .bind(content)
        .bind(hash)
        .bind(content.len() as i64)
        .bind(rag_ingestion_status_to_db(plan.terminal_status()))
        .bind(&request.metadata)
        .execute(&mut *connection)
        .await?;
        write_rag_ingestion_artifacts(&mut *connection, id, version_id, collection_id, hash, plan)
            .await?;
        sqlx::query("update rag_documents set current_version_id = $2 where id = $1")
            .bind(id)
            .bind(version_id)
            .execute(&mut *connection)
            .await?;
        // The row returned by the insert above predates the version insert, so it would
        // report a null ingestion_status. Re-select it so the response reflects the
        // version that was just created. Skipped entirely when no version exists, which
        // keeps `ingestion_status: null` honest for a create without inline content.
        row = sqlx::query(&rag_document_select(
            "select d.* from rag_documents d where d.id = $1",
        ))
        .bind(id)
        .fetch_one(&mut *connection)
        .await?;
    }
    rag_document_record_from_row(&row)
}

pub(crate) async fn ingest_rag_document_with_connection(
    connection: &mut PgConnection,
    public_id: &str,
    request: &RagDocumentIngestRequest,
    content_hash: &str,
    plan: &RagIngestionPlan,
) -> Result<RagDocumentRecord, AppError> {
    let target = sqlx::query(
        "select id, collection_id from rag_documents where public_id = $1 and deleted_at is null for update",
    )
    .bind(public_id)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(|| {
        AppError::coded(
            axum::http::StatusCode::NOT_FOUND,
            "rag_document_not_found",
            "RAG document not found",
        )
    })?;
    let document_id: Uuid = target.try_get("id")?;
    let collection_id: Uuid = target.try_get("collection_id")?;
    let version_number: i32 = sqlx::query_scalar(
        "select coalesce(max(version_number), 0) + 1 from rag_document_versions where document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *connection)
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
    .execute(&mut *connection)
    .await?;
    // Derived, never a literal — see the matching comment on the create path.
    sqlx::query(
        r#"
            insert into rag_document_versions (
                id, document_id, version_number, content_plain, content_hash,
                content_size_bytes, source_etag, source_last_modified,
                ingestion_status, metadata
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
    .bind(rag_ingestion_status_to_db(plan.terminal_status()))
    .bind(&request.metadata)
    .execute(&mut *connection)
    .await?;
    write_rag_ingestion_artifacts(
        &mut *connection,
        document_id,
        version_id,
        collection_id,
        content_hash,
        plan,
    )
    .await?;
    sqlx::query("update rag_documents set current_version_id = $2 where id = $1")
        .bind(document_id)
        .bind(version_id)
        .execute(&mut *connection)
        .await?;
    let row = sqlx::query(&rag_document_select(
        "select d.* from rag_documents d where d.id = $1",
    ))
    .bind(document_id)
    .fetch_one(&mut *connection)
    .await?;
    rag_document_record_from_row(&row)
}

// ---------------------------------------------------------------------------
// RAG ingestion pipeline (plan 11, Sub-Phases A and B).
//
// Same INVARIANTS as the block above: no `begin`/`commit`/`rollback`, `pub(crate)` only.
//
// Nothing here attaches a `notify_moira_runtime_config_change()` trigger, and nothing here
// should. `rag_chunks`, `rag_chunk_embeddings`, `rag_ingestion_runs` and
// `rag_document_versions` carry no NOTIFY trigger today (`migrations/0007…` attaches one to
// `rag_collections` and `rag_documents` only). Adding one would route every chunk write
// through `circuit_reset_scope`'s `other =>` arm in `src/infra/db.rs`, which returns
// `CircuitResetScope::All` — discarding every provider circuit-breaker on every replica, once
// per chunk, with a `warn!` each time. If a later change ever does attach one, it must add the
// table to `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` and to `every_triggered_table_has_a_scope` in
// the same commit.
// ---------------------------------------------------------------------------

/// Writes the chunks, embeddings and run row for one document version.
///
/// Called from both ingestion entry points so `/ingest`, `/reindex` and the inline-content
/// create path cannot drift — `/reindex` is a literal alias of `/ingest` and the create path
/// writes version 1, and a pipeline wired into only one of them would leave the others writing
/// bare, chunk-less versions forever.
async fn write_rag_ingestion_artifacts(
    connection: &mut PgConnection,
    document_id: Uuid,
    document_version_id: Uuid,
    collection_id: Uuid,
    content_hash: &str,
    plan: &RagIngestionPlan,
) -> Result<(), AppError> {
    for chunk in &plan.chunks {
        let chunk_id = Uuid::now_v7();
        sqlx::query(
            r#"
                insert into rag_chunks (
                    id, public_id, document_version_id, collection_id, chunk_index,
                    chunk_text_plain, chunk_hash, token_count, start_offset, end_offset,
                    section_title, metadata
                )
                values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                "#,
        )
        .bind(chunk_id)
        // `rag_chunks_public_id_valid` requires `^chunk_[0-9a-fA-F-]{36}$`, which is the
        // hyphenated UUID form — not `simple()`.
        .bind(format!("chunk_{chunk_id}"))
        .bind(document_version_id)
        .bind(collection_id)
        .bind(chunk.chunk_index)
        .bind(&chunk.text)
        .bind(&chunk.chunk_hash)
        .bind(chunk.token_estimate)
        .bind(chunk.start_offset)
        .bind(chunk.end_offset)
        .bind(&chunk.section_title)
        .bind(Value::Object(serde_json::Map::from_iter([(
            "chunk_strategy".to_string(),
            Value::String(plan.strategy.to_string()),
        )])))
        .execute(&mut *connection)
        .await?;

        if let Some(vector) = &chunk.embedding {
            // The vector is bound as `text` and cast with `$4::vector` rather than through the
            // `pgvector` crate — decision and reversal condition are recorded on
            // `crate::orchestration::encode_vector_literal`.
            sqlx::query(
                r#"
                    insert into rag_chunk_embeddings (
                        chunk_id, embedding_model_id, embedding_version, dimension, embedding
                    )
                    values ($1, $2, 1, $3, $4::vector)
                    "#,
            )
            .bind(chunk_id)
            .bind(plan.embedding_model_id)
            .bind(vector.len() as i32)
            .bind(encode_vector_literal(vector))
            .execute(&mut *connection)
            .await?;
        }
    }

    sqlx::query(
        r#"
            insert into rag_ingestion_runs (
                id, document_id, document_version_id, status, content_hash,
                chunk_count, embedded_chunk_count, completed_at, failure_class, metadata
            )
            values ($1, $2, $3, $4, $5, $6, $7, now(), $8, $9)
            "#,
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(document_version_id)
    .bind(plan.run_status())
    .bind(content_hash)
    .bind(plan.chunk_count())
    .bind(plan.embedded_chunk_count())
    .bind(plan.failure_class)
    .bind(Value::Object(serde_json::Map::from_iter([(
        "chunk_strategy".to_string(),
        Value::String(plan.strategy.to_string()),
    )])))
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Everything the ingestion pipeline needs to know before it opens a transaction.
#[derive(Debug, Clone)]
pub(crate) struct RagIngestionContext {
    /// The application that owns the collection. Embedding credentials resolve against this,
    /// never against the acting admin's application.
    pub application_id: Uuid,
    pub mime_type: Option<String>,
    /// `None` when the application has no embedding policy, or has one with
    /// `rag_embeddings_enabled = false`. That is not a failure — it is an application that has
    /// not asked for semantic indexing, and its documents are still chunked.
    pub embedding: Option<RagEmbeddingTarget>,
}

#[derive(Debug, Clone)]
pub(crate) struct RagEmbeddingTarget {
    pub provider_id: Option<Uuid>,
    pub model_id: Option<Uuid>,
    pub declared_dimension: Option<i32>,
    pub batch_size: i32,
    pub timeout_ms: i32,
}

fn ingestion_context_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<RagIngestionContext, AppError> {
    let enabled: Option<bool> = row.try_get("rag_embeddings_enabled")?;
    let embedding = if enabled == Some(true) {
        Some(RagEmbeddingTarget {
            provider_id: row.try_get("embedding_provider_id")?,
            model_id: row.try_get("embedding_model_id")?,
            declared_dimension: row.try_get("embedding_dimension")?,
            batch_size: row.try_get::<Option<i32>, _>("batch_size")?.unwrap_or(32),
            timeout_ms: row
                .try_get::<Option<i32>, _>("timeout_ms")?
                .unwrap_or(60_000),
        })
    } else {
        None
    };
    Ok(RagIngestionContext {
        application_id: row.try_get("application_id")?,
        mime_type: row.try_get("mime_type")?,
        embedding,
    })
}

/// Ingestion context for `/ingest` and `/reindex`, keyed on the document's public id.
///
/// Returns `None` for a document that does not exist. The caller deliberately does **not**
/// turn that into a `404` itself: the not-found error must keep being raised inside the
/// command transaction, where it has always been raised, so the idempotency envelope's
/// treatment of a failed ingest is unchanged by this plan.
///
/// This is a free function rather than a `ConversationRepository` trait method on purpose.
/// `ConversationService` holds the concrete `PgConversationRepository`, the ingestion write
/// bodies next to it are already free functions for the transaction-sharing reason documented
/// above, and adding a method to the trait would oblige `InMemoryConversationRepository` to
/// stub a method nothing calls.
pub(crate) async fn find_document_ingestion_context(
    pool: &PgPool,
    document_public_id: &str,
) -> Result<Option<RagIngestionContext>, AppError> {
    let row = sqlx::query(
        r#"
            select c.application_id,
                   d.mime_type,
                   p.embedding_provider_id,
                   p.embedding_model_id,
                   p.embedding_dimension,
                   p.batch_size,
                   p.timeout_ms,
                   p.rag_embeddings_enabled
            from rag_documents d
            join rag_collections c on c.id = d.collection_id
            left join application_embedding_policies p on p.application_id = c.application_id
            where d.public_id = $1 and d.deleted_at is null
            "#,
    )
    .bind(document_public_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(ingestion_context_from_row).transpose()
}

/// Ingestion context for the inline-content create path, keyed on the collection's public id.
///
/// The document does not exist yet, so the MIME type comes from the request rather than a row.
pub(crate) async fn find_collection_ingestion_context(
    pool: &PgPool,
    collection_public_id: &str,
) -> Result<Option<RagIngestionContext>, AppError> {
    let row = sqlx::query(
        r#"
            select c.application_id,
                   null::varchar as mime_type,
                   p.embedding_provider_id,
                   p.embedding_model_id,
                   p.embedding_dimension,
                   p.batch_size,
                   p.timeout_ms,
                   p.rag_embeddings_enabled
            from rag_collections c
            left join application_embedding_policies p on p.application_id = c.application_id
            where c.public_id = $1 and c.deleted_at is null
            "#,
    )
    .bind(collection_public_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(ingestion_context_from_row).transpose()
}

// The outer `order by` in each of the three helpers below is load-bearing, for the reason
// already recorded on `rag_document_select`: an inner query's own `order by` decides only
// which rows the CTE keeps under its `limit`, and does not survive into the outer result once
// a join is present — the planner may drive the outer result from the joined table and emit
// rows in that table's heap order. Restating the sort key here is what makes the emitted page
// match the order the keyset cursor was computed against; without it `next_cursor` could name
// a row that is not the page's true boundary and the following page would skip rows.
//
// Each helper restates the sort key of the *list* query that uses it. Single-row call sites
// (find/patch/insert … returning) are unaffected by an `order by` over one row.

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
        order by conversation_rows.updated_at desc, conversation_rows.id desc
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
        order by message_rows.sequence_number asc
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
        order by memory_rows.updated_at desc, memory_rows.id desc
        "#
    )
}

fn rag_document_select(inner: &str) -> String {
    // The outer `order by` is load-bearing, not decorative. An inner query's own
    // `order by` only decides which rows the CTE keeps under its `limit`; it does not
    // survive into the outer result, and the joins below let the planner emit rows in
    // whatever order suits it (a hash join driven by rag_document_versions reorders
    // them by that table's heap). Restating the ordering here keeps
    // list_rag_documents newest-first. Single-row call sites are unaffected.
    format!(
        r#"
        with document_rows as ({inner})
        select document_rows.*,
               c.public_id as collection_public_id,
               v.ingestion_status as ingestion_status
        from document_rows
        join rag_collections c on c.id = document_rows.collection_id
        left join rag_document_versions v on v.id = document_rows.current_version_id
        order by document_rows.created_at desc, document_rows.id desc
        "#
    )
}

fn message_sequence(value: Option<&str>) -> Option<i64> {
    value.and_then(|value| value.strip_prefix("msgseq_").unwrap_or(value).parse().ok())
}

#[cfg(test)]
mod tests {
    use super::{
        LIST_CONVERSATIONS_SQL, LIST_MEMORIES_SQL, LIST_MESSAGES_SQL, LIST_RAG_COLLECTIONS_SQL,
        LIST_RAG_DOCUMENTS_SQL, conversation_message_select, conversation_select, memory_select,
        rag_document_select,
    };

    /// The list endpoint's ordering is a property of the OUTER select, and is asserted here at the
    /// SQL level rather than through a query.
    ///
    /// `list_rag_documents` puts `order by d.created_at desc, d.id desc` inside the CTE, where it
    /// only decides which rows survive the `limit`; the outer select does not inherit it. Once
    /// `rag_document_select` joins `rag_document_versions`, the planner may drive the outer result
    /// from that table and emit documents in its heap order.
    ///
    /// A behavioural test cannot be trusted to catch this: the planner only switches to the
    /// reordering hash join once the table is large enough, so at fixture scale it keeps a nested
    /// loop and the CTE order survives by accident — an end-to-end assertion passes even with the
    /// clause deleted. Asserting the emitted SQL is deterministic and fails the moment the clause is
    /// removed, at any table size.
    #[test]
    fn rag_document_select_orders_the_outer_result() {
        let sql = rag_document_select("select d.* from rag_documents d");

        let left_join = sql
            .find("left join rag_document_versions")
            .expect("the ingestion_status projection depends on this join");
        let order_by = sql
            .find("order by document_rows.created_at desc, document_rows.id desc")
            .expect("the outer select must restate the newest-first ordering");

        assert!(
            order_by > left_join,
            "the ordering must apply to the joined outer result, not inside the CTE:\n{sql}"
        );
        assert!(
            sql.contains("v.ingestion_status as ingestion_status"),
            "every read path must project the current version's ingestion_status:\n{sql}"
        );
    }

    /// The other three CTE wrappers carry the same hazard `rag_document_select` was fixed for.
    ///
    /// Each joins another table onto the CTE, so the inner `order by` — which only decides
    /// which rows survive the `limit` — does not survive into the outer result. Without the
    /// outer `order by` the emitted page can be in a different order from the one the keyset
    /// cursor was computed against, which makes `next_cursor` name a row that is not the
    /// page's true boundary and silently skips rows on the following page.
    ///
    /// Asserted at the SQL level for the reason given on the test above: at fixture scale the
    /// planner keeps a nested loop and the CTE order survives by accident, so a behavioural
    /// test passes even with the clause deleted.
    #[test]
    fn every_joined_select_orders_the_outer_result() {
        for (label, sql, join, order_by) in [
            (
                "conversation_select",
                conversation_select("select c.* from conversations c"),
                "left join application_memory_policies",
                "order by conversation_rows.updated_at desc, conversation_rows.id desc",
            ),
            (
                "conversation_message_select",
                conversation_message_select("select m.* from conversation_messages m"),
                "join conversations c",
                "order by message_rows.sequence_number asc",
            ),
            (
                "memory_select",
                memory_select("select m.* from memory_records m"),
                "left join conversations c",
                "order by memory_rows.updated_at desc, memory_rows.id desc",
            ),
        ] {
            let join_at = sql
                .find(join)
                .unwrap_or_else(|| panic!("{label} must still join the table it projects:\n{sql}"));
            let order_at = sql.find(order_by).unwrap_or_else(|| {
                panic!("{label} must restate its list query's sort key on the outer select:\n{sql}")
            });

            assert!(
                order_at > join_at,
                "{label}'s ordering must apply to the joined outer result, not inside the CTE:\n{sql}"
            );
        }
    }

    /// Each helper's outer sort key must match the list query that flows through it.
    ///
    /// A mismatch would be the worst kind of failure: rows come back ordered, so nothing looks
    /// broken, but `next_cursor` is minted from a key that does not correspond to the emitted
    /// order and pagination quietly loses rows.
    #[test]
    fn outer_ordering_matches_the_list_query_it_wraps() {
        for (label, outer, inner) in [
            (
                "conversations",
                conversation_select(LIST_CONVERSATIONS_SQL),
                "order by c.updated_at desc, c.id desc",
            ),
            (
                "messages",
                conversation_message_select(LIST_MESSAGES_SQL),
                "order by m.sequence_number asc",
            ),
            (
                "memories",
                memory_select(LIST_MEMORIES_SQL),
                "order by m.updated_at desc, m.id desc",
            ),
            (
                "rag documents",
                rag_document_select(LIST_RAG_DOCUMENTS_SQL),
                "order by d.created_at desc, d.id desc",
            ),
        ] {
            assert!(
                outer.contains(inner),
                "{label}: the inner query must keep its own sort key:\n{outer}"
            );
            // The outer clause is the last `order by` in the emitted statement.
            let last = outer
                .rfind("order by")
                .unwrap_or_else(|| panic!("{label}: no outer ordering at all:\n{outer}"));
            let inner_at = outer.find(inner).expect("checked above");
            assert!(
                last > inner_at,
                "{label}: the outer ordering must come after the CTE's own:\n{outer}"
            );
        }
    }

    #[test]
    fn keyset_predicate_uses_strict_less_than_for_descending_lists() {
        for (label, sql, predicate) in [
            (
                "conversations",
                LIST_CONVERSATIONS_SQL,
                "(c.updated_at, c.id) < ($11, $12::uuid)",
            ),
            (
                "memories",
                LIST_MEMORIES_SQL,
                "(m.updated_at, m.id) < ($7, $8::uuid)",
            ),
            (
                "rag collections",
                LIST_RAG_COLLECTIONS_SQL,
                "(created_at, id) < ($4, $5::uuid)",
            ),
            (
                "rag documents",
                LIST_RAG_DOCUMENTS_SQL,
                "(d.created_at, d.id) < ($2, $3::uuid)",
            ),
        ] {
            assert!(
                sql.contains(predicate),
                "{label}: a `desc` ordering needs a strictly-less-than row comparison against \
                 the SAME key it orders by, or the page boundary drifts:\n{sql}"
            );
        }
    }

    /// Conversations and memories key on `updated_at`, **not** `created_at`.
    ///
    /// Both tables carry both columns, and both are `timestamptz`, so keying the cursor on the
    /// wrong one compiles, runs, and returns plausible-looking pages that skip rows. Pinning
    /// the column name is the only cheap guard.
    #[test]
    fn conversation_and_memory_cursors_key_on_updated_at_not_created_at() {
        assert!(LIST_CONVERSATIONS_SQL.contains("(c.updated_at, c.id) <"));
        assert!(!LIST_CONVERSATIONS_SQL.contains("(c.created_at, c.id) <"));
        assert!(LIST_MEMORIES_SQL.contains("(m.updated_at, m.id) <"));
        assert!(!LIST_MEMORIES_SQL.contains("(m.created_at, m.id) <"));
    }

    #[test]
    fn keyset_predicate_uses_strict_greater_than_for_the_ascending_sequence_list() {
        assert!(
            LIST_MESSAGES_SQL.contains("($4::bigint is null or m.sequence_number > $4)"),
            "messages order ascending, so resuming means `>`, not `<`:\n{LIST_MESSAGES_SQL}"
        );
        assert!(
            LIST_MESSAGES_SQL.contains("order by m.sequence_number asc"),
            "the predicate direction only makes sense against an ascending sort"
        );
    }

    /// The SQL-injection guard, made mechanical.
    ///
    /// Every one of these queries is a `const`, so it is a compile-time constant that no
    /// runtime value can enter — that alone settles it. This test additionally pins the
    /// weaker property a reader can check by eye: every cursor comparison is written against
    /// `$N` placeholders, so no future edit can start splicing a value in without failing
    /// here first.
    #[test]
    fn keyset_predicates_bind_parameters_and_never_interpolate_values() {
        for (label, sql) in [
            ("conversations", LIST_CONVERSATIONS_SQL),
            ("messages", LIST_MESSAGES_SQL),
            ("memories", LIST_MEMORIES_SQL),
            ("rag collections", LIST_RAG_COLLECTIONS_SQL),
            ("rag documents", LIST_RAG_DOCUMENTS_SQL),
        ] {
            assert!(
                !sql.contains('{') && !sql.contains('}'),
                "{label}: a `format!` placeholder in a list query means a value can reach the \
                 query text:\n{sql}"
            );
            for comparison in sql.match_indices("<").chain(sql.match_indices(">")) {
                let tail = &sql[comparison.0 + 1..];
                let operand = tail.trim_start();
                assert!(
                    operand.starts_with('$')
                        || operand.starts_with("($")
                        || operand.starts_with('=')
                        || operand.starts_with("all")
                        || operand.starts_with("any"),
                    "{label}: comparison operand must be a bound parameter, found {:?}:\n{sql}",
                    &operand[..operand.len().min(24)]
                );
            }
            assert!(
                sql.contains("limit $"),
                "{label}: the row limit must be bound too:\n{sql}"
            );
        }
    }
}

/// In-memory [`ConversationRepository`] for unit tests (plan 06, Module 8 / P2-3).
///
/// Backs the **conversation-policy document** — the `get_or_create` / `put` pair that gates every
/// conversation, memory and RAG feature — with real state, and returns an explicit `not_stubbed`
/// error for every other method. A fake that answered unbacked reads with `Ok(default)` would let
/// a test pass while exercising nothing, so unbacked methods fail loudly instead.
///
/// Carries no conversation content and no credential material: nothing is seeded that a leak test
/// would have to recognise as synthetic.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct InMemoryConversationRepository {
    policies: std::sync::Mutex<std::collections::HashMap<Uuid, ConversationPolicyRecord>>,
}

#[cfg(test)]
fn not_stubbed(method: &str) -> AppError {
    AppError::Internal(format!(
        "InMemoryConversationRepository::{method} is not stubbed"
    ))
}

#[cfg(test)]
fn seed_conversation_policy(application_id: Uuid) -> ConversationPolicyRecord {
    ConversationPolicyRecord {
        id: application_id,
        application_id,
        conversations_enabled: false,
        conversation_content_persistence: crate::domain::ConversationContentPersistence::None,
        default_retention_days: 30,
        maximum_retention_days: 365,
        history_strategy: crate::domain::HistoryStrategy::RecentMessages,
        maximum_recent_messages: 20,
        maximum_history_tokens: 4096,
        summarization_enabled: false,
        summary_trigger_tokens: 3000,
        summary_target_tokens: 512,
        minimum_messages_since_summary: 10,
        memory_enabled: false,
        memory_extraction_enabled: false,
        memory_retrieval_enabled: false,
        memory_consent_mode: crate::domain::MemoryConsentMode::Disabled,
        rag_enabled: false,
        default_collection_ids: Vec::new(),
        caller_can_create_conversations: false,
        caller_can_delete_conversations: false,
        caller_can_export_conversations: false,
        protected_instruction_policy: "reject".to_string(),
        metadata: Value::Object(serde_json::Map::new()),
        updated_at: Utc::now(),
        version: 1,
    }
}

#[cfg(test)]
#[async_trait]
impl ConversationRepository for InMemoryConversationRepository {
    async fn get_or_create_conversation_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ConversationPolicyRecord, AppError> {
        Ok(self
            .policies
            .lock()
            .unwrap()
            .entry(application_id)
            .or_insert_with(|| seed_conversation_policy(application_id))
            .clone())
    }

    /// Mirrors the SQL's `coalesce($n, existing)` semantics: `None` leaves the stored value
    /// alone, and every write bumps `version`. That is the part of the contract the application
    /// layer actually depends on — a fake that overwrote absent fields with defaults would make
    /// a partial-update bug invisible.
    async fn put_conversation_policy(
        &self,
        application_id: Uuid,
        request: &ConversationPolicyPutRequest,
    ) -> Result<ConversationPolicyRecord, AppError> {
        let mut guard = self.policies.lock().unwrap();
        let policy = guard
            .entry(application_id)
            .or_insert_with(|| seed_conversation_policy(application_id));

        macro_rules! apply {
            ($($field:ident),* $(,)?) => {$(
                if let Some(value) = request.$field.clone() {
                    policy.$field = value;
                }
            )*};
        }
        apply!(
            conversations_enabled,
            conversation_content_persistence,
            default_retention_days,
            maximum_retention_days,
            history_strategy,
            maximum_recent_messages,
            maximum_history_tokens,
            summarization_enabled,
            summary_trigger_tokens,
            summary_target_tokens,
            minimum_messages_since_summary,
            memory_enabled,
            memory_extraction_enabled,
            memory_retrieval_enabled,
            memory_consent_mode,
            rag_enabled,
            default_collection_ids,
            caller_can_create_conversations,
            caller_can_delete_conversations,
            caller_can_export_conversations,
            protected_instruction_policy,
            metadata,
        );
        policy.updated_at = Utc::now();
        policy.version += 1;
        Ok(policy.clone())
    }

    async fn get_or_create_memory_policy(
        &self,
        _application_id: Uuid,
    ) -> Result<MemoryPolicyRecord, AppError> {
        Err(not_stubbed("get_or_create_memory_policy"))
    }

    async fn put_memory_policy(
        &self,
        _application_id: Uuid,
        _request: &MemoryPolicyPutRequest,
    ) -> Result<MemoryPolicyRecord, AppError> {
        Err(not_stubbed("put_memory_policy"))
    }

    async fn get_or_create_retrieval_policy(
        &self,
        _application_id: Uuid,
    ) -> Result<RetrievalPolicyRecord, AppError> {
        Err(not_stubbed("get_or_create_retrieval_policy"))
    }

    async fn put_retrieval_policy(
        &self,
        _application_id: Uuid,
        _request: &RetrievalPolicyPutRequest,
    ) -> Result<RetrievalPolicyRecord, AppError> {
        Err(not_stubbed("put_retrieval_policy"))
    }

    async fn get_or_create_embedding_policy(
        &self,
        _application_id: Uuid,
    ) -> Result<EmbeddingPolicyRecord, AppError> {
        Err(not_stubbed("get_or_create_embedding_policy"))
    }

    async fn put_embedding_policy(
        &self,
        _application_id: Uuid,
        _request: &EmbeddingPolicyPutRequest,
    ) -> Result<EmbeddingPolicyRecord, AppError> {
        Err(not_stubbed("put_embedding_policy"))
    }

    async fn create_conversation(
        &self,
        _insert: &ConversationInsert<'_>,
    ) -> Result<ConversationRecord, AppError> {
        Err(not_stubbed("create_conversation"))
    }

    async fn find_conversation_authorized(
        &self,
        _public_id: &str,
        _access: &ConversationAccess,
    ) -> Result<ConversationRecord, AppError> {
        Err(not_stubbed("find_conversation_authorized"))
    }

    async fn list_conversations_authorized(
        &self,
        _access: &ConversationAccess,
        _query: &ConversationQuery,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<(ConversationRecord, ListCursor)>, AppError> {
        Err(not_stubbed("list_conversations_authorized"))
    }

    async fn patch_conversation(
        &self,
        _public_id: &str,
        _request: &ConversationPatchRequest,
    ) -> Result<ConversationRecord, AppError> {
        Err(not_stubbed("patch_conversation"))
    }

    async fn set_conversation_status(
        &self,
        _public_id: &str,
        _status: ConversationStatus,
    ) -> Result<ConversationRecord, AppError> {
        Err(not_stubbed("set_conversation_status"))
    }

    async fn add_message(
        &self,
        _insert: &ConversationMessageInsert,
    ) -> Result<ConversationMessageRecord, AppError> {
        Err(not_stubbed("add_message"))
    }

    async fn list_messages(
        &self,
        _conversation_public_id: &str,
        _query: &ConversationMessageQuery,
        _cursor: Option<SeqCursor>,
        _limit: i64,
    ) -> Result<Vec<(ConversationMessageRecord, SeqCursor)>, AppError> {
        Err(not_stubbed("list_messages"))
    }

    async fn create_memory(&self, _insert: &MemoryInsert<'_>) -> Result<MemoryRecord, AppError> {
        Err(not_stubbed("create_memory"))
    }

    async fn find_memory_authorized(
        &self,
        _public_id: &str,
        _access: &ConversationAccess,
    ) -> Result<MemoryRecord, AppError> {
        Err(not_stubbed("find_memory_authorized"))
    }

    async fn list_memories_authorized(
        &self,
        _access: &ConversationAccess,
        _query: &MemoryQuery,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<(MemoryRecord, ListCursor)>, AppError> {
        Err(not_stubbed("list_memories_authorized"))
    }

    async fn patch_memory(
        &self,
        _public_id: &str,
        _request: &MemoryPatchRequest,
        _content_hash: Option<&str>,
    ) -> Result<MemoryRecord, AppError> {
        Err(not_stubbed("patch_memory"))
    }

    async fn delete_memory(&self, _public_id: &str) -> Result<(), AppError> {
        Err(not_stubbed("delete_memory"))
    }

    async fn create_rag_collection(
        &self,
        _id: Uuid,
        _public_id: &str,
        _request: &RagCollectionCreateRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        Err(not_stubbed("create_rag_collection"))
    }

    async fn list_rag_collections(
        &self,
        _query: &RagCollectionQuery,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<(RagCollectionRecord, ListCursor)>, AppError> {
        Err(not_stubbed("list_rag_collections"))
    }

    async fn get_rag_collection(&self, _public_id: &str) -> Result<RagCollectionRecord, AppError> {
        Err(not_stubbed("get_rag_collection"))
    }

    async fn patch_rag_collection(
        &self,
        _public_id: &str,
        _request: &RagCollectionPatchRequest,
    ) -> Result<RagCollectionRecord, AppError> {
        Err(not_stubbed("patch_rag_collection"))
    }

    async fn set_rag_collection_status(
        &self,
        _public_id: &str,
        _status: RagCollectionStatus,
    ) -> Result<RagCollectionRecord, AppError> {
        Err(not_stubbed("set_rag_collection_status"))
    }

    async fn create_rag_document(
        &self,
        _id: Uuid,
        _public_id: &str,
        _collection_public_id: &str,
        _request: &RagDocumentCreateRequest,
        _content_hash: Option<&str>,
        _plan: &RagIngestionPlan,
    ) -> Result<RagDocumentRecord, AppError> {
        Err(not_stubbed("create_rag_document"))
    }

    async fn list_rag_documents(
        &self,
        _collection_public_id: &str,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<(RagDocumentRecord, ListCursor)>, AppError> {
        Err(not_stubbed("list_rag_documents"))
    }

    async fn get_rag_document(&self, _public_id: &str) -> Result<RagDocumentRecord, AppError> {
        Err(not_stubbed("get_rag_document"))
    }

    async fn delete_rag_document(&self, _public_id: &str) -> Result<(), AppError> {
        Err(not_stubbed("delete_rag_document"))
    }

    async fn ingest_rag_document(
        &self,
        _public_id: &str,
        _request: &RagDocumentIngestRequest,
        _content_hash: &str,
        _plan: &RagIngestionPlan,
    ) -> Result<RagDocumentRecord, AppError> {
        Err(not_stubbed("ingest_rag_document"))
    }
}

#[cfg(test)]
mod repository_trait_tests {
    use super::*;

    /// P2-3: the conversation-policy document — the gate in front of every conversation, memory
    /// and RAG feature — is now exercisable in a unit test without Postgres.
    #[tokio::test]
    async fn fake_conversation_repository_supports_policy_unit_test() {
        let application_id = Uuid::now_v7();
        let repo = InMemoryConversationRepository::default();

        // `get_or_create` mints a closed-by-default policy.
        let created = repo
            .get_or_create_conversation_policy(application_id)
            .await
            .expect("policy");
        assert!(!created.conversations_enabled);
        assert!(!created.rag_enabled);
        assert_eq!(created.version, 1);

        // A partial `put` changes only the fields it names and bumps the version.
        let updated = repo
            .put_conversation_policy(
                application_id,
                &ConversationPolicyPutRequest {
                    conversations_enabled: Some(true),
                    maximum_recent_messages: Some(50),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .expect("put policy");
        assert!(updated.conversations_enabled);
        assert_eq!(updated.maximum_recent_messages, 50);
        assert_eq!(updated.version, 2);
        // Untouched fields survive — `coalesce($n, existing)`, not overwrite-with-default.
        assert_eq!(
            updated.maximum_history_tokens,
            created.maximum_history_tokens
        );
        assert!(!updated.rag_enabled);

        // The store is the source of truth for the next read.
        let reread = repo
            .get_or_create_conversation_policy(application_id)
            .await
            .expect("policy");
        assert!(reread.conversations_enabled);
        assert_eq!(reread.version, 2);

        // Policies are per application.
        let other = repo
            .get_or_create_conversation_policy(Uuid::now_v7())
            .await
            .expect("policy");
        assert!(!other.conversations_enabled);
    }

    #[tokio::test]
    async fn unbacked_fake_conversation_methods_fail_loudly() {
        let repo = InMemoryConversationRepository::default();
        let error = repo
            .list_rag_collections(&RagCollectionQuery::default(), None, 10)
            .await
            .expect_err("unbacked method");
        assert!(error.to_string().contains("not stubbed"));
    }
}
