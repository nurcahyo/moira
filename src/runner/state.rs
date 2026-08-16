//! The runner state machine, the Docker label vocabulary that backs it, and the derivation
//! that turns "what Docker says" into "what state is this runner in".
//!
//! # Statelessness is the design constraint here
//!
//! The frozen contract requires that a service restart neither orphans a runner nor loses
//! its state, so **Docker is the registry**. Everything durable lives on the container:
//!
//! | Fact | Where it lives | Durable across a restart |
//! |---|---|---|
//! | this container is a runner | label [`LABEL_MARKER`] | yes |
//! | runner id | label [`LABEL_ID`] | yes |
//! | expiry | label [`LABEL_EXPIRES_AT`] | yes |
//! | the token has been handed out | container **name** suffix, [`CONSUMED_SUFFIX`] | yes |
//! | the authorization URL | the container's tty stream | yes |
//! | the minted token | the container's tty stream | yes |
//! | outcome (`ready` / `failed`) | the tty stream + whether the process is still alive | yes |
//! | a code has just been submitted | in-process only | **no** — see below |
//!
//! ## Why the consumed marker is a rename and not a label
//!
//! The Engine API has no operation that changes a running container's labels; labels are
//! fixed at create time. A rename is the one durable, Docker-side mutation available, and the
//! one-shot property is worth having a durable marker for — an in-memory set would let a
//! restart hand the same credential out twice, which is the exact thing "one-shot" is
//! promising not to do.
//!
//! ## The one honest gap: `exchanging`
//!
//! `exchanging` is a live-process observation with no durable representation. The moment the
//! outcome lands in the tty stream it becomes `ready` or `failed`, both of which are fully
//! derived from Docker and therefore restart-safe. In the seconds between a code being
//! submitted and its outcome appearing, a restarted service reads the runner back as
//! `awaiting_authorization`. That is a benign degradation — the exchange itself is running
//! inside the container and is unaffected, and the caller's next poll sees the real outcome —
//! but it is a deviation from a strict reading of the contract and is stated here rather than
//! left for someone to discover.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{config::RunnerConfig, engine::ContainerRecord, scrape, token::TtyTranscript};

/// Marks a container as one of ours. The reaper and the registry are both defined by it, so a
/// container on the host that lacks it is invisible to this service — which is deliberate:
/// `moira-runner` must never remove a container it did not create.
pub const LABEL_MARKER: &str = "moira.runner";
/// The only accepted value of [`LABEL_MARKER`].
pub const LABEL_MARKER_VALUE: &str = "1";
/// The runner's UUID, which is the id the whole control contract is keyed on.
pub const LABEL_ID: &str = "moira.runner.id";
/// RFC 3339 expiry. The reaper's sole input.
pub const LABEL_EXPIRES_AT: &str = "moira.runner.expires_at";

/// Prefix of every container name this service creates.
pub const NAME_PREFIX: &str = "moira-runner";
/// Suffix appended to a container's name once its token has been handed out.
pub const CONSUMED_SUFFIX: &str = "-consumed";

/// The frozen contract's state machine.
///
/// ```text
/// provisioning -> awaiting_authorization -> exchanging -> ready
///                      |                        |
///                      +------------------------+--> failed
/// any state (past ttl) --> expired
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// The container exists but has not yet printed an authorization URL.
    Provisioning,
    /// The URL is available and the service is waiting for the operator's pasted code.
    AwaitingAuthorization,
    /// A code has been written to the container's stdin and the CLI is exchanging it.
    Exchanging,
    /// A token is available. `GET /v1/runners/{id}/token` will yield it exactly once.
    Ready,
    /// The exchange failed, or the CLI exited without producing a token.
    Failed,
    /// Past its TTL. The reaper will force-remove it.
    Expired,
}

impl RunnerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::AwaitingAuthorization => "awaiting_authorization",
            Self::Exchanging => "exchanging",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }
}

impl std::fmt::Display for RunnerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `error_code` values that appear on `GET /v1/runners/{id}`.
///
/// Distinct from the HTTP error codes in the frozen contract: those describe what the control
/// plane refused, these describe what happened to the runner.
pub const ERROR_OAUTH_EXCHANGE_FAILED: &str = "oauth_exchange_failed";
pub const ERROR_RUNNER_EXITED: &str = "runner_exited";

/// What one runner looks like from the outside: the projection `GET /v1/runners/{id}`
/// serialises, plus the container identity the service needs to act on it.
#[derive(Debug, Clone)]
pub struct RunnerView {
    pub id: Uuid,
    /// Docker's container id, for the follow-up calls. Never serialised to the caller — it
    /// is a host-level handle and the caller has no use for it.
    pub container_id: String,
    /// Current container name, which carries the consumed marker.
    pub container_name: String,
    pub state: RunnerState,
    pub authorization_url: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub error_code: Option<&'static str>,
    /// Whether the token has already been handed out.
    pub token_consumed: bool,
}

/// The labels a new runner container is created with.
///
/// Exactly the three the frozen contract names, and no more: an extra label is a change to a
/// contract another process is being written against in parallel. The operator's `label` goes
/// into the container name instead, where it is visible to `docker ps` without touching the
/// agreed vocabulary.
pub fn runner_labels(id: Uuid, expires_at: DateTime<Utc>) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_MARKER.to_string(), LABEL_MARKER_VALUE.to_string()),
        (LABEL_ID.to_string(), id.to_string()),
        (
            LABEL_EXPIRES_AT.to_string(),
            expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
    ])
}

/// The container name for a new runner.
///
/// Docker requires `/?[a-zA-Z0-9][a-zA-Z0-9_.-]+`; `label` is validated to `[a-z0-9-]` by the
/// HTTP layer and the prefix supplies the leading alphanumeric, so the result is always
/// legal. The UUID makes it unique, which matters because two runners may legitimately carry
/// the same operator label.
pub fn container_name(label: &str, id: Uuid) -> String {
    format!("{NAME_PREFIX}-{label}-{id}")
}

/// The name a container is renamed to once its token has been handed out.
pub fn consumed_name(current: &str) -> String {
    if current.ends_with(CONSUMED_SUFFIX) {
        return current.to_string();
    }
    format!("{current}{CONSUMED_SUFFIX}")
}

/// Whether a container's name says its token has already been handed out.
pub fn is_consumed(name: &str) -> bool {
    name.ends_with(CONSUMED_SUFFIX)
}

/// The durable half of a runner, read back from a container's labels and name.
///
/// A container whose labels do not parse is **not** a runner: it is skipped rather than
/// force-removed, because a malformed label is more likely to be somebody else's container
/// that happens to carry our marker than it is to be ours.
#[derive(Debug, Clone)]
pub struct RunnerIdentity {
    pub id: Uuid,
    pub container_id: String,
    pub container_name: String,
    pub expires_at: DateTime<Utc>,
    pub running: bool,
    pub consumed: bool,
}

impl RunnerIdentity {
    pub fn from_record(record: &ContainerRecord) -> Option<Self> {
        if record.labels.get(LABEL_MARKER).map(String::as_str) != Some(LABEL_MARKER_VALUE) {
            return None;
        }
        let id = record.labels.get(LABEL_ID)?.parse::<Uuid>().ok()?;
        let expires_at = record
            .labels
            .get(LABEL_EXPIRES_AT)
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())?
            .with_timezone(&Utc);

        Some(Self {
            id,
            container_id: record.id.clone(),
            container_name: record.name.clone(),
            expires_at,
            running: record.running,
            consumed: is_consumed(&record.name),
        })
    }
}

/// Derives the observable state of a runner from Docker plus its tty stream.
///
/// # Ordering, and why it is what it is
///
/// 1. **Expiry first**, because the contract says so explicitly: "any state (past ttl) -->
///    expired". Note that `take_token` checks the consumed marker *before* it checks state,
///    so an expired-and-consumed runner still answers `410 token_already_retrieved` rather
///    than `409`; the one-shot promise outranks the tidiness of the state report.
/// 2. **A token in the stream means `ready`**, even after the container has exited —
///    `claude setup-token` exits as soon as it has printed one, so requiring a live process
///    would make every successful runner unreachable.
/// 3. **A failure marker means `failed`**, and so does a container that exited without ever
///    producing either a URL or a token.
/// 4. `exchanging` comes from `code_submitted`, which is in-process only. See the module docs.
/// 5. **`awaiting_authorization` needs the paste prompt, not just the URL.** This state is
///    what licenses a write into the container's stdin, so it has to mean "a reader is
///    attached". A runner that has printed its URL but not yet its prompt stays
///    `provisioning` — while still reporting the URL, so a console can render it early — and
///    `POST .../authorization-code` answers `409` for that window.
pub fn derive_state(
    identity: &RunnerIdentity,
    transcript: &TtyTranscript,
    config: &RunnerConfig,
    now: DateTime<Utc>,
    code_submitted: bool,
) -> (RunnerState, Option<String>, Option<&'static str>) {
    let url = scrape::extract_authorization_url(transcript, &config.authorization_url_prefix);
    let has_token = scrape::extract_token(transcript, &config.token_prefix).is_some();

    if now >= identity.expires_at {
        return (RunnerState::Expired, url, None);
    }
    if has_token {
        return (RunnerState::Ready, url, None);
    }
    if scrape::contains_failure_marker(transcript, &config.failure_markers) {
        return (RunnerState::Failed, url, Some(ERROR_OAUTH_EXCHANGE_FAILED));
    }
    if !identity.running {
        // The CLI is gone and produced no token. Whatever it was doing, it is not going to
        // finish; reporting `provisioning` for ever would be a lie a caller polls on.
        return (RunnerState::Failed, url, Some(ERROR_RUNNER_EXITED));
    }
    if code_submitted {
        return (RunnerState::Exchanging, url, None);
    }
    // BOTH conditions, and the second one is the load-bearing half.
    //
    // `awaiting_authorization` is what licenses `POST /v1/runners/{id}/authorization-code`, so
    // it must mean "there is a reader attached to this container's stdin", not merely "a URL
    // has appeared". The two events are close together but they are not the same event, and a
    // write into the gap is accepted by the daemon and lands nowhere — the operator sees a
    // runner that took their code and did nothing with it.
    //
    // The URL is still required as well: a caller that has the prompt but no URL has nothing
    // to authorize against.
    if url.is_some() && scrape::contains_paste_prompt(transcript, &config.paste_prompt_marker) {
        return (RunnerState::AwaitingAuthorization, url, None);
    }
    (RunnerState::Provisioning, url, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::config::RunnerConfig;
    use std::collections::HashMap;

    fn config() -> RunnerConfig {
        let pairs: HashMap<String, String> = HashMap::from([
            (
                "MOIRA_RUNNER__CONTROL_TOKEN".to_string(),
                "unit-fixture-control-aaaaaaaaaaaaaaaa".to_string(),
            ),
            (
                "MOIRA_RUNNER__IMAGE".to_string(),
                "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7f8"
                    .to_string(),
            ),
        ]);
        RunnerConfig::from_source(&move |key: &str| pairs.get(key).cloned())
            .expect("valid test configuration")
    }

    fn identity(running: bool, expires_in_secs: i64) -> RunnerIdentity {
        RunnerIdentity {
            id: Uuid::now_v7(),
            container_id: "fake-1".to_string(),
            container_name: "moira-runner-demo-1".to_string(),
            expires_at: Utc::now() + chrono::Duration::seconds(expires_in_secs),
            running,
            consumed: false,
        }
    }

    /// The URL alone: emitted before the CLI's reader exists.
    const URL_ONLY_FRAME: &str =
        "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c\r\n";

    /// The paste prompt as Ink lays it out — positioned with cursor-forward sequences rather
    /// than spaces, so it strips to `Pastecodehereifprompted>` with no whitespace at all.
    const PROMPT_LINE: &str =
        "\u{1b}[2GPaste\u{1b}[8Gcode\u{1b}[13Ghere\u{1b}[18Gif\u{1b}[21Gprompted\u{1b}[30G>\r\r\n";

    fn awaiting_frame() -> String {
        format!("{URL_ONLY_FRAME}{PROMPT_LINE}")
    }

    #[test]
    fn an_empty_stream_on_a_live_container_is_provisioning() {
        let (state, url, error) = derive_state(
            &identity(true, 900),
            &TtyTranscript::new(""),
            &config(),
            Utc::now(),
            false,
        );

        assert_eq!(state, RunnerState::Provisioning);
        assert!(url.is_none());
        assert!(error.is_none());
    }

    #[test]
    fn a_url_and_a_prompt_without_a_submitted_code_is_awaiting_authorization() {
        let (state, url, _) = derive_state(
            &identity(true, 900),
            &TtyTranscript::new(awaiting_frame()),
            &config(),
            Utc::now(),
            false,
        );

        assert_eq!(state, RunnerState::AwaitingAuthorization);
        assert!(url.expect("url").contains("state=abc"));
    }

    /// `awaiting_authorization` licenses a write into the container's stdin, so it must mean
    /// a reader is attached. A URL on its own does not: the two render close together but are
    /// not the same event, and a write into that gap is accepted by the daemon and lands
    /// nowhere.
    #[test]
    fn a_url_without_the_paste_prompt_is_still_provisioning() {
        let (state, url, _) = derive_state(
            &identity(true, 900),
            &TtyTranscript::new(URL_ONLY_FRAME),
            &config(),
            Utc::now(),
            false,
        );

        assert_eq!(state, RunnerState::Provisioning);
        assert!(
            url.is_some_and(|url| url.contains("state=abc")),
            "the URL is still reported, so a console can render it before the prompt lands"
        );
    }

    /// Ink positions the prompt's words with cursor-forward sequences instead of spaces, so
    /// the marker `Paste code here` never appears literally in the stripped stream. Matching
    /// has to be whitespace-insensitive, and this pins that it is — a naive `contains` would
    /// leave every real runner stuck in `provisioning` for ever.
    #[test]
    fn the_prompt_is_recognised_through_inks_cursor_positioned_layout() {
        assert_eq!(
            crate::runner::scrape::strip_ansi(PROMPT_LINE).trim(),
            "Pastecodehereifprompted>",
            "the literal marker is NOT present in the stripped stream"
        );

        let (state, _, _) = derive_state(
            &identity(true, 900),
            &TtyTranscript::new(awaiting_frame()),
            &config(),
            Utc::now(),
            false,
        );
        assert_eq!(state, RunnerState::AwaitingAuthorization);

        // And a plain-text build that emits real spaces must match the same marker.
        let plain = format!("{URL_ONLY_FRAME}Paste code here if prompted > \r\n");
        let (state, _, _) = derive_state(
            &identity(true, 900),
            &TtyTranscript::new(plain),
            &config(),
            Utc::now(),
            false,
        );
        assert_eq!(state, RunnerState::AwaitingAuthorization);
    }

    #[test]
    fn a_submitted_code_moves_it_to_exchanging() {
        let (state, _, _) = derive_state(
            &identity(true, 900),
            &TtyTranscript::new(awaiting_frame()),
            &config(),
            Utc::now(),
            true,
        );

        assert_eq!(state, RunnerState::Exchanging);
    }

    #[test]
    fn a_token_means_ready_even_after_the_container_exited() {
        // `claude setup-token` exits as soon as it has printed a token, so requiring a live
        // process here would make every successful runner unreachable.
        let transcript = TtyTranscript::new(
            "https://claude.com/cai/oauth/authorize?state=abc\r\nsk-ant-oat01-fake-aaaaaaaa\r\n",
        );

        let (state, _, error) = derive_state(
            &identity(false, 900),
            &transcript,
            &config(),
            Utc::now(),
            true,
        );

        assert_eq!(state, RunnerState::Ready);
        assert!(error.is_none());
    }

    #[test]
    fn a_failure_marker_means_failed_with_the_oauth_error_code() {
        let transcript = TtyTranscript::new(
            "https://claude.com/cai/oauth/authorize?state=abc\r\nOAuth error: status code 400\r\n",
        );

        let (state, _, error) = derive_state(
            &identity(true, 900),
            &transcript,
            &config(),
            Utc::now(),
            true,
        );

        assert_eq!(state, RunnerState::Failed);
        assert_eq!(error, Some(ERROR_OAUTH_EXCHANGE_FAILED));
    }

    #[test]
    fn a_container_that_exited_with_nothing_is_failed_not_provisioning() {
        let (state, _, error) = derive_state(
            &identity(false, 900),
            &TtyTranscript::new("boot\r\n"),
            &config(),
            Utc::now(),
            false,
        );

        assert_eq!(state, RunnerState::Failed);
        assert_eq!(error, Some(ERROR_RUNNER_EXITED));
    }

    #[test]
    fn expiry_outranks_every_other_state() {
        // Including `ready`: the contract's machine says "any state (past ttl) --> expired".
        let transcript = TtyTranscript::new("sk-ant-oat01-fake-aaaaaaaa\r\n");

        let (state, _, _) = derive_state(
            &identity(true, -1),
            &transcript,
            &config(),
            Utc::now(),
            true,
        );

        assert_eq!(state, RunnerState::Expired);
    }

    #[test]
    fn identity_round_trips_through_labels() {
        let id = Uuid::now_v7();
        let expires_at = Utc::now() + chrono::Duration::seconds(900);
        let record = ContainerRecord {
            id: "abc123".to_string(),
            name: container_name("demo", id),
            labels: runner_labels(id, expires_at),
            running: true,
        };

        let identity = RunnerIdentity::from_record(&record).expect("labels parse");

        assert_eq!(identity.id, id);
        assert_eq!(identity.container_id, "abc123");
        assert!(!identity.consumed);
        // Serialised to whole seconds, so compare at that resolution.
        assert_eq!(identity.expires_at.timestamp(), expires_at.timestamp());
    }

    #[test]
    fn a_container_without_our_marker_or_with_broken_labels_is_not_a_runner() {
        let id = Uuid::now_v7();
        let expires_at = Utc::now();

        let mut unmarked = ContainerRecord {
            id: "abc".to_string(),
            name: "someone-elses".to_string(),
            labels: runner_labels(id, expires_at),
            running: true,
        };
        unmarked.labels.remove(LABEL_MARKER);
        assert!(RunnerIdentity::from_record(&unmarked).is_none());

        let mut bad_id = ContainerRecord {
            id: "abc".to_string(),
            name: "x".to_string(),
            labels: runner_labels(id, expires_at),
            running: true,
        };
        bad_id
            .labels
            .insert(LABEL_ID.to_string(), "not-a-uuid".to_string());
        assert!(RunnerIdentity::from_record(&bad_id).is_none());

        let mut bad_expiry = ContainerRecord {
            id: "abc".to_string(),
            name: "x".to_string(),
            labels: runner_labels(id, expires_at),
            running: true,
        };
        bad_expiry
            .labels
            .insert(LABEL_EXPIRES_AT.to_string(), "yesterday".to_string());
        assert!(RunnerIdentity::from_record(&bad_expiry).is_none());
    }

    #[test]
    fn the_consumed_marker_is_idempotent_and_detectable() {
        let name = container_name("demo", Uuid::now_v7());
        let consumed = consumed_name(&name);

        assert!(is_consumed(&consumed));
        assert!(!is_consumed(&name));
        // Renaming twice must not produce `-consumed-consumed`; the marker has to survive a
        // retry of a call that already succeeded.
        assert_eq!(consumed_name(&consumed), consumed);
    }

    #[test]
    fn exactly_the_three_contract_labels_are_written() {
        let labels = runner_labels(Uuid::now_v7(), Utc::now());

        let mut keys: Vec<&str> = labels.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![LABEL_MARKER, LABEL_EXPIRES_AT, LABEL_ID]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            "the frozen contract names three labels; another process is being written \
             against them in parallel, so adding a fourth is a contract change"
        );
    }
}
