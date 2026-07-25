#![allow(dead_code)]

use std::{
    collections::VecDeque,
    convert::Infallible,
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify, Semaphore},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub authorization: Option<String>,
    pub body: Value,
}

#[derive(Debug)]
pub struct ScriptGate {
    arrived: Semaphore,
    release: Semaphore,
    completed: Semaphore,
    connection_closed: Semaphore,
    normal_completion: AtomicBool,
}

impl ScriptGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            arrived: Semaphore::new(0),
            release: Semaphore::new(0),
            completed: Semaphore::new(0),
            connection_closed: Semaphore::new(0),
            normal_completion: AtomicBool::new(false),
        })
    }

    pub async fn wait_arrived(&self) {
        wait_signal(&self.arrived, "provider request arrival").await;
    }

    pub fn release(&self) {
        self.release.add_permits(1);
    }

    pub async fn wait_completed(&self) {
        wait_signal(&self.completed, "provider response completion").await;
    }

    pub fn is_completed(&self) -> bool {
        self.normal_completion.load(Ordering::Acquire)
    }

    pub async fn wait_connection_closed(&self) {
        wait_signal(&self.connection_closed, "provider connection closure").await;
    }

    async fn wait_for_release(&self) {
        wait_signal(&self.release, "test release signal").await;
    }
}

#[derive(Debug, Clone)]
pub enum ProviderScript {
    Completion {
        text: String,
    },
    HeldCompletion {
        text: String,
        gate: Arc<ScriptGate>,
    },
    HttpError {
        status: StatusCode,
        body: String,
    },
    MalformedResponse,
    Stream {
        deltas: Vec<String>,
    },
    HeldStream {
        first_delta: String,
        remaining_deltas: Vec<String>,
        gate: Arc<ScriptGate>,
    },
    StreamErrorAfterDelta {
        delta: String,
    },
    StreamErrorAfterToolCall {
        name: String,
    },
    StalledStream {
        first_delta: Option<String>,
        gate: Arc<ScriptGate>,
    },
    /// Emits `first_delta`, signals arrival, waits for the test's release, then emits
    /// `flood_delta` in an unbounded loop until the consumer drops the connection.
    ///
    /// Distinct from [`ProviderScript::HeldStream`], whose `remaining_deltas` are a fixed
    /// list. A fixed list cannot *guarantee* server-side send backpressure: the number of
    /// events needed to fill Moira's two internal `mpsc` channels **plus** hyper's write
    /// buffer **plus** the kernel socket buffers on both ends is platform-dependent, so
    /// any fixed count is either a guess that may be too small or a large magic number.
    /// Flooding until the connection closes makes the backpressure deterministic with no
    /// `sleep` and no buffer-size arithmetic. The loop is naturally throttled: it only
    /// advances when hyper can write, so it cannot busy-spin.
    FloodingStream {
        first_delta: String,
        flood_delta: String,
        gate: Arc<ScriptGate>,
    },
}

#[derive(Debug)]
struct MockState {
    scripts: Mutex<VecDeque<ProviderScript>>,
    requests: Mutex<Vec<RecordedRequest>>,
    request_observed: Notify,
}

#[derive(Debug)]
pub struct MockOpenAiServer {
    address: SocketAddr,
    state: Arc<MockState>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl MockOpenAiServer {
    pub async fn start(scripts: impl IntoIterator<Item = ProviderScript>) -> Self {
        let state = Arc::new(MockState {
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            request_observed: Notify::new(),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(handle_completion))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock OpenAI provider");
        let address = listener.local_addr().expect("mock provider address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve mock OpenAI provider");
        });
        Self {
            address,
            state,
            shutdown,
            task,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    pub async fn wait_for_call_count(&self, expected: usize) {
        timeout(WAIT_TIMEOUT, async {
            loop {
                if self.call_count().await >= expected {
                    return;
                }
                self.state.request_observed.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("provider did not receive {expected} calls"));
    }

    pub async fn call_count(&self) -> usize {
        self.state.requests.lock().await.len()
    }

    pub async fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().await.clone()
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        timeout(WAIT_TIMEOUT, self.task)
            .await
            .expect("mock provider shutdown timed out")
            .expect("mock provider task panicked");
    }
}

async fn handle_completion(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed_body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.requests.lock().await.push(RecordedRequest {
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: parsed_body,
    });
    state.request_observed.notify_waiters();

    let script =
        state
            .scripts
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| ProviderScript::HttpError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: "no provider script remains".to_string(),
            });
    match script {
        ProviderScript::Completion { text } => completion_response(text),
        ProviderScript::HeldCompletion { text, gate } => {
            gate.arrived.add_permits(1);
            let mut guard = ConnectionGuard::new(gate.clone());
            gate.wait_for_release().await;
            guard.mark_completed();
            gate.normal_completion.store(true, Ordering::Release);
            gate.completed.add_permits(1);
            completion_response(text)
        }
        ProviderScript::HttpError { status, body } => (status, body).into_response(),
        ProviderScript::MalformedResponse => (
            [(header::CONTENT_TYPE, "application/json")],
            "{not valid JSON",
        )
            .into_response(),
        ProviderScript::Stream { deltas } => streaming_response(stream_body(deltas)),
        ProviderScript::HeldStream {
            first_delta,
            remaining_deltas,
            gate,
        } => streaming_response(held_stream_body(first_delta, remaining_deltas, gate)),
        ProviderScript::StreamErrorAfterDelta { delta } => {
            streaming_response(error_after_delta_body(delta))
        }
        ProviderScript::StreamErrorAfterToolCall { name } => {
            streaming_response(error_after_tool_call_body(name))
        }
        ProviderScript::StalledStream { first_delta, gate } => {
            streaming_response(stalled_stream_body(first_delta, gate))
        }
        ProviderScript::FloodingStream {
            first_delta,
            flood_delta,
            gate,
        } => streaming_response(flooding_stream_body(first_delta, flood_delta, gate)),
    }
}

fn completion_response(text: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": text
                },
                "logprobs": null,
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 2,
                "completion_tokens": 1,
                "total_tokens": 3
            }
        })
        .to_string(),
    )
        .into_response()
}

fn streaming_response(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("streaming response")
}

fn stream_body(deltas: Vec<String>) -> Body {
    let stream = async_stream::stream! {
        for delta in deltas {
            yield Ok::<_, Infallible>(Bytes::from(sse_delta(&delta)));
        }
        yield Ok(Bytes::from(sse_usage()));
        yield Ok(Bytes::from("data: [DONE]\n\n"));
    };
    Body::from_stream(stream)
}

fn held_stream_body(
    first_delta: String,
    remaining_deltas: Vec<String>,
    gate: Arc<ScriptGate>,
) -> Body {
    let stream = async_stream::stream! {
        let mut guard = ConnectionGuard::new(gate.clone());
        yield Ok::<_, Infallible>(Bytes::from(sse_delta(&first_delta)));
        gate.arrived.add_permits(1);
        gate.wait_for_release().await;
        for delta in remaining_deltas {
            yield Ok(Bytes::from(sse_delta(&delta)));
        }
        yield Ok(Bytes::from(sse_usage()));
        yield Ok(Bytes::from("data: [DONE]\n\n"));
        guard.mark_completed();
        gate.normal_completion.store(true, Ordering::Release);
        gate.completed.add_permits(1);
    };
    Body::from_stream(stream)
}

fn error_after_delta_body(delta: String) -> Body {
    let stream = async_stream::stream! {
        yield Ok::<_, io::Error>(Bytes::from(sse_delta(&delta)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        yield Err(io::Error::other("scripted provider body failure"));
    };
    Body::from_stream(stream)
}

fn error_after_tool_call_body(name: String) -> Body {
    let stream = async_stream::stream! {
        let chunk = format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl-test",
                "model": "test-model",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_test",
                            "function": {
                                "name": name,
                                "arguments": "{}"
                            }
                        }]
                    },
                    "finish_reason": null
                }],
                "usage": null
            })
        );
        yield Ok::<_, io::Error>(Bytes::from(chunk));
        let finish = format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl-test",
                "model": "test-model",
                "choices": [{
                    "delta": { "tool_calls": [] },
                    "finish_reason": "tool_calls"
                }],
                "usage": null
            })
        );
        yield Ok(Bytes::from(finish));
        tokio::time::sleep(Duration::from_millis(50)).await;
        yield Err(io::Error::other("scripted provider body failure after tool call"));
    };
    Body::from_stream(stream)
}

fn stalled_stream_body(first_delta: Option<String>, gate: Arc<ScriptGate>) -> Body {
    let stream = async_stream::stream! {
        let _guard = ConnectionGuard::new(gate.clone());
        if let Some(delta) = first_delta {
            yield Ok::<_, Infallible>(Bytes::from(sse_delta(&delta)));
        }
        gate.arrived.add_permits(1);
        gate.wait_for_release().await;
    };
    Body::from_stream(stream)
}

fn flooding_stream_body(first_delta: String, flood_delta: String, gate: Arc<ScriptGate>) -> Body {
    let stream = async_stream::stream! {
        let _guard = ConnectionGuard::new(gate.clone());
        yield Ok::<_, Infallible>(Bytes::from(sse_delta(&first_delta)));
        gate.arrived.add_permits(1);
        gate.wait_for_release().await;
        let chunk = Bytes::from(sse_delta(&flood_delta));
        loop {
            yield Ok(chunk.clone());
        }
    };
    Body::from_stream(stream)
}

fn sse_delta(delta: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-test",
            "model": "test-model",
            "choices": [{
                "delta": {
                    "content": delta,
                    "tool_calls": []
                },
                "finish_reason": null
            }],
            "usage": null
        })
    )
}

fn sse_usage() -> String {
    format!(
        "data: {}\n\n",
        json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 2,
                "completion_tokens": 1,
                "total_tokens": 3
            }
        })
    )
}

async fn wait_signal(signal: &Semaphore, description: &str) {
    timeout(WAIT_TIMEOUT, signal.acquire())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {description}"))
        .expect("test signal semaphore closed")
        .forget();
}

struct ConnectionGuard {
    gate: Arc<ScriptGate>,
    completed: AtomicBool,
}

impl ConnectionGuard {
    fn new(gate: Arc<ScriptGate>) -> Self {
        Self {
            gate,
            completed: AtomicBool::new(false),
        }
    }

    fn mark_completed(&mut self) {
        self.completed.store(true, Ordering::Release);
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if !self.completed.load(Ordering::Acquire) {
            self.gate.connection_closed.add_permits(1);
        }
    }
}
