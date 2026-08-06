//! Finding F14 — `memory_records.content_hash` survives an idempotency-pepper rotation, and
//! `conversation_messages.content_hash` deliberately does not.
//!
//! # What is actually under test
//!
//! F14 was recorded as "memory dedupe silently stops matching after a pepper rotation": the
//! column was written with `IdempotencyHasher::hash`, whose `verify` accepts **only** the active
//! pepper. That contract is justified in its own module doc by a retention argument — every
//! `idempotency_records` row expires within 24 hours — which `memory_records` does not have.
//! The column moved to the unkeyed `crate::security::request_hash`.
//!
//! **A test that never rotates proves nothing here.** Every assertion about the *shape* of the
//! stored value — "it has no `:`", "it is 43 characters", "it equals `request_hash(content)`" —
//! passes in both arrangements for at least one plausible mutation, because shape is not the
//! property. The property is that the *same content hashed on either side of a rotation
//! produces the same value*, so a dedupe lookup written before the rotation still finds the row
//! after it. So both tests here build a second `AppState` on the **same database** with a
//! different pepper and a bumped `pepper_version`, which is exactly what an operator rotating
//! `MOIRA_IDEMPOTENCY__PEPPER_BASE64` does, and compare across it.
//!
//! # Why the negative test is not optional
//!
//! `conversation_messages.content_hash` fails the first clause of the `chunk_hash` admitting
//! rule outright: it is returned to callers on `ConversationMessageRecord`. Handing out an
//! unkeyed digest of message content would give any holder an offline verifier against content
//! the schema expects to be able to hold encrypted. So the two columns must stay *different*,
//! and the cheapest way for that to be lost is a later "cleanup" that unifies them on the
//! grounds that they share a name. `conversation_message_content_hash_stays_peppered…` fails
//! loudly if that happens, in both directions.
//!
//! That second test also pins the API-visible consequence nobody had recorded: rotating the
//! pepper changes the `content_hash` the API returns for a message whose content never changed.

mod support;

use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use moira::{
    app::AppState,
    application::ConversationService,
    domain::{
        ConversationCreateRequest, ConversationMessageCreateRequest, ConversationMessageRole,
        MemoryCreateRequest, MemoryPatchRequest, MemoryType,
    },
    security::{Actor, IdempotencyHasher, request_hash},
};
use serde_json::json;
use sha2::Digest;
use sqlx::Row;
use tokio::time::timeout;

use support::{LifecycleFixture, request_context};

const WAIT: Duration = Duration::from_secs(20);

/// The pepper the rotated state is built with. Any 32 bytes that are not the fixture's own
/// (`vec![13; 32]`, from `IdempotencySettings::pepper_bytes`'s dev fallback) will do.
const ROTATED_PEPPER: [u8; 32] = [200; 32];
/// Rotated alongside the bytes, because `IdempotencyHasher::verify` keys its rejection on the
/// version prefix and a realistic rotation moves both.
const ROTATED_VERSION: &str = "v2";

/// The committed re-hash migration, executed verbatim by
/// `migration_0021_rewrites_peppered_memory_hashes_into_content_addresses`.
///
/// Read from disk rather than restated here: a copy would drift, and the thing under test is
/// the file that will run against production, not a paraphrase of it.
const MIGRATION_0021: &str = "migrations/0021_memory_content_hash_is_content_addressed.sql";

struct Case {
    fixture: LifecycleFixture,
    /// The application-bound caller, with the memory scopes `create_memory`/`patch_memory` need.
    caller: Actor,
}

impl Case {
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        fixture.enable_manual_memory().await;
        let mut caller = fixture.caller_actor(Some("tenant-f14"), Some("user-f14"));
        caller.scopes.push("moira:memories:write".to_string());
        caller.scopes.push("moira:conversations:write".to_string());
        Some(Self { fixture, caller })
    }

    /// The state as deployed today.
    fn current(&self) -> ConversationService {
        ConversationService::new(&self.fixture.state).expect("conversation service")
    }

    /// The same database, seen by a process whose idempotency pepper has been rotated.
    ///
    /// Built from the fixture's own settings so nothing but the pepper differs — a rotated
    /// state that also changed, say, the retrieval policy would not be a rotation.
    async fn rotated_state(&self) -> AppState {
        let mut settings = (*self.fixture.state.settings).clone();
        settings.idempotency.pepper_base64 = Some(STANDARD.encode(ROTATED_PEPPER));
        settings.idempotency.pepper_version = ROTATED_VERSION.to_string();
        let state = AppState::new(settings, Some(self.fixture.pool.clone()))
            .await
            .expect("an app state on the rotated pepper");
        assert_ne!(
            state.idempotency_hasher.hash(b"probe"),
            self.fixture.state.idempotency_hasher.hash(b"probe"),
            "the rotated state must actually hash differently, or this test is vacuous"
        );
        state
    }

    async fn create_memory(&self, service: &ConversationService, content: &str) -> String {
        timeout(
            WAIT,
            service.create_memory(
                &self.caller,
                &request_context(),
                MemoryCreateRequest {
                    memory_type: MemoryType::Fact,
                    content: content.to_string(),
                    importance: None,
                    confidence: None,
                    valid_until: None,
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("create memory timed out")
        .expect("create memory")
        .id
    }

    async fn patch_memory_content(
        &self,
        service: &ConversationService,
        memory_id: &str,
        content: &str,
    ) {
        timeout(
            WAIT,
            service.patch_memory(
                &self.caller,
                &request_context(),
                memory_id,
                MemoryPatchRequest {
                    content: Some(content.to_string()),
                    ..MemoryPatchRequest::default()
                },
            ),
        )
        .await
        .expect("patch memory timed out")
        .expect("patch memory");
    }

    async fn stored_memory_hash(&self, memory_id: &str) -> String {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select content_hash from memory_records where public_id = $1",
            )
            .bind(memory_id)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("memory hash query timed out")
        .expect("memory hash query")
    }

    /// The dedupe lookup Sub-Phase F will make: exact match, scoped to one application.
    async fn memories_matching(&self, hash: &str) -> Vec<String> {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select public_id from memory_records \
                 where application_id = $1 and content_hash = $2 and deleted_at is null \
                 order by public_id",
            )
            .bind(self.fixture.application_id)
            .bind(hash)
            .fetch_all(&self.fixture.pool),
        )
        .await
        .expect("dedupe lookup timed out")
        .expect("dedupe lookup")
    }

    async fn create_message(
        &self,
        service: &ConversationService,
        content: &str,
    ) -> (String, String) {
        let conversation = timeout(
            WAIT,
            service.create_conversation(
                &self.caller,
                &request_context(),
                ConversationCreateRequest {
                    title: Some("f14".to_string()),
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("create conversation timed out")
        .expect("create conversation");
        let message = timeout(
            WAIT,
            service.create_message(
                &self.caller,
                &request_context(),
                &conversation.id,
                ConversationMessageCreateRequest {
                    role: ConversationMessageRole::User,
                    content: content.to_string(),
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("create message timed out")
        .expect("create message");
        (message.id.clone(), message.content_hash)
    }

    async fn stored_message_hash(&self, message_id: &str) -> String {
        let row = timeout(
            WAIT,
            sqlx::query("select content_hash from conversation_messages where public_id = $1")
                .bind(message_id)
                .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("message hash query timed out")
        .expect("message hash query");
        row.try_get("content_hash").expect("content_hash column")
    }
}

/// The headline F14 property: hash memory content, rotate the pepper, and the dedupe lookup
/// still matches.
///
/// Both writers are exercised — `create_memory` and `patch_memory` — because the cheapest edit
/// that reopens F14 is reverting only one of them, and a test that drove only `POST /memories`
/// would stay green through it.
#[tokio::test]
async fn memory_content_hash_still_matches_after_an_idempotency_pepper_rotation() {
    let Some(case) = Case::new().await else {
        return;
    };
    let created_content = format!("a remembered fact {}", case.fixture.application_id);
    let patched_content = format!("a corrected fact {}", case.fixture.application_id);

    // --- before the rotation -------------------------------------------------------------
    let before = case.current();
    let first = case.create_memory(&before, &created_content).await;
    let first_created_hash = case.stored_memory_hash(&first).await;
    case.patch_memory_content(&before, &first, &patched_content)
        .await;
    let first_patched_hash = case.stored_memory_hash(&first).await;
    assert_ne!(
        first_created_hash, first_patched_hash,
        "patching the content must re-address the row, or the column is not tracking content"
    );

    // --- rotate --------------------------------------------------------------------------
    let rotated_state = case.rotated_state().await;
    let after = ConversationService::new(&rotated_state).expect("rotated conversation service");

    // --- the same content, hashed by the rotated process ---------------------------------
    let second = case.create_memory(&after, &created_content).await;
    assert_eq!(
        case.stored_memory_hash(&second).await,
        first_created_hash,
        "the same memory content must hash identically across a pepper rotation — this is the \
         whole of F14. If these differ, `create_memory` is back on a keyed hasher and exact-match \
         dedupe silently stops matching every pre-rotation row."
    );

    case.patch_memory_content(&after, &second, &patched_content)
        .await;
    assert_eq!(
        case.stored_memory_hash(&second).await,
        first_patched_hash,
        "`patch_memory` must write the same content address as `create_memory`, across the \
         rotation too — reverting only the patch writer is the cheapest way back to F14"
    );

    // --- the lookup a dedupe would actually perform ---------------------------------------
    let matched = case.memories_matching(&first_patched_hash).await;
    assert_eq!(
        matched.len(),
        2,
        "an application-scoped exact-match lookup by content hash must find the pre-rotation row \
         *and* the post-rotation one; it found {matched:?}"
    );
    assert!(
        matched.contains(&first) && matched.contains(&second),
        "the lookup returned the wrong rows: {matched:?}"
    );

    // --- and the stored value really is the unkeyed content address -----------------------
    assert_eq!(
        first_patched_hash,
        request_hash(patched_content.as_bytes()),
        "the stored value must be `request_hash` over the exact content bytes"
    );
    assert!(
        !first_patched_hash.contains(':'),
        "a content address carries no pepper-version prefix, got {first_patched_hash}"
    );
    assert!(
        first_patched_hash.len() <= 128,
        "content hash must fit varchar(128)"
    );
}

/// The negative: `conversation_messages.content_hash` is **still** keyed, so a later change
/// cannot quietly unify the two columns on the grounds that they share a name.
///
/// It also pins the consequence that makes keeping it keyed a documented trade rather than an
/// oversight: because the value is returned to callers, rotating the pepper changes an
/// API-visible field for a message whose content never changed.
#[tokio::test]
async fn conversation_message_content_hash_stays_peppered_and_changes_on_rotation() {
    let Some(case) = Case::new().await else {
        return;
    };
    let content = format!(
        "a message that is not a content address {}",
        case.fixture.application_id
    );

    let before_state = case.fixture.state.clone();
    let (first_id, first_returned) = case.create_message(&case.current(), &content).await;
    let first_stored = case.stored_message_hash(&first_id).await;

    let hasher_before = IdempotencyHasher::new(
        before_state
            .settings
            .idempotency
            .pepper_bytes()
            .expect("the test profile resolves an idempotency pepper"),
        before_state.settings.idempotency.pepper_version.clone(),
    );
    assert_eq!(
        first_stored,
        hasher_before.hash(content.as_bytes()),
        "message content must still be hashed with the active peppered hasher"
    );
    assert_ne!(
        first_stored,
        request_hash(content.as_bytes()),
        "message content must NOT be the unkeyed content address — it is returned to callers, so \
         an unkeyed digest is an offline verifier against content the schema can hold encrypted"
    );
    assert_eq!(
        first_returned, first_stored,
        "the API returns exactly the stored value, which is what makes it caller-visible"
    );
    assert!(
        first_stored.starts_with(&format!("{}:", hasher_before.pepper_version())),
        "a keyed hash carries its pepper-version prefix, got {first_stored}"
    );

    // --- rotate, and write the identical content again ------------------------------------
    let rotated_state = case.rotated_state().await;
    let after = ConversationService::new(&rotated_state).expect("rotated conversation service");
    let (second_id, second_returned) = case.create_message(&after, &content).await;
    let second_stored = case.stored_message_hash(&second_id).await;

    assert_eq!(
        second_stored,
        IdempotencyHasher::new(ROTATED_PEPPER.to_vec(), ROTATED_VERSION).hash(content.as_bytes()),
        "after the rotation the new pepper must be the one in use"
    );
    assert_ne!(
        first_returned, second_returned,
        "documented consequence of keeping this column keyed: identical message content reports a \
         different `content_hash` to the caller on either side of a pepper rotation. If this ever \
         becomes equal, the column has been moved to an unkeyed digest — which is the change this \
         test exists to prevent."
    );
    assert!(
        second_stored.starts_with(&format!("{ROTATED_VERSION}:")),
        "the rotated value must carry the rotated version prefix, got {second_stored}"
    );
}

/// Migration `0021` recomputes the digest **in SQL**, so nothing in the Rust suite would
/// otherwise notice if the two disagreed — and a wrong recompute is silent by construction: it
/// writes a plausible 43-character base64url string that simply never matches anything.
///
/// So the committed file is executed verbatim against a real PostgreSQL, over content chosen to
/// break a careless expression: non-ASCII (`convert_to(…, 'UTF8')` is load-bearing — the server
/// encoding is not guaranteed), and a `+`/`/`-producing digest is covered by the alphabet
/// translation being asserted against Rust's own `URL_SAFE_NO_PAD` output rather than by shape.
///
/// The two rows the migration must *not* touch are asserted too, because "recompute everything"
/// is the obvious wrong version of this statement: a row already holding a content address must
/// be left alone (re-hashing a hash), and a row with no `content_plain` cannot be recomputed at
/// all — the plaintext is not in the database, so hashing whatever is there would fabricate an
/// address rather than compute one.
#[tokio::test]
async fn migration_0021_rewrites_peppered_memory_hashes_into_content_addresses() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // Multi-byte, combining marks, and an emoji: three different ways a byte-vs-character
    // mistake in the SQL would show up.
    let content = "mémoire — 記憶 🧠 f14";
    // Dropping `translate(…, '+/', '-_')` from the migration is the classic silent mistake, and
    // whether this test notices depends *entirely* on the literal above: roughly one digest in
    // eight contains neither `+` nor `/`, and a future edit that innocently reworded the string
    // could leave the alphabet unexercised and the guard green through the exact defect it was
    // written for. So the literal's own suitability is asserted rather than assumed.
    let standard_alphabet = STANDARD.encode(sha2::Sha256::digest(content.as_bytes()));
    assert!(
        standard_alphabet.contains('+') && standard_alphabet.contains('/'),
        "this test's content must digest to a value using BOTH characters the base64url \
         translation rewrites, or dropping that translation from the migration goes unnoticed. \
         `{content}` gives {standard_alphabet}; pick a different literal."
    );
    let hasher = IdempotencyHasher::new(vec![7; 32], "v1");

    let peppered = seed_memory_row(&fixture, Some(content), &hasher.hash(content.as_bytes())).await;
    let already_addressed =
        seed_memory_row(&fixture, Some(content), &request_hash(content.as_bytes())).await;
    let no_plaintext = seed_memory_row(&fixture, None, &hasher.hash(b"unavailable")).await;

    let sql = std::fs::read_to_string(MIGRATION_0021)
        .unwrap_or_else(|err| panic!("read {MIGRATION_0021}: {err}"));
    timeout(WAIT, sqlx::raw_sql(&sql).execute(&fixture.pool))
        .await
        .expect("migration 0021 timed out")
        .expect("run migration 0021");

    assert_eq!(
        row_hash(&fixture, &peppered).await,
        request_hash(content.as_bytes()),
        "the migration's SQL digest must equal `crate::security::request_hash` over the same \
         bytes; if it does not, every re-hashed row on a real deployment holds a value nothing \
         will ever match"
    );
    assert_eq!(
        row_hash(&fixture, &already_addressed).await,
        request_hash(content.as_bytes()),
        "a row already holding a content address must be left exactly as it was"
    );
    assert_eq!(
        row_hash(&fixture, &no_plaintext).await,
        hasher.hash(b"unavailable"),
        "a row with no `content_plain` has no recoverable plaintext, so the migration must leave \
         its value alone rather than hash something else"
    );
    assert!(
        row_hash(&fixture, &no_plaintext).await.contains(':'),
        "and the value it keeps stays self-describing: a `:` can never appear in a content \
         address, so a dedupe reader misses such a row instead of matching it wrongly"
    );
}

/// Inserts an `application`-scoped memory row directly — the scope that the
/// `memory_records_scope_valid` check constraint allows with neither a tenant nor a user, so a
/// row with a deliberately absent `content_plain` is still legal.
async fn seed_memory_row(
    fixture: &LifecycleFixture,
    content_plain: Option<&str>,
    content_hash: &str,
) -> String {
    let id = uuid::Uuid::now_v7();
    let public_id = format!("mem_{id}");
    timeout(
        WAIT,
        sqlx::query(
            "insert into memory_records \
             (id, public_id, application_id, memory_scope, memory_type, content_plain, \
              content_hash) \
             values ($1, $2, $3, 'application', 'fact', $4, $5)",
        )
        .bind(id)
        .bind(&public_id)
        .bind(fixture.application_id)
        .bind(content_plain)
        .bind(content_hash)
        .execute(&fixture.pool),
    )
    .await
    .expect("seed memory row timed out")
    .expect("seed memory row");
    public_id
}

async fn row_hash(fixture: &LifecycleFixture, public_id: &str) -> String {
    timeout(
        WAIT,
        sqlx::query_scalar::<_, String>(
            "select content_hash from memory_records where public_id = $1",
        )
        .bind(public_id)
        .fetch_one(&fixture.pool),
    )
    .await
    .expect("row hash query timed out")
    .expect("row hash query")
}
