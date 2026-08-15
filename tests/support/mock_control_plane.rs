#![allow(dead_code)]

//! A scripted local HTTP server standing in for both an OAuth2 token endpoint and a
//! provider's reachability probe target — never a real provider, never a real token, per the
//! owner-approved testing policy for `oauth-token-refresh` and `provider-health-check`
//! (plan 12 §1's "Testing policy for this workstream").

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::Mutex,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How `/oauth/token` responds to the next request.
#[derive(Debug, Clone)]
pub enum TokenScript {
    Success {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: i64,
    },
    HttpError {
        status: StatusCode,
    },
    /// A 307/308 to another absolute URL.
    ///
    /// Exists for one test: the refresh client is built with `redirect::Policy::none()`, and
    /// 307/308 are the two statuses that make `reqwest`'s *default* policy re-issue the
    /// original **POST with the original body** — which on this path is a decrypted refresh
    /// token. A 301/302 would degrade to a bodyless GET and would not prove the mechanism.
    Redirect {
        status: StatusCode,
        location: String,
    },
}

#[derive(Debug)]
struct MockState {
    token_script: Mutex<TokenScript>,
    token_calls: AtomicUsize,
    /// Every `/oauth/token` request body this plane received, verbatim.
    ///
    /// The token-endpoint assertions want to say "the secret never arrived here", which is a
    /// strictly stronger claim than "no request arrived" and the only one that distinguishes
    /// a control from a coincidence.
    token_bodies: Mutex<Vec<String>>,
    health_status: Mutex<StatusCode>,
    health_delay: Mutex<Duration>,
    health_calls: AtomicUsize,
}

pub struct MockControlPlane {
    address: SocketAddr,
    state: Arc<MockState>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl MockControlPlane {
    pub async fn start() -> Self {
        let state = Arc::new(MockState {
            token_script: Mutex::new(TokenScript::Success {
                access_token: "mock-access-token".to_string(),
                refresh_token: Some("mock-refresh-token-2".to_string()),
                expires_in: 3_600,
            }),
            token_calls: AtomicUsize::new(0),
            token_bodies: Mutex::new(Vec::new()),
            health_status: Mutex::new(StatusCode::OK),
            health_delay: Mutex::new(Duration::ZERO),
            health_calls: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/oauth/token", post(handle_token))
            .route("/health", get(handle_health))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock control plane");
        let address = listener.local_addr().expect("mock control plane address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve mock control plane");
        });
        Self {
            address,
            state,
            shutdown,
            task,
        }
    }

    pub fn token_endpoint(&self) -> String {
        format!("http://{}/oauth/token", self.address)
    }

    pub fn health_url(&self) -> String {
        format!("http://{}/health", self.address)
    }

    pub async fn set_token_script(&self, script: TokenScript) {
        *self.state.token_script.lock().await = script;
    }

    pub async fn set_health_status(&self, status: StatusCode) {
        *self.state.health_status.lock().await = status;
    }

    pub async fn set_health_delay(&self, delay: Duration) {
        *self.state.health_delay.lock().await = delay;
    }

    pub fn token_call_count(&self) -> usize {
        self.state.token_calls.load(Ordering::SeqCst)
    }

    /// Every `/oauth/token` request body this plane received.
    pub async fn token_bodies(&self) -> Vec<String> {
        self.state.token_bodies.lock().await.clone()
    }

    /// Whether `needle` appeared in any body this plane received — the direct form of "the
    /// secret never left the process".
    pub async fn observed_secret(&self, needle: &str) -> bool {
        self.state
            .token_bodies
            .lock()
            .await
            .iter()
            .any(|body| body.contains(needle))
    }

    pub fn health_call_count(&self) -> usize {
        self.state.health_calls.load(Ordering::SeqCst)
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        timeout(WAIT_TIMEOUT, self.task)
            .await
            .expect("mock control plane shutdown timed out")
            .expect("mock control plane task panicked");
    }
}

/// A `http://127.0.0.1:<port>/` URL nothing is listening on — used to prove a probe against a
/// genuinely unreachable provider classifies as `unhealthy` rather than merely slow.
///
/// Binds a real listener to grab an ephemeral port the OS has confirmed is free, then drops it
/// before returning, so the port is guaranteed unbound for the caller rather than merely
/// guessed to be — a hardcoded port number could collide with something else already running
/// on the test host.
pub async fn unreachable_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a throwaway listener to mint an unreachable port");
    let address = listener.local_addr().expect("throwaway listener address");
    drop(listener);
    format!("http://{address}/")
}

async fn handle_token(State(state): State<Arc<MockState>>, body: String) -> Response {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    state.token_bodies.lock().await.push(body);
    let script = state.token_script.lock().await.clone();
    match script {
        TokenScript::Success {
            access_token,
            refresh_token,
            expires_in,
        } => {
            let body: Value = json!({
                "access_token": access_token,
                "refresh_token": refresh_token,
                "token_type": "Bearer",
                "expires_in": expires_in,
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        TokenScript::HttpError { status } => (status, "mock token error").into_response(),
        TokenScript::Redirect { status, location } => (
            status,
            [(axum::http::header::LOCATION, location)],
            "mock token redirect",
        )
            .into_response(),
    }
}

async fn handle_health(State(state): State<Arc<MockState>>) -> Response {
    state.health_calls.fetch_add(1, Ordering::SeqCst);
    let delay = *state.health_delay.lock().await;
    if !delay.is_zero() {
        sleep(delay).await;
    }
    let status = *state.health_status.lock().await;
    (status, "ok").into_response()
}
