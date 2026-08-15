//! The `bollard` implementor of [`ContainerEngine`], and the only file in this crate that may
//! import `bollard`.
//!
//! A unit test in [`super`] asserts that confinement rather than leaving it to review.
//!
//! # The container this creates, and why every flag on it is what it is
//!
//! [`container_create_body`] is a pure function so that the hardening can be asserted by a
//! unit test with no daemon — the hardening is the part of this file most worth pinning, and
//! it is the part that is otherwise only observable by running a container on a real host.
//!
//! | Setting | Value | Why |
//! |---|---|---|
//! | `Tty` | `true` | The daemon allocates the pty. Without it the Ink UI renders **zero bytes** — measured. |
//! | `OpenStdin` | `true` | The paste prompt is a terminal read; nothing can be written without it. |
//! | `StdinOnce` | `false` | `true` closes stdin after the first attach detaches, and the attach here is one short write. |
//! | `CapDrop` | `["ALL"]` | The CLI needs no capability at all. |
//! | `SecurityOpt` | `["no-new-privileges"]` | A setuid binary inside the image cannot escalate. |
//! | `NetworkDisabled` | not set | The CLI must reach `claude.com` and `platform.claude.com` to perform the exchange. |
//! | `Binds` / `Mounts` | **never set** | A bind mount is how a container escapes to the host. [`ContainerSpec`] cannot express one. |
//! | `Privileged` | `false` (default) | Stated for the reader; it is never set to anything. |
//! | `ReadonlyRootfs` | not set | `claude setup-token` writes its own config under `$HOME` inside the container. |
//! | `AutoRemove` | **false** | Load-bearing: the token is scraped out of the container's log stream *after* the process exits, so a self-removing container would delete the credential before it could be read. |
//! | `Memory` / `NanoCpus` / `PidsLimit` | from config | A runner is a fork-bomb blast radius otherwise. |
//!
//! # Reading the stream
//!
//! On a `Tty: true` container the Engine API log stream is **raw** — there is no 8-byte
//! multiplexing header, and stdout and stderr are already interleaved by the pty. `bollard`
//! reports that as [`LogOutput::Console`], but the other variants are handled too rather than
//! discarded: a daemon that ever answers with the multiplexed framing would otherwise produce
//! a silently empty transcript, and "the authorization URL never appeared" is a very
//! expensive way to discover a framing assumption was wrong.
//!
//! # Submitting an authorization code: the sequence is measured, not reasoned about
//!
//! Read this before touching [`ContainerEngine::write_stdin`], [`SUBMIT_KEY_GAP`],
//! [`ATTACH_HOLD`] or their callers. Code submission works end to end against the real image,
//! and it works because of a specific, unintuitive sequence that nine attempts converged on.
//! Every plausible simplification of it has already been tried and measured NOT to submit.
//!
//! ## What the CLI actually requires
//!
//! `claude setup-token` does **not** submit on the carriage return glued to the end of the
//! pasted text. The code appears masked at the prompt and the line just sits there — no error,
//! no timeout, the runner stays in `awaiting_authorization` until its TTL. It submits on a
//! **second, bare** carriage return delivered on the same still-open connection a few seconds
//! later. So the shipped sequence is:
//!
//! 1. attach, and start draining the read half immediately;
//! 2. wait [`ATTACH_SETTLE`] before the first byte;
//! 3. write the frozen contract's payload, `code + "\r"`, unchanged, in one write;
//! 4. wait [`SUBMIT_KEY_GAP`];
//! 5. write one bare `\r`;
//! 6. keep the connection open and draining for [`ATTACH_HOLD`].
//!
//! Verified twice, independently, against `moira-claude-runner:local` (`claude setup-token`
//! 2.1.233, Docker Desktop 29.6.2): both runs produced
//! `OAuth error: Request failed with status code 400` for a deliberately bogus code, and the
//! runner's own state machine then reported `failed` / `oauth_exchange_failed`.
//!
//! ## What does NOT work, so nobody "tidies" this into one of them
//!
//! | Sequence | Result |
//! |---|---|
//! | `code + "\r"` in one write, connection closed straight after | text lands, no submit |
//! | `code + "\r"` in one write, connection held open 30 s while draining | text lands, no submit |
//! | `code` alone, then a bare `\r` after 2 s or after 5 s | text lands, no submit |
//! | `code` and `\r` as two writes 100 ms / 250 ms apart | text lands, no submit |
//! | bracketed paste `ESC[200~…ESC[201~` | worse — the terminator is typed literally, so this CLI does not implement it |
//!
//! The third row is the one that kills the obvious theory: "deliver the carriage return as its
//! own read" is **not** sufficient on its own — the payload's own trailing `\r` has to be there
//! too. The mechanism inside the CLI was not chased further.
//!
//! ## How it was isolated, and the two theories that were eliminated on the way
//!
//! * **`bollard` is not at fault.** A hand-rolled HTTP 101 upgrade over the raw unix socket
//!   behaved identically (`HTTP/1.1 101 UPGRADED`, 38 bytes written, ending in `\r`). It was
//!   reverted rather than shipped, since it added ~150 lines of bespoke HTTP and two tokio
//!   features for no measured benefit.
//! * **The write half was never half-closed.** That was the leading theory, and a chunk count
//!   cannot test it — Docker delivers only *new* output on attach, so a quiet CLI yields zero
//!   chunks whether or not the writer is alive. What settled it: a diagnostic wrote
//!   `code + "\r"`, waited 5 s, then wrote a bare `\r` on the *same* writer. The probe write
//!   returned `Ok`, proving the write half had been alive throughout — and the container
//!   immediately performed the exchange. That single run eliminated the half-close theory and
//!   revealed the working sequence at the same time.
//!
//! ## The container config also matters, and is pinned by a test
//!
//! Containers are created **detached**, exactly as `docker run -dit` does it:
//! `AttachStdin`/`AttachStdout`/`AttachStderr` false, with `OpenStdin: true` and
//! `StdinOnce: false`. `Attach*` and `OpenStdin` read like the same thing and are not — the
//! former say "a client is attached at start time", which is a promise nothing here keeps.

use std::collections::HashMap;

use async_trait::async_trait;
use bollard::{
    Docker,
    container::{AttachContainerResults, LogOutput},
    errors::Error as BollardError,
    models::{ContainerCreateBody, HostConfig},
    query_parameters::{
        AttachContainerOptionsBuilder, CreateContainerOptionsBuilder, ListContainersOptionsBuilder,
        LogsOptionsBuilder, RemoveContainerOptionsBuilder, RenameContainerOptions,
        StartContainerOptions,
    },
};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use super::{
    engine::{ContainerEngine, ContainerRecord, ContainerSpec, EngineError},
    state::{LABEL_MARKER, LABEL_MARKER_VALUE},
    token::TtyTranscript,
};

/// How long the attach connection is left idle before the first byte is written.
///
/// # This is a property of the mechanism, not a guess
///
/// The known-good reference client for this flow (issue #272's probe, and the coordinator's
/// independent reproduction against R4's hardened image) opens the upgraded connection, waits,
/// **then** writes. Writing the instant the upgrade completes races the daemon's own wiring of
/// the connection to the container's pty: the bytes are accepted and the container's reader is
/// not yet on the other end of them.
///
/// It matches the reference client's own delay rather than being tuned. This is an interactive
/// operator flow whose previous step was a browser round trip, so 1.5 s is not a cost anyone
/// can perceive.
const ATTACH_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

/// How long the attach connection is held open after the write, still draining.
///
/// # Necessary, and — from this client — measured NOT to be sufficient
///
/// The reference client's behaviour is unambiguous: **the carriage return that submits the
/// pasted line is only acted on while the attach connection stays open.** Closing right after
/// the write discards it, even though the write succeeded and the daemon accepted the bytes.
/// That is a real property of the mechanism, so this hold exists and carries a compile-time
/// floor.
///
/// It does not, on its own, make submission work from this process. See the module docs for
/// the full 2×2 matrix; the short version is that 30 s of holding, with the read half drained
/// continuously and both facts confirmed by instrumentation, still does not submit here while
/// a Node client holding ~12 s does. Do not read this constant as "the fix" — read it as one
/// of the two things the reference client does that this client must not stop doing.
///
/// 30 s is generous against the reference client's ~12 s, because the wait is for an OAuth
/// round trip over somebody else's network. It costs one idle socket and one task per code
/// submission, and it is off the request path — [`ContainerEngine::write_stdin`] returns as
/// soon as the write is flushed.
const ATTACH_HOLD: std::time::Duration = std::time::Duration::from_secs(30);

/// The compile-time floor on [`ATTACH_HOLD`].
///
/// A runtime test can be deleted along with the behaviour it guards; this cannot. Setting the
/// hold to zero — or back to the 2 s an earlier revision of this file used and called a
/// "linger" — fails the build. The floor sits below the reference client's measured ~12 s and
/// well above the 2 s that is known not to be enough.
const _: () = assert!(
    ATTACH_HOLD.as_secs() >= 10,
    "ATTACH_HOLD must stay well above the 2s an earlier revision used: the CLI only acts on \
     the submitting carriage return while the attach connection is open"
);

/// How long to wait after the pasted line before sending the carriage return that submits it.
///
/// # This is the defect that took nine attempts to find, and the shape of it is unintuitive
///
/// `claude setup-token` does **not** submit on the carriage return that arrives glued to the
/// end of the pasted text. The code appears masked at the prompt and the line just sits there
/// — no error, no timeout, the runner stays in `awaiting_authorization` until its TTL. It
/// submits on a **second, bare** carriage return delivered on the same connection a few
/// seconds later.
///
/// The frozen contract's payload is therefore written unchanged, and then one extra `\r`
/// follows it. Both halves matter and both were measured:
///
/// | Sequence | Result |
/// |---|---|
/// | `code + "\r"` in one write | text lands, **no submit** |
/// | `code + "\r"`, then a bare `\r` after 5 s | **submits**, exchange runs |
/// | `code` alone, then `\r` after 2 s or 5 s | text lands, **no submit** |
/// | `code + "\r"` with the connection merely held open 30 s while draining | text lands, **no submit** |
///
/// That third row is the one that kills the obvious theory. "Deliver the carriage return as
/// its own read" is not sufficient — the payload's own trailing `\r` has to be there too. The
/// mechanism inside the CLI was not chased further; the sequence is reproduced exactly as
/// measured rather than reasoned about.
///
/// How it was isolated, after connection lifetime had been eliminated: with the attach held
/// open 30 s and the read half drained throughout, a diagnostic wrote `code + "\r"`, waited
/// 5 s, then wrote a bare `\r` on the *same* writer. The probe write returned `Ok` — proving
/// the write half had been alive the whole time, which is what ruled out the half-close
/// theory — and the container immediately produced
/// `OAuth error: Request failed with status code 400`.
///
/// 5 s is the measured value, and 2 s is measured *not* to work, so this is pinned at the
/// value that was verified rather than trimmed for latency. It is paid once per code
/// submission on an interactive flow whose previous step was a browser round trip.
const SUBMIT_KEY_GAP: std::time::Duration = std::time::Duration::from_millis(5000);

/// A [`ContainerEngine`] backed by the real Docker Engine API.
pub struct DockerEngine {
    docker: Docker,
}

impl std::fmt::Debug for DockerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Docker`'s own Debug can print the transport configuration, including the socket
        // path and any TLS material paths. Nothing here needs that.
        f.write_str("DockerEngine")
    }
}

impl DockerEngine {
    /// Connects to `host` when given, otherwise to whatever `DOCKER_HOST` names, otherwise to
    /// the platform default.
    ///
    /// Both paths go through `bollard`'s scheme dispatch, which picks the unix-socket,
    /// named-pipe, HTTP or SSL transport from the URI. Hard-coding one here would silently
    /// ignore an operator's `DOCKER_HOST=tcp://…`.
    ///
    /// **The explicit `host` is not decoration.** `bollard`'s fallback is
    /// `/var/run/docker.sock`, which on Docker Desktop for macOS does not exist unless the
    /// operator has enabled the compatibility symlink — see
    /// [`super::config::RunnerConfig::docker_host`] for the measurement.
    pub fn connect(host: Option<&str>) -> Result<Self, EngineError> {
        let docker = match host {
            Some(host) => Docker::connect_with_host(host),
            None => Docker::connect_with_defaults(),
        }
        .map_err(|error| EngineError::Unavailable(error.to_string()))?;

        Ok(Self { docker })
    }
}

/// Translates a `bollard` error into the coarse classification the service acts on.
///
/// The status code is authoritative where there is one; only the transport-level variants
/// fall back on the variant itself. A 404 from the daemon is the *only* thing that means "not
/// found" — a 404 inferred from a message substring would also match a daemon reporting that
/// an *image* was not found, which is a different failure with a different remedy.
fn classify(error: BollardError) -> EngineError {
    match error {
        BollardError::DockerResponseServerError {
            status_code,
            message,
        } => match status_code {
            404 => EngineError::NotFound,
            409 => EngineError::Conflict,
            500..=599 => EngineError::Unavailable(message),
            _ => EngineError::Failed(message),
        },
        // The daemon could not be reached at all: a stopped Docker Desktop, a missing socket,
        // a permission denial on the socket, a dead TCP endpoint.
        BollardError::IOError { .. }
        | BollardError::HyperResponseError { .. }
        | BollardError::HyperLegacyError { .. }
        | BollardError::HttpClientError { .. }
        | BollardError::DockerStreamError { .. } => EngineError::Unavailable(error.to_string()),
        other => EngineError::Failed(other.to_string()),
    }
}

/// Builds the create-container body for a runner.
///
/// Pure and public so the hardening in the module docs is unit-testable without a daemon —
/// see the tests at the bottom of this file, which are the actual guarantee that a future
/// edit cannot quietly add a bind mount.
pub fn container_create_body(spec: &ContainerSpec) -> ContainerCreateBody {
    ContainerCreateBody {
        image: Some(spec.image.clone()),
        cmd: Some(spec.command.clone()),
        // The three that make an interactive CLI work inside a container the caller has no
        // terminal for. See the table in the module docs.
        tty: Some(true),
        open_stdin: Some(true),
        stdin_once: Some(false),
        // `Attach*` are FALSE, and getting this wrong is what broke code submission.
        //
        // These say "a client is attached at start time", which is what `docker run` sets
        // when it is *not* given `-d`. The known-good configuration is `docker run -dit`:
        // detached, so `AttachStdin=false`, with stdin opened later by the Engine API attach
        // call itself. Setting them true makes the daemon wire the container's streams for a
        // start-time attachment that never arrives, and a later attach then delivers bytes
        // that reach the pty — the code echoes back masked — while the carriage return that
        // submits the line does not take effect.
        //
        // Measured: with these true the CLI sat at its prompt with the code visibly typed in,
        // indefinitely. With them false the same payload over the same attach performs the
        // exchange. `OpenStdin`/`StdinOnce` above are what keep stdin available; these are a
        // different thing that reads like the same thing.
        attach_stdin: Some(false),
        attach_stdout: Some(false),
        attach_stderr: Some(false),
        labels: Some(spec.labels.clone().into_iter().collect::<HashMap<_, _>>()),
        host_config: Some(HostConfig {
            memory: Some(spec.memory_bytes),
            nano_cpus: Some(spec.nano_cpus),
            pids_limit: Some(spec.pids_limit),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            privileged: Some(false),
            // Explicitly `None`, not merely omitted, so that a `..Default::default()`
            // creeping in later cannot start populating them from somewhere else.
            binds: None,
            mounts: None,
            // `AutoRemove: true` would delete the container — and its log stream — the moment
            // `claude setup-token` exits, which is precisely when the token becomes readable.
            // The reaper is what bounds the lifetime instead.
            auto_remove: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[async_trait]
impl ContainerEngine for DockerEngine {
    async fn ping(&self) -> Result<(), EngineError> {
        self.docker.ping().await.map(|_| ()).map_err(classify)
    }

    async fn create(&self, spec: &ContainerSpec) -> Result<String, EngineError> {
        let options = CreateContainerOptionsBuilder::new()
            .name(&spec.name)
            .build();
        let created = self
            .docker
            .create_container(Some(options), container_create_body(spec))
            .await
            .map_err(classify)?;
        Ok(created.id)
    }

    async fn start(&self, container: &str) -> Result<(), EngineError> {
        self.docker
            .start_container(container, None::<StartContainerOptions>)
            .await
            .map_err(classify)
    }

    async fn list_runners(&self) -> Result<Vec<ContainerRecord>, EngineError> {
        // Filtered daemon-side by the marker label: this service must never so much as
        // enumerate containers it does not own, let alone act on them.
        let filters = HashMap::from([(
            "label".to_string(),
            vec![format!("{LABEL_MARKER}={LABEL_MARKER_VALUE}")],
        )]);
        let options = ListContainersOptionsBuilder::new()
            // `all` includes exited containers, and a finished `claude setup-token` is
            // exactly that — with the token in its log stream.
            .all(true)
            .filters(&filters)
            .build();

        let summaries = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(classify)?;

        Ok(summaries
            .into_iter()
            .filter_map(|summary| {
                let id = summary.id?;
                let name = summary
                    .names
                    .and_then(|names| names.into_iter().next())
                    .map(|name| name.trim_start_matches('/').to_string())
                    .unwrap_or_default();
                let running = matches!(
                    summary.state,
                    Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
                );
                Some(ContainerRecord {
                    id,
                    name,
                    labels: summary.labels.unwrap_or_default().into_iter().collect(),
                    running,
                })
            })
            .collect())
    }

    async fn inspect(&self, container: &str) -> Result<ContainerRecord, EngineError> {
        let inspected = self
            .docker
            .inspect_container(container, None)
            .await
            .map_err(classify)?;

        Ok(ContainerRecord {
            id: inspected.id.unwrap_or_else(|| container.to_string()),
            name: inspected
                .name
                .map(|name| name.trim_start_matches('/').to_string())
                .unwrap_or_default(),
            labels: inspected
                .config
                .and_then(|config| config.labels)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            running: inspected
                .state
                .and_then(|state| state.running)
                .unwrap_or(false),
        })
    }

    async fn transcript(&self, container: &str) -> Result<TtyTranscript, EngineError> {
        let options = LogsOptionsBuilder::new()
            .stdout(true)
            .stderr(true)
            // `follow(false)`: this is a snapshot read on every poll, not a subscription. A
            // followed stream would have to be held open for the container's whole life and
            // would keep a credential-bearing buffer resident in this process between polls.
            .follow(false)
            .timestamps(false)
            .build();

        let mut stream = self.docker.logs(container, Some(options));
        let mut transcript = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk.map_err(classify)? {
                // A `Tty: true` container yields raw output, which bollard reports as
                // `Console`. The rest are handled anyway — see the module docs.
                LogOutput::Console { message }
                | LogOutput::StdOut { message }
                | LogOutput::StdErr { message }
                | LogOutput::StdIn { message } => {
                    transcript.push_str(&String::from_utf8_lossy(&message));
                }
            }
        }
        Ok(TtyTranscript::new(transcript))
    }

    async fn write_stdin(&self, container: &str, bytes: &[u8]) -> Result<(), EngineError> {
        // The flags are the frozen contract's, verbatim: `?stream=1&stdin=1&stdout=1&stderr=1`.
        // `logs` stays off, so no history is replayed.
        let options = AttachContainerOptionsBuilder::new()
            .stream(true)
            .stdin(true)
            .stdout(true)
            .stderr(true)
            .logs(false)
            .build();

        // The attach is awaited here so a refusal — no such container, daemon down — surfaces
        // to the caller instead of disappearing into a spawned task.
        let attached = self
            .docker
            .attach_container(container, Some(options))
            .await
            .map_err(classify)?;

        // THE CONNECTION OUTLIVES THIS CALL, deliberately. See [`ATTACH_HOLD`]: the CLI only
        // acts on the submitting carriage return while the attach is open, so the stream is
        // owned by a detached task that keeps draining and keeps the socket alive well past
        // the write. `write_stdin` returns as soon as the write is flushed, so
        // `POST /v1/runners/{id}/authorization-code` still answers promptly.
        let payload = bytes.to_vec();
        let (report, written) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let AttachContainerResults { mut input, output } = attached;

            // Drain from the moment of upgrade, not from after the write: the reference
            // client registers its reader first, and a socket nobody reads is a socket whose
            // peer can stall. Instrumented once and confirmed live — the stream delivers
            // chunks and does not end early.
            //
            // The bytes are dropped. On a successful exchange this stream carries the minted
            // token, so it is never accumulated, never returned and never logged;
            // `transcript` is the one place that reads a container's output for its content.
            let mut output = output;
            let pump = tokio::spawn(async move { while output.next().await.is_some() {} });

            tokio::time::sleep(ATTACH_SETTLE).await;

            // THE SEQUENCE THAT WORKS, reproduced exactly as it was measured. Do not
            // "simplify" it without re-measuring against the real image — three plausible
            // simplifications have already been tried and none of them submit.
            //
            //   1. write `code + "\r"` — the frozen contract's payload, unchanged, in one
            //      write. The text appears masked at the prompt. The line is NOT submitted.
            //   2. wait `SUBMIT_KEY_GAP`.
            //   3. write a bare `\r` on the same writer. NOW the line submits and the CLI
            //      performs the exchange.
            //
            // Measured alternatives that do NOT submit: the payload alone; the payload with
            // the trailing `\r` split off and sent separately after 2 s or 5 s; the payload
            // in one write with the connection merely held open for 30 s while draining.
            let outcome: Result<(), String> = async {
                input
                    .write_all(&payload)
                    .await
                    .map_err(|error| format!("write to container stdin: {error}"))?;
                input
                    .flush()
                    .await
                    .map_err(|error| format!("flush container stdin: {error}"))?;

                tokio::time::sleep(SUBMIT_KEY_GAP).await;

                input
                    .write_all(b"\r")
                    .await
                    .map_err(|error| format!("submit the line to container stdin: {error}"))?;
                input
                    .flush()
                    .await
                    .map_err(|error| format!("flush container stdin: {error}"))
            }
            .await;

            let succeeded = outcome.is_ok();
            // If the receiver is gone the caller was cancelled. Hold the connection anyway:
            // the container has already been written to, and abandoning it now is exactly the
            // shape of the bug this design exists to avoid.
            let _ = report.send(outcome);

            if succeeded {
                tokio::time::sleep(ATTACH_HOLD).await;
            }
            pump.abort();
            drop(input);
        });

        match written.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(EngineError::Failed(reason)),
            Err(_) => Err(EngineError::Failed(
                "the container attach task ended before it reported the write".to_string(),
            )),
        }
    }

    async fn rename(&self, container: &str, new_name: &str) -> Result<(), EngineError> {
        self.docker
            .rename_container(
                container,
                RenameContainerOptions {
                    name: new_name.to_string(),
                },
            )
            .await
            .map_err(classify)
    }

    async fn remove(&self, container: &str) -> Result<(), EngineError> {
        let options = RemoveContainerOptionsBuilder::new()
            .force(true)
            // The container has no volumes — `ContainerSpec` cannot express one — so this is
            // belt and braces against an image that declares a `VOLUME` of its own, which
            // would otherwise leak an anonymous volume per runner.
            .v(true)
            .build();

        match self.docker.remove_container(container, Some(options)).await {
            Ok(()) => Ok(()),
            // Idempotent by contract: the reaper and `DELETE /v1/runners/{id}` race by design.
            Err(error) => match classify(error) {
                EngineError::NotFound => Ok(()),
                other => Err(other),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn spec() -> ContainerSpec {
        ContainerSpec {
            name: "moira-runner-demo-0198".to_string(),
            image: "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7f8"
                .to_string(),
            command: vec!["claude".to_string(), "setup-token".to_string()],
            labels: BTreeMap::from([
                ("moira.runner".to_string(), "1".to_string()),
                ("moira.runner.id".to_string(), "0198-abc".to_string()),
            ]),
            memory_bytes: 536_870_912,
            nano_cpus: 500_000_000,
            pids_limit: 128,
        }
    }

    /// The three flags without which the whole approach does not work. Measured on issue
    /// #272: a container without `Tty` yields **zero bytes** from the Ink UI, and a container
    /// without `OpenStdin` has no stdin for the attach endpoint to write the pasted code to.
    #[test]
    fn the_container_gets_a_daemon_allocated_tty_and_an_open_stdin() {
        let body = container_create_body(&spec());

        assert_eq!(body.tty, Some(true));
        assert_eq!(body.open_stdin, Some(true));
        assert_eq!(
            body.stdin_once,
            Some(false),
            "StdinOnce=true closes stdin when the first attach detaches, and this design \
             attaches once, writes, and detaches"
        );
    }

    /// The three timings that make code submission work, guarded together.
    ///
    /// # What this catches, and what it honestly cannot
    ///
    /// It catches the regression that cost this workstream nine attempts: someone reading
    /// `write_stdin`, seeing a settle, a five-second gap and a thirty-second hold around what
    /// looks like a fire-and-forget write, and "tidying" them away. Each one is measured —
    /// see [`SUBMIT_KEY_GAP`] for the table of sequences that do *not* submit — and removing
    /// any of them silently returns the service to accepting a code and doing nothing with it,
    /// with no error anywhere.
    ///
    /// It cannot catch it *behaviourally*, and that is worth stating rather than implying
    /// otherwise. The property lives in a real CLI's reader on the far side of a real daemon.
    /// The in-memory engine has no connection to close, and even the opt-in Docker suite could
    /// not see it — its probe container runs `sh -c 'read line'`, and a POSIX `read` submits on
    /// the first newline it gets and does not care whether the writer is still attached. That
    /// suite passed green throughout the entire period this bug was live, which is exactly why
    /// it is not trusted for this.
    #[test]
    fn the_submission_timings_stay_at_their_measured_values() {
        // Measured NOT to submit at 2s; measured to submit at 5s.
        assert!(
            SUBMIT_KEY_GAP >= std::time::Duration::from_secs(5),
            "SUBMIT_KEY_GAP is {SUBMIT_KEY_GAP:?}; 2s is measured NOT to submit and 5s is \
             measured to submit, so this must not be trimmed for latency"
        );
        // The reference client's own delay before its first byte.
        assert!(ATTACH_SETTLE >= std::time::Duration::from_millis(1000));
        // The connection has to outlive the submitting carriage return by a wide margin, and
        // that return is itself SUBMIT_KEY_GAP after the payload.
        assert!(
            ATTACH_HOLD > SUBMIT_KEY_GAP,
            "the hold must outlast the gap, or the connection closes before the carriage \
             return is even sent"
        );
        assert!(ATTACH_HOLD >= std::time::Duration::from_secs(10));
    }

    /// `Attach*` and `OpenStdin` read like the same thing and are not, so the difference is
    /// pinned rather than left to whoever edits this next.
    ///
    /// The known-good configuration for this flow is `docker run -dit`: **detached**, so
    /// `AttachStdin=false`, with stdin opened later by the Engine API attach call itself.
    /// Setting these true tells the daemon to expect a client attached at start time, which
    /// never arrives.
    #[test]
    fn the_container_is_created_detached_like_docker_run_dit() {
        let body = container_create_body(&spec());

        assert_eq!(body.attach_stdin, Some(false));
        assert_eq!(body.attach_stdout, Some(false));
        assert_eq!(body.attach_stderr, Some(false));
        // And the pair that actually keeps stdin usable is still on, which is the whole point
        // of separating the two ideas.
        assert_eq!(body.open_stdin, Some(true));
        assert_eq!(body.stdin_once, Some(false));
    }

    #[test]
    fn the_container_is_hardened_exactly_as_the_contract_requires() {
        let body = container_create_body(&spec());
        let host = body.host_config.expect("a host config is always set");

        assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges".to_string()])
        );
        assert_eq!(host.privileged, Some(false));
        assert_eq!(host.memory, Some(536_870_912));
        assert_eq!(host.nano_cpus, Some(500_000_000));
        assert_eq!(host.pids_limit, Some(128));
    }

    /// The one that matters most. A bind mount is how a container reaches the host, and
    /// mounting the Docker socket into a runner would hand the container the very capability
    /// this whole service exists to keep in one process.
    #[test]
    fn no_container_ever_gets_a_bind_a_mount_or_the_docker_socket() {
        let body = container_create_body(&spec());
        let host = body
            .host_config
            .clone()
            .expect("a host config is always set");

        assert!(
            host.binds.is_none(),
            "a runner must never have a bind mount"
        );
        assert!(host.mounts.is_none(), "a runner must never have a mount");

        // The stronger version of the same assertion: the socket path must not appear
        // anywhere in the serialised body, whatever field a future edit might route it
        // through.
        let serialised = serde_json::to_string(&body).expect("serialise");
        assert!(!serialised.contains("docker.sock"));
        assert!(!serialised.contains("/var/run"));
    }

    /// `AutoRemove: true` is the plausible-looking flag that would silently destroy the
    /// credential: the token becomes readable from the log stream at the same moment the
    /// process exits, and a self-removing container deletes the stream at that instant.
    #[test]
    fn auto_remove_is_off_so_the_token_survives_the_process_exit() {
        let body = container_create_body(&spec());
        let host = body.host_config.expect("a host config is always set");

        assert_eq!(host.auto_remove, Some(false));
    }

    #[test]
    fn the_image_command_and_labels_are_passed_through_verbatim() {
        let spec = spec();
        let body = container_create_body(&spec);

        assert_eq!(body.image, Some(spec.image.clone()));
        assert_eq!(body.cmd, Some(spec.command.clone()));
        let labels = body.labels.expect("labels are always set");
        assert_eq!(labels.len(), 2);
        assert_eq!(labels.get("moira.runner").map(String::as_str), Some("1"));
    }

    #[test]
    fn a_daemon_404_is_not_found_and_a_daemon_500_is_unavailable() {
        assert!(matches!(
            classify(BollardError::DockerResponseServerError {
                status_code: 404,
                message: "no such container".to_string(),
            }),
            EngineError::NotFound
        ));
        assert!(matches!(
            classify(BollardError::DockerResponseServerError {
                status_code: 409,
                message: "name already in use".to_string(),
            }),
            EngineError::Conflict
        ));
        assert!(matches!(
            classify(BollardError::DockerResponseServerError {
                status_code: 500,
                message: "daemon is shutting down".to_string(),
            }),
            EngineError::Unavailable(_)
        ));
        assert!(matches!(
            classify(BollardError::DockerResponseServerError {
                status_code: 400,
                message: "invalid argument".to_string(),
            }),
            EngineError::Failed(_)
        ));
    }

    #[test]
    fn a_transport_failure_is_unavailable_rather_than_a_generic_failure() {
        // The realistic case: Docker Desktop is not running, so the socket is not there.
        let io = BollardError::IOError {
            err: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        assert!(matches!(classify(io), EngineError::Unavailable(_)));
    }
}
