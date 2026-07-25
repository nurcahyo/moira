//! End-to-end coverage for plan 02b (`plans/02b-idempotency-replay.md` §9b).
//!
//! Every assertion drives the real HTTP surface (`moira::build_router`) against a real
//! PostgreSQL/pgvector database, and every "did it mutate?" claim is settled with direct SQL
//! against the same pool the router writes through. An HTTP read cannot substitute: the defect
//! this file guards (P0-2) is a replayed *response* that nonetheless performed a second
//! *mutation*, and only the row counts can tell those apart.
//!
//! Fail-closed behaviour is inherited verbatim from `tests/support/mod.rs`: when
//! `MOIRA_TEST_DATABASE_URL` is absent and `CI` is `true` the shared fixture panics
//! (`env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true"))`), and otherwise it
//! prints a reason and skips (`plans/CONVENTIONS.md` §3). It is reused, never re-implemented, so
//! this file cannot diverge from the harness.
//!
//! There is **no `sleep()` anywhere in this file**. The one concurrency test uses a
//! `tokio::sync::Barrier` acknowledgement gate, mirroring `tests/admin_idempotency.rs`
//! (`plans/CONVENTIONS.md` §3; finding P2-12).

mod support;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{
    application::{ConversationService, RequestContext},
    domain::RagDocumentIngestRequest,
    security::{Actor, ActorType, request_hash},
};
use serde_json::{Value, json};
use tokio::{sync::Barrier, task::JoinHandle, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;

use support::LifecycleFixture;

const WAIT: Duration = Duration::from_secs(10);
const DOCUMENT_BODY: &str = "Moira replays this ingest verbatim rather than writing a new version.";
const REVISED_BODY: &str = "A different body under the same key must be rejected, not applied.";

const INGEST_OPERATION: &str = "rag.document.ingest";
const COLLECTION_CREATE_OPERATION: &str = "rag.collection.create";
const DOCUMENT_CREATE_OPERATION: &str = "rag.document.create";

struct HttpResult {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
}

impl HttpResult {
    /// Reads a field and proves it was actually serialised, so a *missing* key can never be
    /// mistaken for an explicit JSON `null` (`Value::Index` returns `Null` for both).
    fn field(&self, name: &str) -> &Value {
        let object = self
            .body
            .as_object()
            .unwrap_or_else(|| panic!("expected a JSON object response, got: {}", self.body));
        object
            .get(name)
            .unwrap_or_else(|| panic!("response is missing the `{name}` field: {}", self.body))
    }

    fn public_id(&self) -> String {
        self.field("id")
            .as_str()
            .unwrap_or_else(|| panic!("resource id is not a string: {}", self.body))
            .to_string()
    }

    fn current_version_id(&self) -> Uuid {
        let raw = self
            .field("current_version_id")
            .as_str()
            .unwrap_or_else(|| panic!("current_version_id is not set: {}", self.body));
        Uuid::parse_str(raw).expect("current_version_id is a UUID")
    }
}

struct RagFixture {
    fixture: LifecycleFixture,
    router: Router,
    suffix: String,
}

impl RagFixture {
    /// Returns `None` when the shared fixture decided to skip. The fail-closed rule lives in
    /// `tests/support/mod.rs` and is reused here rather than re-implemented.
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let router = moira::build_router(fixture.state.clone()).expect("build Moira test router");
        let suffix = Uuid::now_v7().simple().to_string();
        Some(Self {
            fixture,
            router,
            suffix,
        })
    }

    fn key(&self, label: &str) -> String {
        format!("rag-idem-{label}-{}", self.suffix)
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> HttpResult {
        send(
            self.router.clone(),
            method,
            path,
            key,
            &format!("rag-replay-{}", Uuid::now_v7()),
            body,
        )
        .await
    }

    async fn create_collection(&self, label: &str, key: Option<&str>) -> HttpResult {
        self.request(
            "POST",
            "/api/v1/admin/rag-collections",
            key,
            Some(self.collection_body(label)),
        )
        .await
    }

    fn collection_body(&self, label: &str) -> Value {
        json!({
            "application_id": self.fixture.application_id,
            "external_tenant_id": null,
            "collection_key": format!("{label}-{}", self.suffix),
            "display_name": format!("{label} {}", self.suffix),
            "description": null,
            "visibility": "application",
            "metadata": {"suite": "rag_idempotency_replay"}
        })
    }

    /// Creates a collection with no idempotency key and returns its public id.
    async fn collection(&self, label: &str) -> String {
        let created = self.create_collection(label, None).await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "create collection failed: {}",
            created.body
        );
        created.public_id()
    }

    fn document_body(&self, label: &str, content: Option<&str>) -> Value {
        json!({
            "external_document_id": format!("{label}-{}", self.suffix),
            "title": format!("{label} {}", self.suffix),
            "source_type": "direct_text",
            "source_uri": null,
            "mime_type": "text/plain",
            "content": content,
            "metadata": {"suite": "rag_idempotency_replay"}
        })
    }

    async fn create_document(
        &self,
        collection_id: &str,
        label: &str,
        content: Option<&str>,
        key: Option<&str>,
    ) -> HttpResult {
        self.request(
            "POST",
            &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
            key,
            Some(self.document_body(label, content)),
        )
        .await
    }

    /// Creates a content-less document with no idempotency key and returns its public id.
    async fn document(&self, collection_id: &str, label: &str) -> String {
        let created = self.create_document(collection_id, label, None, None).await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "create document failed: {}",
            created.body
        );
        created.public_id()
    }

    async fn ingest(&self, document_id: &str, content: &str, key: Option<&str>) -> HttpResult {
        self.request(
            "POST",
            &format!("/api/v1/admin/rag-documents/{document_id}/ingest"),
            key,
            Some(json!({ "content": content })),
        )
        .await
    }

    async fn reindex(&self, document_id: &str, content: &str, key: Option<&str>) -> HttpResult {
        self.request(
            "POST",
            &format!("/api/v1/admin/rag-documents/{document_id}/reindex"),
            key,
            Some(json!({ "content": content })),
        )
        .await
    }

    // --- direct SQL, deliberately not routed through HTTP -------------------------------------

    async fn document_uuid(&self, public_id: &str) -> Uuid {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, Uuid>("select id from rag_documents where public_id = $1")
                .bind(public_id)
                .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("document lookup timed out")
        .expect("document row exists")
    }

    async fn version_count(&self, document_public_id: &str) -> i64 {
        let document_uuid = self.document_uuid(document_public_id).await;
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from rag_document_versions where document_id = $1",
            )
            .bind(document_uuid)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("version count timed out")
        .expect("version count")
    }

    async fn collection_count(&self, label: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from rag_collections where collection_key = $1",
            )
            .bind(format!("{label}-{}", self.suffix))
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("collection count timed out")
        .expect("collection count")
    }

    async fn document_count(&self, collection_public_id: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from rag_documents document
                 join rag_collections collection on collection.id = document.collection_id
                 where collection.public_id = $1",
            )
            .bind(collection_public_id)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("document count timed out")
        .expect("document count")
    }

    async fn audit_count(&self, action: &str, resource_id: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from audit_logs
                 where action = $1 and resource_id = $2 and result = 'success'",
            )
            .bind(action)
            .bind(resource_id)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("audit count timed out")
        .expect("audit count")
    }

    async fn audit_count_for_request(&self, request_id: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>("select count(*) from audit_logs where request_id = $1")
                .bind(request_id)
                .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("audit-by-request count timed out")
        .expect("audit-by-request count")
    }

    /// Every ledger row for `(operation, key)` regardless of whether it was finalized.
    async fn ledger_count(&self, operation: &str, key: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_records
                 where operation = $1 and idempotency_key_hash = $2",
            )
            .bind(operation)
            .bind(request_hash(key.as_bytes()))
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("ledger count timed out")
        .expect("ledger count")
    }

    async fn ledger_count_for_resource(&self, operation: &str, resource_id: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_records
                 where operation = $1 and resource_id = $2",
            )
            .bind(operation)
            .bind(resource_id)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("ledger-by-resource count timed out")
        .expect("ledger-by-resource count")
    }

    async fn execute(&self, statement: &str, key: &str, operation: &str) {
        timeout(
            WAIT,
            sqlx::query(statement)
                .bind(request_hash(key.as_bytes()))
                .bind(operation)
                .execute(&self.fixture.pool),
        )
        .await
        .expect("ledger statement timed out")
        .expect("ledger statement");
    }
}

async fn send(
    router: Router,
    method: &str,
    path: &str,
    key: Option<&str>,
    request_id: &str,
    body: Option<Value>,
) -> HttpResult {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-request-id", request_id);
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let payload = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let response = timeout(
        WAIT,
        router.oneshot(builder.body(payload).expect("HTTP request")),
    )
    .await
    .expect("HTTP request timed out")
    .expect("HTTP response");
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON response")
    };
    HttpResult { status, body, etag }
}

/// The acknowledgement gate. Each task parks on the shared barrier and only issues its request
/// once every participant (both tasks plus the test body) has arrived, so the two requests race
/// deterministically without any timing assumption. `plans/CONVENTIONS.md` §3 rejects `sleep()`.
fn spawn_ingest(
    router: Router,
    barrier: Arc<Barrier>,
    document_id: String,
    key: String,
    content: String,
) -> JoinHandle<HttpResult> {
    tokio::spawn(async move {
        barrier.wait().await;
        send(
            router,
            "POST",
            &format!("/api/v1/admin/rag-documents/{document_id}/ingest"),
            Some(&key),
            &format!("rag-replay-{}", Uuid::now_v7()),
            Some(json!({ "content": content })),
        )
        .await
    })
}

/// Asserts a coded error envelope carries everything `plans/CONVENTIONS.md` §4 requires: the code,
/// the mechanically derived `message_key`, a non-empty default English `message`, and — the half
/// 02a could not assert — that the key actually resolves in the compiled Rust catalog.
fn assert_catalogued_error(result: &HttpResult, status: StatusCode, code: &str) {
    assert_eq!(result.status, status, "unexpected body: {}", result.body);
    let error = result.body["error"]
        .as_object()
        .unwrap_or_else(|| panic!("missing ErrorDetail envelope: {}", result.body));
    assert_eq!(
        error.get("code"),
        Some(&json!(code)),
        "unexpected error code: {}",
        result.body
    );
    let expected_key = format!("moira.error.{code}");
    let message_key = error
        .get("message_key")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("message_key must be a string: {}", result.body));
    assert_eq!(
        message_key, expected_key,
        "message_key must be derived as moira.error.<code>: {}",
        result.body
    );
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("message must be a string: {}", result.body));
    assert!(
        !message.trim().is_empty(),
        "message must be a non-empty default English string: {}",
        result.body
    );
    assert!(
        moira::i18n::is_known_key(message_key),
        "{message_key} is not present in the compiled i18n catalog"
    );
}

/// A replayed failure must be the *same* envelope, not a re-executed one: identical status, code,
/// message_key, message and details, with a fresh `request_id` proving it is a new HTTP exchange.
fn assert_replayed_error(first: &HttpResult, replay: &HttpResult) {
    assert_eq!(
        first.status, replay.status,
        "replayed failure changed status: {} vs {}",
        first.body, replay.body
    );
    let mut first_error = first.body["error"].clone();
    let mut replay_error = replay.body["error"].clone();
    let first_request_id = first_error
        .as_object_mut()
        .and_then(|error| error.remove("request_id"))
        .expect("first error request ID");
    let replay_request_id = replay_error
        .as_object_mut()
        .and_then(|error| error.remove("request_id"))
        .expect("replayed error request ID");
    assert_ne!(
        first_request_id, replay_request_id,
        "a replay is a new request and must carry a fresh request_id"
    );
    assert_eq!(
        first_error, replay_error,
        "the replayed failure envelope must be identical apart from request_id"
    );
}

fn context(key: &str) -> RequestContext {
    RequestContext {
        request_id: format!("rag-replay-{}", Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("rag-idempotency-replay-test".to_string()),
        idempotency_key: Some(key.to_string()),
    }
}

fn admin_actor(subject: &str) -> Actor {
    Actor {
        actor_type: ActorType::DevAdmin,
        subject: Some(subject.to_string()),
        scopes: vec!["moira:admin".to_string()],
        ..Actor::default()
    }
}

/// Test 1 — `POST /api/v1/admin/rag-collections` replays verbatim and mutates once.
#[tokio::test]
async fn repeated_rag_collection_create_with_the_same_key_replays_one_collection() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let key = fixture.key("collection-create");
    let first = fixture
        .create_collection("replay-collection", Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::CREATED,
        "first create failed: {}",
        first.body
    );
    let replay = fixture
        .create_collection("replay-collection", Some(&key))
        .await;

    assert_eq!(replay.status, first.status, "replayed status must match");
    assert_eq!(
        replay.body, first.body,
        "the replayed body must be the stored serialisation of the original response"
    );
    assert_eq!(replay.etag, first.etag, "the replayed ETag must match");
    assert!(
        first.etag.is_some(),
        "the create response must carry an ETag"
    );

    assert_eq!(
        fixture.collection_count("replay-collection").await,
        1,
        "a replayed create must not insert a second collection"
    );
    assert_eq!(
        fixture
            .audit_count("rag.collection.created", &first.public_id())
            .await,
        1,
        "a replayed create must not write a second audit row"
    );
    assert_eq!(
        fixture
            .ledger_count(COLLECTION_CREATE_OPERATION, &key)
            .await,
        1
    );
}

/// Test 2 — `POST .../{collection_id}/documents` replays verbatim, including the inline-content
/// version-1 write, which must happen exactly once.
#[tokio::test]
async fn repeated_rag_document_create_with_the_same_key_replays_one_document() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("document-create-collection").await;
    let key = fixture.key("document-create");

    let first = fixture
        .create_document(
            &collection_id,
            "replay-document",
            Some(DOCUMENT_BODY),
            Some(&key),
        )
        .await;
    assert_eq!(
        first.status,
        StatusCode::CREATED,
        "first create failed: {}",
        first.body
    );
    let replay = fixture
        .create_document(
            &collection_id,
            "replay-document",
            Some(DOCUMENT_BODY),
            Some(&key),
        )
        .await;

    assert_eq!(replay.status, first.status);
    assert_eq!(
        replay.body, first.body,
        "the replayed document body must be byte-identical"
    );
    assert_eq!(replay.etag, first.etag, "the replayed ETag must match");

    let document_id = first.public_id();
    assert_eq!(
        fixture.document_count(&collection_id).await,
        1,
        "a replayed create must not insert a second document"
    );
    assert_eq!(
        fixture.version_count(&document_id).await,
        1,
        "an inline-content create replayed under one key must write exactly one version"
    );
    assert_eq!(
        fixture
            .audit_count("rag.document.created", &document_id)
            .await,
        1
    );
    assert_eq!(
        fixture.ledger_count(DOCUMENT_CREATE_OPERATION, &key).await,
        1
    );
}

/// Test 3 — the inversion of 02a's characterization test. The same key twice now replays and
/// leaves exactly one version row, and 02a's honesty contract (`pending`, never `indexed`) is not
/// regressed by the move into the runner's transaction.
#[tokio::test]
async fn repeated_ingest_with_the_same_key_replays_and_creates_exactly_one_version() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("ingest-replay-collection").await;
    let document_id = fixture.document(&collection_id, "ingest-replay").await;
    let key = fixture.key("ingest-replay");

    let first = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "first ingest failed: {}",
        first.body
    );
    let replay = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;

    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(
        replay.body.to_string(),
        first.body.to_string(),
        "the replayed ingest body must be byte-identical"
    );
    assert_eq!(replay.etag, first.etag, "the replayed ETag must match");
    assert_eq!(
        replay.current_version_id(),
        first.current_version_id(),
        "a replay must not point at a newly created version"
    );
    assert_eq!(
        first.field("ingestion_status"),
        &json!("pending"),
        "02a's honesty contract must not regress: {}",
        first.body
    );

    assert_eq!(
        fixture.version_count(&document_id).await,
        1,
        "the same key twice must write exactly one rag_document_versions row"
    );
    assert_eq!(
        fixture
            .audit_count("rag.document.ingested", &document_id)
            .await,
        1
    );
    assert_eq!(fixture.ledger_count(INGEST_OPERATION, &key).await, 1);
}

/// Test 4 — pins the deliberate `/ingest` ↔ `/reindex` shared operation identity at the wire level.
#[tokio::test]
async fn reindex_replays_an_ingest_performed_under_the_same_key() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("reindex-alias-collection").await;
    let document_id = fixture.document(&collection_id, "reindex-alias").await;
    let key = fixture.key("reindex-alias");

    let ingested = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        ingested.status,
        StatusCode::OK,
        "ingest failed: {}",
        ingested.body
    );
    let reindexed = fixture
        .reindex(&document_id, DOCUMENT_BODY, Some(&key))
        .await;

    assert_eq!(
        reindexed.status,
        StatusCode::OK,
        "reindex under the shared key must replay, not fail: {}",
        reindexed.body
    );
    assert_eq!(
        reindexed.body, ingested.body,
        "/reindex shares rag.document.ingest with /ingest, so the same key and body replays"
    );
    assert_eq!(reindexed.etag, ingested.etag);
    assert_eq!(
        fixture.version_count(&document_id).await,
        1,
        "the aliased route must not create version 2 under a replayed key"
    );
    assert_eq!(fixture.ledger_count(INGEST_OPERATION, &key).await, 1);
}

/// Test 5 — same key, different body ⇒ `409 idempotency_conflict`, and nothing is written.
#[tokio::test]
async fn same_key_with_a_different_body_returns_idempotency_conflict() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("conflict-collection").await;
    let document_id = fixture.document(&collection_id, "conflict").await;
    let key = fixture.key("body-conflict");

    let first = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "first ingest failed: {}",
        first.body
    );
    let conflicting = fixture.ingest(&document_id, REVISED_BODY, Some(&key)).await;

    assert_catalogued_error(&conflicting, StatusCode::CONFLICT, "idempotency_conflict");
    assert_eq!(
        fixture.version_count(&document_id).await,
        1,
        "a conflicting request must not create a second version"
    );
}

/// Test 6 — the `path` envelope is inside the request hash, so the same key on a different
/// document conflicts rather than replaying the wrong document's response.
#[tokio::test]
async fn same_key_on_a_different_document_returns_idempotency_conflict() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("path-conflict-collection").await;
    let first_document = fixture.document(&collection_id, "path-conflict-a").await;
    let second_document = fixture.document(&collection_id, "path-conflict-b").await;
    let key = fixture.key("path-conflict");

    let first = fixture
        .ingest(&first_document, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "first ingest failed: {}",
        first.body
    );
    let other_document = fixture
        .ingest(&second_document, DOCUMENT_BODY, Some(&key))
        .await;

    assert_catalogued_error(
        &other_document,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    );
    assert_eq!(
        fixture.version_count(&second_document).await,
        0,
        "the second document must not have been ingested"
    );
}

/// Test 7 — actor-fingerprint isolation, mirroring
/// `tests/admin_idempotency.rs::trusted_actor_fingerprint_isolates_issuer_and_application_identity`.
///
/// Driven through `ConversationService` rather than HTTP for the same reason the exemplar does:
/// the dev-admin HTTP path yields one fixed actor, so the only way to vary *just* the fingerprint
/// — holding key, operation, path and request body constant — is at the service seam.
#[tokio::test]
async fn different_actors_with_the_same_key_do_not_replay_each_others_responses() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("actor-isolation-collection").await;
    let document_id = fixture.document(&collection_id, "actor-isolation").await;
    let key = fixture.key("actor-isolation");
    let service = ConversationService::new(&fixture.fixture.state).expect("conversation service");
    let request = RagDocumentIngestRequest {
        content: Some(DOCUMENT_BODY.to_string()),
        source_etag: None,
        source_last_modified: None,
        metadata: json!({"suite": "rag_idempotency_replay"}),
    };

    let first = service
        .ingest_rag_document(
            &admin_actor("isolation-actor-one"),
            &context(&key),
            &document_id,
            request.clone(),
        )
        .await
        .expect("first actor ingest");
    let second = service
        .ingest_rag_document(
            &admin_actor("isolation-actor-two"),
            &context(&key),
            &document_id,
            request,
        )
        .await
        .expect("second actor ingest must execute independently, not replay");

    assert_ne!(
        first.current_version_id, second.current_version_id,
        "one actor must never receive another actor's replayed response"
    );
    assert_eq!(
        fixture.version_count(&document_id).await,
        2,
        "two isolated actors must each perform their own mutation"
    );
    assert_eq!(
        fixture.ledger_count(INGEST_OPERATION, &key).await,
        2,
        "actor_fingerprint is part of the ledger's unique index, so each actor owns a row"
    );
}

/// Test 8 — the concurrency gate. Two duplicate ingests are released simultaneously by a
/// `tokio::sync::Barrier`; the winner mutates, the loser either replays the identical body or is
/// told `409 idempotency_in_progress`. Exactly one version, one audit row and one ledger row.
#[tokio::test]
async fn concurrent_same_key_ingests_produce_one_version_and_one_audit_row() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("concurrent-collection").await;
    let document_id = fixture.document(&collection_id, "concurrent").await;
    let key = fixture.key("concurrent-ingest");

    // Three participants: both request tasks and the test body itself. Nothing is released until
    // every participant has acknowledged arrival — no sleep, no timing assumption.
    let barrier = Arc::new(Barrier::new(3));
    let first = spawn_ingest(
        fixture.router.clone(),
        barrier.clone(),
        document_id.clone(),
        key.clone(),
        DOCUMENT_BODY.to_string(),
    );
    let second = spawn_ingest(
        fixture.router.clone(),
        barrier.clone(),
        document_id.clone(),
        key.clone(),
        DOCUMENT_BODY.to_string(),
    );
    barrier.wait().await;
    let first = first.await.expect("first concurrent ingest task");
    let second = second.await.expect("second concurrent ingest task");

    let succeeded =
        usize::from(first.status == StatusCode::OK) + usize::from(second.status == StatusCode::OK);
    assert!(
        succeeded >= 1,
        "at least one concurrent ingest must succeed: {} / {}",
        first.body,
        second.body
    );
    for result in [&first, &second] {
        match result.status {
            StatusCode::OK => {}
            StatusCode::CONFLICT => {
                assert_catalogued_error(result, StatusCode::CONFLICT, "idempotency_in_progress");
            }
            other => panic!("unexpected concurrent status {other}: {}", result.body),
        }
    }
    if succeeded == 2 {
        assert_eq!(
            first.body, second.body,
            "when the loser waits for the winner it must receive the identical replayed body"
        );
        assert_eq!(first.etag, second.etag);
    }

    assert_eq!(
        fixture.version_count(&document_id).await,
        1,
        "a concurrent duplicate must not produce a second version"
    );
    assert_eq!(
        fixture
            .audit_count("rag.document.ingested", &document_id)
            .await,
        1,
        "a concurrent duplicate must not produce a second audit row"
    );
    assert_eq!(
        fixture.ledger_count(INGEST_OPERATION, &key).await,
        1,
        "the ledger's unique index must admit exactly one row for the key"
    );
}

/// Test 9 — the unfinalized-record branch, forced deterministically rather than by racing.
///
/// A completed ledger row is stripped of its `response_status`/`response_body` with direct SQL,
/// which is exactly the state a still-executing request leaves behind. Re-sending the same key
/// then takes `PgAdminCommandTransaction::claim_idempotency`'s in-progress arm. This is the test
/// that closes 02b's slice of P0-5: it proves the newly catalogued key is reachable on a live
/// response.
#[tokio::test]
async fn in_progress_claim_returns_a_catalogued_idempotency_in_progress_error() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("in-progress-collection").await;
    let document_id = fixture.document(&collection_id, "in-progress").await;
    let key = fixture.key("in-progress");

    let first = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "seed ingest failed: {}",
        first.body
    );
    fixture
        .execute(
            "update idempotency_records
             set response_status = null, response_body = null, resource_id = null
             where idempotency_key_hash = $1 and operation = $2",
            &key,
            INGEST_OPERATION,
        )
        .await;

    let in_progress = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;

    assert_catalogued_error(
        &in_progress,
        StatusCode::CONFLICT,
        "idempotency_in_progress",
    );
    assert_eq!(
        fixture.version_count(&document_id).await,
        1,
        "an in-progress rejection must not mutate"
    );
}

/// Test 10 — a deterministic failure is cached and replayed as the same failure envelope.
#[tokio::test]
async fn failed_ingest_replays_the_same_deterministic_failure() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let missing_document = format!("doc_{}", Uuid::now_v7());
    let key = fixture.key("failure-replay");

    let first = fixture
        .ingest(&missing_document, DOCUMENT_BODY, Some(&key))
        .await;
    let replay = fixture
        .ingest(&missing_document, DOCUMENT_BODY, Some(&key))
        .await;

    assert_catalogued_error(&first, StatusCode::NOT_FOUND, "rag_document_not_found");
    assert_catalogued_error(&replay, StatusCode::NOT_FOUND, "rag_document_not_found");
    assert_replayed_error(&first, &replay);
    assert_eq!(
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_records
                 where operation = $1 and idempotency_key_hash = $2
                   and response_status = 404 and resource_id is null",
            )
            .bind(INGEST_OPERATION)
            .bind(request_hash(key.as_bytes()))
            .fetch_one(&fixture.fixture.pool),
        )
        .await
        .expect("cached failure query timed out")
        .expect("cached failure query"),
        1,
        "the deterministic failure must be cached exactly once"
    );
}

/// Test 11 — atomicity of the audit write that moved inside the runner's transaction. A scripted
/// audit-insert failure must leave zero audit rows *and* zero version rows *and* zero ledger rows.
/// Before §6 moved the audit inside the transaction, the audit row could outlive a failed mutation.
#[tokio::test]
async fn rolled_back_ingest_leaves_no_audit_row_and_no_partial_version() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("rollback-collection").await;
    let document_id = fixture.document(&collection_id, "rollback").await;
    let key = fixture.key("rollback");
    let marker = format!("rollback_{}", fixture.suffix);
    let function_name = format!("fail_rag_audit_{}", fixture.suffix);
    let trigger_name = format!("fail_rag_audit_trigger_{}", fixture.suffix);
    let ddl = format!(
        "create function {function_name}() returns trigger language plpgsql as $$ begin if new.request_id = '{marker}' then raise exception 'scripted audit failure'; end if; return new; end $$; create trigger {trigger_name} before insert on audit_logs for each row execute function {function_name}()"
    );
    sqlx::raw_sql(&ddl)
        .execute(&fixture.fixture.pool)
        .await
        .expect("install audit failure trigger");

    let failed = send(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/rag-documents/{document_id}/ingest"),
        Some(&key),
        &marker,
        Some(json!({ "content": DOCUMENT_BODY })),
    )
    .await;

    let cleanup =
        format!("drop trigger {trigger_name} on audit_logs; drop function {function_name}()");
    sqlx::raw_sql(&cleanup)
        .execute(&fixture.fixture.pool)
        .await
        .expect("remove audit failure trigger");

    assert_eq!(
        failed.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a failing audit insert must fail the whole command: {}",
        failed.body
    );
    assert_eq!(
        fixture.audit_count_for_request(&marker).await,
        0,
        "no audit row may survive the rolled-back mutation"
    );
    assert_eq!(
        fixture.version_count(&document_id).await,
        0,
        "no partial version row may survive the rolled-back mutation"
    );
    assert_eq!(
        fixture.ledger_count(INGEST_OPERATION, &key).await,
        0,
        "an infrastructure failure is not cached, so the key stays reusable"
    );
}

/// Test 12 — a ledger record past `expires_at` is purged on the next claim and the request
/// legitimately re-executes, mirroring `tests/admin_idempotency.rs`'s expired-record coverage.
#[tokio::test]
async fn expired_ledger_record_is_purged_and_the_request_re_executes() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("expiry-collection").await;
    let document_id = fixture.document(&collection_id, "expiry").await;
    let key = fixture.key("expiry");

    let first = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "first ingest failed: {}",
        first.body
    );
    assert_eq!(fixture.version_count(&document_id).await, 1);
    fixture
        .execute(
            "update idempotency_records set expires_at = now() - interval '1 second'
             where idempotency_key_hash = $1 and operation = $2",
            &key,
            INGEST_OPERATION,
        )
        .await;

    let reused = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;

    assert_eq!(
        reused.status,
        StatusCode::OK,
        "an expired key must be reusable: {}",
        reused.body
    );
    assert_ne!(
        reused.current_version_id(),
        first.current_version_id(),
        "the expired record must have been purged and the request re-executed"
    );
    assert_eq!(
        fixture.version_count(&document_id).await,
        2,
        "re-execution after expiry legitimately creates version 2"
    );
    assert_eq!(
        fixture.ledger_count(INGEST_OPERATION, &key).await,
        1,
        "the purge-then-claim path leaves exactly one live record"
    );
}

/// Test 13 — the no-key regression guard. Requests without `Idempotency-Key` behave exactly as
/// before: every call mutates, and nothing is claimed in the ledger.
#[tokio::test]
async fn requests_without_an_idempotency_key_still_create_new_versions() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.collection("keyless-collection").await;
    let document_id = fixture.document(&collection_id, "keyless").await;

    let first = fixture.ingest(&document_id, DOCUMENT_BODY, None).await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "first keyless ingest failed: {}",
        first.body
    );
    let second = fixture.ingest(&document_id, DOCUMENT_BODY, None).await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "second keyless ingest failed: {}",
        second.body
    );

    assert_ne!(
        second.current_version_id(),
        first.current_version_id(),
        "a keyless retry must still create a new version"
    );
    assert_eq!(
        fixture.version_count(&document_id).await,
        2,
        "two keyless ingests must write two versions"
    );
    assert_eq!(
        fixture
            .ledger_count_for_resource(INGEST_OPERATION, &document_id)
            .await,
        0,
        "a keyless request must claim nothing in the idempotency ledger"
    );
    assert_eq!(
        fixture
            .ledger_count_for_resource(DOCUMENT_CREATE_OPERATION, &document_id)
            .await,
        0,
        "the keyless create that seeded this document must not have claimed a ledger row either"
    );
}
