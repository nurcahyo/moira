//! The use-case layer: create a runner, observe it, feed it the pasted code, take its token
//! once, delete it, and reap what has expired.
//!
//! Everything here is generic over [`ContainerEngine`], so every behaviour below — including
//! the one-shot token semantics and the reaper — is exercised by the unit tests at the bottom
//! of this file with no Docker daemon anywhere.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use chrono::{DateTime, Utc};
use tracing::{info, warn};
use uuid::Uuid;

use super::{
    config::RunnerConfig,
    engine::{ContainerEngine, ContainerSpec, EngineError},
    scrape,
    state::{
        self, RunnerIdentity, RunnerState, RunnerView, consumed_name, container_name, runner_labels,
    },
    token::MintedToken,
};

/// Maximum accepted authorization-code length.
///
/// The measured code is on the order of 100 characters. 4 KiB is generous enough that a
/// format change does not break the service and small enough that the bound is real — the
/// value is written straight into a container's stdin, so an unbounded one is an unbounded
/// write to a process this service does not control.
const MAX_AUTHORIZATION_CODE_BYTES: usize = 4096;

/// Maximum accepted operator label length, per the frozen contract.
const MAX_LABEL_LENGTH: usize = 64;

/// What the service can refuse, mapped 1:1 onto the frozen contract's error codes by
/// [`super::http`].
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("runner not found")]
    NotFound,
    #[error("runner is {current}")]
    WrongState { current: RunnerState },
    #[error("token already retrieved")]
    TokenAlreadyRetrieved,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("docker unavailable: {0}")]
    DockerUnavailable(String),
    #[error("runner failed: {0}")]
    Failed(String),
}

impl From<EngineError> for ServiceError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Unavailable(reason) => Self::DockerUnavailable(reason),
            EngineError::NotFound => Self::NotFound,
            EngineError::Conflict => Self::Failed("container name already in use".to_string()),
            EngineError::Failed(reason) => Self::Failed(reason),
        }
    }
}

/// The runner control plane.
pub struct RunnerService<E: ContainerEngine> {
    engine: Arc<E>,
    config: Arc<RunnerConfig>,
    /// Runners whose authorization code has been written to stdin but whose outcome has not
    /// yet appeared in the tty stream.
    ///
    /// **A cache of a live observation, never the registry.** Losing it across a restart
    /// costs a transient state report and nothing else; see the [`super::state`] module docs
    /// for the full accounting of what is durable and what is not.
    exchanging: Mutex<HashSet<Uuid>>,
    /// Serialises `take_token` so two concurrent fetches cannot both observe "not yet
    /// consumed" before either has renamed the container.
    ///
    /// The durable barrier is the rename; this is what stops the read-modify-write around it
    /// from interleaving within one process. A `tokio::sync::Mutex` rather than a `std` one
    /// because the critical section awaits the engine.
    token_gate: tokio::sync::Mutex<()>,
}

impl<E: ContainerEngine> RunnerService<E> {
    pub fn new(engine: Arc<E>, config: Arc<RunnerConfig>) -> Self {
        Self {
            engine,
            config,
            exchanging: Mutex::new(HashSet::new()),
            token_gate: tokio::sync::Mutex::new(()),
        }
    }

    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    /// Whether the Docker daemon answers. Backs `GET /healthz`'s `docker` field.
    pub async fn docker_reachable(&self) -> bool {
        self.engine.ping().await.is_ok()
    }

    /// Creates and starts a runner container.
    ///
    /// The hardening the contract requires is applied by the engine implementation, not
    /// here — see [`super::docker_engine::container_create_body`] — so it cannot be varied
    /// per call.
    pub async fn create(
        &self,
        label: &str,
        ttl_seconds: Option<u64>,
    ) -> Result<RunnerView, ServiceError> {
        validate_label(label)?;
        let ttl = self.resolve_ttl(ttl_seconds)?;

        let id = Uuid::now_v7();
        let expires_at = Utc::now()
            + chrono::Duration::from_std(ttl).map_err(|error| {
                ServiceError::Invalid(format!("ttl_seconds is out of range: {error}"))
            })?;
        let name = container_name(label, id);

        let spec = ContainerSpec {
            name: name.clone(),
            image: self.config.image.clone(),
            command: self.config.command.clone(),
            labels: runner_labels(id, expires_at),
            memory_bytes: self.config.memory_bytes,
            nano_cpus: self.config.nano_cpus,
            pids_limit: self.config.pids_limit,
        };

        let container_id = self.engine.create(&spec).await?;
        if let Err(error) = self.engine.start(&container_id).await {
            // A created-but-unstarted container carries our labels, so the reaper would
            // eventually collect it — but leaving it until then means `GET /v1/runners`
            // reports a runner that will never do anything. Remove it now and report the
            // real failure.
            if let Err(cleanup) = self.engine.remove(&container_id).await {
                warn!(
                    runner_id = %id,
                    error = %cleanup,
                    "failed to remove a runner container that could not be started"
                );
            }
            return Err(error.into());
        }

        info!(runner_id = %id, %expires_at, "provisioned a runner container");

        Ok(RunnerView {
            id,
            container_id,
            container_name: name,
            state: RunnerState::Provisioning,
            authorization_url: None,
            expires_at,
            error_code: None,
            token_consumed: false,
        })
    }

    /// The current projection of one runner, derived fresh from Docker on every call.
    pub async fn view(&self, id: Uuid) -> Result<RunnerView, ServiceError> {
        let identity = self.find(id).await?;
        self.view_of(&identity).await
    }

    /// Writes the operator's pasted authorization code into the container's stdin.
    ///
    /// The payload is `code + "\r"` in a single write, exactly as the frozen contract
    /// specifies. A pty delivers CR, not LF, when a human presses Return.
    ///
    /// # The payload is settled. This path has an open defect that is NOT the payload.
    ///
    /// `code + "\r"` is measured working twice from a reference client — once on issue #272's
    /// spike and once independently against R4's hardened image under this contract's exact
    /// attach flags. **Do not vary it.** An earlier revision of this comment listed payload
    /// shapes that "did not submit" and invited the next reader to hunt for another; that
    /// framing was wrong and is recorded here so nobody repeats it.
    ///
    /// What is broken is the write itself, from this process. The text lands — the CLI echoes
    /// it back masked — and the carriage return does not take effect. The full account, the
    /// evidence that the container is blameless, and the eight things already tried are in the
    /// [`super::docker_engine`] module docs. **Read them before changing anything here.**
    ///
    /// One real bug on this side *was* found and fixed: `awaiting_authorization` used to mean
    /// only "a URL has been scraped", which let a caller submit before the CLI's reader
    /// existed. It now also requires the paste prompt — see [`super::state::derive_state`].
    /// That was necessary and is not sufficient.
    pub async fn submit_code(&self, id: Uuid, code: &str) -> Result<RunnerView, ServiceError> {
        validate_authorization_code(code)?;

        let identity = self.find(id).await?;
        let view = self.view_of(&identity).await?;
        if view.state != RunnerState::AwaitingAuthorization {
            return Err(ServiceError::WrongState {
                current: view.state,
            });
        }

        let mut payload = String::with_capacity(code.len() + 1);
        payload.push_str(code);
        payload.push('\r');
        self.engine
            .write_stdin(&identity.container_id, payload.as_bytes())
            .await?;

        self.exchanging_mut().insert(id);
        // The code itself is a single-use OAuth artifact, but it is still an authorization
        // secret, so only its length is recorded.
        info!(runner_id = %id, code_bytes = code.len(), "submitted an authorization code");

        Ok(RunnerView {
            state: RunnerState::Exchanging,
            ..view
        })
    }

    /// Yields the minted token exactly once.
    ///
    /// # The ordering is the whole guarantee
    ///
    /// The container is renamed to its consumed name **before** the token is returned. If the
    /// rename fails, the caller gets an error and no token — which is the correct direction
    /// to fail in, because the alternative is a token handed out with no durable record that
    /// it was.
    pub async fn take_token(&self, id: Uuid) -> Result<MintedToken, ServiceError> {
        let _gate = self.token_gate.lock().await;

        let identity = self.find(id).await?;
        // Checked before the state check on purpose: an expired-and-consumed runner must
        // still answer `410 token_already_retrieved`, because "you already have it" is more
        // useful and more truthful than "wrong state".
        if identity.consumed {
            return Err(ServiceError::TokenAlreadyRetrieved);
        }

        let view = self.view_of(&identity).await?;
        if view.state != RunnerState::Ready {
            return Err(ServiceError::WrongState {
                current: view.state,
            });
        }

        let transcript = self.engine.transcript(&identity.container_id).await?;
        let token =
            scrape::extract_token(&transcript, &self.config.token_prefix).ok_or_else(|| {
                // Reachable only if the transcript changed between the state derivation and
                // this read, which a running CLI can do.
                ServiceError::Failed("the runner reported ready but produced no token".to_string())
            })?;

        self.engine
            .rename(
                &identity.container_id,
                &consumed_name(&identity.container_name),
            )
            .await?;
        self.exchanging_mut().remove(&id);

        // Length only. The token itself never appears in a log line, a tracing field, or an
        // error message anywhere in this crate.
        info!(runner_id = %id, token_bytes = token.len(), "handed over a minted token");
        Ok(token)
    }

    /// Force-removes a runner. Idempotent: removing one that is already gone succeeds.
    pub async fn delete(&self, id: Uuid) -> Result<(), ServiceError> {
        match self.find(id).await {
            Ok(identity) => {
                self.engine.remove(&identity.container_id).await?;
                self.exchanging_mut().remove(&id);
                info!(runner_id = %id, "removed a runner container");
                Ok(())
            }
            Err(ServiceError::NotFound) => Ok(()),
            Err(other) => Err(other),
        }
    }

    /// Force-removes every runner past its expiry, and returns how many went.
    ///
    /// **Driven off the labels, not off anything this process remembers.** That is what makes
    /// it clean up containers orphaned by a restart or a crash: a runner this process never
    /// created is indistinguishable from one it did, which is the intent.
    pub async fn reap(&self, now: DateTime<Utc>) -> Result<usize, ServiceError> {
        let identities = self.list().await?;
        let mut reaped = 0;
        for identity in identities {
            if identity.expires_at > now {
                continue;
            }
            match self.engine.remove(&identity.container_id).await {
                Ok(()) => {
                    self.exchanging_mut().remove(&identity.id);
                    reaped += 1;
                    info!(runner_id = %identity.id, "reaped an expired runner");
                }
                Err(error) => {
                    // One stubborn container must not stop the sweep: the next tick retries.
                    warn!(
                        runner_id = %identity.id,
                        error = %error,
                        "failed to reap an expired runner"
                    );
                }
            }
        }
        Ok(reaped)
    }

    async fn view_of(&self, identity: &RunnerIdentity) -> Result<RunnerView, ServiceError> {
        let transcript = self.engine.transcript(&identity.container_id).await?;
        let code_submitted = self.exchanging_mut().contains(&identity.id);
        let (state, authorization_url, error_code) = state::derive_state(
            identity,
            &transcript,
            &self.config,
            Utc::now(),
            code_submitted,
        );

        Ok(RunnerView {
            id: identity.id,
            container_id: identity.container_id.clone(),
            container_name: identity.container_name.clone(),
            state,
            authorization_url,
            expires_at: identity.expires_at,
            error_code,
            token_consumed: identity.consumed,
        })
    }

    async fn find(&self, id: Uuid) -> Result<RunnerIdentity, ServiceError> {
        self.list()
            .await?
            .into_iter()
            .find(|identity| identity.id == id)
            .ok_or(ServiceError::NotFound)
    }

    async fn list(&self) -> Result<Vec<RunnerIdentity>, ServiceError> {
        Ok(self
            .engine
            .list_runners()
            .await?
            .iter()
            .filter_map(RunnerIdentity::from_record)
            .collect())
    }

    fn resolve_ttl(&self, ttl_seconds: Option<u64>) -> Result<Duration, ServiceError> {
        let ttl = match ttl_seconds {
            None => self.config.default_ttl,
            Some(0) => {
                return Err(ServiceError::Invalid(
                    "ttl_seconds must be greater than zero".to_string(),
                ));
            }
            Some(seconds) => Duration::from_secs(seconds),
        };
        if ttl > self.config.max_ttl {
            return Err(ServiceError::Invalid(format!(
                "ttl_seconds must not exceed {}",
                self.config.max_ttl.as_secs()
            )));
        }
        Ok(ttl)
    }

    fn exchanging_mut(&self) -> std::sync::MutexGuard<'_, HashSet<Uuid>> {
        self.exchanging
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The frozen contract's label rule: `<=64`, `[a-z0-9-]`.
///
/// Additionally the label must start with an alphanumeric, because it lands inside a Docker
/// container name and the daemon requires `/?[a-zA-Z0-9][a-zA-Z0-9_.-]+` — a leading `-`
/// would be rejected by the daemon with an opaque error instead of by this validator with a
/// useful one. The prefix `moira-runner-` already supplies a leading alphanumeric for the
/// name as a whole, so this is about keeping the label itself readable rather than about
/// legality.
pub fn validate_label(label: &str) -> Result<(), ServiceError> {
    if label.is_empty() {
        return Err(ServiceError::Invalid("label must not be empty".to_string()));
    }
    if label.len() > MAX_LABEL_LENGTH {
        return Err(ServiceError::Invalid(format!(
            "label must be at most {MAX_LABEL_LENGTH} characters"
        )));
    }
    if !label
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ServiceError::Invalid(
            "label must match [a-z0-9-]".to_string(),
        ));
    }
    if !label
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(ServiceError::Invalid(
            "label must start with a lowercase letter or a digit".to_string(),
        ));
    }
    Ok(())
}

/// Validates an authorization code before it is written into a container's stdin.
///
/// # The control-character rule is a security check, not tidiness
///
/// The write goes to a **terminal read**, and the service appends the `\r` that submits it. A
/// code containing its own `\r` or `\n` would therefore submit one line and leave the rest
/// queued as input to whatever the CLI prompts for next — a caller-controlled injection into
/// an interactive session running with the operator's credentials. Rejecting every control
/// character closes that, and also closes the smaller version of it: an embedded ESC that
/// would be interpreted as a terminal escape sequence.
pub fn validate_authorization_code(code: &str) -> Result<(), ServiceError> {
    if code.is_empty() {
        return Err(ServiceError::Invalid("code must not be empty".to_string()));
    }
    if code.len() > MAX_AUTHORIZATION_CODE_BYTES {
        return Err(ServiceError::Invalid(format!(
            "code must be at most {MAX_AUTHORIZATION_CODE_BYTES} bytes"
        )));
    }
    if code.chars().any(char::is_control) {
        return Err(ServiceError::Invalid(
            "code must not contain control characters".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{engine::InMemoryEngine, state::RunnerState};
    use std::collections::HashMap;

    /// The URL alone — what the CLI has emitted *before* its reader exists. A runner here is
    /// still `provisioning`, and a code submitted into this window would land nowhere.
    const URL_ONLY_FRAME: &str =
        "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c\r\n";

    /// The paste prompt as Ink actually lays it out: positioned with cursor-forward sequences
    /// rather than spaces, so it strips to `Pastecodehereifprompted>`. Using the real
    /// rendering here is what makes these fixtures exercise the whitespace-insensitive match
    /// instead of a prettier one that would never occur.
    const PROMPT_LINE: &str =
        "\u{1b}[2GPaste\u{1b}[8Gcode\u{1b}[13Ghere\u{1b}[18Gif\u{1b}[21Gprompted\u{1b}[30G>\r\r\n";

    /// URL **and** prompt: the only combination that means `awaiting_authorization`.
    fn awaiting_frame() -> String {
        format!("{URL_ONLY_FRAME}{PROMPT_LINE}")
    }

    const TOKEN_LINE: &str = "sk-ant-oat01-fake-aaaaaaaaaaaaaaaaaaaa\r\n";

    fn config_with(extra: &[(&str, &str)]) -> Arc<RunnerConfig> {
        let mut pairs: Vec<(String, String)> = vec![
            (
                "MOIRA_RUNNER__CONTROL_TOKEN".to_string(),
                "unit-fixture-control-aaaaaaaaaaaaaaaa".to_string(),
            ),
            (
                "MOIRA_RUNNER__IMAGE".to_string(),
                "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7f8"
                    .to_string(),
            ),
        ];
        pairs.extend(
            extra
                .iter()
                .map(|(key, value)| (format!("MOIRA_RUNNER__{key}"), (*value).to_string())),
        );
        let map: HashMap<String, String> = pairs.into_iter().collect();
        Arc::new(
            RunnerConfig::from_source(&move |key: &str| map.get(key).cloned())
                .expect("valid test configuration"),
        )
    }

    fn service(extra: &[(&str, &str)]) -> (Arc<InMemoryEngine>, RunnerService<InMemoryEngine>) {
        let engine = Arc::new(InMemoryEngine::new());
        let service = RunnerService::new(Arc::clone(&engine), config_with(extra));
        (engine, service)
    }

    /// Drives a runner all the way to `ready` without a daemon: create, print a URL, submit a
    /// code, print a token.
    async fn ready_runner(
        engine: &InMemoryEngine,
        service: &RunnerService<InMemoryEngine>,
    ) -> RunnerView {
        let created = service.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());
        service
            .submit_code(created.id, "code#state")
            .await
            .expect("submit");
        engine.set_transcript(
            &created.container_id,
            &format!("{}{TOKEN_LINE}", awaiting_frame()),
        );
        engine.set_running(&created.container_id, false);
        service.view(created.id).await.expect("view")
    }

    #[tokio::test]
    async fn create_writes_exactly_the_contract_labels_and_the_configured_limits() {
        let (engine, service) = service(&[
            ("MEMORY_BYTES", "536870912"),
            ("NANO_CPUS", "500000000"),
            ("PIDS_LIMIT", "128"),
        ]);

        let created = service.create("demo", Some(120)).await.expect("create");

        let specs = engine.created_specs();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(
            spec.labels.get(state::LABEL_MARKER).map(String::as_str),
            Some("1")
        );
        assert_eq!(
            spec.labels.get(state::LABEL_ID).map(String::as_str),
            Some(created.id.to_string().as_str())
        );
        assert!(spec.labels.contains_key(state::LABEL_EXPIRES_AT));
        assert_eq!(spec.labels.len(), 3);
        assert_eq!(spec.memory_bytes, 536_870_912);
        assert_eq!(spec.nano_cpus, 500_000_000);
        assert_eq!(spec.pids_limit, 128);
        assert_eq!(spec.command, vec!["claude", "setup-token"]);
        assert_eq!(created.state, RunnerState::Provisioning);
    }

    #[tokio::test]
    async fn a_created_runner_is_started() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");

        assert!(
            engine
                .inspect(&created.container_id)
                .await
                .expect("inspect")
                .running,
            "a container that is created but never started prints nothing, so the runner \
             would sit in `provisioning` for ever"
        );
    }

    #[tokio::test]
    async fn labels_survive_a_service_restart_because_docker_is_the_registry() {
        let (engine, first) = service(&[]);
        let created = first.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());

        // A brand-new service over the same engine — nothing carried over in memory.
        let restarted = RunnerService::new(Arc::clone(&engine), config_with(&[]));

        let view = restarted.view(created.id).await.expect("rediscovered");
        assert_eq!(view.id, created.id);
        assert_eq!(view.state, RunnerState::AwaitingAuthorization);
    }

    #[tokio::test]
    async fn ttl_is_defaulted_and_bounded() {
        // The default must be at or below the ceiling, which `RunnerConfig::validate`
        // enforces at startup — so lowering the ceiling in a test means lowering the default
        // with it rather than constructing a configuration the binary would refuse to run.
        // And the stock configuration really does default to 900s.
        let (_, stock) = service(&[]);
        let stock_ttl = (stock
            .create("demo", None)
            .await
            .expect("default ttl")
            .expires_at
            - Utc::now())
        .num_seconds();

        let (_, bounded) = service(&[("MAX_TTL_SECONDS", "600"), ("DEFAULT_TTL_SECONDS", "600")]);

        let defaulted = bounded.create("demo", None).await.expect("default ttl");
        let ttl = (defaulted.expires_at - Utc::now()).num_seconds();
        assert!((580..=600).contains(&ttl), "unexpected default ttl: {ttl}s");

        assert!(matches!(
            bounded.create("demo", Some(0)).await,
            Err(ServiceError::Invalid(_))
        ));
        assert!(matches!(
            bounded.create("demo", Some(601)).await,
            Err(ServiceError::Invalid(_))
        ));

        assert!(
            (880..=900).contains(&stock_ttl),
            "unexpected stock default ttl: {stock_ttl}s"
        );
    }

    #[tokio::test]
    async fn an_unknown_runner_is_not_found_everywhere() {
        let (_, service) = service(&[]);
        let missing = Uuid::now_v7();

        assert!(matches!(
            service.view(missing).await,
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            service.submit_code(missing, "code").await,
            Err(ServiceError::NotFound)
        ));
        assert!(matches!(
            service.take_token(missing).await,
            Err(ServiceError::NotFound)
        ));
        // DELETE is idempotent: a runner that is already gone is a success, not a 404.
        service.delete(missing).await.expect("delete is idempotent");
    }

    // ---- illegal transitions --------------------------------------------------------

    #[tokio::test]
    async fn a_code_is_refused_while_the_runner_is_still_provisioning() {
        let (_, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");

        let error = service
            .submit_code(created.id, "code#state")
            .await
            .expect_err("no authorization URL has been printed yet");

        assert!(matches!(
            error,
            ServiceError::WrongState {
                current: RunnerState::Provisioning
            }
        ));
    }

    /// The regression test for the bug the coordinator's reproduction isolated: a runner that
    /// has printed its URL but not yet its paste prompt has **no reader attached**, so a code
    /// submitted then is accepted by the daemon and lands nowhere. The operator sees a runner
    /// that swallowed their code and did nothing.
    ///
    /// `awaiting_authorization` therefore means "the prompt is on screen", not "a URL exists".
    #[tokio::test]
    async fn a_url_without_the_paste_prompt_is_not_yet_ready_for_a_code() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, URL_ONLY_FRAME);

        // The URL is reported — a console can render it early — but the state does not
        // license a write yet.
        let view = service.view(created.id).await.expect("view");
        assert_eq!(view.state, RunnerState::Provisioning);
        assert!(
            view.authorization_url
                .as_deref()
                .is_some_and(|url| url.contains("state=abc")),
            "the URL is still surfaced while the runner is not yet writable"
        );

        assert!(matches!(
            service.submit_code(created.id, "too-early").await,
            Err(ServiceError::WrongState {
                current: RunnerState::Provisioning
            })
        ));
        assert!(
            engine.stdin_writes(&created.container_id).is_empty(),
            "nothing may be written before there is a reader on the other end"
        );

        // The prompt renders; only now is the runner writable.
        engine.set_transcript(&created.container_id, &awaiting_frame());
        assert_eq!(
            service.view(created.id).await.expect("view").state,
            RunnerState::AwaitingAuthorization
        );
        service
            .submit_code(created.id, "now-fine")
            .await
            .expect("a code is accepted once the prompt exists");
    }

    #[tokio::test]
    async fn a_second_code_is_refused_once_the_runner_is_exchanging() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());
        service
            .submit_code(created.id, "first")
            .await
            .expect("first");

        let error = service
            .submit_code(created.id, "second")
            .await
            .expect_err("a second code must be refused");

        assert!(matches!(
            error,
            ServiceError::WrongState {
                current: RunnerState::Exchanging
            }
        ));
        assert_eq!(
            engine.stdin_writes(&created.container_id),
            b"first\r".to_vec(),
            "the refused code must never reach the container"
        );
    }

    #[tokio::test]
    async fn a_token_is_refused_before_the_runner_is_ready() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");

        assert!(matches!(
            service.take_token(created.id).await,
            Err(ServiceError::WrongState {
                current: RunnerState::Provisioning
            })
        ));

        engine.set_transcript(&created.container_id, &awaiting_frame());
        assert!(matches!(
            service.take_token(created.id).await,
            Err(ServiceError::WrongState {
                current: RunnerState::AwaitingAuthorization
            })
        ));
    }

    #[tokio::test]
    async fn a_failed_exchange_is_refused_with_the_failed_state() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());
        service
            .submit_code(created.id, "bogus")
            .await
            .expect("submit");
        engine.set_transcript(
            &created.container_id,
            &format!("{}OAuth error: status code 400\r\n", awaiting_frame()),
        );

        let view = service.view(created.id).await.expect("view");
        assert_eq!(view.state, RunnerState::Failed);
        assert_eq!(view.error_code, Some(state::ERROR_OAUTH_EXCHANGE_FAILED));
        assert!(matches!(
            service.take_token(created.id).await,
            Err(ServiceError::WrongState {
                current: RunnerState::Failed
            })
        ));
    }

    // ---- the paste write ------------------------------------------------------------

    #[tokio::test]
    async fn the_code_reaches_stdin_with_the_carriage_return_the_prompt_needs() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());

        let view = service
            .submit_code(created.id, "abc123#state-xyz")
            .await
            .expect("submit");

        assert_eq!(view.state, RunnerState::Exchanging);
        assert_eq!(
            engine.stdin_writes(&created.container_id),
            b"abc123#state-xyz\r".to_vec()
        );
    }

    #[tokio::test]
    async fn a_code_carrying_control_characters_is_refused_before_it_reaches_the_container() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());

        for hostile in [
            "abc\rrm -rf /",
            "abc\nsecond line",
            "abc\u{1b}[2J",
            "abc\u{0}",
        ] {
            assert!(
                matches!(
                    service.submit_code(created.id, hostile).await,
                    Err(ServiceError::Invalid(_))
                ),
                "{hostile:?} must be refused"
            );
        }
        assert!(
            engine.stdin_writes(&created.container_id).is_empty(),
            "nothing hostile may reach the interactive session"
        );

        assert!(matches!(
            service.submit_code(created.id, "").await,
            Err(ServiceError::Invalid(_))
        ));
        assert!(matches!(
            service
                .submit_code(created.id, &"a".repeat(MAX_AUTHORIZATION_CODE_BYTES + 1))
                .await,
            Err(ServiceError::Invalid(_))
        ));
    }

    // ---- one-shot -------------------------------------------------------------------

    #[tokio::test]
    async fn the_token_is_yielded_once_and_the_second_fetch_is_already_retrieved() {
        let (engine, service) = service(&[]);
        let view = ready_runner(&engine, &service).await;
        assert_eq!(view.state, RunnerState::Ready);

        let token = service.take_token(view.id).await.expect("first fetch");
        assert_eq!(token.expose(), "sk-ant-oat01-fake-aaaaaaaaaaaaaaaaaaaa");

        assert!(matches!(
            service.take_token(view.id).await,
            Err(ServiceError::TokenAlreadyRetrieved)
        ));
    }

    #[tokio::test]
    async fn the_consumed_marker_is_durable_across_a_restart() {
        let (engine, service) = service(&[]);
        let view = ready_runner(&engine, &service).await;
        service.take_token(view.id).await.expect("first fetch");

        // A brand-new service: nothing about the handover survives in memory. The marker is
        // the container's name, so the one-shot promise survives anyway — which is the whole
        // reason it is a rename rather than a HashSet.
        let restarted = RunnerService::new(Arc::clone(&engine), config_with(&[]));

        assert!(matches!(
            restarted.take_token(view.id).await,
            Err(ServiceError::TokenAlreadyRetrieved)
        ));
    }

    /// The property the gate exists for. No `sleep`: all racers are spawned and then joined,
    /// and the assertion is over the outcomes rather than over timing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_fetches_yield_exactly_one_token() {
        let (engine, service) = service(&[]);
        let view = ready_runner(&engine, &service).await;
        let service = Arc::new(service);
        let barrier = Arc::new(tokio::sync::Barrier::new(8));

        let mut racers = Vec::new();
        for _ in 0..8 {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            let id = view.id;
            racers.push(tokio::spawn(async move {
                // A barrier, not a sleep: every racer is released at the same instant and
                // the release is an acknowledgement rather than a guess.
                barrier.wait().await;
                service.take_token(id).await
            }));
        }

        let mut granted = 0;
        let mut already = 0;
        for racer in racers {
            match racer.await.expect("racer did not panic") {
                Ok(_) => granted += 1,
                Err(ServiceError::TokenAlreadyRetrieved) => already += 1,
                Err(other) => panic!("unexpected error from a racer: {other}"),
            }
        }

        assert_eq!(granted, 1, "the token must be handed out exactly once");
        assert_eq!(already, 7);
    }

    #[tokio::test]
    async fn a_failed_rename_withholds_the_token() {
        // If the durable marker cannot be written, handing the token over anyway would break
        // the one-shot promise silently. The correct direction to fail in is "no token".
        let (engine, service) = service(&[]);
        let view = ready_runner(&engine, &service).await;
        engine.set_engine_down(true);

        assert!(matches!(
            service.take_token(view.id).await,
            Err(ServiceError::DockerUnavailable(_))
        ));
    }

    // ---- expiry and reaping ---------------------------------------------------------

    #[tokio::test]
    async fn a_runner_past_its_ttl_reads_as_expired() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", Some(1)).await.expect("create");
        engine.set_transcript(&created.container_id, &awaiting_frame());

        // Time is a parameter of the derivation, so expiry is asserted by moving the clock
        // forward rather than by waiting for it.
        let identity = service.find(created.id).await.expect("identity");
        let (state, _, _) = state::derive_state(
            &identity,
            &engine
                .transcript(&created.container_id)
                .await
                .expect("transcript"),
            service.config(),
            created.expires_at + chrono::Duration::seconds(1),
            false,
        );
        assert_eq!(state, RunnerState::Expired);
    }

    #[tokio::test]
    async fn the_reaper_removes_only_what_has_expired() {
        let (engine, service) = service(&[]);
        let short = service
            .create("short", Some(1))
            .await
            .expect("create short");
        let long = service
            .create("long", Some(3600))
            .await
            .expect("create long");

        let reaped = service
            .reap(short.expires_at + chrono::Duration::seconds(1))
            .await
            .expect("reap");

        assert_eq!(reaped, 1);
        let names = engine.container_names();
        assert_eq!(names.len(), 1);
        assert!(names[0].contains(&long.id.to_string()));
        assert!(matches!(
            service.view(short.id).await,
            Err(ServiceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn the_reaper_collects_a_runner_this_process_never_created() {
        // The orphan case: a container carrying our labels, created before a restart. It is
        // indistinguishable from one we created, which is exactly the intent.
        let (engine, first) = service(&[]);
        let orphan = first.create("orphan", Some(1)).await.expect("create");

        let restarted = RunnerService::new(Arc::clone(&engine), config_with(&[]));
        let reaped = restarted
            .reap(orphan.expires_at + chrono::Duration::seconds(1))
            .await
            .expect("reap");

        assert_eq!(reaped, 1);
        assert!(engine.container_names().is_empty());
    }

    #[tokio::test]
    async fn the_reaper_never_touches_a_container_that_is_not_ours() {
        let (engine, service) = service(&[]);
        engine
            .create(&ContainerSpec {
                name: "someone-elses-database".to_string(),
                image: "sha256:00".to_string(),
                command: vec!["postgres".to_string()],
                labels: std::collections::BTreeMap::new(),
                memory_bytes: 1,
                nano_cpus: 1,
                pids_limit: 1,
            })
            .await
            .expect("create a foreign container");

        let reaped = service
            .reap(Utc::now() + chrono::Duration::days(3650))
            .await
            .expect("reap");

        assert_eq!(reaped, 0);
        assert_eq!(
            engine.container_names(),
            vec!["someone-elses-database".to_string()],
            "a container without our marker label is invisible to this service"
        );
    }

    // ---- engine failures ------------------------------------------------------------

    #[tokio::test]
    async fn a_dead_daemon_surfaces_as_docker_unavailable_not_as_not_found() {
        let (engine, service) = service(&[]);
        let created = service.create("demo", None).await.expect("create");
        engine.set_engine_down(true);

        assert!(matches!(
            service.view(created.id).await,
            Err(ServiceError::DockerUnavailable(_))
        ));
        assert!(!service.docker_reachable().await);
    }

    #[tokio::test]
    async fn a_container_that_cannot_be_started_is_removed_rather_than_left_behind() {
        // The engine accepts `create` and then goes away before `start`.
        struct StartFails(InMemoryEngine);

        #[async_trait::async_trait]
        impl ContainerEngine for StartFails {
            async fn ping(&self) -> Result<(), EngineError> {
                self.0.ping().await
            }
            async fn create(&self, spec: &ContainerSpec) -> Result<String, EngineError> {
                self.0.create(spec).await
            }
            async fn start(&self, _container: &str) -> Result<(), EngineError> {
                Err(EngineError::Failed("no such image".to_string()))
            }
            async fn list_runners(
                &self,
            ) -> Result<Vec<crate::runner::engine::ContainerRecord>, EngineError> {
                self.0.list_runners().await
            }
            async fn inspect(
                &self,
                container: &str,
            ) -> Result<crate::runner::engine::ContainerRecord, EngineError> {
                self.0.inspect(container).await
            }
            async fn transcript(
                &self,
                container: &str,
            ) -> Result<crate::runner::token::TtyTranscript, EngineError> {
                self.0.transcript(container).await
            }
            async fn write_stdin(&self, container: &str, bytes: &[u8]) -> Result<(), EngineError> {
                self.0.write_stdin(container, bytes).await
            }
            async fn rename(&self, container: &str, new_name: &str) -> Result<(), EngineError> {
                self.0.rename(container, new_name).await
            }
            async fn remove(&self, container: &str) -> Result<(), EngineError> {
                self.0.remove(container).await
            }
        }

        let engine = Arc::new(StartFails(InMemoryEngine::new()));
        let service = RunnerService::new(Arc::clone(&engine), config_with(&[]));

        assert!(matches!(
            service.create("demo", None).await,
            Err(ServiceError::Failed(_))
        ));
        assert!(
            engine.0.container_names().is_empty(),
            "a container that could not be started must not be left for the reaper"
        );
    }

    // ---- validators -----------------------------------------------------------------

    #[test]
    fn labels_follow_the_frozen_contract_grammar() {
        validate_label("demo").expect("plain");
        validate_label("seat-01").expect("with a hyphen");
        validate_label(&"a".repeat(MAX_LABEL_LENGTH)).expect("at the limit");

        for bad in ["", "Demo", "demo_1", "demo runner", "-leading", "démo"] {
            assert!(validate_label(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(validate_label(&"a".repeat(MAX_LABEL_LENGTH + 1)).is_err());
    }
}
