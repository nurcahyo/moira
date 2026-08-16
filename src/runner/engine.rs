//! The trait seam every Docker call goes through, and the in-memory fake that lets the rest
//! of the service be tested with no daemon present.
//!
//! # Why a seam at all
//!
//! Two reasons, and the second is the one that actually matters.
//!
//! The obvious one is testability: CI has no `claude` image and pulling one would be
//! prohibitive, so the state machine, the one-shot token semantics, the reaper and the whole
//! HTTP surface have to be exercisable without a daemon. [`InMemoryEngine`] is what makes
//! `cargo test` fully green on a machine with Docker stopped.
//!
//! The one that matters is **confinement**. Docker access is root-equivalent, so the set of
//! Docker operations this service can perform should be a short, readable list rather than
//! "whatever `bollard` exposes". This trait *is* that list: nine operations, no image build,
//! no volume management, no `exec`, no network creation, and — deliberately — no way to
//! express a bind mount or a published port at all, because [`ContainerSpec`] has no field
//! for either. A future change that wanted to mount the Docker socket into a runner would
//! have to widen this trait first, in a diff a reviewer can see.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

use async_trait::async_trait;

use super::token::TtyTranscript;

/// Everything the service can ask the container engine to do.
///
/// Errors are deliberately coarse — see the type docs.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The daemon could not be reached, or answered with a server error. Maps to the
    /// contract's `503 docker_unavailable`.
    #[error("container engine unavailable: {0}")]
    Unavailable(String),
    /// The named container does not exist. Maps to `404 runner_not_found`.
    #[error("container not found")]
    NotFound,
    /// A name collision on create. Distinguished from [`Self::Failed`] because it is the one
    /// error a retry can legitimately resolve.
    #[error("container name already in use")]
    Conflict,
    /// Anything else the daemon refused.
    #[error("container engine refused the request: {0}")]
    Failed(String),
}

/// The request to create one runner container.
///
/// **What this struct does not have is the point.** There is no `binds`, no `mounts`, no
/// `ports`, no `privileged`, no `cap_add`, and no `network_mode`. The hardening the frozen
/// contract requires — `Tty: true`, `OpenStdin: true`, `StdinOnce: false`,
/// `CapDrop: ["ALL"]`, `SecurityOpt: ["no-new-privileges"]`, and the absence of any mount —
/// is applied by [`super::docker_engine::container_create_body`] and is not configurable,
/// so an operator cannot weaken it and a caller cannot ask for it to be weakened.
///
/// There is no port field because there is nothing to publish: inside a container
/// `claude setup-token` uses Anthropic's hosted redirect and a paste prompt, measured on
/// issue #272. The listener the original design worried about does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
    /// Container name. Also the durable marker for the one-shot token handoff — see
    /// [`super::state::consumed_name`].
    pub name: String,
    /// Content-pinned image reference, already validated by
    /// [`super::config::validate_image_reference`].
    pub image: String,
    /// argv.
    pub command: Vec<String>,
    /// Labels. The source of truth for the runner registry; see [`super::state`].
    pub labels: BTreeMap<String, String>,
    /// `HostConfig.Memory`.
    pub memory_bytes: i64,
    /// `HostConfig.NanoCpus`.
    pub nano_cpus: i64,
    /// `HostConfig.PidsLimit`.
    pub pids_limit: i64,
}

/// What the engine reports back about one container.
///
/// This is everything the state machine is allowed to know from Docker. Note that it carries
/// no transcript: reading the tty stream is a separate, explicit call, so a caller cannot
/// accidentally end up holding a credential-bearing string it did not ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerRecord {
    /// Docker's container id.
    pub id: String,
    /// Primary container name, without the leading `/` the Engine API returns.
    pub name: String,
    pub labels: BTreeMap<String, String>,
    /// Whether the container's process is still alive. `claude setup-token` exits once it has
    /// printed a token or failed, so this is the durable "the exchange is over" signal.
    pub running: bool,
}

/// The nine operations `moira-runner` may perform against a container engine.
#[async_trait]
pub trait ContainerEngine: Send + Sync + 'static {
    /// Liveness probe for `GET /healthz`.
    async fn ping(&self) -> Result<(), EngineError>;

    /// Creates a container from `spec` and returns its id. Does not start it.
    async fn create(&self, spec: &ContainerSpec) -> Result<String, EngineError>;

    /// Starts a created container.
    async fn start(&self, container: &str) -> Result<(), EngineError>;

    /// Every container carrying the runner marker label, running or not.
    ///
    /// **This is the registry.** The service holds no authoritative list of its own, so a
    /// restart re-discovers every runner — including ones it created before the restart —
    /// and the reaper cleans up containers nothing in this process remembers creating.
    async fn list_runners(&self) -> Result<Vec<ContainerRecord>, EngineError>;

    /// One container by id or name.
    async fn inspect(&self, container: &str) -> Result<ContainerRecord, EngineError>;

    /// The container's whole tty stream from the beginning.
    ///
    /// Returns a [`TtyTranscript`] rather than a `String` so that `debug!(?transcript)`
    /// prints a byte count instead of a live OAuth token.
    async fn transcript(&self, container: &str) -> Result<TtyTranscript, EngineError>;

    /// Writes bytes to the container's stdin over the Engine API attach endpoint.
    ///
    /// `docker exec -i -t` refuses this ("cannot attach stdin to a TTY-enabled container
    /// because stdin is not a terminal"); the attach endpoint on a container created with
    /// `Tty: true` and `OpenStdin: true` accepts it over an HTTP 101 upgrade. Measured on
    /// issue #272 — this is the mechanism `node-pty` was wanted for.
    ///
    /// **One attach per call, and that is not incidental.** A second attach opened after the
    /// first has been dropped was observed not to reach the application at all: the bytes
    /// were accepted (HTTP 101) and never appeared at the prompt. Anything that needs several
    /// writes to land must therefore pass them in one call, not call this twice.
    async fn write_stdin(&self, container: &str, bytes: &[u8]) -> Result<(), EngineError>;

    /// Renames a container. Used as the durable marker that a token has been handed out;
    /// see [`super::state::consumed_name`].
    async fn rename(&self, container: &str, new_name: &str) -> Result<(), EngineError>;

    /// Force-removes a container. **Idempotent**: removing something already gone is `Ok`,
    /// because both the reaper and `DELETE /v1/runners/{id}` race each other by design.
    async fn remove(&self, container: &str) -> Result<(), EngineError>;
}

// ---------------------------------------------------------------------------------------
// The in-memory fake.
// ---------------------------------------------------------------------------------------

/// One container inside [`InMemoryEngine`].
///
/// The `spec` is kept verbatim so a test can assert exactly what the service asked for — the
/// labels, the resource limits and the argv — which is the property the frozen contract puts
/// requirements on.
#[derive(Debug, Clone)]
struct FakeContainer {
    id: String,
    name: String,
    spec: ContainerSpec,
    running: bool,
    transcript: String,
    stdin: Vec<u8>,
}

#[derive(Debug, Default)]
struct FakeState {
    containers: HashMap<String, FakeContainer>,
    next_id: u64,
    ping_fails: bool,
    /// When set, every operation other than `ping` reports the engine as unavailable. Models
    /// a daemon that went away mid-session.
    engine_down: bool,
}

/// An in-memory [`ContainerEngine`] with no daemon behind it.
///
/// # Why this ships in the library rather than living under `#[cfg(test)]`
///
/// An integration test under `tests/` links the library built **without** `cfg(test)`, so a
/// `#[cfg(test)]` fake is invisible to it — the same constraint that made the `test-routes`
/// feature necessary for `tests/http_middleware_contract.rs` (see `Cargo.toml`). The options
/// were a feature gate, a duplicated fake in every integration test file, or this. A feature
/// gate was rejected because the default `cargo test` run must be green, and a feature-gated
/// fake would not be compiled by it; duplication was rejected because two copies of a fake
/// drift, and a drifted fake is a test suite asserting against a model of the system that no
/// longer matches the one the unit tests use.
///
/// The residual cost is honest and small: this type is compiled into the shipped
/// `moira-runner` binary. It holds a `HashMap`, is reachable only by constructing it
/// explicitly, and — unlike the `test-routes` probes, which added *routes* to a live
/// router — it adds no reachable surface to a running process. Nothing in
/// `src/bin/moira-runner.rs` refers to it.
#[derive(Debug, Default)]
pub struct InMemoryEngine {
    state: Mutex<FakeState>,
}

impl InMemoryEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `ping` fail, so `GET /healthz` reports `docker: unreachable`.
    pub fn set_ping_fails(&self, fails: bool) {
        self.lock().ping_fails = fails;
    }

    /// Makes every operation other than `ping` report [`EngineError::Unavailable`].
    pub fn set_engine_down(&self, down: bool) {
        self.lock().engine_down = down;
    }

    /// Replaces a container's tty transcript, which is how a test drives the state machine:
    /// write an authorization URL to move a runner to `awaiting_authorization`, write a token
    /// to move it to `ready`.
    pub fn set_transcript(&self, container: &str, transcript: &str) {
        let mut state = self.lock();
        if let Some(key) = Self::resolve(&state, container) {
            state
                .containers
                .get_mut(&key)
                .expect("resolved key exists")
                .transcript = transcript.to_string();
        }
    }

    /// Marks a container as exited, which is the durable "the CLI is finished" signal.
    pub fn set_running(&self, container: &str, running: bool) {
        let mut state = self.lock();
        if let Some(key) = Self::resolve(&state, container) {
            state
                .containers
                .get_mut(&key)
                .expect("resolved key exists")
                .running = running;
        }
    }

    /// Everything written to a container's stdin, so a test can assert the exact bytes that
    /// reached the paste prompt — including the trailing `\r` the contract requires.
    pub fn stdin_writes(&self, container: &str) -> Vec<u8> {
        let state = self.lock();
        Self::resolve(&state, container)
            .and_then(|key| state.containers.get(&key).map(|found| found.stdin.clone()))
            .unwrap_or_default()
    }

    /// Current container names, for asserting what the reaper removed and what it did not.
    pub fn container_names(&self) -> Vec<String> {
        let state = self.lock();
        let mut names: Vec<String> = state
            .containers
            .values()
            .map(|container| container.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The `ContainerSpec` values `create` was called with, ordered by creation. Lets a test
    /// assert the labels and resource limits without reaching into a daemon.
    pub fn created_specs(&self) -> Vec<ContainerSpec> {
        let state = self.lock();
        let mut created: Vec<&FakeContainer> = state.containers.values().collect();
        created.sort_by(|left, right| left.id.cmp(&right.id));
        created
            .into_iter()
            .map(|container| container.spec.clone())
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        // A poisoned mutex here means a test panicked while holding it; recovering is
        // strictly better than turning every subsequent assertion into a second panic that
        // hides the first.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Docker accepts an id or a name wherever a container is named, so the fake does too.
    fn resolve(state: &FakeState, container: &str) -> Option<String> {
        if state.containers.contains_key(container) {
            return Some(container.to_string());
        }
        state
            .containers
            .values()
            .find(|candidate| candidate.name == container)
            .map(|found| found.id.clone())
    }

    fn record(container: &FakeContainer) -> ContainerRecord {
        ContainerRecord {
            id: container.id.clone(),
            name: container.name.clone(),
            labels: container.spec.labels.clone(),
            running: container.running,
        }
    }

    fn require_up(state: &FakeState) -> Result<(), EngineError> {
        if state.engine_down {
            return Err(EngineError::Unavailable(
                "in-memory engine is marked down".to_string(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ContainerEngine for InMemoryEngine {
    async fn ping(&self) -> Result<(), EngineError> {
        let state = self.lock();
        if state.ping_fails || state.engine_down {
            return Err(EngineError::Unavailable(
                "in-memory engine is down".to_string(),
            ));
        }
        Ok(())
    }

    async fn create(&self, spec: &ContainerSpec) -> Result<String, EngineError> {
        let mut state = self.lock();
        Self::require_up(&state)?;
        if state
            .containers
            .values()
            .any(|candidate| candidate.name == spec.name)
        {
            return Err(EngineError::Conflict);
        }
        state.next_id += 1;
        let id = format!("fake-{:012}", state.next_id);
        state.containers.insert(
            id.clone(),
            FakeContainer {
                id: id.clone(),
                name: spec.name.clone(),
                spec: spec.clone(),
                running: false,
                transcript: String::new(),
                stdin: Vec::new(),
            },
        );
        Ok(id)
    }

    async fn start(&self, container: &str) -> Result<(), EngineError> {
        let mut state = self.lock();
        Self::require_up(&state)?;
        let key = Self::resolve(&state, container).ok_or(EngineError::NotFound)?;
        state
            .containers
            .get_mut(&key)
            .expect("resolved key exists")
            .running = true;
        Ok(())
    }

    async fn list_runners(&self) -> Result<Vec<ContainerRecord>, EngineError> {
        let state = self.lock();
        Self::require_up(&state)?;
        let mut records: Vec<ContainerRecord> = state
            .containers
            .values()
            .filter(|container| {
                container
                    .spec
                    .labels
                    .get(super::state::LABEL_MARKER)
                    .is_some_and(|value| value == super::state::LABEL_MARKER_VALUE)
            })
            .map(Self::record)
            .collect();
        records.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(records)
    }

    async fn inspect(&self, container: &str) -> Result<ContainerRecord, EngineError> {
        let state = self.lock();
        Self::require_up(&state)?;
        let key = Self::resolve(&state, container).ok_or(EngineError::NotFound)?;
        Ok(Self::record(
            state.containers.get(&key).expect("resolved key exists"),
        ))
    }

    async fn transcript(&self, container: &str) -> Result<TtyTranscript, EngineError> {
        let state = self.lock();
        Self::require_up(&state)?;
        let key = Self::resolve(&state, container).ok_or(EngineError::NotFound)?;
        Ok(TtyTranscript::new(
            state
                .containers
                .get(&key)
                .expect("resolved key exists")
                .transcript
                .clone(),
        ))
    }

    async fn write_stdin(&self, container: &str, bytes: &[u8]) -> Result<(), EngineError> {
        let mut state = self.lock();
        Self::require_up(&state)?;
        let key = Self::resolve(&state, container).ok_or(EngineError::NotFound)?;
        state
            .containers
            .get_mut(&key)
            .expect("resolved key exists")
            .stdin
            .extend_from_slice(bytes);
        Ok(())
    }

    async fn rename(&self, container: &str, new_name: &str) -> Result<(), EngineError> {
        let mut state = self.lock();
        Self::require_up(&state)?;
        let key = Self::resolve(&state, container).ok_or(EngineError::NotFound)?;
        if state
            .containers
            .values()
            .any(|candidate| candidate.id != key && candidate.name == new_name)
        {
            return Err(EngineError::Conflict);
        }
        state
            .containers
            .get_mut(&key)
            .expect("resolved key exists")
            .name = new_name.to_string();
        Ok(())
    }

    async fn remove(&self, container: &str) -> Result<(), EngineError> {
        let mut state = self.lock();
        Self::require_up(&state)?;
        // Idempotent by contract: the reaper and DELETE race by design.
        if let Some(key) = Self::resolve(&state, container) {
            state.containers.remove(&key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> ContainerSpec {
        ContainerSpec {
            name: name.to_string(),
            image: "sha256:00".to_string(),
            command: vec!["claude".to_string()],
            labels: BTreeMap::from([(
                super::super::state::LABEL_MARKER.to_string(),
                super::super::state::LABEL_MARKER_VALUE.to_string(),
            )]),
            memory_bytes: 1,
            nano_cpus: 1,
            pids_limit: 1,
        }
    }

    #[tokio::test]
    async fn create_start_and_list_round_trip() {
        let engine = InMemoryEngine::new();
        let id = engine.create(&spec("runner-a")).await.expect("create");

        let listed = engine.list_runners().await.expect("list");
        assert_eq!(listed.len(), 1);
        assert!(
            !listed[0].running,
            "a created container has not started yet"
        );

        engine.start(&id).await.expect("start");
        assert!(engine.inspect(&id).await.expect("inspect").running);
        // Docker accepts an id or a name interchangeably, so the fake must too.
        assert!(engine.inspect("runner-a").await.expect("by name").running);
    }

    #[tokio::test]
    async fn a_container_without_the_marker_label_is_not_a_runner() {
        let engine = InMemoryEngine::new();
        let mut unmarked = spec("not-a-runner");
        unmarked.labels.clear();
        engine.create(&unmarked).await.expect("create");

        assert!(
            engine.list_runners().await.expect("list").is_empty(),
            "the registry is defined by the marker label, not by everything on the host"
        );
    }

    #[tokio::test]
    async fn remove_is_idempotent_and_duplicate_names_conflict() {
        let engine = InMemoryEngine::new();
        engine.create(&spec("runner-a")).await.expect("create");

        assert!(matches!(
            engine.create(&spec("runner-a")).await,
            Err(EngineError::Conflict)
        ));

        engine.remove("runner-a").await.expect("first remove");
        engine
            .remove("runner-a")
            .await
            .expect("removing something already gone is Ok — the reaper and DELETE race");
        assert!(engine.container_names().is_empty());
    }

    #[tokio::test]
    async fn stdin_writes_are_recorded_verbatim() {
        let engine = InMemoryEngine::new();
        engine.create(&spec("runner-a")).await.expect("create");

        engine
            .write_stdin("runner-a", b"code#state\r")
            .await
            .expect("write");

        assert_eq!(engine.stdin_writes("runner-a"), b"code#state\r".to_vec());
    }

    #[tokio::test]
    async fn a_down_engine_reports_unavailable_rather_than_not_found() {
        let engine = InMemoryEngine::new();
        engine.create(&spec("runner-a")).await.expect("create");
        engine.set_engine_down(true);

        assert!(matches!(
            engine.inspect("runner-a").await,
            Err(EngineError::Unavailable(_))
        ));
        assert!(matches!(
            engine.ping().await,
            Err(EngineError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn rename_moves_the_name_and_refuses_a_collision() {
        let engine = InMemoryEngine::new();
        engine.create(&spec("runner-a")).await.expect("create a");
        engine.create(&spec("runner-b")).await.expect("create b");

        engine
            .rename("runner-a", "runner-a-consumed")
            .await
            .expect("rename");
        assert_eq!(
            engine.container_names(),
            vec!["runner-a-consumed".to_string(), "runner-b".to_string()]
        );

        assert!(matches!(
            engine.rename("runner-b", "runner-a-consumed").await,
            Err(EngineError::Conflict)
        ));
    }
}
