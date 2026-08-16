//! End-to-end read over HTTP for the derived relationship graph (plan 12 §4, issue #234).
//!
//! Drives `GET /api/v1/admin/graph` against a real Postgres database created by
//! [`support::TestDatabase`], which applies `migrations/0031_agent_platform.sql`. Skips (never
//! fails) when no test database is configured, following the CONVENTIONS §3 gating pattern —
//! CI's Postgres shard is the authoritative run.
//!
//! `agent_profiles.skill_refs` has no admin CRUD wired to it yet — sub-plan 1 of the agent
//! platform is schema-only (`migrations/0031_agent_platform.sql`'s own header says so), so
//! there is no HTTP path that sets it. This test seeds it directly through the test database's
//! pool, which is honest about what is and is not wired today: the graph reads whatever is in
//! that column, regardless of how it got there.

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
    database: TestDatabase,
}

struct HttpResult {
    status: StatusCode,
    body: Value,
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
            database,
        })
    }

    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> HttpResult {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("x-request-id", format!("admin-graph-{}", Uuid::now_v7()));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
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

    /// Sets `agent_profiles.skill_refs` directly — see this file's header for why no HTTP path
    /// does this yet.
    async fn set_skill_refs(&self, agent_id: Uuid, skill_ids: &[Uuid]) {
        sqlx::query("update agent_profiles set skill_refs = $1 where id = $2")
            .bind(skill_ids)
            .bind(agent_id)
            .execute(&self.database.pool)
            .await
            .expect("seed skill_refs");
    }

    /// Sets `agent_profiles.memory_scope_refs` directly, same reason as `set_skill_refs`.
    async fn set_memory_scope_refs(&self, agent_id: Uuid, scopes: &[&str]) {
        sqlx::query("update agent_profiles set memory_scope_refs = $1 where id = $2")
            .bind(json!(scopes))
            .bind(agent_id)
            .execute(&self.database.pool)
            .await
            .expect("seed memory_scope_refs");
    }
}

fn id_of(result: &HttpResult) -> Uuid {
    Uuid::parse_str(result.body["id"].as_str().expect("resource id")).expect("UUID id")
}

fn node<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .find(|node| node["id"] == id)
}

fn has_edge(body: &Value, from: &str, to: &str, kind: &str) -> bool {
    body["edges"]
        .as_array()
        .expect("edges array")
        .iter()
        .any(|edge| edge["from"] == from && edge["to"] == to && edge["kind"] == kind)
}

#[tokio::test]
async fn graph_includes_a_seeded_agent_and_skill_and_the_edge_between_them() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    // Seed an agent profile.
    let agent_created = fixture
        .request(
            "POST",
            "/api/v1/admin/agent-profiles",
            Some(json!({
                "profile_key": format!("a-{}", fixture.suffix),
                "display_name": "Graph Test Agent",
            })),
        )
        .await;
    assert_eq!(
        agent_created.status,
        StatusCode::CREATED,
        "body: {}",
        agent_created.body
    );
    let agent_id = id_of(&agent_created);

    // Seed a skill.
    let skill_created = fixture
        .request(
            "POST",
            "/api/v1/admin/skills",
            Some(json!({
                "skill_key": format!("s-{}", fixture.suffix),
                "display_name": "Graph Test Skill",
                "kind": "tool",
            })),
        )
        .await;
    assert_eq!(
        skill_created.status,
        StatusCode::CREATED,
        "body: {}",
        skill_created.body
    );
    let skill_id = id_of(&skill_created);

    // Wire the agent to the skill (no admin endpoint does this yet — see the file header).
    fixture.set_skill_refs(agent_id, &[skill_id]).await;
    fixture
        .set_memory_scope_refs(agent_id, &["tenant_application"])
        .await;

    let graph = fixture.request("GET", "/api/v1/admin/graph", None).await;
    assert_eq!(graph.status, StatusCode::OK, "body: {}", graph.body);

    let agent_node_id = format!("agent:{agent_id}");
    let skill_node_id = format!("skill:{skill_id}");
    let memory_scope_node_id = "memory_scope:tenant_application";

    let agent_node = node(&graph.body, &agent_node_id).expect("agent node present");
    assert_eq!(agent_node["type"], "agent");
    assert_eq!(agent_node["label"], "Graph Test Agent");
    assert_eq!(agent_node["status"], "active");

    let skill_node = node(&graph.body, &skill_node_id).expect("skill node present");
    assert_eq!(skill_node["type"], "skill");
    assert_eq!(skill_node["label"], "Graph Test Skill");
    assert_eq!(skill_node["status"], "draft");

    let memory_scope_node =
        node(&graph.body, memory_scope_node_id).expect("memory_scope node present");
    assert_eq!(memory_scope_node["type"], "memory_scope");
    assert!(memory_scope_node["status"].is_null());

    assert!(
        has_edge(
            &graph.body,
            &agent_node_id,
            &skill_node_id,
            "agent_uses_skill"
        ),
        "missing agent_uses_skill edge in {}",
        graph.body
    );
    assert!(
        has_edge(
            &graph.body,
            &agent_node_id,
            memory_scope_node_id,
            "agent_reads_memory_scope"
        ),
        "missing agent_reads_memory_scope edge in {}",
        graph.body
    );
    assert!(
        graph.body["generated_at"].is_string(),
        "generated_at must be an RFC 3339 timestamp"
    );
}

#[tokio::test]
async fn graph_answers_200_with_empty_arrays_when_no_registries_have_any_rows() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    // A fresh database seeds none of the six registries the graph reads from. This pins that
    // every join and every registry read in `PgGraphRepository::fetch_graph_data` tolerates
    // "nothing here yet" as a normal answer, not a 500 — the failure mode a derived,
    // cross-cutting read is most likely to hit first.
    let graph = fixture.request("GET", "/api/v1/admin/graph", None).await;
    assert_eq!(graph.status, StatusCode::OK, "body: {}", graph.body);
    assert_eq!(
        graph.body["nodes"].as_array().expect("nodes array").len(),
        0
    );
    assert_eq!(
        graph.body["edges"].as_array().expect("edges array").len(),
        0
    );
}
