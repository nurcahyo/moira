//! `MOIRA_RUNNER__*` configuration, and the four things it refuses to start without.
//!
//! # Why this is not `crate::config::Settings`
//!
//! `Settings` configures the API process: a database, Redis, providers, telemetry, identity.
//! `moira-runner` needs none of it and must not carry it — a control plane that holds
//! root-equivalent Docker access should not also be holding a database URL and a master key
//! it has no use for. The two processes are deployed separately and are configured
//! separately, so this is a small, flat struct read straight from the environment.
//!
//! # The four startup refusals, and why each one is fatal rather than a warning
//!
//! A warning in a service that starts anyway is a warning nobody reads. Each of these leaves
//! the deployment in a state where the *absence* of an attack is the only thing protecting
//! it, so each one refuses to start:
//!
//! 1. **The image must be content-pinned.** A mutable tag means the operator does not know
//!    what code runs inside a container that is about to hold a live credential; whoever can
//!    push to that tag chooses. See [`validate_image_reference`] for the two accepted forms.
//! 2. **A non-loopback bind without TLS is refused.** This service authenticates with a
//!    bearer token in a plaintext header. On a routable interface that token — and the minted
//!    OAuth token in the response body — are on the wire in clear. The opt-out exists for the
//!    operator who terminates TLS in front of it, and it has to be typed out.
//! 3. **The control token must be present and non-trivial.** An empty or sample token is an
//!    unauthenticated Docker socket exposed over HTTP.
//! 4. **TTLs must be sane.** A runner that never expires is a container holding a live login
//!    session for ever, and the reaper is what bounds that.

use std::{
    env::{self, VarError},
    net::SocketAddr,
    time::Duration,
};

use super::token::ControlToken;

/// Prefix for every environment variable this service reads.
///
/// The double underscore matches the API process's `MOIRA_*__*` convention (`config` crate
/// separator) so an operator reading a compose file sees one grammar, not two.
pub const ENV_PREFIX: &str = "MOIRA_RUNNER__";

/// Default listen address. Loopback, because this service is never exposed to the internet
/// and is called only by the Moira process on the same host or over a private network.
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// Minimum accepted control-token length in bytes.
///
/// 32 is not a round number chosen for looks: a 32-byte token drawn from a 62-character
/// alphabet carries ~190 bits, which stays out of reach of an online guessing attack against
/// a service that does not rate-limit its own authentication.
pub const MIN_CONTROL_TOKEN_BYTES: usize = 32;

/// Minimum number of distinct bytes in the control token.
///
/// Length alone is satisfied by `aaaaaaaa…`, which has 32 bytes and one bit of entropy.
pub const MIN_CONTROL_TOKEN_DISTINCT_BYTES: usize = 8;

/// Configuration errors are reported by message, not by variant: every one of them is fatal
/// at startup, nothing branches on them, and the operator's whole interface to them is the
/// text. A variant per refusal would be an enum whose only consumer is `Display`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Everything `moira-runner` needs to run.
///
/// No `#[derive(Debug)]`: [`control_token`](Self::control_token) holds a secret. The manual
/// impl below prints every other field and a placeholder for that one, so the whole
/// configuration can still be logged at startup — which is what an operator debugging a
/// refusal actually needs.
pub struct RunnerConfig {
    /// Address to bind. Loopback unless [`allow_insecure_bind`](Self::allow_insecure_bind).
    pub bind: SocketAddr,
    /// Bearer token every route except `GET /healthz` requires.
    pub control_token: ControlToken,
    /// Set to accept a non-loopback bind with no TLS in front. See the module docs.
    pub allow_insecure_bind: bool,
    /// Where the Docker daemon is, overriding `DOCKER_HOST`.
    ///
    /// # Why this exists rather than "just set `DOCKER_HOST`"
    ///
    /// `bollard`'s dispatching constructor falls back to `/var/run/docker.sock`, and **on
    /// Docker Desktop for macOS that path does not exist by default** — the daemon listens on
    /// `unix://$HOME/.docker/run/docker.sock` and `/var/run/docker.sock` is created only when
    /// the "allow the default Docker socket to be used" setting is enabled. Measured on this
    /// project's own development host: `docker version` reports a healthy 29.6.2 server while
    /// a bollard client fails with `Socket not found: /var/run/docker.sock`. The `docker` CLI
    /// finds it because it reads its *context*, which is a CLI concept no library implements.
    ///
    /// So an operator whose CLI works can still get an unreachable daemon here, and the fix
    /// needs to live in this service's own namespace rather than in an environment variable
    /// they have to know to discover. `DOCKER_HOST` still works when this is unset.
    pub docker_host: Option<String>,
    /// Content-pinned image reference. See [`validate_image_reference`].
    pub image: String,
    /// argv for the container. Default `["claude", "setup-token"]`.
    pub command: Vec<String>,
    /// `HostConfig.Memory`.
    pub memory_bytes: i64,
    /// `HostConfig.NanoCpus` — 1_000_000_000 is one CPU.
    pub nano_cpus: i64,
    /// `HostConfig.PidsLimit`.
    pub pids_limit: i64,
    /// TTL applied when the caller does not ask for one.
    pub default_ttl: Duration,
    /// Ceiling on a caller-supplied TTL.
    pub max_ttl: Duration,
    /// How often the reaper sweeps for expired runners.
    pub reap_interval: Duration,
    /// Prefix identifying the authorization URL in the tty stream.
    pub authorization_url_prefix: String,
    /// Text that marks the CLI's paste prompt in the tty stream.
    ///
    /// A runner only reaches `awaiting_authorization` once this is present, because the prompt
    /// — not the authorization URL — is what says a reader is attached and a write will land.
    /// Matched with whitespace removed and case folded, so it survives Ink laying the prompt
    /// out with cursor-forward sequences instead of spaces. See
    /// [`super::scrape::contains_paste_prompt`].
    pub paste_prompt_marker: String,
    /// Prefix identifying the minted token in the tty stream.
    ///
    /// Configurable because the success output was never measured — see the "What is NOT
    /// proven" section of the [module docs](super).
    pub token_prefix: String,
    /// Substrings that mark a failed OAuth exchange in the tty stream.
    pub failure_markers: Vec<String>,
}

impl std::fmt::Debug for RunnerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerConfig")
            .field("bind", &self.bind)
            .field("control_token", &self.control_token)
            .field("allow_insecure_bind", &self.allow_insecure_bind)
            .field("docker_host", &self.docker_host)
            .field("image", &self.image)
            .field("command", &self.command)
            .field("memory_bytes", &self.memory_bytes)
            .field("nano_cpus", &self.nano_cpus)
            .field("pids_limit", &self.pids_limit)
            .field("default_ttl", &self.default_ttl)
            .field("max_ttl", &self.max_ttl)
            .field("reap_interval", &self.reap_interval)
            .field("authorization_url_prefix", &self.authorization_url_prefix)
            .field("paste_prompt_marker", &self.paste_prompt_marker)
            .field("token_prefix", &self.token_prefix)
            .field("failure_markers", &self.failure_markers)
            .finish()
    }
}

impl RunnerConfig {
    /// Reads the process environment and validates the result.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&|key| match env::var(key) {
            Ok(value) => Some(value),
            Err(VarError::NotPresent) => None,
            // A non-UTF-8 value is reported as absent rather than silently coerced: the
            // alternative is a token that "is set" and can never match.
            Err(VarError::NotUnicode(_)) => None,
        })
    }

    /// The testable half of [`from_env`](Self::from_env).
    ///
    /// Every unit test in this module drives this, so no test mutates the process
    /// environment. `std::env::set_var` is process-global and `cargo test` runs test
    /// functions on threads of one process, so environment-mutating tests race each other in
    /// a way that is invisible until it is not — and in Rust 2024 `set_var` is `unsafe` for
    /// precisely that reason.
    pub fn from_source(source: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let read = |suffix: &str| source(&format!("{ENV_PREFIX}{suffix}"));

        let bind_raw = read("BIND").unwrap_or_else(|| DEFAULT_BIND.to_string());
        let bind: SocketAddr = bind_raw.trim().parse().map_err(|error| {
            ConfigError::new(format!(
                "{ENV_PREFIX}BIND is not a socket address ({bind_raw:?}): {error}"
            ))
        })?;

        let allow_insecure_bind = read_bool(&read, "ALLOW_INSECURE_BIND", false)?;

        let control_token = ControlToken::new(
            read("CONTROL_TOKEN")
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "{ENV_PREFIX}CONTROL_TOKEN is required: every route except GET /healthz \
                         is authenticated with it, and an unset token would leave a \
                         root-equivalent Docker capability unauthenticated"
                    ))
                })?
                .trim()
                .to_string(),
        );

        let docker_host = read("DOCKER_HOST")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let image = read("IMAGE")
            .ok_or_else(|| {
                ConfigError::new(format!(
                    "{ENV_PREFIX}IMAGE is required and must be content-pinned, either \
                     `registry/name@sha256:<64 hex>` or a local image id `sha256:<64 hex>`"
                ))
            })?
            .trim()
            .to_string();

        let command = match read("COMMAND") {
            Some(raw) => parse_command(&raw)?,
            None => vec!["claude".to_string(), "setup-token".to_string()],
        };

        let memory_bytes = read_i64(&read, "MEMORY_BYTES", 1024 * 1024 * 1024)?;
        let nano_cpus = read_i64(&read, "NANO_CPUS", 1_000_000_000)?;
        let pids_limit = read_i64(&read, "PIDS_LIMIT", 256)?;

        let default_ttl = Duration::from_secs(read_u64(&read, "DEFAULT_TTL_SECONDS", 900)?);
        let max_ttl = Duration::from_secs(read_u64(&read, "MAX_TTL_SECONDS", 3600)?);
        let reap_interval = Duration::from_secs(read_u64(&read, "REAP_INTERVAL_SECONDS", 30)?);

        let authorization_url_prefix = read("AUTHORIZATION_URL_PREFIX")
            .unwrap_or_else(|| "https://claude.com/cai/oauth/authorize".to_string())
            .trim()
            .to_string();
        // Deliberately short. The full rendered prompt is `Paste code here if prompted >`, but
        // matching the whole phrase would break on any wording change; the first three words
        // are what identifies it.
        let paste_prompt_marker = read("PASTE_PROMPT_MARKER")
            .unwrap_or_else(|| "Paste code here".to_string())
            .trim()
            .to_string();
        let token_prefix = read("TOKEN_PREFIX")
            .unwrap_or_else(|| "sk-ant-".to_string())
            .trim()
            .to_string();
        let failure_markers = match read("FAILURE_MARKERS") {
            Some(raw) => raw
                .split('\n')
                .map(|marker| marker.trim().to_string())
                .filter(|marker| !marker.is_empty())
                .collect(),
            None => vec!["OAuth error".to_string()],
        };

        let config = Self {
            bind,
            control_token,
            allow_insecure_bind,
            docker_host,
            image,
            command,
            memory_bytes,
            nano_cpus,
            pids_limit,
            default_ttl,
            max_ttl,
            reap_interval,
            authorization_url_prefix,
            paste_prompt_marker,
            token_prefix,
            failure_markers,
        };
        config.validate()?;
        Ok(config)
    }

    /// Every refusal in the module docs, in one place so a caller cannot construct an
    /// unvalidated config and start anyway.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_image_reference(&self.image)?;

        if !self.bind.ip().is_loopback() && !self.allow_insecure_bind {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}BIND is {} which is not a loopback address. This service \
                 authenticates with a plaintext bearer token and returns a live OAuth token in \
                 a response body, so on a routable interface without TLS in front of it both \
                 are on the wire in clear. Bind loopback, or set \
                 {ENV_PREFIX}ALLOW_INSECURE_BIND=true if you terminate TLS in front of it.",
                self.bind
            )));
        }

        if self.control_token.is_empty() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}CONTROL_TOKEN is empty"
            )));
        }
        if self.control_token.len() < MIN_CONTROL_TOKEN_BYTES {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}CONTROL_TOKEN is {} bytes; at least {MIN_CONTROL_TOKEN_BYTES} are \
                 required",
                self.control_token.len()
            )));
        }
        if self.control_token.distinct_bytes() < MIN_CONTROL_TOKEN_DISTINCT_BYTES {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}CONTROL_TOKEN uses only {} distinct bytes; at least \
                 {MIN_CONTROL_TOKEN_DISTINCT_BYTES} are required. Length alone is satisfied by a \
                 repeated character, which carries almost no entropy.",
                self.control_token.distinct_bytes()
            )));
        }
        if self.control_token.is_well_known_placeholder() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}CONTROL_TOKEN contains a well-known placeholder word. Generate a \
                 random value, for example `openssl rand -hex 32`."
            )));
        }

        if self.command.is_empty() {
            return Err(ConfigError::new(format!("{ENV_PREFIX}COMMAND is empty")));
        }
        if self.memory_bytes <= 0 {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}MEMORY_BYTES must be positive"
            )));
        }
        if self.nano_cpus <= 0 {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}NANO_CPUS must be positive"
            )));
        }
        if self.pids_limit <= 0 {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}PIDS_LIMIT must be positive"
            )));
        }

        if self.default_ttl.is_zero() || self.max_ttl.is_zero() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}DEFAULT_TTL_SECONDS and {ENV_PREFIX}MAX_TTL_SECONDS must be \
                 positive: a runner with no expiry is a container holding a live login session \
                 for ever"
            )));
        }
        if self.default_ttl > self.max_ttl {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}DEFAULT_TTL_SECONDS ({}s) exceeds {ENV_PREFIX}MAX_TTL_SECONDS ({}s)",
                self.default_ttl.as_secs(),
                self.max_ttl.as_secs()
            )));
        }
        if self.reap_interval.is_zero() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}REAP_INTERVAL_SECONDS must be positive"
            )));
        }

        if self.authorization_url_prefix.is_empty() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}AUTHORIZATION_URL_PREFIX is empty"
            )));
        }
        if self.token_prefix.is_empty() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}TOKEN_PREFIX is empty"
            )));
        }
        if self.paste_prompt_marker.is_empty() {
            return Err(ConfigError::new(format!(
                "{ENV_PREFIX}PASTE_PROMPT_MARKER is empty: without it no runner could ever \
                 reach awaiting_authorization, so no code could ever be submitted"
            )));
        }

        Ok(())
    }
}

/// Accepts the two forms that pin an image to *content*, and rejects everything that pins it
/// to a moving name.
///
/// | Form | Example | Verdict |
/// |---|---|---|
/// | registry digest | `ghcr.io/acme/claude-runner@sha256:<64 hex>` | accepted |
/// | local image id | `sha256:<64 hex>` | accepted |
/// | mutable tag | `claude-runner:2.1.233`, `claude-runner:latest` | refused |
/// | bare name | `claude-runner` | refused |
///
/// **The local-image-id form is not a loophole, it is the normal case for this project.** An
/// image built on the operator's own host by workstream R4's build script has no registry
/// digest at all — a digest is assigned by a registry on push — but it does have an image id,
/// which is the sha256 of its config blob and is every bit as content-addressed. Requiring a
/// registry digest would have forced operators to push a credential-bearing image to a
/// registry purely to satisfy a validator, which is worse for security, not better.
///
/// What is refused in both forms is the same thing: a reference whose meaning can change
/// after the operator approved it.
pub fn validate_image_reference(image: &str) -> Result<(), ConfigError> {
    let refusal = |reason: &str| {
        ConfigError::new(format!(
            "{ENV_PREFIX}IMAGE {image:?} {reason}. It must be content-pinned, either a registry \
             digest `registry/name@sha256:<64 hex>` or a local image id `sha256:<64 hex>`. A \
             mutable tag means whoever can push to that tag chooses what code runs inside a \
             container that is about to hold a live credential."
        ))
    };

    if image.is_empty() {
        return Err(refusal("is empty"));
    }
    if image.chars().any(char::is_whitespace) {
        return Err(refusal("contains whitespace"));
    }

    // Split on the LAST '@': a repository component cannot contain '@', so anything before
    // the final one is the name and anything after it is the digest.
    let (name, digest) = match image.rsplit_once('@') {
        Some((name, digest)) => {
            if name.is_empty() {
                return Err(refusal("has an empty name before the digest"));
            }
            (Some(name), digest)
        }
        None => (None, image),
    };

    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| refusal("is not digest-pinned"))?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(refusal("has a sha256 digest that is not 64 hex characters"));
    }
    // Docker digests are lowercase hex; accepting uppercase would let two spellings of the
    // same pin exist, and string comparisons elsewhere would disagree about them.
    if hex.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(refusal(
            "has an uppercase sha256 digest; digests are lowercase hex",
        ));
    }

    // A name is optional (the bare image-id form has none), but if present it must not
    // itself carry a tag after the last '/' — `name:latest@sha256:…` is legal Docker syntax
    // and the tag is ignored, but accepting it invites an operator to believe the tag is
    // what runs.
    if let Some(name) = name {
        let last_segment = name.rsplit('/').next().unwrap_or(name);
        if last_segment.contains(':') {
            return Err(refusal(
                "carries a tag as well as a digest; the tag is ignored by the daemon, so \
                 including it is misleading",
            ));
        }
    }

    Ok(())
}

/// Parses `MOIRA_RUNNER__COMMAND` in either of two spellings.
///
/// A JSON array (`["claude","setup-token"]`) is the exact form, needed the moment an argument
/// contains a space. Anything else is split on whitespace, which is what an operator types
/// for the ordinary case. There is no shell quoting: this argv goes to the Docker daemon and
/// is never handed to a shell, so inventing quoting rules here would only create a gap
/// between what the string looks like and what runs.
fn parse_command(raw: &str) -> Result<Vec<String>, ConfigError> {
    let trimmed = raw.trim();
    let parsed = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<String>>(trimmed).map_err(|error| {
            ConfigError::new(format!(
                "{ENV_PREFIX}COMMAND looks like a JSON array but did not parse as an array of \
                 strings: {error}"
            ))
        })?
    } else {
        trimmed
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    if parsed.is_empty() {
        return Err(ConfigError::new(format!(
            "{ENV_PREFIX}COMMAND is empty; the container would have nothing to run"
        )));
    }
    Ok(parsed)
}

fn read_bool(
    read: &dyn Fn(&str) -> Option<String>,
    suffix: &str,
    default: bool,
) -> Result<bool, ConfigError> {
    match read(suffix) {
        None => Ok(default),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" | "" => Ok(false),
            other => Err(ConfigError::new(format!(
                "{ENV_PREFIX}{suffix} is not a boolean ({other:?})"
            ))),
        },
    }
}

fn read_i64(
    read: &dyn Fn(&str) -> Option<String>,
    suffix: &str,
    default: i64,
) -> Result<i64, ConfigError> {
    match read(suffix) {
        None => Ok(default),
        Some(raw) => raw.trim().parse().map_err(|error| {
            ConfigError::new(format!(
                "{ENV_PREFIX}{suffix} is not an integer ({raw:?}): {error}"
            ))
        }),
    }
}

fn read_u64(
    read: &dyn Fn(&str) -> Option<String>,
    suffix: &str,
    default: u64,
) -> Result<u64, ConfigError> {
    match read(suffix) {
        None => Ok(default),
        Some(raw) => raw.trim().parse().map_err(|error| {
            ConfigError::new(format!(
                "{ENV_PREFIX}{suffix} is not a non-negative integer ({raw:?}): {error}"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GOOD_TOKEN: &str = "unit-fixture-control-aaaaaaaaaaaaaaaa";
    const GOOD_DIGEST: &str =
        "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7f8";

    fn source(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| (format!("{ENV_PREFIX}{key}"), (*value).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn minimal(extra: &[(&str, &str)]) -> Result<RunnerConfig, ConfigError> {
        let mut pairs = vec![("CONTROL_TOKEN", GOOD_TOKEN), ("IMAGE", GOOD_DIGEST)];
        pairs.extend_from_slice(extra);
        RunnerConfig::from_source(&source(&pairs))
    }

    #[test]
    fn defaults_are_loopback_and_the_documented_argv() {
        let config = minimal(&[]).expect("minimal configuration is valid");

        assert_eq!(config.bind.to_string(), DEFAULT_BIND);
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.command, vec!["claude", "setup-token"]);
        assert_eq!(config.default_ttl, Duration::from_secs(900));
        assert_eq!(config.max_ttl, Duration::from_secs(3600));
        assert!(!config.allow_insecure_bind);
        assert_eq!(config.token_prefix, "sk-ant-");
        assert_eq!(config.failure_markers, vec!["OAuth error"]);
    }

    #[test]
    fn debug_of_the_whole_config_never_prints_the_control_token() {
        let config = minimal(&[]).expect("valid");
        let rendered = format!("{config:?}");

        assert!(!rendered.contains(GOOD_TOKEN));
        assert!(rendered.contains("ControlToken(<redacted>)"));
        // Everything else must still be visible, or an operator cannot debug a refusal.
        assert!(rendered.contains("127.0.0.1:8090"));
        assert!(rendered.contains("setup-token"));
    }

    #[test]
    fn a_missing_control_token_is_fatal() {
        let error = RunnerConfig::from_source(&source(&[("IMAGE", GOOD_DIGEST)]))
            .expect_err("a missing control token must refuse to start");
        assert!(error.to_string().contains("CONTROL_TOKEN is required"));
    }

    #[test]
    fn a_short_or_trivial_or_placeholder_control_token_is_fatal() {
        let short = minimal(&[("CONTROL_TOKEN", "abc")]).expect_err("too short");
        assert!(short.to_string().contains("at least 32"));

        let trivial = minimal(&[("CONTROL_TOKEN", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")])
            .expect_err("trivial");
        assert!(trivial.to_string().contains("distinct bytes"));

        let placeholder = minimal(&[("CONTROL_TOKEN", "changeme-changeme-changeme-01234")])
            .expect_err("placeholder");
        assert!(placeholder.to_string().contains("placeholder"));
    }

    #[test]
    fn a_non_loopback_bind_is_refused_unless_explicitly_opted_out() {
        let refused =
            minimal(&[("BIND", "0.0.0.0:8090")]).expect_err("non-loopback without opt-out");
        assert!(refused.to_string().contains("ALLOW_INSECURE_BIND"));

        let allowed = minimal(&[("BIND", "0.0.0.0:8090"), ("ALLOW_INSECURE_BIND", "true")])
            .expect("explicit opt-out is honoured");
        assert!(allowed.allow_insecure_bind);

        // IPv6 loopback must be accepted without the opt-out, or the check is an IPv4 check
        // wearing a loopback name.
        let ipv6 = minimal(&[("BIND", "[::1]:8090")]).expect("::1 is loopback");
        assert!(ipv6.bind.ip().is_loopback());
    }

    #[test]
    fn ttl_bounds_are_enforced() {
        assert!(
            minimal(&[("DEFAULT_TTL_SECONDS", "0")])
                .expect_err("zero default ttl")
                .to_string()
                .contains("must be positive")
        );
        assert!(
            minimal(&[("DEFAULT_TTL_SECONDS", "7200")])
                .expect_err("default above max")
                .to_string()
                .contains("exceeds")
        );
    }

    #[test]
    fn command_parses_as_json_or_as_whitespace_separated_argv() {
        let json =
            minimal(&[("COMMAND", r#"["claude","setup-token","--verbose"]"#)]).expect("json");
        assert_eq!(json.command, vec!["claude", "setup-token", "--verbose"]);

        let words = minimal(&[("COMMAND", "  claude   setup-token  ")]).expect("words");
        assert_eq!(words.command, vec!["claude", "setup-token"]);

        assert!(minimal(&[("COMMAND", "   ")]).is_err());
        assert!(minimal(&[("COMMAND", "[not json")]).is_err());
    }

    // ---- image pinning -------------------------------------------------------------
    //
    // The corrected rule (issue #272, sent after the first draft of the contract): BOTH a
    // registry digest and a bare local image id are content-pinned and must be accepted,
    // because R4 builds the image locally and a locally built image has no registry digest
    // until it is pushed.

    #[test]
    fn a_registry_digest_is_accepted() {
        let image = format!("ghcr.io/acme/moira-claude-runner@{GOOD_DIGEST}");
        validate_image_reference(&image).expect("a registry digest is content-pinned");
        // With a port on the registry host, which puts a ':' in the name.
        let ported = format!("registry.internal:5000/acme/runner@{GOOD_DIGEST}");
        validate_image_reference(&ported).expect("a registry port is not a tag");
    }

    #[test]
    fn a_bare_local_image_id_is_accepted() {
        validate_image_reference(GOOD_DIGEST)
            .expect("a local image id is content-pinned; it simply has no registry name");
    }

    #[test]
    fn mutable_tags_and_bare_names_are_refused() {
        for image in [
            "moira-claude-runner",
            "moira-claude-runner:latest",
            "moira-claude-runner:2.1.233",
            "ghcr.io/acme/moira-claude-runner:latest",
            "sha256:short",
            "sha256:",
            "",
        ] {
            assert!(
                validate_image_reference(image).is_err(),
                "{image:?} is not content-pinned and must be refused"
            );
        }
    }

    #[test]
    fn a_malformed_digest_is_refused() {
        // 63 hex characters.
        let short = "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7f";
        assert!(validate_image_reference(short).is_err());
        // A non-hex character in the right length.
        let nonhex = "sha256:8f6e4c1a2b3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6e7fZ";
        assert!(validate_image_reference(nonhex).is_err());
        // Uppercase.
        let upper = "sha256:8F6E4C1A2B3D5E7F90A1B2C3D4E5F60718293A4B5C6D7E8F9012A3B4C5D6E7F8";
        assert!(validate_image_reference(upper).is_err());
        // Tag AND digest: legal Docker syntax, but the tag is ignored, so accepting it
        // invites the operator to believe the tag is what runs.
        let both = format!("ghcr.io/acme/runner:latest@{GOOD_DIGEST}");
        assert!(validate_image_reference(&both).is_err());
        // Empty name before the digest.
        let empty_name = format!("@{GOOD_DIGEST}");
        assert!(validate_image_reference(&empty_name).is_err());
        // Whitespace anywhere.
        assert!(validate_image_reference(&format!("{GOOD_DIGEST} ")).is_err());
    }

    #[test]
    fn an_unpinned_image_is_fatal_at_startup_not_merely_at_the_validator() {
        let error = RunnerConfig::from_source(&source(&[
            ("CONTROL_TOKEN", GOOD_TOKEN),
            ("IMAGE", "moira-claude-runner:latest"),
        ]))
        .expect_err("a mutable tag must refuse to start");
        assert!(error.to_string().contains("content-pinned"));
    }
}
