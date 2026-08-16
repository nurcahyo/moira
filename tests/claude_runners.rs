//! End-to-end coverage for the containerised Claude runner admin surface (issue #275,
//! workstream R2 of #272).
//!
//! The whole suite runs against a real `axum` fake of `moira-runner` on a real ephemeral port
//! (`tests/support/mock_runner_service.rs`) — never `wiremock`, and **never a Docker daemon**.
//! That is not a convenience: the production design's entire security claim is that Moira holds
//! no Docker access, so a test process that needed one would be exercising a different system.
//!
//! Four properties dominate:
//!
//! * **The minted token never leaves the Moira process.** Asserted against every response body,
//!   against the captured log stream, and against the stored rows — not described.
//! * **A failed finalize stores nothing.** No credential row, no state change.
//! * **Scopes are enforced in the application layer**, including the `moira:credentials:write`
//!   pre-check that stops an authorization mistake from destroying a one-shot token.
//! * **Illegal state transitions are refused** on both sides of the boundary under one code.

mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use support::{
    LifecycleFixture, install_log_capture,
    mock_runner_service::{
        MOCK_RUNNER_AUTH_TOKEN, MOCK_RUNNER_TOKEN, MockRunnerService, NeverContactedServer,
        RunnerScript,
    },
};
use tower::ServiceExt;
use uuid::Uuid;

struct HttpResult {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
    raw: String,
}

impl HttpResult {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    fn version(&self) -> i64 {
        self.etag
            .as_deref()
            .map(|value| value.trim_matches('"'))
            .and_then(|value| value.parse().ok())
            .expect("an ETag carrying the resource version")
    }

    fn id(&self) -> String {
        self.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id in {}", self.raw))
            .to_string()
    }
}

async fn send(router: Router, request: Request<Body>) -> HttpResult {
    let response = router.oneshot(request).await.expect("HTTP response");
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(raw.clone()))
    };
    HttpResult {
        status,
        body,
        etag,
        raw,
    }
}

async fn request(
    router: Router,
    method: &str,
    path: &str,
    headers: HeaderMap,
    if_match: Option<i64>,
    body: Option<Value>,
) -> HttpResult {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", format!("runners-{}", Uuid::now_v7()));
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    if let Some(version) = if_match {
        builder = builder.header("if-match", version.to_string());
    }
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    send(router, builder.body(body).unwrap()).await
}

fn system_key_headers(secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-moira-system-key",
        secret.parse().expect("system key header"),
    );
    headers
}

async fn mint_system_key(router: &Router, scopes: &[&str]) -> String {
    let created = request(
        router.clone(),
        "POST",
        "/api/v1/admin/system-keys",
        HeaderMap::new(),
        None,
        Some(json!({
            "display_name": format!("runners-{}", Uuid::now_v7()),
            "scopes": scopes
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    created.body["secret"]
        .as_str()
        .expect("the secret is returned exactly once, at creation")
        .to_string()
}

/// A fixture wired to a live fake runner service.
struct RunnerFixture {
    fixture: LifecycleFixture,
    runner: MockRunnerService,
    router: Router,
}

impl RunnerFixture {
    /// `None` when there is no test database, matching every other suite's skip contract.
    async fn start() -> Option<Self> {
        Self::start_with(|_| {}).await
    }

    async fn start_with(customize: impl FnOnce(&mut moira::config::Settings)) -> Option<Self> {
        let runner = MockRunnerService::start().await;
        let base_url = runner.base_url();
        let fixture = LifecycleFixture::with_settings(move |settings| {
            settings.claude_runner.enabled = true;
            settings.claude_runner.base_url = base_url;
            settings.claude_runner.auth_token = Some(MOCK_RUNNER_AUTH_TOKEN.to_string());
            settings.claude_runner.request_timeout_ms = 5_000;
            customize(settings);
        })
        .await?;
        let router = moira::build_router(fixture.state.clone()).expect("router");
        Some(Self {
            fixture,
            runner,
            router,
        })
    }

    async fn provision(&self, label: &str) -> HttpResult {
        self.provision_scoped(label, None).await
    }

    async fn provision_scoped(&self, label: &str, scope: Option<Value>) -> HttpResult {
        let mut body = json!({ "label": label, "ttl_seconds": 900 });
        if let Some(scope) = scope {
            body["scope"] = scope;
        }
        request(
            self.router.clone(),
            "POST",
            "/api/v1/admin/runners",
            HeaderMap::new(),
            None,
            Some(body),
        )
        .await
    }

    /// Drives a runner provisioned at `scope` all the way to `ready`.
    async fn ready_runner_scoped(&self, label: &str, scope: Option<Value>) -> (String, i64) {
        let created = self.provision_scoped(label, scope).await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.raw);
        let id = created.id();
        let reference = self.reference(&id).await;
        self.runner
            .set_script(
                &reference,
                RunnerScript {
                    state: "ready".to_string(),
                    ..RunnerScript::default()
                },
            )
            .await;
        let refreshed = self.get(&id).await;
        assert_eq!(refreshed.body["state"], json!("ready"), "{}", refreshed.raw);
        (id, refreshed.version())
    }

    async fn get(&self, id: &str) -> HttpResult {
        request(
            self.router.clone(),
            "GET",
            &format!("/api/v1/admin/runners/{id}"),
            HeaderMap::new(),
            None,
            None,
        )
        .await
    }

    /// The runner reference Moira recorded for `id`, read straight from the mirror table.
    async fn reference(&self, id: &str) -> String {
        sqlx::query_scalar("select runner_reference from claude_runners where id = $1")
            .bind(Uuid::parse_str(id).expect("uuid"))
            .fetch_one(&self.fixture.pool)
            .await
            .expect("read runner_reference")
    }

    /// Provisions a runner and drives the fake all the way to `ready`, returning `(id, version)`.
    async fn ready_runner(&self, label: &str) -> (String, i64) {
        let created = self.provision(label).await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.raw);
        let id = created.id();
        let reference = self.reference(&id).await;
        self.runner
            .set_script(
                &reference,
                RunnerScript {
                    state: "ready".to_string(),
                    ..RunnerScript::default()
                },
            )
            .await;
        let refreshed = self.get(&id).await;
        assert_eq!(refreshed.body["state"], json!("ready"), "{}", refreshed.raw);
        (id, refreshed.version())
    }

    async fn shutdown(self) {
        self.runner.shutdown().await;
    }
}

/// A provider row the finalized credential can bind to.
async fn create_provider(router: &Router) -> Uuid {
    let created = request(
        router.clone(),
        "POST",
        "/api/v1/admin/providers",
        HeaderMap::new(),
        None,
        Some(json!({
            "provider_type": "anthropic",
            "display_name": format!("claude-{}", Uuid::now_v7()),
            "metadata": {}
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.raw);
    Uuid::parse_str(created.body["id"].as_str().expect("provider id")).expect("uuid")
}

// ---------------------------------------------------------------------------------------------
// Provisioning and the mirror row
// ---------------------------------------------------------------------------------------------

/// **The version-1 assertion, and why it is not 2.**
///
/// `moira_bump_resource_version()` is a `before update` trigger. It never runs on an INSERT, so a
/// freshly created row sits at the column default of 1. A test written against the intuition that
/// "create counts as a write" would assert 2 and fail here — this assertion is what stops the
/// migration being "fixed" to satisfy that intuition.
#[tokio::test]
async fn provisioning_a_runner_writes_a_mirror_row_at_version_one() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let created = fixture.provision("claude-a").await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.raw);
    assert_eq!(created.body["state"], json!("provisioning"));
    assert_eq!(created.body["version"], json!(1));
    assert_eq!(created.version(), 1, "the ETag must carry the same version");
    assert_eq!(created.body["credential_id"], Value::Null);
    assert_eq!(fixture.runner.create_calls(), 1);
    assert_eq!(
        fixture.runner.unauthorized_calls(),
        0,
        "every runner call must carry the configured bearer token"
    );
    assert!(fixture.runner.authorized_calls() >= 1);

    let version: i64 = sqlx::query_scalar("select version from claude_runners where id = $1")
        .bind(Uuid::parse_str(&created.id()).expect("uuid"))
        .fetch_one(&fixture.fixture.pool)
        .await
        .expect("read version");
    assert_eq!(
        version, 1,
        "moira_bump_resource_version is `before update` only, so an INSERT lands at 1"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_label_outside_the_container_safe_charset_is_refused_before_any_runner_call() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    for label in ["Claude", "runner_1", "", "../etc"] {
        let refused = fixture.provision(label).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "label {label:?} -> {}",
            refused.raw
        );
        assert_eq!(refused.code(), "runner_label_invalid");
    }
    assert_eq!(
        fixture.runner.create_calls(),
        0,
        "a label Moira can refuse locally must never reach the runner service"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_ttl_outside_the_window_is_refused_rather_than_clamped() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let refused = request(
        fixture.router.clone(),
        "POST",
        "/api/v1/admin/runners",
        HeaderMap::new(),
        None,
        Some(json!({ "label": "claude-ttl", "ttl_seconds": 86_400 })),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.code(), "runner_ttl_invalid");
    assert_eq!(fixture.runner.create_calls(), 0);

    fixture.shutdown().await;
}

#[tokio::test]
async fn two_live_runners_cannot_share_a_label() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    assert_eq!(
        fixture.provision("claude-dup").await.status,
        StatusCode::CREATED
    );
    let duplicate = fixture.provision("claude-dup").await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT, "{}", duplicate.raw);
    assert_eq!(duplicate.code(), "duplicate_runner_label");

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Refresh, and the authorization URL
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_read_refreshes_the_mirror_and_surfaces_the_authorization_url() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let created = fixture.provision("claude-url").await;
    let id = created.id();
    assert_eq!(created.body["authorization_url"], Value::Null);

    let reference = fixture.reference(&id).await;
    fixture
        .runner
        .set_script(
            &reference,
            RunnerScript {
                state: "awaiting_authorization".to_string(),
                authorization_url: Some(
                    "https://claude.com/cai/oauth/authorize?code_challenge=abc&state=xyz"
                        .to_string(),
                ),
                ..RunnerScript::default()
            },
        )
        .await;

    let refreshed = fixture.get(&id).await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.raw);
    assert_eq!(refreshed.body["state"], json!("awaiting_authorization"));
    assert_eq!(
        refreshed.body["authorization_url"],
        json!("https://claude.com/cai/oauth/authorize?code_challenge=abc&state=xyz")
    );
    assert!(
        refreshed.version() > created.version(),
        "the refresh writes, so the ETag advances — which is exactly why the transitions carry \
         no If-Match"
    );

    fixture.shutdown().await;
}

/// A runner the control plane has reaped is end-of-life, not a missing resource.
#[tokio::test]
async fn a_reaped_runner_is_marked_expired_rather_than_disappearing() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let created = fixture.provision("claude-reaped").await;
    let id = created.id();
    let reference = fixture.reference(&id).await;
    // Point the mirror at a runner the fake has never heard of, which is what a reaped container
    // looks like from Moira's side: `GET /v1/runners/{id}` answers `404 runner_not_found`.
    sqlx::query("update claude_runners set runner_reference = $2 where id = $1")
        .bind(Uuid::parse_str(&id).expect("uuid"))
        .bind(format!("{reference}-missing"))
        .execute(&fixture.fixture.pool)
        .await
        .expect("point the row at a runner the fake does not know");

    let refreshed = fixture.get(&id).await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.raw);
    assert_eq!(refreshed.body["state"], json!("expired"));

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Illegal state transitions
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn submitting_an_authorization_code_before_the_runner_awaits_one_is_refused() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let created = fixture.provision("claude-early").await;
    let id = created.id();
    let refused = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/authorization-code"),
        HeaderMap::new(),
        None,
        Some(json!({ "code": "pasted-code" })),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.raw);
    assert_eq!(refused.code(), "runner_wrong_state");

    // Still `provisioning`: an illegal transition must leave the row exactly as it was.
    let state: String = sqlx::query_scalar("select state from claude_runners where id = $1")
        .bind(Uuid::parse_str(&id).expect("uuid"))
        .fetch_one(&fixture.fixture.pool)
        .await
        .expect("read state");
    assert_eq!(state, "provisioning");

    fixture.shutdown().await;
}

#[tokio::test]
async fn finalizing_a_runner_that_is_not_ready_is_refused_before_the_token_is_touched() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;

    let created = fixture.provision("claude-notready").await;
    let id = created.id();
    let reference = fixture.reference(&id).await;
    fixture
        .runner
        .set_script(
            &reference,
            RunnerScript {
                state: "awaiting_authorization".to_string(),
                ..RunnerScript::default()
            },
        )
        .await;

    let refused = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": provider_id })),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.raw);
    assert_eq!(refused.code(), "runner_wrong_state");
    assert_eq!(
        fixture.runner.token_calls(),
        0,
        "the token read is one-shot, so a speculative one is a token destroyed — the state check \
         must happen first"
    );

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// The happy path, and the token that must never escape
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_full_lifecycle_stores_the_token_as_a_credential_and_never_reveals_it() {
    let logs = install_log_capture();
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;

    // provision -> awaiting_authorization -> submit code -> ready -> finalize
    let created = fixture.provision("claude-happy").await;
    let id = created.id();
    let reference = fixture.reference(&id).await;
    fixture
        .runner
        .set_script(
            &reference,
            RunnerScript {
                state: "awaiting_authorization".to_string(),
                authorization_url: Some("https://claude.com/cai/oauth/authorize?x=1".to_string()),
                ..RunnerScript::default()
            },
        )
        .await;
    let awaiting = fixture.get(&id).await;
    assert_eq!(awaiting.body["state"], json!("awaiting_authorization"));

    let submitted = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/authorization-code"),
        HeaderMap::new(),
        None,
        Some(json!({ "code": "operator-pasted-code" })),
    )
    .await;
    assert_eq!(submitted.status, StatusCode::OK, "{}", submitted.raw);
    assert_eq!(submitted.body["state"], json!("exchanging"));

    fixture
        .runner
        .set_script(
            &reference,
            RunnerScript {
                state: "ready".to_string(),
                ..RunnerScript::default()
            },
        )
        .await;

    let finalized = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": provider_id })),
    )
    .await;
    assert_eq!(finalized.status, StatusCode::OK, "{}", finalized.raw);
    assert_eq!(finalized.body["state"], json!("linked"));
    let credential_id = finalized.body["credential_id"]
        .as_str()
        .expect("a linked runner names the credential its token became");

    // ---- the credential really exists, really encrypted, and does not hold the plaintext ----
    let (credential_type, masked, encrypted): (String, Option<String>, Vec<u8>) = sqlx::query_as(
        "select credential_type, masked_secret, encrypted_payload from provider_credentials \
         where id = $1",
    )
    .bind(Uuid::parse_str(credential_id).expect("uuid"))
    .fetch_one(&fixture.fixture.pool)
    .await
    .expect("read the stored credential");
    assert_eq!(credential_type, "oauth2");
    assert!(
        !String::from_utf8_lossy(&encrypted).contains(MOCK_RUNNER_TOKEN),
        "the stored payload must be ciphertext, not the token"
    );
    assert!(
        !masked.unwrap_or_default().contains(MOCK_RUNNER_TOKEN),
        "the mask must not reconstruct the token"
    );

    // ---- no response body anywhere on this surface carries it ----
    let listed = request(
        fixture.router.clone(),
        "GET",
        "/api/v1/admin/runners",
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    let read_back = fixture.get(&id).await;
    let credential = request(
        fixture.router.clone(),
        "GET",
        &format!("/api/v1/admin/provider-credentials/{credential_id}"),
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    let audit = request(
        fixture.router.clone(),
        "GET",
        "/api/v1/admin/audit-events",
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    for (label, response) in [
        ("provision", &created),
        ("get-awaiting", &awaiting),
        ("submit-code", &submitted),
        ("finalize", &finalized),
        ("list", &listed),
        ("get-linked", &read_back),
        ("credential", &credential),
        ("audit-events", &audit),
    ] {
        assert!(
            !response.raw.contains(MOCK_RUNNER_TOKEN),
            "{label} response leaked the runner token: {}",
            response.raw
        );
        // The operator's pasted authorization code is forwarded and dropped; it is not a token,
        // but it is single-use credential material and must not be echoed or audited either.
        assert!(
            !response.raw.contains("operator-pasted-code"),
            "{label} response leaked the authorization code: {}",
            response.raw
        );
    }

    // ---- and nothing Moira logged carries it ----
    let captured = logs.contents();
    assert!(
        !captured.contains(MOCK_RUNNER_TOKEN),
        "the runner token reached the log stream"
    );
    assert!(
        !captured.contains("operator-pasted-code"),
        "the authorization code reached the log stream"
    );

    // ---- nor does any column of either table ----
    let runner_row: String = sqlx::query_scalar("select to_jsonb(t)::text from claude_runners t")
        .fetch_one(&fixture.fixture.pool)
        .await
        .expect("dump the runner row");
    assert!(!runner_row.contains(MOCK_RUNNER_TOKEN));
    assert!(!runner_row.contains("operator-pasted-code"));
    let audit_rows: Vec<String> = sqlx::query_scalar("select to_jsonb(t)::text from audit_logs t")
        .fetch_all(&fixture.fixture.pool)
        .await
        .expect("dump the audit rows");
    for row in &audit_rows {
        assert!(
            !row.contains(MOCK_RUNNER_TOKEN),
            "audit row leaked the token"
        );
        assert!(
            !row.contains("operator-pasted-code"),
            "audit row leaked the authorization code"
        );
    }

    assert_eq!(
        fixture.runner.token_calls(),
        1,
        "the token read is one-shot"
    );

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// The failure path
// ---------------------------------------------------------------------------------------------

/// **The "stores nothing" property.**
///
/// The token is read — that is unavoidable, it is what finalize does — and the credential write
/// then fails because the named provider does not exist. Nothing may be written: no credential
/// row, no state change on the mirror. And because the runner service's token endpoint is
/// one-shot, the second attempt lands on `runner_token_unavailable`, which is the honest signal
/// that this runner is now a write-off.
#[tokio::test]
async fn a_failed_finalize_stores_nothing_and_the_token_becomes_unrecoverable() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let credentials_before: i64 = sqlx::query_scalar("select count(*) from provider_credentials")
        .fetch_one(&fixture.fixture.pool)
        .await
        .expect("count credentials");

    let (id, _version) = fixture.ready_runner("claude-doomed").await;
    let missing_provider = Uuid::now_v7();

    let failed = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": missing_provider })),
    )
    .await;
    assert!(
        failed.status.is_client_error(),
        "a finalize naming a provider that does not exist must fail: {}",
        failed.raw
    );

    let credentials_after: i64 = sqlx::query_scalar("select count(*) from provider_credentials")
        .fetch_one(&fixture.fixture.pool)
        .await
        .expect("count credentials");
    assert_eq!(
        credentials_before, credentials_after,
        "a failed finalize must store no credential row"
    );

    let (state, credential_id): (String, Option<Uuid>) =
        sqlx::query_as("select state, credential_id from claude_runners where id = $1")
            .bind(Uuid::parse_str(&id).expect("uuid"))
            .fetch_one(&fixture.fixture.pool)
            .await
            .expect("read the runner row");
    assert_eq!(
        state, "ready",
        "a failed finalize must not advance the state machine"
    );
    assert_eq!(credential_id, None);

    // The one-shot read already happened, so the runner can never yield its token again.
    let retried = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": create_provider(&fixture.router).await })),
    )
    .await;
    assert_eq!(retried.status, StatusCode::CONFLICT, "{}", retried.raw);
    assert_eq!(retried.code(), "runner_token_unavailable");
    assert!(!retried.raw.contains(MOCK_RUNNER_TOKEN));

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn read_scope_alone_cannot_provision_finalize_or_delete() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let reader = mint_system_key(&fixture.router, &["moira:runners:read"]).await;
    let (id, version) = fixture.ready_runner("claude-scopes").await;

    let listed = request(
        fixture.router.clone(),
        "GET",
        "/api/v1/admin/runners",
        system_key_headers(&reader),
        None,
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.raw);

    for (method, path, body) in [
        (
            "POST",
            "/api/v1/admin/runners".to_string(),
            Some(json!({ "label": "claude-nope", "ttl_seconds": 900 })),
        ),
        (
            "POST",
            format!("/api/v1/admin/runners/{id}/authorization-code"),
            Some(json!({ "code": "x" })),
        ),
        (
            "POST",
            format!("/api/v1/admin/runners/{id}/finalize"),
            Some(json!({ "provider_id": Uuid::now_v7() })),
        ),
    ] {
        let refused = request(
            fixture.router.clone(),
            method,
            &path,
            system_key_headers(&reader),
            None,
            body,
        )
        .await;
        assert_eq!(
            refused.status,
            StatusCode::FORBIDDEN,
            "{method} {path} -> {}",
            refused.raw
        );
    }

    let refused_delete = request(
        fixture.router.clone(),
        "DELETE",
        &format!("/api/v1/admin/runners/{id}"),
        system_key_headers(&reader),
        Some(version),
        None,
    )
    .await;
    assert_eq!(refused_delete.status, StatusCode::FORBIDDEN);
    assert_eq!(
        fixture.runner.token_calls(),
        0,
        "an authorization failure must never spend the one-shot token"
    );

    fixture.shutdown().await;
}

/// The documented split: runner scopes do not carry credential-write authority.
///
/// The check is made **before** the token is read, so a permissions mistake costs a `403` rather
/// than a destroyed runner.
#[tokio::test]
async fn runner_write_scope_alone_cannot_mint_a_credential() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;
    let runner_only = mint_system_key(
        &fixture.router,
        &[
            "moira:runners:read",
            "moira:runners:write",
            "moira:runners:delete",
        ],
    )
    .await;
    let (id, _version) = fixture.ready_runner("claude-nocreds").await;

    let refused = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        system_key_headers(&runner_only),
        None,
        Some(json!({ "provider_id": provider_id })),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.raw);
    assert_eq!(
        fixture.runner.token_calls(),
        0,
        "the credential-write check must run BEFORE the one-shot token read, or an \
         authorization mistake destroys a runner"
    );

    let state: String = sqlx::query_scalar("select state from claude_runners where id = $1")
        .bind(Uuid::parse_str(&id).expect("uuid"))
        .fetch_one(&fixture.fixture.pool)
        .await
        .expect("read state");
    assert_eq!(state, "ready");

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_runner_needs_if_match_and_leaves_its_credential_alone() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;
    let (id, _version) = fixture.ready_runner("claude-delete").await;

    let finalized = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": provider_id })),
    )
    .await;
    assert_eq!(finalized.status, StatusCode::OK, "{}", finalized.raw);
    let credential_id = Uuid::parse_str(finalized.body["credential_id"].as_str().expect("id"))
        .expect("credential uuid");

    let without_precondition = request(
        fixture.router.clone(),
        "DELETE",
        &format!("/api/v1/admin/runners/{id}"),
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    assert_eq!(without_precondition.status, StatusCode::BAD_REQUEST);
    assert_eq!(without_precondition.code(), "if_match_required");

    let stale = request(
        fixture.router.clone(),
        "DELETE",
        &format!("/api/v1/admin/runners/{id}"),
        HeaderMap::new(),
        Some(1),
        None,
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.raw);
    // **The regression this test exists for.** The version precondition has to be refused before
    // the container is destroyed, not after. It was checked only inside `soft_delete`'s
    // transaction, which runs after the control-plane teardown, so a stale ETag returned `409`
    // — "nothing happened" — while the runner's container had in fact already been removed. That
    // is the lost update `If-Match` exists to prevent, in its least recoverable form. Measured:
    // this counter read 2 for one successful delete.
    assert_eq!(
        fixture.runner.delete_calls(),
        0,
        "a stale If-Match must not reach the runner service: a 409 that has already destroyed \
         the container is not a refusal"
    );

    let deleted = request(
        fixture.router.clone(),
        "DELETE",
        &format!("/api/v1/admin/runners/{id}"),
        HeaderMap::new(),
        Some(finalized.version()),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.raw);
    assert_eq!(
        fixture.runner.delete_calls(),
        1,
        "exactly one teardown, from the one request that satisfied the precondition"
    );

    let live: i64 = sqlx::query_scalar(
        "select count(*) from provider_credentials where id = $1 and deleted_at is null",
    )
    .bind(credential_id)
    .fetch_one(&fixture.fixture.pool)
    .await
    .expect("count the credential");
    assert_eq!(
        live, 1,
        "deleting a runner must not take the credential routing may be executing against with it"
    );

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Tenant-owned runners
// ---------------------------------------------------------------------------------------------

/// A tenant that has connected its own subscribed Claude account.
///
/// Two things are proven here, and the second is the whole point of the feature:
///
/// 1. The runner carries the tenant scope on every read, so an operator can tell a tenant's runner
///    from the platform's without opening the database, and the credential finalize writes is
///    **sealed under that scope** — it is part of the AAD, so this cannot be corrected afterwards.
/// 2. With a `global` credential and a `tenant` credential both present on the same provider,
///    `resolve_runtime_credential` picks the tenant one for that tenant. That ordering
///    (`tenant` ranks 7, `global` 8) already existed; this asserts the runner path actually feeds
///    it, rather than assuming it does.
#[tokio::test]
async fn a_tenant_runners_credential_is_tenant_scoped_and_beats_the_platform_default() {
    use moira::{
        domain::CredentialType,
        infra::repositories::{PgRuntimeRepository, RuntimeRepository},
    };

    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;
    let tenant = format!("acme-{}", Uuid::now_v7().simple());

    // The platform-wide account: an ordinary global credential on the same provider.
    let platform = request(
        fixture.router.clone(),
        "POST",
        "/api/v1/admin/provider-credentials",
        HeaderMap::new(),
        None,
        Some(json!({
            "provider_id": provider_id,
            "credential_type": "oauth2",
            "scope": { "type": "global" },
            "secret": {
                "access_token": "platform-wide-account-token",
                "token_type": "Bearer"
            },
            "display_name": "platform default"
        })),
    )
    .await;
    assert_eq!(platform.status, StatusCode::CREATED, "{}", platform.raw);
    let platform_credential = Uuid::parse_str(platform.body["id"].as_str().expect("id")).unwrap();

    // The tenant's own subscription, connected through a runner.
    let (id, _version) = fixture
        .ready_runner_scoped(
            "claude-acme",
            Some(json!({ "type": "tenant", "external_tenant_id": tenant })),
        )
        .await;

    let read_back = fixture.get(&id).await;
    assert_eq!(
        read_back.body["scope"],
        json!({ "type": "tenant", "external_tenant_id": tenant }),
        "the runner must publish whose account it is for: {}",
        read_back.raw
    );

    let finalized = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": provider_id })),
    )
    .await;
    assert_eq!(finalized.status, StatusCode::OK, "{}", finalized.raw);
    let tenant_credential =
        Uuid::parse_str(finalized.body["credential_id"].as_str().expect("id")).unwrap();
    assert_ne!(tenant_credential, platform_credential);

    // Sealed at the tenant scope — read from the table, because this is what the AAD binds.
    let (scope_type, stored_tenant): (String, Option<String>) = sqlx::query_as(
        "select scope_type, external_tenant_id from provider_credentials where id = $1",
    )
    .bind(tenant_credential)
    .fetch_one(&fixture.fixture.pool)
    .await
    .expect("read the stored credential scope");
    assert_eq!(scope_type, "tenant");
    assert_eq!(stored_tenant.as_deref(), Some(tenant.as_str()));

    // ---- and the point of the whole feature ----
    let runtime = PgRuntimeRepository::new(fixture.fixture.pool.clone());
    let for_tenant = runtime
        .resolve_runtime_credential(
            provider_id,
            &[CredentialType::Oauth2],
            None,
            Some(&tenant),
            None,
            None,
        )
        .await
        .expect("resolve for the tenant")
        .expect("a credential resolves for the tenant");
    assert_eq!(
        for_tenant.record.id, tenant_credential,
        "a tenant that connected its own subscription must not execute on the platform's account"
    );

    // And a caller with no tenant still gets the platform default — the tenant credential must not
    // leak sideways into everyone else's traffic.
    let for_platform = runtime
        .resolve_runtime_credential(
            provider_id,
            &[CredentialType::Oauth2],
            None,
            None,
            None,
            None,
        )
        .await
        .expect("resolve with no tenant")
        .expect("the platform default still resolves");
    assert_eq!(for_platform.record.id, platform_credential);

    // A different tenant also falls back to the platform account.
    let other = runtime
        .resolve_runtime_credential(
            provider_id,
            &[CredentialType::Oauth2],
            None,
            Some("some-other-tenant"),
            None,
            None,
        )
        .await
        .expect("resolve for another tenant")
        .expect("the platform default still resolves");
    assert_eq!(other.record.id, platform_credential);

    assert!(!finalized.raw.contains(MOCK_RUNNER_TOKEN));

    fixture.shutdown().await;
}

/// The scope is fixed at provisioning time and finalize may not restate it.
///
/// `deny_unknown_fields` makes a client that still sends one fail loudly. Silently ignoring it
/// would be worse: the caller would believe it had chosen a scope, and the credential's AAD would
/// have been sealed under a different one.
#[tokio::test]
async fn a_scope_on_the_finalize_request_is_rejected_rather_than_ignored() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;
    let (id, _version) = fixture.ready_runner("claude-scopeonfinalize").await;

    let refused = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({
            "provider_id": provider_id,
            "scope": { "type": "tenant", "external_tenant_id": "acme" }
        })),
    )
    .await;
    assert!(
        refused.status.is_client_error(),
        "a scope on the finalize request must be refused, not dropped: {} {}",
        refused.status,
        refused.raw
    );
    assert_eq!(
        fixture.runner.token_calls(),
        0,
        "a malformed body must not spend the one-shot token"
    );

    fixture.shutdown().await;
}

/// A malformed tenant scope is refused before a container is started.
#[tokio::test]
async fn an_invalid_tenant_scope_is_refused_before_a_container_is_started() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };

    let refused = fixture
        .provision_scoped(
            "claude-badscope",
            Some(json!({ "type": "tenant", "external_tenant_id": "" })),
        )
        .await;
    assert!(
        refused.status.is_client_error(),
        "{} {}",
        refused.status,
        refused.raw
    );
    assert_eq!(
        fixture.runner.create_calls(),
        0,
        "a scope the credential table would refuse must be caught before a container exists — \
         otherwise the failure surfaces at finalize, after the one-shot token has been spent"
    );

    fixture.shutdown().await;
}

/// The default is the platform-wide account, unchanged for every deployment that never sets one.
#[tokio::test]
async fn a_runner_provisioned_without_a_scope_produces_a_global_credential() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let provider_id = create_provider(&fixture.router).await;
    let (id, _version) = fixture.ready_runner("claude-platform").await;

    let read_back = fixture.get(&id).await;
    assert_eq!(read_back.body["scope"], json!({ "type": "global" }));

    let finalized = request(
        fixture.router.clone(),
        "POST",
        &format!("/api/v1/admin/runners/{id}/finalize"),
        HeaderMap::new(),
        None,
        Some(json!({ "provider_id": provider_id })),
    )
    .await;
    assert_eq!(finalized.status, StatusCode::OK, "{}", finalized.raw);
    let scope_type: String =
        sqlx::query_scalar("select scope_type from provider_credentials where id = $1")
            .bind(Uuid::parse_str(finalized.body["credential_id"].as_str().expect("id")).unwrap())
            .fetch_one(&fixture.fixture.pool)
            .await
            .expect("read scope_type");
    assert_eq!(scope_type, "global");

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// The outbound client's hardening
// ---------------------------------------------------------------------------------------------

/// `redirect::Policy::none()`, proven rather than described.
///
/// Every runner call carries the bearer token for a service with Docker Engine API access. A
/// followed redirect would hand that token to a host the operator never named — and, per the
/// measurement `security::ssrf` records, the target's hit counter is what distinguishes a
/// refusal from a blind probe that happened not to be trusted. It must read zero.
#[tokio::test]
async fn the_runner_client_refuses_to_follow_a_redirect() {
    let Some(fixture) = RunnerFixture::start().await else {
        return;
    };
    let elsewhere = NeverContactedServer::start().await;
    fixture.runner.set_redirect_to(Some(elsewhere.url())).await;

    let refused = fixture.provision("claude-redirect").await;
    assert_eq!(
        refused.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        refused.raw
    );
    assert_eq!(refused.code(), "runner_service_unavailable");
    assert_eq!(
        elsewhere.hits(),
        0,
        "the redirect target was contacted; redirect::Policy::none() is not in force and the \
         runner bearer token left the endpoint the operator configured"
    );
    assert!(!refused.raw.contains(MOCK_RUNNER_TOKEN));

    elsewhere.shutdown().await;
    fixture.shutdown().await;
}

/// A bad bearer token is a configuration problem with its own code.
#[tokio::test]
async fn a_rejected_bearer_token_is_reported_as_a_configuration_failure() {
    let Some(fixture) = RunnerFixture::start_with(|settings| {
        settings.claude_runner.auth_token = Some("not-the-configured-token".to_string());
    })
    .await
    else {
        return;
    };

    let refused = fixture.provision("claude-badtoken").await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.code(), "runner_service_unauthorized");
    assert_eq!(fixture.runner.unauthorized_calls(), 1);

    fixture.shutdown().await;
}

/// A deployment with no runner service refuses before any network call.
#[tokio::test]
async fn every_route_answers_one_named_503_when_the_runner_service_is_disabled() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    assert!(
        !fixture.state.settings.claude_runner.enabled,
        "the shipped default must be off"
    );

    let id = Uuid::now_v7();
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/admin/runners".to_string(),
            Some(json!({ "label": "claude-off", "ttl_seconds": 900 })),
        ),
        (
            "POST",
            format!("/api/v1/admin/runners/{id}/authorization-code"),
            Some(json!({ "code": "x" })),
        ),
        (
            "POST",
            format!("/api/v1/admin/runners/{id}/finalize"),
            Some(json!({ "provider_id": Uuid::now_v7() })),
        ),
    ] {
        let refused = request(router.clone(), method, &path, HeaderMap::new(), None, body).await;
        // `authorization-code` and `finalize` load the row first, so a runner that does not exist
        // answers 404 before the disabled check is reached; the write path that does not need a
        // row is the one that proves the refusal.
        assert!(
            matches!(
                refused.status,
                StatusCode::SERVICE_UNAVAILABLE | StatusCode::NOT_FOUND
            ),
            "{method} {path} -> {} {}",
            refused.status,
            refused.raw
        );
        if refused.status == StatusCode::SERVICE_UNAVAILABLE {
            assert_eq!(refused.code(), "runner_service_disabled");
        }
    }

    // The read side stays available: a mirror is still readable with no runner service to poll.
    let listed = request(
        router,
        "GET",
        "/api/v1/admin/runners",
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.raw);
}
