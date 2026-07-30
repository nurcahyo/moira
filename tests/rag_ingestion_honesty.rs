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
//!
//! # Plan 11 wave 1 — what the honesty contract now says
//!
//! 02a's contract was `'pending'` everywhere, because there was no pipeline and `'pending'` was
//! the only truthful thing a version row could say. Plan 11 wave 1 builds the pipeline, so the
//! truthful terminal state changes — but the *contract* does not. It is still "the status must
//! equal what actually happened", and it is still enforced against the database rather than
//! against the API's opinion of it.
//!
//! Every assertion below that reads `'indexed'` is paired with an assertion about **rows**:
//! `rag_chunks` for the version, `rag_chunk_embeddings` for those chunks, and the
//! `rag_ingestion_runs` row's `chunk_count`/`embedded_chunk_count`. That pairing is the whole
//! point. A status string is trivially forgeable by a regressed write site binding a literal —
//! which is exactly the P0-1 defect, whose original form was `values (…, 'indexed', …)`. Row
//! counts are not forgeable without doing the work.
//!
//! The three terminal states this surface can now produce, and what each one asserts:
//!
//! | Application configuration | Terminal status | What must also be true |
//! |---|---|---|
//! | content present, `rag_embeddings_enabled = false` (the default) | `'indexed'` | chunks > 0, embeddings == 0 |
//! | content present, embeddings enabled and working | `'indexed'` | chunks > 0, embeddings == chunks |
//! | content present, embeddings enabled but broken or unconfigured | `'failed'` | chunks > 0, embeddings == 0 |
//! | no content | *(no version row at all)* | — |
//! | content that is only whitespace | `'pending'` | chunks == 0 |

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

use support::{
    LifecycleFixture,
    mock_openai::{EmbeddingBehaviour, MockOpenAiServer, mock_embedding_for},
};

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

/// A `rag_ingestion_runs` row, read straight out of PostgreSQL.
#[derive(Debug)]
struct IngestionRunRow {
    status: String,
    chunk_count: i32,
    embedded_chunk_count: i32,
    failure_class: Option<String>,
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

    /// How many `rag_chunks` rows exist for a version.
    ///
    /// The load-bearing counterpart to every `'indexed'` assertion in this file: the status is a
    /// string a regressed write site could bind unconditionally, whereas a chunk row only exists
    /// if the chunker ran and its output was persisted.
    async fn chunk_count(&self, version_id: Uuid) -> i64 {
        self.scalar(
            "select count(*) from rag_chunks where document_version_id = $1",
            version_id,
        )
        .await
    }

    /// How many of that version's chunks carry a non-null embedding vector.
    async fn embedding_count(&self, version_id: Uuid) -> i64 {
        self.scalar(
            "select count(*) from rag_chunk_embeddings e
             join rag_chunks c on c.id = e.chunk_id
             where c.document_version_id = $1 and e.embedding is not null",
            version_id,
        )
        .await
    }

    async fn scalar(&self, query: &str, id: Uuid) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(query)
                .bind(id)
                .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("count query timed out")
        .expect("count query")
    }

    /// The chunk texts of a version, in `chunk_index` order.
    async fn chunk_texts(&self, version_id: Uuid) -> Vec<String> {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select chunk_text_plain from rag_chunks
                 where document_version_id = $1 order by chunk_index",
            )
            .bind(version_id)
            .fetch_all(&self.fixture.pool),
        )
        .await
        .expect("chunk text query timed out")
        .expect("chunk text query")
    }

    /// The stored embedding for one chunk, decoded from pgvector's text output.
    ///
    /// Reads `embedding::text` rather than binding a vector type, mirroring the write side:
    /// Moira encodes vectors as text and casts, and adds no `pgvector` crate.
    async fn stored_embedding(&self, version_id: Uuid, chunk_index: i32) -> Vec<f32> {
        let encoded: String = timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select e.embedding::text from rag_chunk_embeddings e
                 join rag_chunks c on c.id = e.chunk_id
                 where c.document_version_id = $1 and c.chunk_index = $2",
            )
            .bind(version_id)
            .bind(chunk_index)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("embedding query timed out")
        .expect("embedding row exists");
        encoded
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|part| part.trim().parse::<f32>().expect("vector component"))
            .collect()
    }

    /// The `rag_ingestion_runs` row for a version — the table plan 11 exists to finally write.
    async fn ingestion_run(&self, version_id: Uuid) -> IngestionRunRow {
        let row = timeout(
            WAIT,
            sqlx::query(
                "select status, chunk_count, embedded_chunk_count, failure_class
                 from rag_ingestion_runs where document_version_id = $1",
            )
            .bind(version_id)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("ingestion run query timed out")
        .expect("exactly one ingestion run row exists for this version");
        IngestionRunRow {
            status: row.get("status"),
            chunk_count: row.get("chunk_count"),
            embedded_chunk_count: row.get("embedded_chunk_count"),
            failure_class: row.get("failure_class"),
        }
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

/// Test 1 — the API and the audit trail must agree, and `'indexed'` must be earned.
///
/// 02a's version of this test pinned `'pending'` on both surfaces, which was truthful while no
/// pipeline existed. Plan 11 wave 1 gives `'pending'` somewhere to go, so the pin moves to the
/// new terminal state — and gains the row-level assertions that make the new state mean
/// something. The API/audit-trail agreement half is unchanged: it is still the defect P0-1
/// actually was, an API reporting one thing while `rag_document_versions` recorded another.
#[tokio::test]
async fn ingest_rag_document_reports_indexed_only_when_chunks_were_really_written() {
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
        &json!("indexed"),
        "HTTP ingest response must report the terminal status, got: {}",
        ingested.body
    );

    // Second half: the row itself, read with SQL. The API could be honest while the persisted
    // audit trail still claims something else — that is exactly defect P0-1.
    let version_id = ingested.current_version_id();
    assert_eq!(
        fixture.version_status(version_id).await,
        "indexed",
        "rag_document_versions.ingestion_status for the current version must be 'indexed'"
    );

    // Third half, and the one that carries the weight. `'indexed'` is a string; a write site
    // that regressed to binding it as a literal would satisfy both assertions above while doing
    // nothing at all, which is precisely the shape of the original P0-1 defect. Chunk rows
    // cannot be produced without running the chunker.
    let chunks = fixture.chunk_count(version_id).await;
    assert!(
        chunks > 0,
        "a version marked 'indexed' must have rag_chunks rows; found {chunks}"
    );
    let texts = fixture.chunk_texts(version_id).await;
    assert_eq!(
        texts.concat().replace(char::is_whitespace, ""),
        DOCUMENT_BODY.replace(char::is_whitespace, ""),
        "the stored chunks must reconstruct the ingested body, not arbitrary text: {texts:?}"
    );

    // This fixture's application has no embedding policy, so `rag_embeddings_enabled` is false
    // and zero embeddings is the honest outcome — recorded on the run row rather than implied.
    assert_eq!(
        fixture.embedding_count(version_id).await,
        0,
        "no embedding policy is configured, so no embeddings may exist"
    );
    let run = fixture.ingestion_run(version_id).await;
    assert_eq!(run.status, "completed", "ingestion run: {run:?}");
    assert_eq!(
        i64::from(run.chunk_count),
        chunks,
        "the run row's chunk_count must equal the chunks actually stored: {run:?}"
    );
    assert_eq!(run.embedded_chunk_count, 0, "ingestion run: {run:?}");
    assert_eq!(run.failure_class, None, "ingestion run: {run:?}");
}

/// Test 1b — whitespace-only content indexes nothing, and says so.
///
/// The gap this closes: with the terminal status derived from "did we produce chunks", a body
/// that chunks to nothing must not reach `'indexed'`. A pipeline that marked every accepted
/// request `'indexed'` regardless of output would pass test 1 and fail here.
#[tokio::test]
async fn content_that_chunks_to_nothing_stays_pending_rather_than_claiming_indexed() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let collection_id = fixture.create_collection("whitespace").await;
    let created = fixture
        .create_document(&collection_id, "whitespace", None)
        .await;
    let document_id = created.document_id();

    let ingested = fixture.ingest(&document_id, "   \n\n\t  \n", None).await;
    assert_eq!(
        ingested.status,
        StatusCode::OK,
        "ingest failed: {}",
        ingested.body
    );
    assert_eq!(
        ingested.ingestion_status(),
        &json!("pending"),
        "content that produces no chunks must not claim to be indexed: {}",
        ingested.body
    );
    let version_id = ingested.current_version_id();
    assert_eq!(fixture.version_status(version_id).await, "pending");
    assert_eq!(fixture.chunk_count(version_id).await, 0);
}

/// Test 2 — the create path's own write site, plus the in-transaction response-row re-select.
/// Without that re-select the insert's `RETURNING` row predates the version insert and this
/// response carries `null`; that is the trap this test exists to spring.
#[tokio::test]
async fn create_rag_document_with_inline_content_runs_the_same_pipeline_as_ingest() {
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
        &json!("indexed"),
        "inline-content create must report the terminal status (a `null` here means the \
         response-row re-select is missing): {}",
        created.body
    );

    let version_id = created.current_version_id();
    assert_eq!(
        fixture.version_status(version_id).await,
        "indexed",
        "the version written by the create path must be persisted as 'indexed'"
    );
    // Create-with-inline-content is a third ingestion entry point alongside `/ingest` and
    // `/reindex`, and it is the one the plan's own scope table never mentions. A pipeline wired
    // into only the other two would leave this path writing chunk-less versions forever, so it
    // gets the same row-level proof rather than a status check.
    assert!(
        fixture.chunk_count(version_id).await > 0,
        "the create path must run the chunker, not only the version insert"
    );
    assert_eq!(fixture.ingestion_run(version_id).await.status, "completed");

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
    assert_eq!(
        fixture
            .scalar(
                "select count(*) from rag_ingestion_runs where document_id = $1",
                document_uuid
            )
            .await,
        0,
        "a content-less create ran no pipeline, so it must record no ingestion run"
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
    let expected = json!("indexed");
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

/// Test 5 — supersession still works, and `'indexed'` is reached only by doing the work.
///
/// # Why the old `indexed_version_count == 0` assertion could not simply be inverted
///
/// 02a's version of this test asserted that no row for the document ever holds `'indexed'`, and
/// its own comment (kept below, updated) recorded why that assertion was weaker than it looked:
/// the supersession `CASE` in `ingest_rag_document_with_connection` rewrites `'indexed'` into
/// `'superseded'`, so a write site that regressed to binding `'indexed'` would have been
/// laundered into `'superseded'` and the count would still have read zero.
///
/// The naive inversion — `indexed_version_count == 1` — has the mirror-image flaw. It is
/// satisfied by *any* write site that binds the literal `'indexed'`, which is precisely the
/// P0-1 defect in its original form. It would pass against a pipeline that does no work at all.
///
/// So this test asserts the two things a status string cannot fake, once each:
///
/// 1. **Version 2 is `'indexed'` *and* has chunk rows, an embedding-run row, and a
///    `chunk_count` on that row equal to the chunks actually stored.** Rows require the
///    chunker to have run.
/// 2. **Version 1 is `'superseded'`.** That is stronger than it appears, and it is why the
///    laundering `CASE` is now an asset rather than a hazard: the `CASE` rewrites *only*
///    `'indexed'`. A version that never reached `'indexed'` stays at whatever it held — which
///    is what 02a observed, and why that assertion read `'pending'`. So `'superseded'` on
///    version 1 is positive evidence that version 1 genuinely reached `'indexed'` under its own
///    ingestion, not an artefact of anything.
#[tokio::test]
async fn reindex_supersedes_the_previous_version_and_indexed_is_earned_not_laundered() {
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
        &json!("indexed"),
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
    // Point 2 of the doc comment. The supersession CASE rewrites *only* 'indexed', so this
    // value is positive evidence that version 1 reached 'indexed' under its own ingestion. Under
    // 02a this same assertion read 'pending', because nothing reached 'indexed' to convert.
    assert_eq!(
        versions[0].ingestion_status, "superseded",
        "version 1 must have reached 'indexed' for the supersession CASE to convert it; \
         'pending' here means the create path stopped chunking: {versions:?}"
    );
    // The superseded version keeps its chunks. They are real rows describing real content, and
    // deleting them here would destroy the only evidence that version 1's ingestion happened.
    assert!(
        fixture.chunk_count(first_version_id).await > 0,
        "supersession must not delete the previous version's chunks"
    );

    assert_eq!(versions[1].id, second_version_id);
    assert_eq!(versions[1].version_number, 2);
    assert!(
        !versions[1].superseded,
        "the newly created version must not be superseded: {versions:?}"
    );
    assert_eq!(
        versions[1].ingestion_status, "indexed",
        "the superseding version must be persisted as indexed: {versions:?}"
    );

    // Point 1. Exactly one live 'indexed' row, and it is backed by work — the assertion the
    // naive `count(*) == 1` inversion would have skipped.
    assert_eq!(
        fixture.indexed_version_count(document_uuid).await,
        1,
        "only the current version may hold 'indexed': {versions:?}"
    );
    let chunks = fixture.chunk_count(second_version_id).await;
    assert!(
        chunks > 0,
        "the reindexed version claims 'indexed' with no chunk rows behind it"
    );
    let texts = fixture.chunk_texts(second_version_id).await;
    assert_eq!(
        texts.concat().replace(char::is_whitespace, ""),
        REVISED_BODY.replace(char::is_whitespace, ""),
        "the reindexed version's chunks must come from the revised body, not the original: \
         {texts:?}"
    );
    let run = fixture.ingestion_run(second_version_id).await;
    assert_eq!(run.status, "completed", "ingestion run: {run:?}");
    assert_eq!(
        i64::from(run.chunk_count),
        chunks,
        "the run row must report the chunks that were actually stored: {run:?}"
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
async fn repeated_ingest_with_the_same_key_replays_exactly_one_indexed_version() {
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
        versions[0].ingestion_status, "indexed",
        "02a's honesty contract must survive the move into the runner's transaction: {versions:?}"
    );
    // Replay must not run the pipeline twice. The chunks are written inside the same
    // transaction the idempotency envelope owns, so a replayed call that re-executed the
    // mutation would double them — and `rag_chunks_version_index_unique` would surface that as
    // a constraint violation rather than as a silent duplicate, but only if a *new* version were
    // created. Counting rows against the single version is what covers the in-place case.
    let chunks = fixture.chunk_count(versions[0].id).await;
    assert!(chunks > 0, "the ingested version must carry chunks");
    let run = fixture.ingestion_run(versions[0].id).await;
    assert_eq!(
        i64::from(run.chunk_count),
        chunks,
        "one key, one ingestion run, one set of chunks: {run:?}"
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

// ---------------------------------------------------------------------------------------
// Plan 11 wave 1 — the embedding half of the pipeline.
//
// There is no live embedding credential in this repository and none is required. The mock at
// `tests/support/mock_openai.rs` serves `/v1/embeddings` with deterministic vectors, so these
// tests prove the pipeline — resolve the policy, resolve the provider and credential, call,
// persist the vectors against the right chunks, derive the status from the outcome — rather
// than any provider's embedding quality.
//
// What is therefore NOT covered here, and would need a live provider: real vector semantics
// (that similar text yields nearby vectors), real provider rate limiting, and real model
// dimensions other than 1536.
// ---------------------------------------------------------------------------------------

/// Test 9 — with embeddings enabled and working, every chunk gets the vector the provider
/// returned for *its own text*.
#[tokio::test]
async fn embeddings_enabled_stores_one_provider_vector_per_chunk() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start(Vec::new()).await;
    fixture
        .fixture
        .enable_rag_embeddings(provider.base_url(), "text-embedding-3-small")
        .await;

    let collection_id = fixture.create_collection("embedded").await;
    let created = fixture
        .create_document(&collection_id, "embedded", None)
        .await;
    let document_id = created.document_id();

    // Three paragraphs, so the batch_size of 2 the fixture configures forces more than one
    // provider call and the batching loop is actually exercised.
    let body = "Alpha paragraph one.\n\nBeta paragraph two.\n\nGamma paragraph three.";
    let ingested = fixture.ingest(&document_id, body, None).await;
    assert_eq!(
        ingested.status,
        StatusCode::OK,
        "ingest failed: {}",
        ingested.body
    );
    assert_eq!(
        ingested.ingestion_status(),
        &json!("indexed"),
        "ingest response: {}",
        ingested.body
    );

    let version_id = ingested.current_version_id();
    assert_eq!(fixture.chunk_count(version_id).await, 3);
    assert_eq!(
        fixture.embedding_count(version_id).await,
        3,
        "every chunk must carry an embedding when embeddings are enabled and working"
    );

    let run = fixture.ingestion_run(version_id).await;
    assert_eq!(run.status, "completed", "ingestion run: {run:?}");
    assert_eq!(run.chunk_count, 3, "ingestion run: {run:?}");
    assert_eq!(run.embedded_chunk_count, 3, "ingestion run: {run:?}");
    assert_eq!(run.failure_class, None, "ingestion run: {run:?}");

    // Batching: three inputs at batch_size 2 is two calls, not one and not three.
    let calls = provider.embedding_requests().await;
    assert_eq!(
        calls.len(),
        2,
        "three chunks at batch_size 2 must be two provider calls: {calls:?}"
    );
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Bearer sk-embedding-secret"),
        "the embedding call must carry the resolved provider credential"
    );

    // The assertion that proves vectors were not shuffled between chunks: each stored vector
    // must be the one the provider produced for that chunk's own text. A zip that silently
    // mis-aligned by one would pass every count assertion above and fail here.
    let texts = fixture.chunk_texts(version_id).await;
    for (index, text) in texts.iter().enumerate() {
        let stored = fixture.stored_embedding(version_id, index as i32).await;
        assert_eq!(
            stored,
            mock_embedding_for(text),
            "chunk {index} ({text:?}) stored a vector belonging to different text"
        );
    }

    provider.shutdown().await;
}

/// Test 10 — embeddings enabled with no provider configured must fail the version, not index it.
///
/// This is the honesty case that most easily regresses into a lie. The application has asked
/// for semantic indexing and is not getting it; reporting `'indexed'` would tell an operator
/// that retrieval will work when it cannot.
#[tokio::test]
async fn embeddings_enabled_without_a_provider_fails_the_version_rather_than_indexing_it() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    fixture
        .fixture
        .enable_rag_embeddings_without_a_provider()
        .await;

    let collection_id = fixture.create_collection("unconfigured").await;
    let created = fixture
        .create_document(&collection_id, "unconfigured", None)
        .await;
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
        &json!("failed"),
        "an application that asked for embeddings and has no provider must not be told its \
         document is indexed: {}",
        ingested.body
    );

    let version_id = ingested.current_version_id();
    assert_eq!(fixture.version_status(version_id).await, "failed");
    // The chunks that did succeed are kept: they were really produced and really stored, and
    // discarding them would lose the only work that completed.
    assert!(
        fixture.chunk_count(version_id).await > 0,
        "a failed embedding stage must not discard the chunks it already produced"
    );
    assert_eq!(fixture.embedding_count(version_id).await, 0);
    let run = fixture.ingestion_run(version_id).await;
    assert_eq!(run.status, "failed", "ingestion run: {run:?}");
    assert_eq!(
        run.failure_class.as_deref(),
        Some("embedding_not_configured"),
        "ingestion run: {run:?}"
    );
}

/// Test 11 — a provider that errors fails the version, and the failure is classified.
#[tokio::test]
async fn an_embedding_provider_error_fails_the_version_and_is_classified() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start(Vec::new()).await;
    provider
        .set_embedding_behaviour(EmbeddingBehaviour::HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "{\"error\":{\"message\":\"upstream exploded\"}}".to_string(),
        })
        .await;
    fixture
        .fixture
        .enable_rag_embeddings(provider.base_url(), "text-embedding-3-small")
        .await;

    let collection_id = fixture.create_collection("provider-error").await;
    let created = fixture
        .create_document(&collection_id, "provider-error", None)
        .await;
    let document_id = created.document_id();
    let ingested = fixture.ingest(&document_id, DOCUMENT_BODY, None).await;

    assert_eq!(ingested.status, StatusCode::OK, "{}", ingested.body);
    assert_eq!(
        ingested.ingestion_status(),
        &json!("failed"),
        "a failed embedding call must not produce an 'indexed' version: {}",
        ingested.body
    );
    let version_id = ingested.current_version_id();
    assert!(fixture.chunk_count(version_id).await > 0);
    assert_eq!(fixture.embedding_count(version_id).await, 0);
    let run = fixture.ingestion_run(version_id).await;
    assert_eq!(run.failure_class.as_deref(), Some("embedding_failed"));

    // The provider's response body must not reach the caller. It can echo the request, and an
    // embedding request body is document content.
    let serialised = ingested.body.to_string();
    assert!(
        !serialised.contains("upstream exploded"),
        "the provider's error body leaked into the ingest response: {serialised}"
    );

    provider.shutdown().await;
}

/// Test 12 — a short provider response is refused rather than zip-truncated.
///
/// Without this, `chunks.zip(vectors)` would silently attach vectors to the wrong chunks and
/// drop the tail, producing an index that is subtly wrong in a way no count assertion catches.
#[tokio::test]
async fn a_short_embedding_response_fails_the_version_rather_than_misaligning_vectors() {
    let Some(fixture) = RagFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start(Vec::new()).await;
    provider
        .set_embedding_behaviour(EmbeddingBehaviour::ShortResponse)
        .await;
    fixture
        .fixture
        .enable_rag_embeddings(provider.base_url(), "text-embedding-3-small")
        .await;

    let collection_id = fixture.create_collection("short-response").await;
    let created = fixture
        .create_document(&collection_id, "short-response", None)
        .await;
    let document_id = created.document_id();
    let ingested = fixture
        .ingest(&document_id, "One.\n\nTwo.\n\nThree.", None)
        .await;

    assert_eq!(
        ingested.ingestion_status(),
        &json!("failed"),
        "a provider returning fewer vectors than inputs must fail the version: {}",
        ingested.body
    );
    let version_id = ingested.current_version_id();
    assert_eq!(
        fixture.embedding_count(version_id).await,
        0,
        "a partially-embedded version must store no embeddings at all"
    );

    provider.shutdown().await;
}
