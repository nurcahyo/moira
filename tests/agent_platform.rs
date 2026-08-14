//! End-to-end CRUD over HTTP for the agent-platform registries (issue #214, plan 12 §3):
//! skills, eval suites/cases, and flows/steps (F2, the deferred remainder of workstream F).
//!
//! Drives `/api/v1/admin/skills`, `/api/v1/admin/eval-suites`, and `/api/v1/admin/flows`
//! against a real Postgres database created by [`support::TestDatabase`], which applies
//! `migrations/0031_agent_platform.sql`. Skips (never fails) when no test database is
//! configured, following the CONVENTIONS §3 gating pattern — CI's Postgres shard is the
//! authoritative run.

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

impl Fixture {
    /// Creates a live `agent_profiles` row over `/api/v1/admin/agent-profiles` so an
    /// eval/flow test can name it as a fail-closed-checked reference (flow step
    /// `agent_profile_id`) without duplicating agent-profile CRUD here.
    async fn create_agent_profile(&self, key_suffix: &str) -> Uuid {
        let created = self
            .request(
                "POST",
                "/api/v1/admin/agent-profiles",
                None,
                None,
                Some(json!({
                    "profile_key": format!("ap-{}-{key_suffix}", self.suffix),
                    "display_name": "F2 fixture agent",
                })),
            )
            .await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "agent profile fixture: {}",
            created.body
        );
        id_of(&created)
    }
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

// =========================================================================================
// F2 (issue #214, plan 12 §3) — eval suites + cases.
// =========================================================================================

#[tokio::test]
async fn eval_suites_and_cases_crud_lifecycle_over_http() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let suite_key = format!("es-{}", fixture.suffix);

    // Create.
    let created = fixture
        .request(
            "POST",
            "/api/v1/admin/eval-suites",
            None,
            None,
            Some(json!({
                "suite_key": suite_key,
                "display_name": "Order lookup regression",
                "description": "Cases that must never regress"
            })),
        )
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "body: {}",
        created.body
    );
    assert_eq!(created.body["status"], "active");
    let suite_id = id_of(&created);

    // A case cannot be attached to a suite that does not exist.
    let orphan = fixture
        .request(
            "POST",
            &format!("/api/v1/admin/eval-suites/{}/cases", Uuid::now_v7()),
            None,
            None,
            Some(json!({
                "input": {"prompt": "hi"},
                "expected": {"text": "hello"},
                "grading_kind": "exact_match"
            })),
        )
        .await;
    assert_eq!(
        orphan.status,
        StatusCode::NOT_FOUND,
        "body: {}",
        orphan.body
    );

    // Create two cases.
    let case_one = fixture
        .request(
            "POST",
            &format!("/api/v1/admin/eval-suites/{suite_id}/cases"),
            None,
            None,
            Some(json!({
                "input": {"prompt": "look up order 1"},
                "expected": {"order_id": "1"},
                "grading_kind": "exact_match"
            })),
        )
        .await;
    assert_eq!(
        case_one.status,
        StatusCode::CREATED,
        "body: {}",
        case_one.body
    );
    assert_eq!(case_one.body["suite_id"], suite_id.to_string());
    let case_one_id = id_of(&case_one);

    let case_two = fixture
        .request(
            "POST",
            &format!("/api/v1/admin/eval-suites/{suite_id}/cases"),
            None,
            None,
            Some(json!({
                "input": {"prompt": "look up order 2"},
                "expected": {"order_id": "2"},
                "grading_kind": "contains"
            })),
        )
        .await;
    assert_eq!(
        case_two.status,
        StatusCode::CREATED,
        "body: {}",
        case_two.body
    );

    // A null input/expected is rejected.
    let null_input = fixture
        .request(
            "POST",
            &format!("/api/v1/admin/eval-suites/{suite_id}/cases"),
            None,
            None,
            Some(json!({
                "input": null,
                "expected": {"order_id": "3"},
                "grading_kind": "exact_match"
            })),
        )
        .await;
    assert_eq!(
        null_input.status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        null_input.body
    );

    // List includes both cases.
    let cases = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/eval-suites/{suite_id}/cases"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(cases.status, StatusCode::OK);
    assert_eq!(cases.body["data"].as_array().expect("data array").len(), 2);

    // The suite's run list exists and is empty — nothing has ever executed it.
    let runs = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/eval-suites/{suite_id}/runs"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(runs.status, StatusCode::OK, "body: {}", runs.body);
    assert_eq!(runs.body["data"].as_array().expect("data array").len(), 0);

    // Patch requires If-Match and bumps version.
    let patched = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/eval-suites/{suite_id}"),
            None,
            Some(version_of(&created)),
            Some(json!({"display_name": "Order lookup regression v2"})),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "body: {}", patched.body);
    assert!(version_of(&patched) > version_of(&created));

    // A stale If-Match is a 409.
    let stale = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/eval-suites/{suite_id}"),
            None,
            Some(version_of(&created)),
            Some(json!({"display_name": "should conflict"})),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "body: {}", stale.body);

    // Delete one case; the list shrinks to one.
    let case_deleted = fixture
        .request(
            "DELETE",
            &format!("/api/v1/admin/eval-suites/{suite_id}/cases/{case_one_id}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(case_deleted.status, StatusCode::NO_CONTENT);
    let cases_after = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/eval-suites/{suite_id}/cases"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(
        cases_after.body["data"]
            .as_array()
            .expect("data array")
            .len(),
        1
    );

    // Delete requires If-Match; afterwards the row is gone.
    let deleted = fixture
        .request(
            "DELETE",
            &format!("/api/v1/admin/eval-suites/{suite_id}"),
            None,
            Some(version_of(&patched)),
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
            &format!("/api/v1/admin/eval-suites/{suite_id}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_eval_suite_is_idempotent_under_a_replay_key() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let suite_key = format!("esi-{}", fixture.suffix);
    let key = format!("idem-eval-{}", fixture.suffix);
    let body = json!({
        "suite_key": suite_key,
        "display_name": "Idempotent suite"
    });

    let first = fixture
        .request(
            "POST",
            "/api/v1/admin/eval-suites",
            Some(&key),
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);

    let replay = fixture
        .request(
            "POST",
            "/api/v1/admin/eval-suites",
            Some(&key),
            None,
            Some(body),
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "body: {}", replay.body);
    assert_eq!(
        replay.body["id"], first.body["id"],
        "replay must return the same eval suite id"
    );
}

// =========================================================================================
// F2 (issue #214, plan 12 §3) — flows + embedded steps.
// =========================================================================================

#[tokio::test]
async fn flows_and_steps_crud_lifecycle_over_http() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let agent_one = fixture.create_agent_profile("one").await;
    let agent_two = fixture.create_agent_profile("two").await;
    let flow_key = format!("fl-{}", fixture.suffix);

    // A step naming a missing agent profile is rejected fail-closed, before the flow is
    // written at all.
    let missing_agent = Uuid::now_v7();
    let rejected = fixture
        .request(
            "POST",
            "/api/v1/admin/flows",
            None,
            None,
            Some(json!({
                "flow_key": format!("{flow_key}-rejected"),
                "display_name": "Should not be created",
                "steps": [
                    {"step_key": "only", "step_order": 0, "agent_profile_id": missing_agent}
                ]
            })),
        )
        .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        rejected.body
    );
    let listed_after_rejection = fixture
        .request("GET", "/api/v1/admin/flows", None, None, None)
        .await;
    assert!(
        !listed_after_rejection.body["data"]
            .as_array()
            .expect("data array")
            .iter()
            .any(|row| row["flow_key"] == format!("{flow_key}-rejected")),
        "a flow rejected for a missing agent profile reference must not be written"
    );

    // Create, with two steps in order.
    let created = fixture
        .request(
            "POST",
            "/api/v1/admin/flows",
            None,
            None,
            Some(json!({
                "flow_key": flow_key,
                "display_name": "Two-step triage",
                "steps": [
                    {"step_key": "triage", "step_order": 0, "agent_profile_id": agent_one},
                    {"step_key": "resolve", "step_order": 1, "agent_profile_id": agent_two}
                ]
            })),
        )
        .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "body: {}",
        created.body
    );
    let steps = created.body["steps"].as_array().expect("steps array");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["step_key"], "triage");
    assert_eq!(steps[0]["on_failure"], "abort");
    assert_eq!(steps[1]["step_key"], "resolve");
    let flow_id = id_of(&created);

    // Get echoes the same steps.
    let fetched = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/flows/{flow_id}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(fetched.status, StatusCode::OK);
    assert_eq!(fetched.body["steps"].as_array().expect("steps").len(), 2);

    // List includes it, with steps populated (not an N+1 that silently drops them).
    let listed = fixture
        .request("GET", "/api/v1/admin/flows", None, None, None)
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    let listed_row = listed.body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .find(|row| row["id"] == flow_id.to_string())
        .expect("created flow must be in the list");
    assert_eq!(listed_row["steps"].as_array().expect("steps").len(), 2);

    // The flow's run list exists and is empty — there is no execution engine yet.
    let runs = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/flows/{flow_id}/runs"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(runs.status, StatusCode::OK, "body: {}", runs.body);
    assert_eq!(runs.body["data"].as_array().expect("data array").len(), 0);

    // Patch without a `steps` field leaves the steps untouched.
    let patched_name_only = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/flows/{flow_id}"),
            None,
            Some(version_of(&created)),
            Some(json!({"display_name": "Two-step triage v2"})),
        )
        .await;
    assert_eq!(
        patched_name_only.status,
        StatusCode::OK,
        "body: {}",
        patched_name_only.body
    );
    assert_eq!(
        patched_name_only.body["steps"]
            .as_array()
            .expect("steps")
            .len(),
        2,
        "omitting `steps` on PATCH must leave the existing step list untouched"
    );

    // A stale If-Match is a 409.
    let stale = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/flows/{flow_id}"),
            None,
            Some(version_of(&created)),
            Some(json!({"display_name": "should conflict"})),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "body: {}", stale.body);

    // Patch with a `steps` field replaces the whole list atomically.
    let patched_steps = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/flows/{flow_id}"),
            None,
            Some(version_of(&patched_name_only)),
            Some(json!({
                "steps": [
                    {"step_key": "solo", "step_order": 0, "agent_profile_id": agent_one}
                ]
            })),
        )
        .await;
    assert_eq!(
        patched_steps.status,
        StatusCode::OK,
        "body: {}",
        patched_steps.body
    );
    let replaced_steps = patched_steps.body["steps"].as_array().expect("steps");
    assert_eq!(replaced_steps.len(), 1);
    assert_eq!(replaced_steps[0]["step_key"], "solo");

    // Delete requires If-Match; afterwards the row is gone.
    let deleted = fixture
        .request(
            "DELETE",
            &format!("/api/v1/admin/flows/{flow_id}"),
            None,
            Some(version_of(&patched_steps)),
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
            &format!("/api/v1/admin/flows/{flow_id}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_flow_is_idempotent_under_a_replay_key() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let flow_key = format!("fli-{}", fixture.suffix);
    let key = format!("idem-flow-{}", fixture.suffix);
    let body = json!({
        "flow_key": flow_key,
        "display_name": "Idempotent flow"
    });

    let first = fixture
        .request(
            "POST",
            "/api/v1/admin/flows",
            Some(&key),
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);

    let replay = fixture
        .request("POST", "/api/v1/admin/flows", Some(&key), None, Some(body))
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "body: {}", replay.body);
    assert_eq!(
        replay.body["id"], first.body["id"],
        "replay must return the same flow id"
    );
}
