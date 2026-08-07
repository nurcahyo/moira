use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use zeroize::Zeroizing;

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

/// What an application persists of its conversation content.
///
/// Enforced at every site that writes a body derived from caller content: `add_message`
/// (`conversation_messages`), the summarization write (`conversation_summaries`), and — since
/// issue #140 — all three memory writers (`create_memory`, `insert_extracted_memory`,
/// `patch_memory`, over `memory_records`). `add_message` is the choke point every message writer
/// already goes through, so a new message call site inherits the policy rather than having to
/// remember it. Every site routes the body through [`ContentWrite`], so a fifth value here is a
/// compile error at each of them rather than a silently defaulted branch.
///
/// # What each value does
///
/// | Value | Body | Length-revealing metadata |
/// |---|---|---|
/// | `plain_content` (default) | stored as plaintext in `*_plain` | stored |
/// | `metadata_only` | **not stored** | stored |
/// | `none` | **not stored** | **not stored** — `content_size_bytes` is `0`, `token_count` is null |
/// | `encrypted_content` | **sealed** into `*_encrypted` (AES-256-GCM, per-row nonce, AAD bound to the row's identity) | stored |
///
/// `conversation_messages.content_hash` is retained under every value. It is an HMAC under a
/// deployment-held pepper, not a content address — see `crate::security::idempotency` — so it is
/// a fingerprint of content rather than content, and dropping it would break the documented
/// `"{pepper_version}:{base64url}"` shape the API contract already publishes. Caller-supplied
/// `metadata` is likewise retained under every value: it is the caller's own JSON, not
/// something derived from the message body.
///
/// # `encrypted_content`, since issue #139
///
/// The body is sealed by [`crate::security::ContentCipher`] under the content keyring's active
/// data key and written to the `*_encrypted` column; `*_plain` is left null, and the CHECK
/// constraints from `migrations/0027_content_encryption_keyring.sql` make holding both a database
/// refusal. Reads open it transparently, so the public API returns the same bytes the caller
/// wrote.
///
/// Three consequences that are easy to misread and are therefore stated:
///
/// * **`content_size_bytes` and `token_count` are still computed on the plaintext.** Ciphertext
///   length never reaches a counter, so flipping the policy does not move a limit or shift a
///   metric.
/// * **Refusal, never fallback.** A write under this value with no usable content key returns
///   `503 content_key_unavailable` and stores **nothing**. Writing plaintext under a policy
///   named for encryption would be finding F32 with extra steps.
/// * **It is not retroactive, in either direction.** Switching *to* `encrypted_content` does not
///   encrypt existing history, and switching *away* does not decrypt it. The policy governs
///   subsequent writes only; already-stored rows keep their storage form and stay readable.
///   Removing content is retention's job.
///
/// # What it does and does not govern
///
/// * **It governs memories, since issue #140.** `application_memory_policies` has no
///   content-persistence column of its own and never had one; this is the application's single
///   setting for "what do you keep of caller content", and a memory body is caller content in
///   exactly the sense it names. All four values apply: a memory written under `none` or
///   `metadata_only` stores no body at all and reads back with `content: null`, which is a
///   **behaviour change** for such applications — before #140 memory bodies were stored in the
///   clear whatever the policy said, which is finding F32's shape one table over.
/// * **`memory_records.content_hash` is retained under every value, and its form no longer
///   follows this setting.** Since issue #168 it is a digest keyed by the keyring's
///   `memory_dedupe` key under **all four** values, where #140 keyed only `encrypted_content`.
///   See [`crate::security::ContentSealer::memory_content_hash`]: a memory body is short and
///   guessable, so an unkeyed digest is a dictionary-attack oracle — under `encrypted_content` it
///   defeated the encryption outright, and under `none` and `metadata_only` it was an oracle for
///   content the row deliberately does not hold.
/// * **It governs RAG bodies on the sealing axis only, since issue #141.**
///   `rag_document_versions.content_plain` and `rag_chunks.chunk_text_plain` are wired to their
///   `*_encrypted` columns through [`ContentWrite::under_policy_for_rag`], which seals under
///   `encrypted_content` and stores plaintext under every other value. `none` and
///   `metadata_only` are deliberately **not** honoured there, and the reason is on that function:
///   honouring them would produce a false privacy claim rather than privacy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationContentPersistence {
    None,
    MetadataOnly,
    PlainContent,
    EncryptedContent,
}

impl ConversationContentPersistence {
    /// Whether a body derived from caller content may be written as plaintext.
    ///
    /// `PlainContent` alone. `EncryptedContent` stays false here now that a cipher exists, for
    /// the same reason it was false before one did: storing the body in the clear under a value
    /// named for encryption is the failure mode this policy exists to prevent. What changed is
    /// only that its body now goes somewhere ([`Self::persists_ciphertext`]) rather than nowhere.
    pub const fn persists_plaintext(self) -> bool {
        matches!(self, Self::PlainContent)
    }

    /// Whether metadata *derived from* the content — its length in bytes and in tokens — may
    /// be written.
    ///
    /// This is the only thing separating `None` from `MetadataOnly`; two enum values with
    /// identical behaviour would be the same defect this policy is being wired to fix.
    pub const fn persists_content_metadata(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether a body derived from caller content is sealed into the `*_encrypted` column.
    ///
    /// `EncryptedContent` alone, and it is the exact complement of [`Self::persists_plaintext`]
    /// rather than an overlap: the CHECK constraints added by
    /// `migrations/0027_content_encryption_keyring.sql` make a row holding both a refusal at the
    /// database, so a policy that claimed both would be a write that cannot commit.
    pub const fn persists_ciphertext(self) -> bool {
        matches!(self, Self::EncryptedContent)
    }
}

/// What a write offers for a content column — and, once the policy has been applied, what is
/// stored in it.
///
/// **This replaces `content_plain: Option<String>` on the insert structs; it does not sit beside
/// it.** Three things follow from replacing rather than adding, and all three are the point:
///
/// 1. A caller **physically cannot** supply a plaintext and a ciphertext for the same row. The
///    CHECK constraint from migration 0027 says the same thing at the database; this says it at
///    compile time, where the fix is cheaper.
/// 2. `Omitted` is a **named state**, not a `None` with a comment next to it explaining which of
///    two very different intentions it encodes.
/// 3. A fourth persistence mode becomes a **compile error at every write site**, because
///    [`ContentWrite::under_policy`] is exhaustive on
///    [`ConversationContentPersistence`] and every storage `match` is exhaustive on this enum
///    with no catch-all arm.
///
/// Point 3 is why the wider diff was worth it. Finding F32 was precisely a write path that
/// ignored its policy while two comments asserted it did not; making "forgotten" unrepresentable
/// is the only version of that fix that a fourth writer inherits for free.
///
/// # `Debug` is hand-written
///
/// A derived `Debug` would render the message body into any stray `{:?}` — including
/// `ConversationMessageInsert`'s own derived one — and `tests/content_leak_snapshots.rs` exists
/// because that has happened. This prints the variant and a byte count.
#[derive(Clone)]
pub enum ContentWrite {
    /// Policy `none` or `metadata_only`: no body is stored at all, in either column.
    Omitted,
    /// Policy `plain_content`: stored verbatim in the `*_plain` column.
    Plain(String),
    /// Policy `encrypted_content`: **the repository seals it**. The plaintext never reaches a
    /// column, and the wrapper keeps it from lingering in freed memory after the seal.
    Encrypt(Zeroizing<String>),
}

impl std::fmt::Debug for ContentWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Omitted => f.write_str("ContentWrite::Omitted"),
            Self::Plain(text) => write!(f, "ContentWrite::Plain({} bytes)", text.len()),
            Self::Encrypt(text) => write!(f, "ContentWrite::Encrypt({} bytes)", text.len()),
        }
    }
}

impl ContentWrite {
    /// Decide what to store, from the application's policy and the body the caller offered.
    ///
    /// The `match` is exhaustive with no catch-all: a fifth
    /// [`ConversationContentPersistence`] value does not compile until somebody decides, here,
    /// what it stores.
    pub fn under_policy(persistence: ConversationContentPersistence, plaintext: String) -> Self {
        match persistence {
            ConversationContentPersistence::None | ConversationContentPersistence::MetadataOnly => {
                Self::Omitted
            }
            ConversationContentPersistence::PlainContent => Self::Plain(plaintext),
            ConversationContentPersistence::EncryptedContent => {
                Self::Encrypt(Zeroizing::new(plaintext))
            }
        }
    }

    /// Decide what to store for a **RAG** body — a `rag_document_versions.content_*` or a
    /// `rag_chunks.chunk_text_*` column.
    ///
    /// Two of the four policy values map differently here than they do in
    /// [`Self::under_policy`], and the asymmetry is a decision rather than an oversight, so it
    /// is written down where it is made.
    ///
    /// | policy | conversations, summaries, memories | RAG bodies |
    /// |---|---|---|
    /// | `plain_content` | `Plain` | `Plain` |
    /// | `encrypted_content` | `Encrypt` | `Encrypt` |
    /// | `metadata_only` | `Omitted` | **`Plain`** |
    /// | `none` | `Omitted` | **`Plain`** |
    ///
    /// # Why `Omitted` is refused here
    ///
    /// Not because RAG content is less sensitive. Because a `rag_chunks` row is one of several
    /// artifacts derived from the same document, and this column is the only one of them a
    /// content policy can currently suppress. Under `Omitted` the row would still carry:
    ///
    /// * `section_title` — **a verbatim substring of the document**, the nearest preceding
    ///   Markdown heading (`crate::orchestration::chunk`);
    /// * `start_offset`, `end_offset` and `token_count` — the chunk's exact position and size;
    /// * `chunk_hash` — an **unkeyed** SHA-256 content address, by the decision recorded in
    ///   `docs/security.md`;
    /// * a `rag_chunk_embeddings` row — a dense vector computed from the plaintext, from which
    ///   embedding-inversion attacks recover substantial source text.
    ///
    /// A build that dropped the body and kept those four would be telling an operator it stored
    /// nothing while storing the headings, the shape and an invertible encoding of the text. That
    /// is a false privacy claim, which is worse than an honest absence of one, and it is exactly
    /// the class of dishonesty finding F32 was.
    ///
    /// `encrypted_content` has no such problem: it is a claim about *this* column, it is true of
    /// this column, and `docs/security.md` states plainly which sibling artifacts it does not
    /// cover.
    ///
    /// # Reversal condition
    ///
    /// Route RAG through [`Self::under_policy`] — one rule for all five columns — the moment the
    /// derived artifacts can be suppressed together with the body: `section_title` dropped,
    /// offsets and token counts dropped, `chunk_hash` keyed or dropped, and the embedding rows
    /// not written. That is a RAG-ingestion design change, not a cipher change, and it is not
    /// this decision to make.
    ///
    /// The `match` is exhaustive with no catch-all, exactly like [`Self::under_policy`], so a
    /// fifth [`ConversationContentPersistence`] value does not compile until somebody decides
    /// what a RAG body does with it.
    pub fn under_policy_for_rag(
        persistence: ConversationContentPersistence,
        plaintext: String,
    ) -> Self {
        match persistence {
            ConversationContentPersistence::None
            | ConversationContentPersistence::MetadataOnly
            | ConversationContentPersistence::PlainContent => Self::Plain(plaintext),
            ConversationContentPersistence::EncryptedContent => {
                Self::Encrypt(Zeroizing::new(plaintext))
            }
        }
    }

    /// The plaintext this write carries, whichever column it is destined for.
    ///
    /// **The counters are computed through this, before sealing.** `content_size_bytes`,
    /// `token_count` and the 262,144-byte content cap are all arithmetic somebody else does
    /// later; letting a ciphertext length reach any of them would move a limit and shift a
    /// metric the moment an operator flips the policy, with nothing in the request to explain it.
    pub fn plaintext(&self) -> Option<&str> {
        match self {
            Self::Omitted => None,
            Self::Plain(text) => Some(text.as_str()),
            Self::Encrypt(text) => Some(text.as_str()),
        }
    }
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

impl MemoryConsentMode {
    /// How much this mode permits, as a rank. Higher is more permissive.
    ///
    /// Deliberately **not** a `PartialOrd`/`Ord` derive. A derive would order the variants by
    /// declaration position, which puts `ApplicationManaged` below `AutomaticWithUserControls`
    /// for no reason anyone chose, and would silently make `<` mean something on a type where the
    /// only meaningful comparison is this one. The rank is also not total: the two consenting
    /// modes differ in *who* asserted consent, not in how much is permitted, and pretending
    /// otherwise would invent a precedence the schema does not have.
    const fn permissiveness(self) -> u8 {
        match self {
            // Consent withheld. There is no weaker thing to do.
            Self::Disabled => 0,
            // Memories may be written, but not used until a human confirms each one.
            Self::ExplicitOnly => 1,
            // Both mean "usable on arrival"; they differ in who asserted consent, not in scope.
            Self::AutomaticWithUserControls | Self::ApplicationManaged => 2,
        }
    }

    /// The effective consent mode for an application, given its **two** consent columns.
    ///
    /// # Why this exists on the domain type — finding F30
    ///
    /// `application_conversation_policies.memory_consent_mode` and
    /// `application_memory_policies.consent_mode` (`migrations/0007…:20-21` and `:40-41`) are
    /// independent `varchar(64)` columns over the same four values, both defaulting to
    /// `'explicit_only'`. **Nothing in the schema reconciles them** — a cross-table `CHECK` is not
    /// something Postgres offers — so the reconciliation has to be a code rule, and F30's whole
    /// point is that a code rule holds only as long as every reader goes through it.
    ///
    /// It did not. `effective_extraction_status` took the stricter of the two from the day
    /// Sub-Phase F landed, but two later readers consulted the memory column alone: the manual
    /// memory API (deliberately — see `ConversationService::create_memory`), and
    /// `conversation_select`'s `memory_behavior`, which was `coalesce(mp.consent_mode,
    /// 'explicit_only')` **in SQL**, where no Rust helper could have been in its way. That is the
    /// failure mode F30 predicted, arriving as predicted, in the one language the prediction did
    /// not cover.
    ///
    /// Putting the rule here rather than in `application::memory_extraction` is what lets
    /// `src/infra/pg_rows.rs` use it, which is what removes the last consent decision from SQL.
    ///
    /// # The tie, and why it resolves toward the memory policy
    ///
    /// `application_managed` and `automatic_with_user_controls` are equally permissive
    /// ([`Self::permissiveness`]), so on that pair "stricter" does not pick a winner and this
    /// returns `memory`. That is not arbitrary: `memory_behavior` has always reported the memory
    /// policy's value, so a tie leaves every existing deployment's reported value byte-identical
    /// and the only value that changes is the one that was wrong — where the conversation policy
    /// is genuinely stricter than the memory policy.
    ///
    /// The asymmetry is confined to the *label*. The extraction decision is symmetric, because
    /// both tied modes map to the same `MemoryStatus`; `the_combined_consent_decision_is_symmetric`
    /// pins that over all sixteen pairs.
    pub fn stricter_of(conversation: Self, memory: Self) -> Self {
        if conversation.permissiveness() < memory.permissiveness() {
            conversation
        } else {
            memory
        }
    }
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
    // The consent mode actually in force: `MemoryConsentMode::stricter_of` over both consent
    // columns, or `"policy_controlled"` when the query did not select them. Computed by
    // `effective_memory_behavior` in `src/infra/pg_rows.rs` — deliberately not in SQL, see F30.
    // A `//` comment rather than `///` on purpose: this struct is `ToSchema`, so a doc comment
    // would land in `docs/openapi.json` and drift from it.
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
