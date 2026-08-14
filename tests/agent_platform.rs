//! End-to-end CRUD over HTTP for the agent-platform skill registry (issue #214, plan 12 §3).
//!
//! Drives `/api/v1/admin/skills` against a real Postgres database created by
//! [`support::TestDatabase`], which applies `migrations/0031_agent_platform.sql`. Skips (never
//! fails) when no test database is configured, following the CONVENTIONS §3 gating pattern —
//! CI's Postgres shard is the authoritative run.

mod support;

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{app::AppState, config::Settings};
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

use support::TestDatabase;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    router: Router,
    suffix: String,
    _database: TestDatabase,
}

struct HttpResult {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let database = TestDatabase::create().await?;
        let pool = database.pool.clone();
        let settings = Settings::default();
        let state = AppState::new(settings, Some(pool))
            .await
            .expect("test app state");
        let router = moira::build_router(state).expect("test router");
        Some(Self {
            router,
            suffix: Uuid::now_v7().simple().to_string(),
            _database: database,
        })
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        if_match: Option<i64>,
        body: Option<Value>,
    ) -> HttpResult {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("x-request-id", format!("agent-platform-{}", Uuid::now_v7()));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        if let Some(version) = if_match {
            builder = builder.header("if-match", version.to_string());
        }
        let request = builder
            .body(match body {
                Some(value) => Body::from(value.to_string()),
                None => Body::empty(),
            })
            .expect("HTTP request");
        let response = timeout(WAIT, self.router.clone().oneshot(request))
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
}

fn version_of(result: &HttpResult) -> i64 {
    result
        .etag
        .as_deref()
        .expect("ETag header")
        // The resource-version ETag is quoted (`"1"`), matching every sibling versioned
        // resource — strip the quotes (and any weak-validator prefix) before parsing.
        .trim_start_matches("W/")
        .trim_matches('"')
        .parse()
        .expect("numeric ETag")
}

fn id_of(result: &HttpResult) -> Uuid {
    Uuid::parse_str(result.body["id"].as_str().expect("resource id")).expect("UUID id")
}

#[tokio::test]
async fn skills_crud_lifecycle_over_http() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let skill_key = format!("s-{}", fixture.suffix);

    // Create.
    let created = fixture
        .request(
            "POST",
            "/api/v1/admin/skills",
            None,
            None,
            Some(json!({
                "skill_key": skill_key,
                "display_name": "Order Lookup",
                "description": "Looks up an order by id",
                "kind": "tool",
                "params_schema": {"type": "object", "properties": {"order_id": {"type": "string"}}},
                "tags": ["orders", "readonly"]
            })),
        )
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "unexpected body: {}",
        created.body
    );
    assert_eq!(created.body["status"], "draft");
    assert_eq!(created.body["kind"], "tool");
    assert_eq!(created.body["tags"], json!(["orders", "readonly"]));
    let skill_id = id_of(&created);

    // Get.
    let fetched = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/skills/{skill_id}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(fetched.status, StatusCode::OK);
    assert_eq!(fetched.body["skill_key"], skill_key);

    // List includes it.
    let listed = fixture
        .request("GET", "/api/v1/admin/skills", None, None, None)
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert!(
        listed.body["data"]
            .as_array()
            .expect("data array")
            .iter()
            .any(|row| row["id"] == created.body["id"]),
        "list did not contain the created skill"
    );

    // Patch requires If-Match and bumps version.
    let patched = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}"),
            None,
            Some(version_of(&created)),
            Some(json!({"display_name": "Order Lookup v2"})),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "body: {}", patched.body);
    assert_eq!(patched.body["display_name"], "Order Lookup v2");
    assert!(version_of(&patched) > version_of(&created));

    // A stale If-Match is a 409.
    let stale = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}"),
            None,
            Some(version_of(&created)),
            Some(json!({"display_name": "should conflict"})),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "body: {}", stale.body);

    // Enable.
    let enabled = fixture
        .request(
            "POST",
            &format!("/api/v1/admin/skills/{skill_id}/enable"),
            None,
            Some(version_of(&patched)),
            None,
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "body: {}", enabled.body);
    assert_eq!(enabled.body["status"], "enabled");

    // Disable.
    let disabled = fixture
        .request(
            "POST",
            &format!("/api/v1/admin/skills/{skill_id}/disable"),
            None,
            Some(version_of(&enabled)),
            None,
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "body: {}", disabled.body);
    assert_eq!(disabled.body["status"], "disabled");

    // Bulk-enable flips it back to enabled and returns it.
    let bulk = fixture
        .request(
            "POST",
            "/api/v1/admin/skills/bulk-enable",
            None,
            None,
            Some(json!({"skill_ids": [skill_id]})),
        )
        .await;
    assert_eq!(bulk.status, StatusCode::OK, "body: {}", bulk.body);
    let bulk_data = bulk.body["data"].as_array().expect("bulk data array");
    assert_eq!(bulk_data.len(), 1);
    assert_eq!(bulk_data[0]["status"], "enabled");

    // Delete requires If-Match; afterwards the row is gone.
    let refetched = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/skills/{skill_id}"),
            None,
            None,
            None,
        )
        .await;
    let deleted = fixture
        .request(
            "DELETE",
            &format!("/api/v1/admin/skills/{skill_id}"),
            None,
            Some(version_of(&refetched)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status,
        StatusCode::NO_CONTENT,
        "body: {}",
        deleted.body
    );

    let gone = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/skills/{skill_id}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_skill_is_idempotent_under_a_replay_key() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let skill_key = format!("i-{}", fixture.suffix);
    let key = format!("idem-{}", fixture.suffix);
    let body = json!({
        "skill_key": skill_key,
        "display_name": "Idempotent Skill",
        "kind": "guard"
    });

    let first = fixture
        .request(
            "POST",
            "/api/v1/admin/skills",
            Some(&key),
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);

    let replay = fixture
        .request("POST", "/api/v1/admin/skills", Some(&key), None, Some(body))
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "body: {}", replay.body);
    assert_eq!(
        replay.body["id"], first.body["id"],
        "replay must return the same skill id"
    );
}

#[tokio::test]
async fn create_skill_rejects_an_invalid_key() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let bad = fixture
        .request(
            "POST",
            "/api/v1/admin/skills",
            None,
            None,
            Some(json!({
                "skill_key": "Not A Valid Key!",
                "display_name": "Bad",
                "kind": "tool"
            })),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "body: {}", bad.body);
}
