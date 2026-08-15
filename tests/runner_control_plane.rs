//! End-to-end coverage of `moira-runner`'s HTTP surface — issue #273, workstream R1 of #272.
//!
//! # What "end to end" means here, and what it deliberately does not
//!
//! Every assertion below goes over a real socket: the runner's own `axum::Router` is served on
//! a `TcpListener::bind("127.0.0.1:0")` and driven with `reqwest`, following the pattern
//! `tests/support/mod.rs` and `tests/jwks_hardening.rs` already use. **No `wiremock`** — this
//! repository spins up real routers instead, and adding a mock-HTTP dev-dependency for one
//! suite would be a new pattern for no gain.
//!
//! What is *not* real is the Docker daemon. Every test here runs against
//! `moira::runner::engine::InMemoryEngine`, because CI has no `claude` image and pulling one
//! would be prohibitive. That is a deliberate and stated limit: this suite proves the control
//! plane — routing, authentication, the state machine, the one-shot handoff, the error
//! envelope — and proves nothing at all about `bollard` talking to a real daemon. The
//! opt-in suite `tests/runner_docker_engine.rs` covers that half, and only when a human asks
//! for it.
//!
//! This suite needs **no PostgreSQL**, so unlike most files under `tests/` it has no database
//! fixture and no skip path: it either runs or it fails.
//!
//! # Concurrency discipline (`plans/CONVENTIONS.md` §3)
//!
//! No `sleep()` anywhere. The one race that matters — two callers fetching the same one-shot
//! token — is gated with a `tokio::sync::Barrier`, which releases every racer on an
//! acknowledgement rather than on a guess about timing.

use std::sync::Arc;

use moira::runner::{
    config::RunnerConfig,
    engine::InMemoryEngine,
    http,
    service::RunnerService,
    state::{CONSUMED_SUFFIX, LABEL_EXPIRES_AT, LABEL_ID, LABEL_MARKER},
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A control token that satisfies every startup rule in `RunnerConfig::validate`.
const CONTROL_TOKEN: &str = "unit-fixture-control-aaaaaaaaaaaaaaaa";
const IMAGE: &str = "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7f8";

/// The URL alone — emitted before the CLI's reader exists. A runner here is `provisioning`.
const URL_ONLY_FRAME: &str = "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c\r\n";

/// The paste prompt as Ink actually lays it out: positioned with cursor-forward sequences
/// rather than spaces, so it strips to `Pastecodehereifprompted>` with no whitespace at all.
const PROMPT_LINE: &str =
    "\u{1b}[2GPaste\u{1b}[8Gcode\u{1b}[13Ghere\u{1b}[18Gif\u{1b}[21Gprompted\u{1b}[30G>\r\r\n";

/// URL **and** prompt — the only combination that means `awaiting_authorization`, because
/// that state is what licenses a write into the container's stdin.
fn awaiting_frame() -> String {
    format!("{URL_ONLY_FRAME}{PROMPT_LINE}")
}

const TOKEN_VALUE: &str = "sk-ant-oat01-fake-aaaaaaaaaaaaaaaaaaaa";

struct RunnerFixture {
    base_url: String,
    client: Client,
    engine: Arc<InMemoryEngine>,
    service: Arc<RunnerService<InMemoryEngine>>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl RunnerFixture {
    async fn start(overrides: &[(&str, &str)]) -> Self {
        let mut pairs: Vec<(String, String)> = vec![
            (
                "MOIRA_RUNNER__CONTROL_TOKEN".to_string(),
                CONTROL_TOKEN.to_string(),
            ),
            ("MOIRA_RUNNER__IMAGE".to_string(), IMAGE.to_string()),
        ];
        pairs.extend(
            overrides
                .iter()
                .map(|(key, value)| (format!("MOIRA_RUNNER__{key}"), (*value).to_string())),
        );
        let map: std::collections::HashMap<String, String> = pairs.into_iter().collect();
        // Read from a map rather than from the process environment: `cargo test` runs these
        // on threads of one process, so `set_var` would have them reconfiguring each other.
        let config = Arc::new(
            RunnerConfig::from_source(&move |key: &str| map.get(key).cloned())
                .expect("test configuration is valid"),
        );

        let engine = Arc::new(InMemoryEngine::new());
        let service = Arc::new(RunnerService::new(Arc::clone(&engine), config));

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the runner test server");
        let address = listener.local_addr().expect("runner test server address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let app = http::router(Arc::clone(&service));
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve the runner test router");
        });

        Self {
            base_url: format!("http://{address}"),
            client: Client::new(),
            engine,
            service,
            shutdown,
            task,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let response = self
            .client
            .get(self.url(path))
            .bearer_auth(CONTROL_TOKEN)
            .send()
            .await
            .expect("GET");
        Self::split(response).await
    }

    async fn post(
        &self,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let response = self
            .client
            .post(self.url(path))
            .bearer_auth(CONTROL_TOKEN)
            .json(&body)
            .send()
            .await
            .expect("POST");
        Self::split(response).await
    }

    async fn delete(&self, path: &str) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let response = self
            .client
            .delete(self.url(path))
            .bearer_auth(CONTROL_TOKEN)
            .send()
            .await
            .expect("DELETE");
        Self::split(response).await
    }

    async fn split(response: reqwest::Response) -> (StatusCode, Value, reqwest::header::HeaderMap) {
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.expect("body");
        let value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, value, headers)
    }

    /// Creates a runner and returns `(runner id, container id)`.
    async fn create(&self, label: &str) -> (Uuid, String) {
        let (status, body, _) = self.post("/v1/runners", json!({ "label": label })).await;
        assert_eq!(status, StatusCode::CREATED, "unexpected body: {body}");
        let id: Uuid = body["id"].as_str().expect("id").parse().expect("uuid");
        let container_id = self
            .service
            .view(id)
            .await
            .expect("the new runner is visible")
            .container_id;
        (id, container_id)
    }

    /// Drives a runner to `ready` by scripting its tty stream.
    async fn make_ready(&self, id: Uuid, container_id: &str) {
        self.engine.set_transcript(container_id, &awaiting_frame());
        let (status, body, _) = self
            .post(
                &format!("/v1/runners/{id}/authorization-code"),
                json!({ "code": "abc123#state" }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "unexpected body: {body}");
        self.engine.set_transcript(
            container_id,
            &format!("{}{TOKEN_VALUE}\r\n", awaiting_frame()),
        );
        self.engine.set_running(container_id, false);
    }

    async fn shutdown(self) {
        self.shutdown.cancel();
        self.task
            .await
            .expect("the runner test server task panicked");
    }
}

// ---------------------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn every_route_except_healthz_requires_the_control_token() {
    let fixture = RunnerFixture::start(&[]).await;
    let id = Uuid::now_v7();

    let unauthenticated: Vec<(reqwest::Method, String)> = vec![
        (reqwest::Method::POST, "/v1/runners".to_string()),
        (reqwest::Method::GET, format!("/v1/runners/{id}")),
        (reqwest::Method::DELETE, format!("/v1/runners/{id}")),
        (
            reqwest::Method::POST,
            format!("/v1/runners/{id}/authorization-code"),
        ),
        (reqwest::Method::GET, format!("/v1/runners/{id}/token")),
    ];

    for (method, path) in unauthenticated {
        let response = fixture
            .client
            .request(method.clone(), fixture.url(&path))
            .json(&json!({ "label": "demo", "code": "abc" }))
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} answered without a token"
        );
        let body: Value = response.json().await.expect("error envelope");
        assert_eq!(body["error"]["code"], "unauthorized");
    }

    // `/healthz` is the deliberate exception: a health probe that needs a credential is a
    // health probe that gets disabled.
    let health = fixture
        .client
        .get(fixture.url("/healthz"))
        .send()
        .await
        .expect("healthz");
    assert_eq!(health.status(), StatusCode::OK);

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_wrong_or_malformed_token_is_rejected_with_the_same_answer_as_a_missing_one() {
    let fixture = RunnerFixture::start(&[]).await;

    let variants = [
        // Wrong value, right length — the case a timing attack would exploit.
        "unit-fixture-control-aaaaaaaaaaaaaaab",
        // A prefix of the real token: must not match.
        "unit-fixture-control-",
        "",
    ];
    for wrong in variants {
        let response = fixture
            .client
            .get(fixture.url("/v1/runners/00000000-0000-0000-0000-000000000000"))
            .header("authorization", format!("Bearer {wrong}"))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body: Value = response.json().await.expect("envelope");
        assert_eq!(body["error"]["message"], "a valid bearer token is required");
    }

    // A non-Bearer scheme, and a header with no scheme at all.
    for header in [format!("Basic {CONTROL_TOKEN}"), CONTROL_TOKEN.to_string()] {
        let response = fixture
            .client
            .get(fixture.url("/v1/runners/00000000-0000-0000-0000-000000000000"))
            .header("authorization", header)
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // Lowercase `bearer` must work: RFC 7235 makes the scheme token case-insensitive.
    let (status, _, _) = RunnerFixture::split(
        fixture
            .client
            .get(fixture.url("/v1/runners/00000000-0000-0000-0000-000000000000"))
            .header("authorization", format!("bearer {CONTROL_TOKEN}"))
            .send()
            .await
            .expect("request"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an authenticated request for a missing runner is 404, not 401"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn every_response_carries_cache_control_no_store() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;
    fixture.make_ready(id, &container_id).await;

    let probes = vec![
        fixture.get("/healthz").await,
        fixture.get(&format!("/v1/runners/{id}")).await,
        fixture.get(&format!("/v1/runners/{id}/token")).await,
        // An error response too: the header must not be attached only on the happy path.
        fixture
            .get("/v1/runners/00000000-0000-0000-0000-000000000000")
            .await,
    ];

    for (status, _, headers) in probes {
        assert_eq!(
            headers
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "a {status} response was missing cache-control: no-store"
        );
    }

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// The happy path and the state machine
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_full_lifecycle_runs_provisioning_to_ready_to_consumed() {
    let fixture = RunnerFixture::start(&[]).await;

    // create -> 201 provisioning
    let (status, body, _) = fixture
        .post(
            "/v1/runners",
            json!({ "label": "seat-01", "ttl_seconds": 600 }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["state"], "provisioning");
    let id: Uuid = body["id"].as_str().expect("id").parse().expect("uuid");
    assert!(
        body["expires_at"]
            .as_str()
            .expect("expires_at")
            .ends_with('Z')
    );
    let container_id = fixture.service.view(id).await.expect("view").container_id;

    // The container carries exactly the three contract labels.
    let spec = fixture
        .engine
        .created_specs()
        .into_iter()
        .next()
        .expect("one container was created");
    assert_eq!(spec.labels.get(LABEL_MARKER).map(String::as_str), Some("1"));
    assert_eq!(
        spec.labels.get(LABEL_ID).map(String::as_str),
        Some(id.to_string().as_str())
    );
    assert!(spec.labels.contains_key(LABEL_EXPIRES_AT));
    assert_eq!(spec.labels.len(), 3);
    assert_eq!(spec.image, IMAGE);

    // still provisioning, no URL yet
    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "provisioning");
    assert_eq!(body["authorization_url"], Value::Null);
    assert_eq!(body["error_code"], Value::Null);

    // the CLI prints the URL but has not reached its prompt -> the URL is surfaced, but the
    // runner is not yet writable, because there is no reader on the other end of stdin
    fixture.engine.set_transcript(&container_id, URL_ONLY_FRAME);
    let (_, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;
    assert_eq!(body["state"], "provisioning");
    assert_eq!(
        body["authorization_url"],
        "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c"
    );

    // the prompt renders -> awaiting_authorization
    fixture
        .engine
        .set_transcript(&container_id, &awaiting_frame());
    let (_, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;
    assert_eq!(body["state"], "awaiting_authorization");
    assert_eq!(
        body["authorization_url"],
        "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c"
    );

    // paste the code -> 202 exchanging, and the exact bytes reach the prompt
    let (status, body, _) = fixture
        .post(
            &format!("/v1/runners/{id}/authorization-code"),
            json!({ "code": "abc123#state-xyz" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["state"], "exchanging");
    assert_eq!(
        fixture.engine.stdin_writes(&container_id),
        b"abc123#state-xyz\r".to_vec(),
        "the trailing carriage return is what submits the line to a terminal read"
    );

    // the token appears -> ready
    fixture.engine.set_transcript(
        &container_id,
        &format!("{}{TOKEN_VALUE}\r\n", awaiting_frame()),
    );
    fixture.engine.set_running(&container_id, false);
    let (_, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;
    assert_eq!(body["state"], "ready");

    // take the token -> 200 once
    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}/token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["token"], TOKEN_VALUE);

    // the consumed marker is durable, on the container itself
    assert!(
        fixture
            .engine
            .container_names()
            .iter()
            .any(|name| name.ends_with(CONSUMED_SUFFIX)),
        "the one-shot marker must live in Docker, not only in this process"
    );

    // delete -> 204, and again -> 204
    let (status, _, _) = fixture.delete(&format!("/v1/runners/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = fixture.delete(&format!("/v1/runners/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "DELETE is idempotent");

    fixture.shutdown().await;
}

/// The measured behaviour that most easily half-works: the Ink UI hard-wraps the URL across
/// lines at terminal width, so a naive single-line match returns a URL truncated mid-query
/// that the authorization server rejects with an opaque error.
#[tokio::test]
async fn a_wrapped_ansi_laden_authorization_url_is_rejoined_over_the_wire() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;

    fixture.engine.set_transcript(
        &container_id,
        concat!(
            "\u{1b}[2K\u{1b}[1mBrowse to:\u{1b}[0m\r\n",
            "\u{1b}[36mhttps://claude.com/cai/oauth/authorize?code=true&client_id=9d1c\u{1b}[0m\r\n",
            "3f2a&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.co\r\n",
            "m%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&state=st-42\r\n",
            "\r\n",
            "Paste code here if prompted > \r\n",
        ),
    );

    let (_, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;

    assert_eq!(body["state"], "awaiting_authorization");
    let url = body["authorization_url"].as_str().expect("url");
    assert_eq!(
        url,
        "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c3f2a&response_type=code\
         &redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback\
         &scope=user%3Ainference&state=st-42"
    );
    assert!(
        url.contains("state=st-42"),
        "the state parameter survived the wrap"
    );
    assert!(
        !url.contains('\u{1b}'),
        "no escape sequence reached the caller"
    );

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// Illegal transitions
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_code_before_the_url_and_a_second_code_after_it_are_both_409() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;

    let (status, body, _) = fixture
        .post(
            &format!("/v1/runners/{id}/authorization-code"),
            json!({ "code": "too-early" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "runner_wrong_state");
    assert!(
        fixture.engine.stdin_writes(&container_id).is_empty(),
        "a refused code must never reach the container"
    );

    // A URL on its own is still not writable: the CLI's reader does not exist until its
    // prompt renders, and a write into that gap is accepted by the daemon and lands nowhere.
    fixture.engine.set_transcript(&container_id, URL_ONLY_FRAME);
    let (status, body, _) = fixture
        .post(
            &format!("/v1/runners/{id}/authorization-code"),
            json!({ "code": "still-too-early" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "runner_wrong_state");
    assert!(fixture.engine.stdin_writes(&container_id).is_empty());

    fixture
        .engine
        .set_transcript(&container_id, &awaiting_frame());
    let (status, _, _) = fixture
        .post(
            &format!("/v1/runners/{id}/authorization-code"),
            json!({ "code": "first" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, body, _) = fixture
        .post(
            &format!("/v1/runners/{id}/authorization-code"),
            json!({ "code": "second" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "runner_wrong_state");
    assert_eq!(
        fixture.engine.stdin_writes(&container_id),
        b"first\r".to_vec()
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn the_token_is_409_until_ready_and_410_after_it_has_been_taken() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;

    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}/token")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "runner_wrong_state");

    fixture.make_ready(id, &container_id).await;

    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}/token")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["token"], TOKEN_VALUE);

    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}/token")).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(body["error"]["code"], "token_already_retrieved");
    assert!(
        !body.to_string().contains(TOKEN_VALUE),
        "the second answer must not leak what the first one returned"
    );

    fixture.shutdown().await;
}

/// The property the whole one-shot design exists for, under contention rather than in
/// sequence. Gated with a barrier — no `sleep`, per `plans/CONVENTIONS.md` §3.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_token_fetches_over_http_yield_exactly_one_token() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;
    fixture.make_ready(id, &container_id).await;

    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut racers = Vec::new();
    for _ in 0..8 {
        let client = fixture.client.clone();
        let url = fixture.url(&format!("/v1/runners/{id}/token"));
        let barrier = Arc::clone(&barrier);
        racers.push(tokio::spawn(async move {
            barrier.wait().await;
            let response = client
                .get(url)
                .bearer_auth(CONTROL_TOKEN)
                .send()
                .await
                .expect("request");
            (response.status(), response.text().await.expect("body"))
        }));
    }

    let mut granted = 0;
    let mut gone = 0;
    for racer in racers {
        let (status, body) = racer.await.expect("racer did not panic");
        match status {
            StatusCode::OK => {
                assert!(body.contains(TOKEN_VALUE));
                granted += 1;
            }
            StatusCode::GONE => {
                assert!(!body.contains(TOKEN_VALUE));
                gone += 1;
            }
            other => panic!("unexpected status from a racer: {other} — {body}"),
        }
    }

    assert_eq!(granted, 1, "the token must be handed out exactly once");
    assert_eq!(gone, 7);

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_failed_exchange_reports_failed_with_an_error_code_and_refuses_the_token() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;
    fixture
        .engine
        .set_transcript(&container_id, &awaiting_frame());
    let (status, _, _) = fixture
        .post(
            &format!("/v1/runners/{id}/authorization-code"),
            json!({ "code": "bogus" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    fixture.engine.set_transcript(
        &container_id,
        &format!(
            "{}OAuth \u{1b}[31merror\u{1b}[0m: status code 400\r\n",
            awaiting_frame()
        ),
    );

    let (_, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;
    assert_eq!(body["state"], "failed");
    assert_eq!(body["error_code"], "oauth_exchange_failed");

    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}/token")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "runner_wrong_state");

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// Expiry and reaping
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_runner_past_its_ttl_reads_as_expired_and_is_reaped() {
    let fixture = RunnerFixture::start(&[("MAX_TTL_SECONDS", "3600")]).await;
    let (short, short_container) = {
        let (status, body, _) = fixture
            .post("/v1/runners", json!({ "label": "short", "ttl_seconds": 1 }))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let id: Uuid = body["id"].as_str().expect("id").parse().expect("uuid");
        let container = fixture.service.view(id).await.expect("view").container_id;
        (id, container)
    };
    let (long, _) = fixture.create("long").await;
    fixture
        .engine
        .set_transcript(&short_container, &awaiting_frame());

    // Expiry is asserted by moving the clock forward through the reaper's parameter, not by
    // waiting for a wall-clock second to pass.
    let expires_at = fixture.service.view(short).await.expect("view").expires_at;
    let reaped = fixture
        .service
        .reap(expires_at + chrono::Duration::seconds(1))
        .await
        .expect("reap");

    assert_eq!(reaped, 1);
    let (status, body, _) = fixture.get(&format!("/v1/runners/{short}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "runner_not_found");

    let (status, _, _) = fixture.get(&format!("/v1/runners/{long}")).await;
    assert_eq!(status, StatusCode::OK, "an unexpired runner must survive");

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_ttl_above_the_configured_ceiling_is_400_invalid_request() {
    // The default has to move with the ceiling: `RunnerConfig::validate` refuses a
    // configuration whose default TTL exceeds its maximum, so a fixture that lowered only the
    // ceiling would be one the binary would never start with.
    let fixture =
        RunnerFixture::start(&[("MAX_TTL_SECONDS", "600"), ("DEFAULT_TTL_SECONDS", "600")]).await;

    let (status, body, _) = fixture
        .post(
            "/v1/runners",
            json!({ "label": "demo", "ttl_seconds": 601 }),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// The error envelope
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn every_error_uses_the_frozen_two_field_envelope() {
    let fixture = RunnerFixture::start(&[]).await;
    let missing = Uuid::now_v7();

    let cases = vec![
        (
            fixture.get(&format!("/v1/runners/{missing}")).await,
            "runner_not_found",
        ),
        (
            fixture
                .post("/v1/runners", json!({ "label": "BAD LABEL" }))
                .await,
            "invalid_request",
        ),
        (
            fixture.get("/v1/runners/not-a-uuid").await,
            "invalid_request",
        ),
        (
            fixture.post("/v1/runners", json!({ "nope": 1 })).await,
            "invalid_request",
        ),
    ];

    for ((status, body, _), expected) in cases {
        assert!(
            status.is_client_error(),
            "expected a client error, got {status}"
        );
        let error = body.get("error").expect("an `error` object");
        assert_eq!(error["code"], expected);
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "every error carries a human-readable message"
        );
        assert_eq!(
            error.as_object().expect("object").len(),
            2,
            "the frozen envelope is exactly {{code, message}} — another workstream is being \
             written against it in parallel"
        );
    }

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_code_containing_control_characters_is_refused_before_it_reaches_the_container() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, container_id) = fixture.create("demo").await;
    fixture
        .engine
        .set_transcript(&container_id, &awaiting_frame());

    // An embedded carriage return would submit one line and leave the rest queued as input
    // to whatever the CLI prompts for next — a caller-controlled injection into an
    // interactive session running with the operator's credentials.
    for hostile in ["abc\rextra", "abc\nextra", "abc\u{1b}[2J"] {
        let (status, body, _) = fixture
            .post(
                &format!("/v1/runners/{id}/authorization-code"),
                json!({ "code": hostile }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{hostile:?} was accepted");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
    assert!(fixture.engine.stdin_writes(&container_id).is_empty());

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// Health, and the daemon being away
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn healthz_reports_the_daemon_without_authentication_and_stays_200_when_it_is_away() {
    let fixture = RunnerFixture::start(&[]).await;

    let (status, body, _) = fixture.get("/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["docker"], "reachable");

    fixture.engine.set_ping_fails(true);
    let (status, body, _) = fixture.get("/healthz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "this is the liveness probe for THIS process; a restarting daemon must not take the \
         control plane out of rotation for a condition the body reports perfectly well"
    );
    assert_eq!(body["docker"], "unreachable");

    fixture.shutdown().await;
}

#[tokio::test]
async fn a_dead_daemon_is_503_docker_unavailable_and_does_not_echo_the_daemon_message() {
    let fixture = RunnerFixture::start(&[]).await;
    let (id, _) = fixture.create("demo").await;
    fixture.engine.set_engine_down(true);

    let (status, body, _) = fixture.get(&format!("/v1/runners/{id}")).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "docker_unavailable");
    assert!(
        !body.to_string().contains("in-memory engine"),
        "the engine's own message describes the host this service protects; it is logged, \
         never returned"
    );

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// Statelessness
// ---------------------------------------------------------------------------------------

/// The contract's statelessness requirement, exercised through the real HTTP surface: a
/// second control plane over the same engine rediscovers every runner from the container
/// labels, including the durable one-shot marker.
#[tokio::test]
async fn a_restarted_control_plane_rediscovers_runners_and_the_consumed_marker() {
    let first = RunnerFixture::start(&[]).await;
    let (ready_id, ready_container) = first.create("ready-one").await;
    first.make_ready(ready_id, &ready_container).await;
    let (status, _, _) = first.get(&format!("/v1/runners/{ready_id}/token")).await;
    assert_eq!(status, StatusCode::OK);

    let (pending_id, pending_container) = first.create("pending-one").await;
    first
        .engine
        .set_transcript(&pending_container, &awaiting_frame());

    // A brand-new service and a brand-new router over the same engine: nothing at all is
    // carried over in this process's memory.
    let engine = Arc::clone(&first.engine);
    first.shutdown().await;

    let config = Arc::new(
        RunnerConfig::from_source(&|key: &str| match key {
            "MOIRA_RUNNER__CONTROL_TOKEN" => Some(CONTROL_TOKEN.to_string()),
            "MOIRA_RUNNER__IMAGE" => Some(IMAGE.to_string()),
            _ => None,
        })
        .expect("configuration"),
    );
    let service = Arc::new(RunnerService::new(engine, config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let app = http::router(Arc::clone(&service));
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(task_shutdown.cancelled_owned())
            .await
            .expect("serve");
    });
    let client = Client::new();
    let base = format!("http://{address}");

    let pending: Value = client
        .get(format!("{base}/v1/runners/{pending_id}"))
        .bearer_auth(CONTROL_TOKEN)
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert_eq!(
        pending["state"], "awaiting_authorization",
        "a runner created before the restart is rediscovered from its labels"
    );

    let consumed = client
        .get(format!("{base}/v1/runners/{ready_id}/token"))
        .bearer_auth(CONTROL_TOKEN)
        .send()
        .await
        .expect("get");
    assert_eq!(
        consumed.status(),
        StatusCode::GONE,
        "the one-shot promise must survive a restart, which is why the marker is a rename \
         and not a HashSet"
    );

    shutdown.cancel();
    task.await.expect("server task");
}
