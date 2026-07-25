//! Per-resource-family proof that `If-Match` is enforced atomically, plus the three
//! precondition outcomes the wire contract pins.
//!
//! `tests/if_match_toctou_harness.rs` proves the defect is closed for one site per owning
//! service — `applications` through `AdminService`/`PgAdminRepository`, and `route_definitions`
//! through `RuntimeAdminService`/`PgRuntimeRepository`. That is enough to show the *mechanism*
//! works, and not enough to show it was applied everywhere: each of the eight versioned admin
//! families is a separate table with its own `UPDATE` statement, and the fix is per-statement.
//! A family whose `and version = $N` predicate was forgotten, or whose `select … for update`
//! was written without the lock, would leave the harness green.
//!
//! So this file covers the remaining six families:
//!
//! | family | table | raced field |
//! |---|---|---|
//! | providers | `providers` | `display_name` |
//! | provider models | `provider_models` | `display_name` |
//! | provider credentials | `provider_credentials` | `display_name` |
//! | trusted JWT issuers | `trusted_jwt_issuers` | `subject_claim` |
//! | routing policies | `routing_policies` | `priority` |
//! | agent profiles | `agent_profiles` | `display_name` |
//!
//! Two of them have no `display_name` to race on, which is why the assertion below is keyed on
//! a caller-supplied field rather than hard-coding one.
//!
//! # The 404-vs-409 pin
//!
//! The single most likely way to get this fix wrong is to append `and version = $N` to the
//! existing `UPDATE` and stop there. Every one of these statements already mapped its zero-row
//! result to `AppError::NotFound`, so the shortcut turns a stale `If-Match` into a **404** —
//! a silent wire-contract change that also tells an unauthorised-to-know caller "no such row"
//! about a row that exists. `if_match_precondition_outcomes_are_unchanged_per_family` pins all
//! three outcomes on all eight families so that regression cannot land quietly.
//!
//! No `sleep()` anywhere: the race window is opened by [`tokio::sync::Barrier`], per
//! `plans/CONVENTIONS.md` §3. Each fixture owns a private migrated database.

mod support;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode},
};
use serde_json::{Value, json};
use tokio::{sync::Barrier, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;

use support::LifecycleFixture;

const WAIT: Duration = Duration::from_secs(15);

struct HttpResult {
    status: StatusCode,
    body: Value,
}

impl HttpResult {
    fn version(&self) -> i64 {
        self.body["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("response carries no numeric `version`: {}", self.body))
    }

    fn field(&self, name: &str) -> &Value {
        &self.body[name]
    }
}

struct Fixture {
    inner: LifecycleFixture,
    router: Router,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let inner = LifecycleFixture::new().await?;
        let router = moira::build_router(inner.state.clone()).expect("build Moira test router");
        Some(Self { inner, router })
    }

    async fn send(&self, request: Request<Body>) -> HttpResult {
        let response = timeout(WAIT, self.router.clone().oneshot(request))
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
        read_result(response).await
    }

    async fn get(&self, path: &str) -> HttpResult {
        self.send(build_request("GET", path, None, None)).await
    }

    async fn post(&self, path: &str, body: Value) -> HttpResult {
        let result = self
            .send(build_request("POST", path, None, Some(body)))
            .await;
        assert_eq!(
            result.status,
            StatusCode::CREATED,
            "fixture setup POST {path} failed: {}",
            result.body
        );
        result
    }

    /// Creates one row of every versioned family and returns its collection-relative path.
    /// Providers, models and credentials are chained because each depends on the previous.
    async fn create_all_families(&self) -> Vec<Family> {
        let suffix = Uuid::now_v7().simple().to_string();

        let provider = self
            .post(
                "/api/v1/admin/providers",
                json!({
                    "provider_type": "open_ai_compatible",
                    "display_name": format!("if-match provider {suffix}"),
                    "base_url": "https://provider.example.com/v1",
                    "metadata": { "test_fixture": true },
                }),
            )
            .await;
        let provider_id = id_of(&provider);

        let model = self
            .post(
                &format!("/api/v1/admin/providers/{provider_id}/models"),
                json!({
                    "model_key": format!("model-{suffix}"),
                    "display_name": "if-match model",
                    "capabilities": { "streaming": true },
                }),
            )
            .await;
        let model_id = id_of(&model);

        let credential = self
            .post(
                "/api/v1/admin/provider-credentials",
                json!({
                    "provider_id": provider_id,
                    "credential_type": "api_key",
                    "scope": { "type": "global" },
                    "secret": { "type": "api_key", "api_key": "sk-if-match-secret" },
                    "display_name": "if-match credential",
                    "priority": 100,
                    "metadata": { "test_fixture": true },
                }),
            )
            .await;

        let issuer = self
            .post(
                "/api/v1/admin/jwt-issuers",
                json!({
                    "issuer": format!("https://issuer-{suffix}.example.com/"),
                    "jwks_url": format!("https://issuer-{suffix}.example.com/.well-known/jwks.json"),
                }),
            )
            .await;

        let routing_policy = self
            .post(
                "/api/v1/admin/routing-policies",
                json!({
                    "application_id": self.inner.application_id,
                    "route_id": self.inner.route_id,
                    "provider_id": provider_id,
                    "provider_model_id": model_id,
                    "priority": 50,
                    "metadata": { "test_fixture": true },
                }),
            )
            .await;

        let agent_profile = self
            .post(
                "/api/v1/admin/agent-profiles",
                json!({
                    "profile_key": format!("profile_{suffix}"),
                    "display_name": format!("if-match profile {suffix}"),
                    "metadata": { "test_fixture": true },
                }),
            )
            .await;

        vec![
            Family::new(
                "applications",
                format!("/api/v1/admin/applications/{}", self.inner.application_id),
                "display_name",
                ["races-application-a", "races-application-b"],
            ),
            Family::new(
                "route_definitions",
                format!("/api/v1/admin/routes/{}", self.inner.route_id),
                "display_name",
                ["races-route-a", "races-route-b"],
            ),
            Family::new(
                "providers",
                format!("/api/v1/admin/providers/{provider_id}"),
                "display_name",
                ["races-provider-a", "races-provider-b"],
            ),
            Family::new(
                "provider_models",
                format!("/api/v1/admin/provider-models/{model_id}"),
                "display_name",
                ["races-model-a", "races-model-b"],
            ),
            Family::new(
                "provider_credentials",
                format!("/api/v1/admin/provider-credentials/{}", id_of(&credential)),
                "display_name",
                ["races-credential-a", "races-credential-b"],
            ),
            Family::new(
                "trusted_jwt_issuers",
                format!("/api/v1/admin/jwt-issuers/{}", id_of(&issuer)),
                "subject_claim",
                ["races_issuer_a", "races_issuer_b"],
            ),
            Family::new(
                "routing_policies",
                format!("/api/v1/admin/routing-policies/{}", id_of(&routing_policy)),
                "priority",
                [json!(61), json!(62)],
            ),
            Family::new(
                "agent_profiles",
                format!("/api/v1/admin/agent-profiles/{}", id_of(&agent_profile)),
                "display_name",
                ["races-profile-a", "races-profile-b"],
            ),
        ]
    }
}

/// One versioned admin resource, plus the field two racing writers will disagree about.
///
/// `trusted_jwt_issuers` and `routing_policies` carry no `display_name`, so the raced field is
/// per-family rather than fixed — an assertion hard-coded to `display_name` would have had to
/// skip exactly the two families whose `UPDATE` statements are the least like the others.
struct Family {
    table: &'static str,
    path: String,
    field: &'static str,
    values: [Value; 2],
}

impl Family {
    fn new<T: Into<Value>>(
        table: &'static str,
        path: String,
        field: &'static str,
        values: [T; 2],
    ) -> Self {
        let [first, second] = values;
        Self {
            table,
            path,
            field,
            values: [first.into(), second.into()],
        }
    }
}

fn id_of(result: &HttpResult) -> Uuid {
    result.body["id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(|| panic!("created resource carries no `id`: {}", result.body))
}

/// Builds a request without sending it, so all setup happens *before* the barrier releases and
/// only the HTTP call sits inside the raced window.
fn build_request(
    method: &str,
    path: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", format!("if-match-{}", Uuid::now_v7()));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(value) = if_match {
        builder = builder.header("if-match", value);
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("HTTP request")
}

async fn read_result(response: Response<Body>) -> HttpResult {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            // Non-JSON bodies are Axum's own plain-text rejections (bad route, unparseable
            // payload). Surfacing the text beats a serde error that names neither.
            Value::String(String::from_utf8_lossy(&bytes).into_owned())
        })
    };
    HttpResult { status, body }
}

/// Releases two `PATCH`es holding the **same, currently valid** `If-Match`, and asserts that
/// exactly one wins, the version advances exactly once, and the row that survives is the
/// winner's — not a blend, and not the loser's values written on top.
async fn assert_one_writer_wins(fixture: &Fixture, family: &Family) {
    let before = fixture.get(&family.path).await;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "{}: {}",
        family.table,
        before.body
    );
    let version = before.version();

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::with_capacity(2);
    for value in family.values.clone() {
        let router = fixture.router.clone();
        let request = build_request(
            "PATCH",
            &family.path,
            Some(&version.to_string()),
            Some(json!({ family.field: value })),
        );
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let response = timeout(WAIT, router.oneshot(request))
                .await
                .expect("concurrent HTTP request timed out")
                .expect("concurrent HTTP response");
            (value, read_result(response).await)
        }));
    }

    let mut results = Vec::with_capacity(2);
    for handle in handles {
        results.push(handle.await.expect("concurrent writer task"));
    }

    let observed: Vec<_> = results
        .iter()
        .map(|(value, result)| {
            format!(
                "{}={value} status={} body={}",
                family.field, result.status, result.body
            )
        })
        .collect();

    let successes: Vec<_> = results
        .iter()
        .filter(|(_, result)| result.status == StatusCode::OK)
        .collect();
    let conflicts: Vec<_> = results
        .iter()
        .filter(|(_, result)| result.status == StatusCode::CONFLICT)
        .collect();

    assert_eq!(
        (successes.len(), conflicts.len()),
        (1, 1),
        "{}: two writers holding the same valid If-Match must resolve to exactly one 200 and \
         one 409 `resource_version_conflict`. Two successes means this family's UPDATE runs \
         without the version predicate, or without the row lock that makes it decisive, and one \
         writer's update was silently lost. Observed: {observed:#?}",
        family.table
    );
    assert_eq!(
        conflicts[0].1.body["error"]["code"], "resource_version_conflict",
        "{}: the loser must be the ordinary versioned-conflict envelope: {}",
        family.table, conflicts[0].1.body
    );

    let (winning_value, winner) = successes[0];
    assert_eq!(
        winner.version(),
        version + 1,
        "{}: the single winning write must advance the version exactly once: {}",
        family.table,
        winner.body
    );

    let settled = fixture.get(&family.path).await;
    assert_eq!(
        settled.version(),
        version + 1,
        "{}: a refused write must not have advanced the version a second time: {}",
        family.table,
        settled.body
    );
    assert_eq!(
        settled.field(family.field),
        winning_value,
        "{}: the persisted row must be the one the successful writer sent: {}",
        family.table,
        settled.body
    );
}

macro_rules! family_race_test {
    ($name:ident, $table:literal) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            let Some(fixture) = Fixture::new().await else {
                return;
            };
            let families = fixture.create_all_families().await;
            let family = families
                .iter()
                .find(|family| family.table == $table)
                .expect(concat!(
                    "family ",
                    $table,
                    " is set up by create_all_families"
                ));
            assert_one_writer_wins(&fixture, family).await;
        }
    };
}

family_race_test!(
    concurrent_provider_patches_yield_one_success_and_one_409,
    "providers"
);
family_race_test!(
    concurrent_provider_model_patches_yield_one_success_and_one_409,
    "provider_models"
);
family_race_test!(
    concurrent_credential_patches_yield_one_success_and_one_409,
    "provider_credentials"
);
family_race_test!(
    concurrent_trusted_jwt_issuer_patches_yield_one_success_and_one_409,
    "trusted_jwt_issuers"
);
family_race_test!(
    concurrent_routing_policy_patches_yield_one_success_and_one_409,
    "routing_policies"
);
family_race_test!(
    concurrent_agent_profile_patches_yield_one_success_and_one_409,
    "agent_profiles"
);

/// The wire contract, pinned on every family at once.
///
/// Moving the comparison from the handler into the repository transaction changed *where* the
/// check happens and nothing a client can observe. Specifically it must not have collapsed the
/// stale-`If-Match` case onto the `UPDATE`'s own zero-row branch, which is `404`. All three
/// outcomes are asserted per family so a single regressed statement is named in the failure
/// rather than hidden behind a suite-level red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn if_match_precondition_outcomes_are_unchanged_per_family() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let families = fixture.create_all_families().await;
    assert_eq!(families.len(), 8, "every versioned admin family is covered");

    for family in &families {
        let current = fixture.get(&family.path).await;
        let version = current.version();
        let patch = json!({ family.field: family.values[0].clone() });

        let missing = fixture
            .send(build_request(
                "PATCH",
                &family.path,
                None,
                Some(patch.clone()),
            ))
            .await;
        assert_eq!(
            missing.status,
            StatusCode::BAD_REQUEST,
            "{}: a missing If-Match is still a 400: {}",
            family.table,
            missing.body
        );
        assert_eq!(
            missing.body["error"]["code"], "if_match_required",
            "{}: {}",
            family.table, missing.body
        );

        let stale = fixture
            .send(build_request(
                "PATCH",
                &family.path,
                Some(&(version - 1).to_string()),
                Some(patch.clone()),
            ))
            .await;
        assert_eq!(
            stale.status,
            StatusCode::CONFLICT,
            "{}: a stale If-Match must be a 409, NOT the 404 that appending `and version = $N` \
             to the existing UPDATE would produce: {}",
            family.table,
            stale.body
        );
        assert_eq!(
            stale.body["error"]["code"], "resource_version_conflict",
            "{}: {}",
            family.table, stale.body
        );

        // A row that genuinely does not exist stays a 404 even with a well-formed If-Match:
        // the version check must not be reachable before the row is found, or the API would
        // confirm the existence of rows it is meant to deny knowledge of.
        let absent_path = replace_trailing_id(&family.path, Uuid::now_v7());
        let absent = fixture
            .send(build_request(
                "PATCH",
                &absent_path,
                Some(&version.to_string()),
                Some(patch),
            ))
            .await;
        assert_eq!(
            absent.status,
            StatusCode::NOT_FOUND,
            "{}: an absent row is still a 404, not a version conflict: {}",
            family.table,
            absent.body
        );

        // Left unmodified: the three calls above were all refused, so the version the next
        // family's assertions read is still the one this one started with.
        let after = fixture.get(&family.path).await;
        assert_eq!(
            after.version(),
            version,
            "{}: three refused preconditions must not have written anything: {}",
            family.table,
            after.body
        );
    }
}

fn replace_trailing_id(path: &str, id: Uuid) -> String {
    let (prefix, _) = path
        .rsplit_once('/')
        .expect("resource paths end in `/{id}`");
    format!("{prefix}/{id}")
}
