#![allow(dead_code)]

use std::{
    collections::{HashMap, VecDeque},
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
    /// Emits `delta`, signals arrival, then fails the response body once the test
    /// releases the gate.
    ///
    /// The gate is what makes "the failure happened *after* committed output" a fact
    /// rather than a hope (P2-12). Both of these scripts used to
    /// `sleep(Duration::from_millis(50))` between the delta and the error, betting that
    /// 50 ms was enough for the delta to be read before the body aborted. It usually
    /// was; when it was not, the assertion under test — that a post-commit failure is
    /// neither retryable nor fallback-eligible — inverted, because nothing had been
    /// committed yet. The gate turns the bet into an ordering the test states
    /// explicitly: release only after the delta has been observed.
    StreamErrorAfterDelta {
        delta: String,
        gate: Arc<ScriptGate>,
    },
    /// As [`ProviderScript::StreamErrorAfterDelta`], with a tool call as the committed
    /// output instead of a text delta.
    StreamErrorAfterToolCall {
        name: String,
        gate: Arc<ScriptGate>,
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

/// How the mock's `/v1/embeddings` route behaves.
///
/// Separate from [`ProviderScript`] because embeddings and completions are independent routes
/// on the same server: a test that ingests a document and then runs a completion must be able
/// to script one without consuming the other's queue.
#[derive(Debug, Clone)]
pub enum EmbeddingBehaviour {
    /// Returns one deterministic unit-length vector per input.
    ///
    /// Deterministic in the strong sense: the vector is a pure function of the input text, so
    /// the same chunk re-ingested embeds identically and two different chunks do not collide.
    /// That is what lets a test assert "this chunk's stored embedding is the one the provider
    /// returned for this chunk's text" rather than merely "some vector was stored".
    Deterministic,
    /// Fails every embedding call with the given status.
    HttpError { status: StatusCode, body: String },
    /// Returns one fewer vector than requested — the short-response case that must be refused
    /// rather than zip-truncated into embeddings attached to the wrong chunks.
    ShortResponse,
    /// Returns vectors of the wrong width.
    WrongDimension { dimension: usize },
    /// Returns a hand-chosen vector for an exact input string, falling back to
    /// [`EmbeddingBehaviour::Deterministic`] for anything unlisted.
    ///
    /// [`mock_embedding_for`] is deterministic but pseudo-random, so two different texts are
    /// near-orthogonal by construction and their cosine similarity is an accident of the
    /// hash. That is fine for "was a vector stored", and useless for "does *this* row outrank
    /// *that* one" — which is exactly the question the cross-tenant isolation suite has to ask.
    ///
    /// With hand-chosen vectors from [`planar_vector`] the distances are arithmetic, so an
    /// assertion like "the other tenant's row is a strictly closer match and is still not
    /// returned" is a fact about the SQL rather than a hope about a hash.
    Fixed { vectors: HashMap<String, Vec<f32>> },
}

#[derive(Debug)]
struct MockState {
    scripts: Mutex<VecDeque<ProviderScript>>,
    requests: Mutex<Vec<RecordedRequest>>,
    request_observed: Notify,
    embedding: Mutex<EmbeddingBehaviour>,
    embedding_requests: Mutex<Vec<RecordedRequest>>,
}

/// The width every embedding this mock returns has, matching `vector(1536)`.
pub const MOCK_EMBEDDING_DIMENSION: usize = 1536;

/// The vector the mock returns for `text`.
///
/// Exposed so a test can assert the *stored* vector equals the one the provider produced for
/// that exact chunk, which is the only assertion that proves embeddings were not shuffled
/// between chunks.
pub fn mock_embedding_for(text: &str) -> Vec<f32> {
    // A tiny xorshift seeded from the text. Not a hash with any security property — it only has
    // to be deterministic, cheap, and different for different inputs.
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        seed ^= u64::from(*byte);
        seed = seed.wrapping_mul(0x1000_0000_01b3);
    }
    let mut vector = Vec::with_capacity(MOCK_EMBEDDING_DIMENSION);
    for _ in 0..MOCK_EMBEDDING_DIMENSION {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        // Map into [-1, 1) with a magnitude that survives the f64 -> f32 narrowing exactly:
        // dividing by a power of two keeps every value representable in binary32.
        let value = ((seed >> 40) as i32 - 8_388_608 / 8) as f32 / 8_388_608.0;
        vector.push(value);
    }
    vector
}

/// A unit vector in the plane spanned by the first two basis axes, at `angle` radians from the
/// first.
///
/// Two of these have cosine similarity `cos(a - b)` exactly, so pgvector's `<=>` between them
/// is `1 - cos(a - b)` — a distance a test can compute in its head. `planar_vector(0.0)` is the
/// natural query vector; a candidate at `0.0` is a perfect match, one at `PI / 3` sits at
/// similarity `0.5`, and one at `PI / 2` is orthogonal.
///
/// Every component beyond the first two is zero, which keeps the vector exactly unit-length in
/// `f32` and therefore keeps the arithmetic exact rather than approximately right.
pub fn planar_vector(angle: f64) -> Vec<f32> {
    let mut vector = vec![0.0f32; MOCK_EMBEDDING_DIMENSION];
    vector[0] = angle.cos() as f32;
    vector[1] = angle.sin() as f32;
    vector
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
            embedding: Mutex::new(EmbeddingBehaviour::Deterministic),
            embedding_requests: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(handle_completion))
            .route("/v1/embeddings", post(handle_embeddings))
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

    pub async fn set_embedding_behaviour(&self, behaviour: EmbeddingBehaviour) {
        *self.state.embedding.lock().await = behaviour;
    }

    /// Every `/v1/embeddings` request the mock has served, in order.
    ///
    /// Counted separately from [`Self::requests`] so an assertion about batching cannot be
    /// satisfied by completion traffic.
    pub async fn embedding_requests(&self) -> Vec<RecordedRequest> {
        self.state.embedding_requests.lock().await.clone()
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        timeout(WAIT_TIMEOUT, self.task)
            .await
            .expect("mock provider shutdown timed out")
            .expect("mock provider task panicked");
    }
}

async fn handle_embeddings(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.embedding_requests.lock().await.push(RecordedRequest {
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: parsed_body.clone(),
    });
    state.request_observed.notify_waiters();

    let inputs: Vec<String> = parsed_body
        .get("input")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();

    let behaviour = state.embedding.lock().await.clone();
    let (count, dimension) = match &behaviour {
        EmbeddingBehaviour::HttpError { status, body } => {
            return (*status, body.clone()).into_response();
        }
        EmbeddingBehaviour::Deterministic | EmbeddingBehaviour::Fixed { .. } => {
            (inputs.len(), MOCK_EMBEDDING_DIMENSION)
        }
        EmbeddingBehaviour::ShortResponse => {
            (inputs.len().saturating_sub(1), MOCK_EMBEDDING_DIMENSION)
        }
        EmbeddingBehaviour::WrongDimension { dimension } => (inputs.len(), *dimension),
    };
    let fixed = match &behaviour {
        EmbeddingBehaviour::Fixed { vectors } => Some(vectors),
        _ => None,
    };

    let data: Vec<Value> = inputs
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, text)| {
            let vector: Vec<f32> = fixed
                .and_then(|vectors| vectors.get(text).cloned())
                .unwrap_or_else(|| mock_embedding_for(text))
                .into_iter()
                .take(dimension)
                .collect();
            json!({
                "object": "embedding",
                "index": index,
                "embedding": vector,
            })
        })
        .collect();

    let total_tokens: usize = inputs.iter().map(|text| text.len().div_ceil(4)).sum();
    let body = json!({
        "object": "list",
        "model": parsed_body.get("model").cloned().unwrap_or(Value::Null),
        "data": data,
        "usage": {
            "prompt_tokens": total_tokens,
            "total_tokens": total_tokens,
            "prompt_tokens_details": Value::Null,
        },
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
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
        ProviderScript::StreamErrorAfterDelta { delta, gate } => {
            streaming_response(error_after_delta_body(delta, gate))
        }
        ProviderScript::StreamErrorAfterToolCall { name, gate } => {
            streaming_response(error_after_tool_call_body(name, gate))
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
                "total_tokens": 3,
                // DeepSeek-only fields, required so this mock can also serve a
                // `ProviderType::DeepSeek` provider (finding F39).
                //
                // DeepSeek is OpenAI-compatible on the *request* side, but rig-core 0.40 gives it
                // its own response type whose `Usage` declares `prompt_cache_hit_tokens` and
                // `prompt_cache_miss_tokens` with **no** `#[serde(default)]`. Omitting them makes
                // an otherwise perfect 200 fail to deserialize, which surfaces as
                // `provider_upstream_error` with the message "provider request failed with HTTP
                // 200" — a genuinely misleading signal, since the HTTP exchange succeeded.
                //
                // Neither rig `Usage` sets `deny_unknown_fields`, so the two extra keys are
                // ignored by every other provider and no existing expectation moves.
                "prompt_cache_hit_tokens": 0,
                "prompt_cache_miss_tokens": 2
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

fn error_after_delta_body(delta: String, gate: Arc<ScriptGate>) -> Body {
    let stream = async_stream::stream! {
        yield Ok::<_, io::Error>(Bytes::from(sse_delta(&delta)));
        gate.arrived.add_permits(1);
        gate.wait_for_release().await;
        yield Err(io::Error::other("scripted provider body failure"));
    };
    Body::from_stream(stream)
}

fn error_after_tool_call_body(name: String, gate: Arc<ScriptGate>) -> Body {
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
        gate.arrived.add_permits(1);
        gate.wait_for_release().await;
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
