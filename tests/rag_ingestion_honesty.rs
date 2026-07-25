//! End-to-end coverage for plan 02a (`plans/02a-mvp-boundary-honesty.md` §12b).
//!
//! Every assertion here goes through the real HTTP surface (`moira::build_router`) against a real
//! PostgreSQL + pgvector database, and the database-facing assertions issue SQL directly against
//! the same pool the router writes through — an HTTP read is not a substitute, because the defect
//! this file guards (P0-1) is precisely an API that reports one thing while the audit trail in
//! `rag_document_versions` records another.
//!
//! Fail-closed behaviour is inherited verbatim from `tests/support/mod.rs`: when
//! `MOIRA_TEST_DATABASE_URL` is absent and `CI` is `true` the shared fixture panics, and otherwise
//! it prints a reason and skips (`plans/CONVENTIONS.md` §3).
//!
//! No `sleep()` and no concurrency test: 02a introduces no concurrent code path, so it warrants
//! neither (`plans/CONVENTIONS.md` §3; finding P2-12).

mod support;

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

use support::LifecycleFixture;

const WAIT: Duration = Duration::from_secs(10);
const DOCUMENT_BODY: &str = "Moira stores this document verbatim. Nothing is chunked or embedded.";
const REVISED_BODY: &str = "Revised body for the superseding version of this RAG document.";

struct HttpResult {
    status: StatusCode,
    body: Value,
}

impl HttpResult {
    fn ingestion_status(&self) -> &Value {
        self.field("ingestion_status")
    }

    /// Reads a field and proves it was actually serialised, so that a *missing* key can never be
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

    fn document_id(&self) -> String {
        self.field("id")
            .as_str()
            .unwrap_or_else(|| panic!("document id is not a string: {}", self.body))
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

/// A `rag_document_versions` row, read straight out of PostgreSQL.
#[derive(Debug)]
struct VersionRow {
    id: Uuid,
    version_number: i32,
    ingestion_status: String,
    superseded: bool,
}

struct RagFixture {
    fixture: LifecycleFixture,
    router: Router,
    suffix: String,
}

impl RagFixture {
    /// Returns `None` when the shared fixture decided to skip. The fail-closed rule
    /// (`panic!` when `CI=true` and `MOIRA_TEST_DATABASE_URL` is absent, using
    /// `env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true"))`) lives in
    /// `tests/support/mod.rs` and is reused here rather than re-implemented, so this file
    /// cannot diverge from it.
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

    async fn request(
        &self,
        method: &str,
        path: &str,
        idempotency_key: Option<&str>,
        body: Option<Value>,
    ) -> HttpResult {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("x-request-id", format!("rag-honesty-{}", Uuid::now_v7()));
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        let payload = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
        let response = timeout(
            WAIT,
            self.router
                .clone()
                .oneshot(builder.body(payload).expect("HTTP request")),
        )
        .await
        .expect("HTTP request timed out")
        .expect("HTTP response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("JSON response")
        };
        HttpResult { status, body }
    }

    async fn create_collection(&self, label: &str) -> String {
        let result = self
            .request(
                "POST",
                "/api/v1/admin/rag-collections",
                None,
                Some(json!({
                    "application_id": self.fixture.application_id,
                    "external_tenant_id": null,
                    "collection_key": format!("{label}-{}", self.suffix),
                    "display_name": format!("{label} {}", self.suffix),
                    "description": null,
                    "visibility": "application",
                    "metadata": {"suite": "rag_ingestion_honesty"}
                })),
            )
            .await;
        assert_eq!(
            result.status,
            StatusCode::CREATED,
            "create collection failed: {}",
            result.body
        );
        result.document_id()
    }

    async fn create_document(
        &self,
        collection_id: &str,
        label: &str,
        content: Option<&str>,
    ) -> HttpResult {
        self.request(
            "POST",
            &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
            None,
            Some(json!({
                "external_document_id": format!("{label}-{}", self.suffix),
                "title": format!("{label} {}", self.suffix),
                "source_type": "direct_text",
                "source_uri": null,
                "mime_type": "text/plain",
                "content": content,
                "metadata": {"suite": "rag_ingestion_honesty"}
            })),
        )
        .await
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

    async fn reindex(&self, document_id: &str, content: &str) -> HttpResult {
        self.request(
            "POST",
            &format!("/api/v1/admin/rag-documents/{document_id}/reindex"),
            None,
            Some(json!({ "content": content })),
        )
        .await
    }

    async fn get_document(&self, document_id: &str) -> HttpResult {
        self.request(
            "GET",
            &format!("/api/v1/admin/rag-documents/{document_id}"),
            None,
            None,
        )
        .await
    }

    async fn list_documents(&self, collection_id: &str) -> HttpResult {
        self.request(
            "GET",
            &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
            None,
            None,
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

    /// The direct-from-the-database half of test 1: the audit trail, not the API's opinion of it.
    async fn version_status(&self, version_id: Uuid) -> String {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select ingestion_status from rag_document_versions where id = $1",
            )
            .bind(version_id)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("version status query timed out")
        .expect("version row exists")
    }

    async fn versions(&self, document_uuid: Uuid) -> Vec<VersionRow> {
        let rows = timeout(
            WAIT,
            sqlx::query(
                "select id, version_number, ingestion_status, superseded_at is not null as superseded
                 from rag_document_versions where document_id = $1 order by version_number",
            )
            .bind(document_uuid)
            .fetch_all(&self.fixture.pool),
        )
        .await
        .expect("version listing timed out")
        .expect("version listing");
        rows.iter()
            .map(|row| VersionRow {
                id: row.get("id"),
                version_number: row.get("version_number"),
                ingestion_status: row.get("ingestion_status"),
                superseded: row.get("superseded"),
            })
            .collect()
    }

    async fn indexed_version_count(&self, document_uuid: Uuid) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from rag_document_versions
                 where document_id = $1 and ingestion_status = 'indexed'",
            )
            .bind(document_uuid)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("indexed count query timed out")
        .expect("indexed count query")
    }
}

/// Test 1 — the API and the audit trail must agree, and both must say `pending`.
#[tokio::test]
async fn ingest_rag_document_reports_pending_over_http_and_in_the_database() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("ingest-honesty").await;
    let created = fixture
        .create_document(&collection_id, "ingest-honesty", None)
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create document failed: {}",
        created.body
    );
    let document_id = created.document_id();

    let ingested = fixture.ingest(&document_id, DOCUMENT_BODY, None).await;
    assert_eq!(
        ingested.status,
        StatusCode::OK,
        "ingest failed: {}",
        ingested.body
    );
    assert_eq!(
        ingested.ingestion_status(),
        &json!("pending"),
        "HTTP ingest response must report pending, got: {}",
        ingested.body
    );

    // Second half: the row itself, read with SQL. The API could be honest while the persisted
    // audit trail still claims 'indexed' — that is exactly defect P0-1.
    let version_id = ingested.current_version_id();
    assert_eq!(
        fixture.version_status(version_id).await,
        "pending",
        "rag_document_versions.ingestion_status for the current version must be 'pending'"
    );
}

/// Test 2 — the create path's own write site, plus the in-transaction response-row re-select.
/// Without that re-select the insert's `RETURNING` row predates the version insert and this
/// response carries `null`; that is the trap this test exists to spring.
#[tokio::test]
async fn create_rag_document_with_inline_content_reports_pending_ingestion_status() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("inline-create").await;
    let created = fixture
        .create_document(&collection_id, "inline-create", Some(DOCUMENT_BODY))
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create document failed: {}",
        created.body
    );
    assert_eq!(
        created.ingestion_status(),
        &json!("pending"),
        "inline-content create must report pending (a `null` here means the response-row \
         re-select is missing): {}",
        created.body
    );

    let version_id = created.current_version_id();
    assert_eq!(
        fixture.version_status(version_id).await,
        "pending",
        "the version written by the create path must be persisted as 'pending'"
    );

    // Pins a deliberate side effect of the response-row re-select. Setting
    // current_version_id fires the rag_documents_bump_version BEFORE UPDATE trigger, so
    // re-selecting after that update reports version 2 where the old insert-RETURNING row
    // reported 1. The handler derives its ETag from this value, so the create response's
    // ETag changed from "1" to "2" for content-carrying creates. That is a correction --
    // the old ETag was stale against the committed row and an immediate If-Match would
    // have spuriously conflicted -- but it is a visible contract change, so it is asserted
    // here rather than left to drift unnoticed.
    assert_eq!(
        created.field("version"),
        &json!(2),
        "a content-carrying create must report the post-trigger version: {}",
        created.body
    );
}

/// Test 3 — `null`, and specifically not `"pending"`. Proves the `Option` is real rather than a
/// default that happens to look right on the paths that do create a version.
#[tokio::test]
async fn create_rag_document_without_content_reports_null_ingestion_status() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("no-content").await;
    let created = fixture
        .create_document(&collection_id, "no-content", None)
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create document failed: {}",
        created.body
    );

    let status = created.ingestion_status();
    assert_ne!(
        status,
        &json!("pending"),
        "a document with no version must not claim pending: {}",
        created.body
    );
    assert_eq!(
        status,
        &Value::Null,
        "ingestion_status must be JSON null when no version exists: {}",
        created.body
    );
    assert_eq!(
        created.field("current_version_id"),
        &Value::Null,
        "no version should have been created: {}",
        created.body
    );

    let document_uuid = fixture.document_uuid(&created.document_id()).await;
    assert!(
        fixture.versions(document_uuid).await.is_empty(),
        "no rag_document_versions row may exist for a content-less create"
    );
}

/// Test 4 — every `rag_document_select(` call site must carry the `LEFT JOIN`, so the single-read
/// and list-read projections cannot drift from the write responses.
#[tokio::test]
async fn rag_document_get_and_list_report_the_same_ingestion_status() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("read-agreement").await;
    let created = fixture
        .create_document(&collection_id, "read-agreement", Some(DOCUMENT_BODY))
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create document failed: {}",
        created.body
    );
    let document_id = created.document_id();
    let expected = json!("pending");
    assert_eq!(
        created.ingestion_status(),
        &expected,
        "create response: {}",
        created.body
    );

    let fetched = fixture.get_document(&document_id).await;
    assert_eq!(
        fetched.status,
        StatusCode::OK,
        "get document failed: {}",
        fetched.body
    );
    assert_eq!(
        fetched.ingestion_status(),
        &expected,
        "GET /rag-documents/{{id}} disagrees with the create response: {}",
        fetched.body
    );

    let listed = fixture.list_documents(&collection_id).await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "list documents failed: {}",
        listed.body
    );
    let entry = listed.body["data"]
        .as_array()
        .expect("list response carries a data array")
        .iter()
        .find(|item| item["id"] == json!(document_id))
        .unwrap_or_else(|| panic!("document missing from listing: {}", listed.body));
    let listed_status = entry
        .as_object()
        .expect("list entry is an object")
        .get("ingestion_status")
        .unwrap_or_else(|| panic!("list entry is missing ingestion_status: {entry}"));
    assert_eq!(
        listed_status, &expected,
        "collection listing disagrees with the create response: {listed_status}"
    );

    // And re-ingesting must move all three read surfaces together, not just the write response.
    let ingested = fixture.ingest(&document_id, REVISED_BODY, None).await;
    assert_eq!(
        ingested.status,
        StatusCode::OK,
        "ingest failed: {}",
        ingested.body
    );
    assert_eq!(ingested.ingestion_status(), &expected);
    assert_eq!(
        fixture.get_document(&document_id).await.ingestion_status(),
        &expected,
        "GET must track the newest version after ingest"
    );
}

/// Test 5 — supersession still works, and no reachable path ever writes `'indexed'`.
#[tokio::test]
async fn reindex_supersedes_the_previous_version_without_ever_writing_indexed() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("reindex").await;
    let created = fixture
        .create_document(&collection_id, "reindex", Some(DOCUMENT_BODY))
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create document failed: {}",
        created.body
    );
    let document_id = created.document_id();
    let document_uuid = fixture.document_uuid(&document_id).await;
    let first_version_id = created.current_version_id();

    let reindexed = fixture.reindex(&document_id, REVISED_BODY).await;
    assert_eq!(
        reindexed.status,
        StatusCode::OK,
        "reindex failed: {}",
        reindexed.body
    );
    assert_eq!(
        reindexed.ingestion_status(),
        &json!("pending"),
        "reindex response: {}",
        reindexed.body
    );
    let second_version_id = reindexed.current_version_id();
    assert_ne!(
        second_version_id, first_version_id,
        "reindex must create a new version rather than mutate the current one"
    );

    let versions = fixture.versions(document_uuid).await;
    assert_eq!(
        versions.len(),
        2,
        "reindex must produce version N+1: {versions:?}"
    );
    assert_eq!(versions[0].id, first_version_id);
    assert_eq!(versions[0].version_number, 1);
    assert!(
        versions[0].superseded,
        "version 1 must carry superseded_at after reindex: {versions:?}"
    );
    // Pins the consequence of the supersession CASE becoming a no-op. It only rewrites
    // 'indexed' into 'superseded', and nothing reaches 'indexed' any more, so a superseded
    // version keeps 'pending' and RagIngestionStatus::Superseded is currently unreachable.
    // superseded_at above remains the authoritative signal. Asserted explicitly because the
    // count(*) = 0 check below is laundered by that same CASE: were a write site to regress
    // to 'indexed', supersession would rewrite it to 'superseded' and the count would still
    // read zero. Plan 11 is expected to change this line when a real pipeline lands.
    assert_eq!(
        versions[0].ingestion_status, "pending",
        "a superseded version keeps its stored status; nothing reaches 'indexed' to convert: {versions:?}"
    );
    assert_eq!(versions[1].id, second_version_id);
    assert_eq!(versions[1].version_number, 2);
    assert!(
        !versions[1].superseded,
        "the newly created version must not be superseded: {versions:?}"
    );
    assert_eq!(
        versions[1].ingestion_status, "pending",
        "the superseding version must be persisted as pending: {versions:?}"
    );

    assert_eq!(
        fixture.indexed_version_count(document_uuid).await,
        0,
        "no rag_document_versions row for this document may ever hold 'indexed': {versions:?}"
    );
}

/// Test 6 — the inverse of 02a's interim characterization test.
///
/// 02a shipped `repeated_ingest_with_the_same_idempotency_key_creates_two_versions_until_02b`,
/// which asserted that a repeated `Idempotency-Key` produced TWO versions because replay was
/// genuinely unimplemented. Plan 02b implements replay, so that assertion is now wrong and has
/// been replaced here by its inverse: one key, one version, one replayed response body.
/// The full replay contract (conflict, in-progress, cached failures, concurrency, actor
/// isolation) lives in `tests/rag_idempotency_replay.rs`; this test stays in the honesty file so
/// 02a's own e2e surface keeps a truthful statement about the same behaviour it once characterised.
///
/// The name deliberately differs from the replay suite's
/// `repeated_ingest_with_the_same_key_replays_and_creates_exactly_one_version`: two test
/// functions sharing one name across two binaries make the `cargo test` transcript ambiguous
/// about which one a failure came from. (Cargo does apply the filter to every test target, so
/// neither would be skipped — the cost is triage confusion, not lost coverage.)
#[tokio::test]
async fn repeated_ingest_with_the_same_key_replays_exactly_one_pending_version() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("idempotency-replay").await;
    let created = fixture
        .create_document(&collection_id, "idempotency-replay", None)
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create document failed: {}",
        created.body
    );
    let document_id = created.document_id();
    let document_uuid = fixture.document_uuid(&document_id).await;
    let key = format!("rag-ingest-{}", fixture.suffix);

    let first = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "first ingest failed: {}",
        first.body
    );
    let second = fixture
        .ingest(&document_id, DOCUMENT_BODY, Some(&key))
        .await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "replayed ingest failed: {}",
        second.body
    );

    assert_eq!(
        second.body.to_string(),
        first.body.to_string(),
        "the second call must replay the original response verbatim"
    );
    assert_eq!(
        second.current_version_id(),
        first.current_version_id(),
        "replay must point at the original version, not a newly created one"
    );

    let versions = fixture.versions(document_uuid).await;
    assert_eq!(
        versions.len(),
        1,
        "the same Idempotency-Key twice must write exactly one version: {versions:?}"
    );
    assert_eq!(versions[0].version_number, 1);
    assert_eq!(
        versions[0].ingestion_status, "pending",
        "02a's honesty contract must survive the move into the runner's transaction: {versions:?}"
    );
}

/// Test 7 — `plans/CONVENTIONS.md` §4 rule 5, live-response half.
///
/// 02a could assert the wire envelope only, because `src/i18n/` was orphaned (`src/lib.rs`
/// declared no `pub mod i18n;`, finding P0-5). 02b Wave 0 wired the catalog, so this test is
/// upgraded to also resolve the key against the compiled Rust catalog via
/// `moira::i18n::is_known_key` — the assertion that catches a `message_key` which merely *looks*
/// well-formed but has no catalog entry behind it.
#[tokio::test]
async fn rag_document_error_responses_carry_catalog_message_keys() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let missing_id = format!("doc_{}", Uuid::now_v7());
    let result = fixture.get_document(&missing_id).await;

    assert_eq!(
        result.status,
        StatusCode::NOT_FOUND,
        "unknown RAG document must 404: {}",
        result.body
    );
    let error = result.body["error"]
        .as_object()
        .unwrap_or_else(|| panic!("missing ErrorDetail envelope: {}", result.body));
    assert_eq!(
        error.get("code"),
        Some(&json!("rag_document_not_found")),
        "unexpected error code: {}",
        result.body
    );
    let message_key = error
        .get("message_key")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("message_key must be a string: {}", result.body));
    assert_eq!(
        message_key, "moira.error.rag_document_not_found",
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
        "{message_key} must resolve in the compiled i18n catalog"
    );
}

/// Test 8 — the document list must stay newest-first, end to end.
///
/// This is behavioural coverage of the ordering contract, and deliberately NOT the regression guard
/// for it. Measured honestly: with the outer `order by` deleted this test still passes at fixture
/// scale, because PostgreSQL only switches to the reordering hash join once
/// `rag_document_versions` is large enough — around five thousand rows in the reproduction. Below
/// that it keeps a nested loop and the CTE's ordering survives by accident. Seeding that many rows
/// per run would be the only way to make this fail reliably, which is not worth the runtime.
///
/// The deterministic guard is the unit test `rag_document_select_orders_the_outer_result` in
/// `src/infra/repositories/conversation.rs`, which asserts the clause in the emitted SQL and fails
/// the instant it is removed. This test still earns its place: it proves the ordering holds through
/// the real HTTP surface, including after re-ingestion moves a document's current version to the
/// end of the version heap.
#[tokio::test]
async fn list_rag_documents_stays_newest_first_after_reingestion() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("list-order").await;

    let mut created_ids = Vec::new();
    for index in 0..5 {
        let created = fixture
            .create_document(
                &collection_id,
                &format!("list-order-{index}"),
                Some(DOCUMENT_BODY),
            )
            .await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "create document {index} failed: {}",
            created.body
        );
        created_ids.push(created.document_id());
    }

    // Give the two OLDEST documents the NEWEST version rows.
    for document_id in created_ids.iter().take(2) {
        let ingested = fixture.ingest(document_id, REVISED_BODY, None).await;
        assert_eq!(
            ingested.status,
            StatusCode::OK,
            "ingest failed: {}",
            ingested.body
        );
    }

    let listed = fixture.list_documents(&collection_id).await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "list documents failed: {}",
        listed.body
    );
    let listed_ids: Vec<String> = listed.body["data"]
        .as_array()
        .unwrap_or_else(|| panic!("list response has no data array: {}", listed.body))
        .iter()
        .map(|entry| {
            entry["id"]
                .as_str()
                .unwrap_or_else(|| panic!("listed document id is not a string: {entry}"))
                .to_string()
        })
        .collect();

    let expected: Vec<String> = created_ids.iter().rev().cloned().collect();
    assert_eq!(
        listed_ids, expected,
        "documents must be listed newest-first regardless of version heap order: {}",
        listed.body
    );
}
