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

use axum::http::StatusCode;
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
            "select content_plain, content_hash, content_size_bytes, token_count, metadata \
             from conversation_messages where public_id = $1",
        )
        .bind(&record.id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("read the message row");
        let stored = StoredMessage {
            content_plain: row.try_get("content_plain").expect("content_plain"),
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
        assert_eq!(
            record.content, stored.content_plain,
            "the returned record disagrees with the stored row about the message body"
        );
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

/// The API must refuse a value it cannot honour rather than storing it and behaving as
/// something else.
#[tokio::test]
async fn encrypted_content_is_refused_because_no_cipher_exists() {
    let Some(case) = Case::new().await else {
        return;
    };
    let error = case
        .set_policy(ConversationContentPersistence::EncryptedContent)
        .await
        .expect_err("encrypted_content must be refused while nothing writes content_encrypted");
    let response = error.error_response(None);
    assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response.error.code, "conversation_content_persistence_unsupported",
        "the refusal must be coded, not a bare 422"
    );

    // And the refusal must not have written anything: a stored value plus an error is worse
    // than either alone.
    let persisted: String = sqlx::query_scalar(
        "select conversation_content_persistence from application_conversation_policies \
         where application_id = $1",
    )
    .bind(case.fixture.application_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("read the policy back");
    assert_ne!(
        persisted, "encrypted_content",
        "the refused value reached the database anyway"
    );
}

/// A deployment that set `encrypted_content` before the refusal existed must be made safer,
/// not broken: the row keeps parsing, and it stores no plaintext.
#[tokio::test]
async fn a_row_already_holding_encrypted_content_fails_closed() {
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
