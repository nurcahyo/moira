#![allow(dead_code)]

//! A scripted stand-in for `moira-runner`'s v1 control contract (issue #275, workstream R2 of
//! #272).
//!
//! A real `axum::Router` on a real `TcpListener::bind("127.0.0.1:0")`, exactly like
//! [`super::mock_control_plane`] — never `wiremock`, and never a Docker daemon. The point of this
//! fake is that Moira's half of the trust chain can be exercised end to end **without any Docker
//! access anywhere in the test process**, which is the same property the production split exists
//! to give.
//!
//! # The token
//!
//! [`MOCK_RUNNER_TOKEN`] is a fabricated string that has never authenticated anything. It is
//! shaped like the real thing on purpose: the leak assertions grep for it, and a token-shaped
//! canary is what makes those assertions meaningful. It must never be replaced with a real
//! credential.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle, time::timeout};
use tokio_util::sync::CancellationToken;

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// The canary. Fabricated, never valid, shaped like a real `claude setup-token` result so the
/// leak assertions have something specific to look for.
pub const MOCK_RUNNER_TOKEN: &str = "sk-ant-oat01-MOCK-RUNNER-TOKEN-DO-NOT-LEAK-0123456789";

/// The bearer token this fake expects. Moira must present it on every route except `/healthz`.
pub const MOCK_RUNNER_AUTH_TOKEN: &str = "mock-runner-bearer-token";

#[derive(Debug, Clone)]
pub struct RunnerScript {
    pub state: String,
    pub authorization_url: Option<String>,
    pub error_code: Option<String>,
    pub expires_at: Option<String>,
    /// Set once the one-shot token read has happened. The contract answers `410` afterwards, and
    /// so does this fake — the "a failed finalize stores nothing" test depends on it.
    pub token_taken: bool,
}

impl Default for RunnerScript {
    fn default() -> Self {
        Self {
            state: "provisioning".to_string(),
            authorization_url: None,
            error_code: None,
            expires_at: None,
            token_taken: false,
        }
    }
}

#[derive(Debug, Default)]
struct MockState {
    runners: Mutex<HashMap<String, RunnerScript>>,
    /// When set, `POST /v1/runners` answers with this status instead of creating a runner.
    create_failure: Mutex<Option<(StatusCode, &'static str)>>,
    /// When set, every route answers `307` pointing at this URL. Drives the redirect-refusal
    /// test: `redirect::Policy::none()` must mean the target is never contacted.
    redirect_to: Mutex<Option<String>>,
    create_calls: AtomicUsize,
    token_calls: AtomicUsize,
    delete_calls: AtomicUsize,
    /// Requests that arrived with a correct bearer token.
    authorized_calls: AtomicUsize,
    /// Requests that arrived with a missing or wrong bearer token.
    unauthorized_calls: AtomicUsize,
}

pub struct MockRunnerService {
    address: SocketAddr,
    state: Arc<MockState>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl MockRunnerService {
    pub async fn start() -> Self {
        let state = Arc::new(MockState::default());
        let app = Router::new()
            .route("/v1/runners", post(handle_create))
            .route("/v1/runners/{id}", get(handle_status).delete(handle_delete))
            .route(
                "/v1/runners/{id}/authorization-code",
                post(handle_authorization_code),
            )
            .route("/v1/runners/{id}/token", get(handle_token))
            .route("/healthz", get(handle_healthz))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock runner service");
        let address = listener.local_addr().expect("mock runner service address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve mock runner service");
        });
        Self {
            address,
            state,
            shutdown,
            task,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Overwrites the script for `runner_reference`, creating it if the test is driving a runner
    /// the fake did not mint.
    pub async fn set_script(&self, runner_reference: &str, script: RunnerScript) {
        self.state
            .runners
            .lock()
            .await
            .insert(runner_reference.to_string(), script);
    }

    pub async fn script(&self, runner_reference: &str) -> Option<RunnerScript> {
        self.state
            .runners
            .lock()
            .await
            .get(runner_reference)
            .cloned()
    }

    pub async fn set_create_failure(&self, failure: Option<(StatusCode, &'static str)>) {
        *self.state.create_failure.lock().await = failure;
    }

    pub async fn set_redirect_to(&self, url: Option<String>) {
        *self.state.redirect_to.lock().await = url;
    }

    pub fn token_calls(&self) -> usize {
        self.state.token_calls.load(Ordering::SeqCst)
    }

    pub fn create_calls(&self) -> usize {
        self.state.create_calls.load(Ordering::SeqCst)
    }

    pub fn delete_calls(&self) -> usize {
        self.state.delete_calls.load(Ordering::SeqCst)
    }

    pub fn authorized_calls(&self) -> usize {
        self.state.authorized_calls.load(Ordering::SeqCst)
    }

    pub fn unauthorized_calls(&self) -> usize {
        self.state.unauthorized_calls.load(Ordering::SeqCst)
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        timeout(WAIT_TIMEOUT, self.task)
            .await
            .expect("mock runner service shutdown timed out")
            .expect("mock runner service task panicked");
    }
}

/// A second server that must never receive a request.
///
/// Stands at the far end of the `307` the redirect test scripts. Its whole assertion is that its
/// counter stays at zero — a redirect Moira followed would show up here as a `1`, which is the
/// live blind-SSRF probe `security::ssrf` records the same reasoning about.
pub struct NeverContactedServer {
    address: SocketAddr,
    hits: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl NeverContactedServer {
    pub async fn start() -> Self {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = Router::new().fallback(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (StatusCode::OK, Json(json!({ "token": MOCK_RUNNER_TOKEN })))
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind never-contacted server");
        let address = listener.local_addr().expect("never-contacted address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve never-contacted server");
        });
        Self {
            address,
            hits,
            shutdown,
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/v1/runners", self.address)
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        timeout(WAIT_TIMEOUT, self.task)
            .await
            .expect("never-contacted shutdown timed out")
            .expect("never-contacted task panicked");
    }
}

fn error_body(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": "mock runner failure" } })),
    )
        .into_response()
}

/// Constant-time is not the point here — this is a fake — but the *presence* check is, because
/// `authorized_calls`/`unauthorized_calls` are what prove Moira sends the header at all.
async fn authorize(state: &Arc<MockState>, headers: &HeaderMap) -> Result<(), Response> {
    let presented = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if presented == Some(MOCK_RUNNER_AUTH_TOKEN) {
        state.authorized_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    } else {
        state.unauthorized_calls.fetch_add(1, Ordering::SeqCst);
        Err(error_body(StatusCode::UNAUTHORIZED, "unauthorized"))
    }
}

async fn redirect_if_scripted(state: &Arc<MockState>) -> Option<Response> {
    let target = state.redirect_to.lock().await.clone()?;
    let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
    response
        .headers_mut()
        .insert("location", target.parse().expect("redirect location"));
    Some(response)
}

fn status_body(id: &str, script: &RunnerScript) -> Value {
    json!({
        "id": id,
        "state": script.state,
        "authorization_url": script.authorization_url,
        "expires_at": script.expires_at,
        "error_code": script.error_code,
    })
}

async fn handle_create(State(state): State<Arc<MockState>>, headers: HeaderMap) -> Response {
    state.create_calls.fetch_add(1, Ordering::SeqCst);
    if let Some(redirect) = redirect_if_scripted(&state).await {
        return redirect;
    }
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if let Some((status, code)) = *state.create_failure.lock().await {
        return error_body(status, code);
    }
    let id = uuid::Uuid::now_v7().to_string();
    let script = RunnerScript {
        expires_at: Some("2099-01-01T00:00:00Z".to_string()),
        ..RunnerScript::default()
    };
    let body = status_body(&id, &script);
    state.runners.lock().await.insert(id, script);
    (StatusCode::CREATED, Json(body)).into_response()
}

async fn handle_status(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(redirect) = redirect_if_scripted(&state).await {
        return redirect;
    }
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let runners = state.runners.lock().await;
    match runners.get(&id) {
        Some(script) => (StatusCode::OK, Json(status_body(&id, script))).into_response(),
        None => error_body(StatusCode::NOT_FOUND, "runner_not_found"),
    }
}

async fn handle_authorization_code(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    if let Some(redirect) = redirect_if_scripted(&state).await {
        return redirect;
    }
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let code = body
        .and_then(|Json(value)| {
            value
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if code.trim().is_empty() {
        return error_body(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let mut runners = state.runners.lock().await;
    let Some(script) = runners.get_mut(&id) else {
        return error_body(StatusCode::NOT_FOUND, "runner_not_found");
    };
    if script.state != "awaiting_authorization" {
        return error_body(StatusCode::CONFLICT, "runner_wrong_state");
    }
    script.state = "exchanging".to_string();
    (StatusCode::ACCEPTED, Json(json!({ "state": "exchanging" }))).into_response()
}

async fn handle_token(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    if let Some(redirect) = redirect_if_scripted(&state).await {
        return redirect;
    }
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let mut runners = state.runners.lock().await;
    let Some(script) = runners.get_mut(&id) else {
        return error_body(StatusCode::NOT_FOUND, "runner_not_found");
    };
    if script.token_taken {
        return error_body(StatusCode::GONE, "token_already_retrieved");
    }
    if script.state != "ready" {
        return error_body(StatusCode::CONFLICT, "runner_wrong_state");
    }
    // One-shot, exactly like the contract. This flag is what makes the second finalize in
    // `a_failed_finalize_stores_nothing_and_the_token_is_unrecoverable` answer `410`.
    script.token_taken = true;
    (StatusCode::OK, Json(json!({ "token": MOCK_RUNNER_TOKEN }))).into_response()
}

async fn handle_delete(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    state.delete_calls.fetch_add(1, Ordering::SeqCst);
    if let Some(redirect) = redirect_if_scripted(&state).await {
        return redirect;
    }
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    state.runners.lock().await.remove(&id);
    StatusCode::NO_CONTENT.into_response()
}

async fn handle_healthz() -> Response {
    (
        StatusCode::OK,
        Json(json!({ "status": "ok", "docker": "reachable" })),
    )
        .into_response()
}
