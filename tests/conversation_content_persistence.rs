//! `application_conversation_policies.conversation_content_persistence`, enforced — finding F32.
//!
//! # What this suite is guarding against
//!
//! The column was validated, versioned, exposed on `ConversationPolicyRecord` and settable
//! through the admin API, and **no code read it**. `add_message` bound `content_plain`
//! unconditionally, so an operator who selected `metadata_only` or `encrypted_content` — which
//! is what a deployment under a PII or data-residency obligation selects — got `plain_content`
//! behaviour with no error and no signal.
//!
//! Two comments in `src/application/conversation.rs` asserted the opposite, both describing a
//! conversation "persisting no plaintext" as a state the code handles. No configuration could
//! produce that state. The comments were doing the work of a guard, which is the shape this
//! project has now been bitten by more than once.
//!
//! # What issue #139 changed here
//!
//! `encrypted_content` now **seals** the body rather than withholding it, and the admin API's
//! 422 narrowed from "this value" to "this deployment cannot seal". The two cases here that
//! asserted the old refusal now assert the new acceptance — kept in this file rather than moved,
//! so a revert to a blanket 422 goes red in the suite whose subject it is. The sealed storage
//! form itself is covered by `tests/conversation_content_encryption.rs`.
//!
//! # Why the first case matters most
//!
//! `plain_content_stores_the_body_and_its_length` is the premise. Every other case here asserts
//! an **absence** — no plaintext in the column — and an absence assertion passes trivially
//! against a build that stores nothing at all, or against a fixture whose message was never
//! written. The `plain_content` case is what proves the writer works, so the withholding cases
//! are measuring a policy decision rather than a broken insert.
//!
//! # What is deliberately not asserted here
//!
//! That tightening the policy removes plaintext already stored. It does not: the policy governs
//! subsequent writes, and `a_tightened_policy_does_not_rewrite_history` pins that as intended
//! behaviour rather than leaving it to be discovered. Deleting stored content is retention's
//! job.

mod support;

use moira::{
    application::ConversationService,
    domain::{
        ConversationContentPersistence, ConversationCreateRequest,
        ConversationMessageCreateRequest, ConversationMessageRole, ConversationPolicyPutRequest,
    },
    security::Actor,
};
use serde_json::json;
use sqlx::Row;
use support::{LifecycleFixture, admin_actor, request_context};

/// High-entropy so a `content_plain = $1` equality is exact and a stray match is impossible.
const BODY: &str = "F32-CANARY-4d9a1c73e05b48f6-MESSAGE-BODY";

/// What a message row actually holds, read back from PostgreSQL rather than from the API.
struct StoredMessage {
    content_plain: Option<String>,
    /// Since issue #139 this is where the body lives under `encrypted_content`. Read here so the
    /// "the record agrees with the row" assertion below can compare against what was *stored*
    /// rather than against one of the two columns it might have been stored in.
    content_encrypted: Option<Vec<u8>>,
    content_hash: String,
    content_size_bytes: i64,
    token_count: Option<i64>,
    metadata: serde_json::Value,
}

struct Case {
    fixture: LifecycleFixture,
    actor: Actor,
    conversation_id: String,
}

impl Case {
    /// A fixture with one conversation, created **before** any policy is tightened.
    ///
    /// Order matters: `create_conversation` calls `get_or_create_conversation_policy`, so the
    /// policy row exists from that moment. A case that wants the *no row* path has to delete it
    /// (see `an_application_with_no_policy_row_stores_plaintext`).
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let actor = Actor {
            internal_application_id: Some(fixture.application_id),
            scopes: vec![
                "moira:admin".to_string(),
                "moira:conversations:create".to_string(),
                "moira:conversations:read".to_string(),
                "moira:conversations:write".to_string(),
                "moira:conversation-policies:read".to_string(),
                "moira:conversation-policies:write".to_string(),
            ],
            ..admin_actor()
        };
        let conversation = ConversationService::new(&fixture.state)
            .expect("conversation service")
            .create_conversation(
                &actor,
                &request_context(),
                ConversationCreateRequest {
                    title: Some("f32".to_string()),
                    metadata: json!({}),
                },
            )
            .await
            .expect("create conversation");
        Some(Self {
            fixture,
            actor,
            conversation_id: conversation.id,
        })
    }

    fn service(&self) -> ConversationService {
        ConversationService::new(&self.fixture.state).expect("conversation service")
    }

    /// Sets the policy through the **application service**, the way an operator does.
    async fn set_policy(
        &self,
        persistence: ConversationContentPersistence,
    ) -> Result<(), moira::error::AppError> {
        self.service()
            .put_conversation_policy(
                &self.actor,
                &request_context(),
                self.fixture.application_id,
                ConversationPolicyPutRequest {
                    conversation_content_persistence: Some(persistence),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .map(|_| ())
    }

    /// Sets the column **directly**, bypassing the admin API's refusal.
    ///
    /// This is how a deployment that set `encrypted_content` before the refusal existed is
    /// represented. Without it, the fail-closed behaviour for that value would be unreachable
    /// and its guard would be one of the toothless ones.
    async fn force_policy_column(&self, value: &str) {
        let updated = sqlx::query(
            "update application_conversation_policies \
             set conversation_content_persistence = $2 where application_id = $1",
        )
        .bind(self.fixture.application_id)
        .bind(value)
        .execute(&self.fixture.pool)
        .await
        .expect("force the policy column")
        .rows_affected();
        assert_eq!(
            updated, 1,
            "no policy row was updated, so this case would assert against the default rather \
             than against {value}"
        );
    }

    /// Writes a message through the public entry point and reads the row back.
    async fn write_and_read(&self, body: &str) -> StoredMessage {
        let record = self
            .service()
            .create_message(
                &self.actor,
                &request_context(),
                &self.conversation_id,
                ConversationMessageCreateRequest {
                    role: ConversationMessageRole::User,
                    content: body.to_string(),
                    metadata: json!({ "case": "f32" }),
                },
            )
            .await
            .expect("create message");
        let row = sqlx::query(
            "select content_plain, content_encrypted, content_hash, content_size_bytes, \
                    token_count, metadata \
             from conversation_messages where public_id = $1",
        )
        .bind(&record.id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("read the message row");
        let stored = StoredMessage {
            content_plain: row.try_get("content_plain").expect("content_plain"),
            content_encrypted: row.try_get("content_encrypted").expect("content_encrypted"),
            content_hash: row.try_get("content_hash").expect("content_hash"),
            content_size_bytes: row
                .try_get("content_size_bytes")
                .expect("content_size_bytes"),
            token_count: row.try_get("token_count").expect("token_count"),
            metadata: row.try_get("metadata").expect("metadata"),
        };
        // The record handed back to the caller must describe what was stored, not what was
        // offered. A record claiming content that the table does not hold is the same class of
        // dishonesty as the policy that was not read.
        //
        // Since #139 "what was stored" can be either column, so the comparison is against
        // *whether a body was stored at all* plus, when it was, the body itself. Comparing only
        // against `content_plain` would now fail every encrypted case for the wrong reason, and
        // dropping the assertion would lose the property.
        match (&stored.content_plain, &stored.content_encrypted) {
            (Some(plain), None) => assert_eq!(
                record.content.as_deref(),
                Some(plain.as_str()),
                "the returned record disagrees with the stored row about the message body"
            ),
            (None, Some(_)) => assert!(
                record.content.is_some(),
                "the row holds a sealed body but the record returned no content; a caller \
                 cannot tell that from a withheld body"
            ),
            (None, None) => assert_eq!(
                record.content, None,
                "the row holds no body at all but the record claims one"
            ),
            (Some(_), Some(_)) => panic!(
                "the row holds both a plaintext and a sealed body; migration 0027's CHECK \
                 constraint forbids that"
            ),
        }
        assert_eq!(record.content_size_bytes, stored.content_size_bytes);
        assert_eq!(record.token_count, stored.token_count);
        stored
    }
}

/// The premise. Without this passing, every absence assertion below is vacuous.
#[tokio::test]
async fn plain_content_stores_the_body_and_its_length() {
    let Some(case) = Case::new().await else {
        return;
    };
    // Set explicitly rather than relying on the default, so the case still means something if
    // the column default is ever changed.
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await
        .expect("plain_content is accepted");

    let stored = case.write_and_read(BODY).await;
    assert_eq!(
        stored.content_plain.as_deref(),
        Some(BODY),
        "plain_content must store the body verbatim"
    );
    assert_eq!(stored.content_size_bytes, BODY.len() as i64);
    assert!(
        stored.token_count.is_some_and(|count| count > 0),
        "plain_content must store a token estimate"
    );
}

#[tokio::test]
async fn metadata_only_withholds_the_body_and_keeps_its_length() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::MetadataOnly)
        .await
        .expect("metadata_only is accepted");

    let stored = case.write_and_read(BODY).await;
    assert_eq!(
        stored.content_plain, None,
        "metadata_only stored the message body in plaintext"
    );
    // The half that distinguishes it from `none`. Asserted positively so the two values cannot
    // silently collapse into the same behaviour.
    assert_eq!(
        stored.content_size_bytes,
        BODY.len() as i64,
        "metadata_only must keep the content's length — that is what makes it different from none"
    );
    assert!(
        stored.token_count.is_some_and(|count| count > 0),
        "metadata_only must keep the token estimate"
    );
    assert!(
        !stored.content_hash.is_empty(),
        "the peppered fingerprint is retained under every policy"
    );
    assert_eq!(
        stored.metadata["case"], "f32",
        "caller-supplied metadata is the caller's own JSON, not content derived from the body"
    );
}

#[tokio::test]
async fn none_withholds_the_body_and_its_length() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::None)
        .await
        .expect("none is accepted");

    let stored = case.write_and_read(BODY).await;
    assert_eq!(
        stored.content_plain, None,
        "none stored the message body in plaintext"
    );
    assert_eq!(
        stored.content_size_bytes, 0,
        "none must not retain the content's length in bytes"
    );
    assert_eq!(
        stored.token_count, None,
        "none must not retain the content's length in tokens"
    );
    assert!(
        !stored.content_hash.is_empty(),
        "the peppered fingerprint is retained: it is an HMAC under a deployment-held key, not \
         content, and the published content_hash format depends on it being present"
    );
}

/// `none` and `metadata_only` must not be two names for one behaviour.
///
/// Asserted as a *comparison* rather than as two independent facts, because that is the property:
/// a refactor that collapsed one into the other would leave both cases above green if each only
/// checked its own row in isolation.
#[tokio::test]
async fn none_and_metadata_only_differ_in_exactly_the_length_metadata() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::MetadataOnly)
        .await
        .expect("metadata_only is accepted");
    let metadata_only = case.write_and_read(BODY).await;

    case.set_policy(ConversationContentPersistence::None)
        .await
        .expect("none is accepted");
    let none = case.write_and_read(BODY).await;

    assert_eq!(metadata_only.content_plain, None);
    assert_eq!(none.content_plain, None);
    assert_ne!(
        metadata_only.content_size_bytes, none.content_size_bytes,
        "none and metadata_only stored identical rows, so one of the two enum values promises \
         something it does not deliver"
    );
    assert!(metadata_only.token_count.is_some() && none.token_count.is_none());
}

/// Issue #139 narrowed this refusal; it did not remove it.
///
/// `encrypted_content` is now honourable — a cipher is wired to the `content_encrypted`
/// columns — so the value is **accepted** on a deployment whose content keyring is loaded, which
/// every fixture here is. The refusal survives for the condition rather than the value:
/// encryption configured but unusable at write time.
///
/// **Why this is asserted here and not only in `tests/conversation_content_encryption.rs`.**
/// This suite is where the old, opposite assertion lived. Deleting it and testing only the new
/// behaviour elsewhere would leave nothing in this file pinning the direction of the change, and
/// a later revert to a blanket 422 would go green in exactly the file whose subject it is.
#[tokio::test]
async fn encrypted_content_is_accepted_now_that_a_cipher_is_wired() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await
        .expect("encrypted_content must be accepted once a cipher is wired");

    let persisted: String = sqlx::query_scalar(
        "select conversation_content_persistence from application_conversation_policies \
         where application_id = $1",
    )
    .bind(case.fixture.application_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("read the policy back");
    assert_eq!(
        persisted, "encrypted_content",
        "the accepted value did not reach the database"
    );

    // The other three values are storage policies and are untouched by the narrowing. Asserted
    // here so a change that widened the refusal back over any of them fails.
    for value in [
        ConversationContentPersistence::None,
        ConversationContentPersistence::MetadataOnly,
        ConversationContentPersistence::PlainContent,
    ] {
        case.set_policy(value)
            .await
            .unwrap_or_else(|error| panic!("{value:?} must still be accepted: {error:?}"));
    }
}

/// A deployment that set `encrypted_content` before it was implemented now gets the behaviour the
/// value always promised: no plaintext, and a sealed body.
///
/// The `content_plain is null` half is the same assertion this case always made. What is new is
/// the second half — before #139 that row held *nothing*, which is a strictly weaker outcome and
/// is what this assertion now forbids regressing to.
#[tokio::test]
async fn a_row_already_holding_encrypted_content_now_seals_the_body() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.force_policy_column("encrypted_content").await;

    // It still reads back through the API — the variant was not removed from the enum.
    let policy = case
        .service()
        .get_conversation_policy(&case.actor, case.fixture.application_id)
        .await
        .expect("an existing encrypted_content row still parses");
    assert_eq!(
        policy.conversation_content_persistence,
        ConversationContentPersistence::EncryptedContent
    );

    let stored = case.write_and_read(BODY).await;
    assert_eq!(
        stored.content_plain, None,
        "encrypted_content stored plaintext — the value promises encryption, and storing the \
         body in the clear under it is the exact misrepresentation F32 was about"
    );
    let sealed: Option<Vec<u8>> = sqlx::query_scalar(
        "select content_encrypted from conversation_messages m join conversations c \
         on c.id = m.conversation_id where c.public_id = $1 order by m.sequence_number desc \
         limit 1",
    )
    .bind(&case.conversation_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("read content_encrypted");
    let sealed = sealed.expect("encrypted_content must now write a sealed body, not nothing");
    assert!(
        !sealed
            .windows(BODY.len())
            .any(|window| window == BODY.as_bytes()),
        "the body sits verbatim inside content_encrypted; the cipher was not called"
    );
}

/// An application with no policy row at all gets the column default, not "no policy".
///
/// The enforcement reads the policy through a `left join`; a missing row yields `NULL`, and a
/// `NULL` that fell through to "withhold everything" would silently stop storing content for
/// every application that has never had a policy written.
#[tokio::test]
async fn an_application_with_no_policy_row_stores_plaintext() {
    let Some(case) = Case::new().await else {
        return;
    };
    let deleted =
        sqlx::query("delete from application_conversation_policies where application_id = $1")
            .bind(case.fixture.application_id)
            .execute(&case.fixture.pool)
            .await
            .expect("delete the policy row")
            .rows_affected();
    assert_eq!(
        deleted, 1,
        "no policy row existed to delete, so this case never exercised the missing-row path"
    );

    let stored = case.write_and_read(BODY).await;
    assert_eq!(
        stored.content_plain.as_deref(),
        Some(BODY),
        "an application with no policy row must get the 'plain_content' column default"
    );
}

/// Tightening the policy governs subsequent writes only. Stated as a test so the boundary is
/// pinned deliberately rather than discovered.
#[tokio::test]
async fn a_tightened_policy_does_not_rewrite_history() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await
        .expect("plain_content is accepted");
    let before = case.write_and_read(BODY).await;
    assert_eq!(
        before.content_plain.as_deref(),
        Some(BODY),
        "premise: the first message really was stored in plaintext"
    );

    case.set_policy(ConversationContentPersistence::None)
        .await
        .expect("none is accepted");
    let after = case.write_and_read(BODY).await;
    assert_eq!(after.content_plain, None, "the new write must be withheld");

    let still_there: i64 =
        sqlx::query_scalar("select count(*) from conversation_messages where content_plain = $1")
            .bind(BODY)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count surviving plaintext");
    assert_eq!(
        still_there, 1,
        "the policy governs writes, not history: content stored under plain_content stays \
         stored. Removing it is retention's job, and if that ever changes this assertion is \
         the one to update deliberately"
    );
}
