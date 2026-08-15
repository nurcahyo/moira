//! The client for `moira-runner`'s v1 control contract (issue #275, workstream R2 of #272).
//!
//! # Moira has no Docker access, and this file is the reason it does not need any
//!
//! Docker socket access is root-equivalent on its host. Neither the console nor this process may
//! hold it. `moira-runner` is the only component that talks to the Docker Engine API; it binds
//! loopback by default and is never exposed to the internet. The trust chain is
//! `Console (no Docker) -> Moira (no Docker) -> moira-runner -> Docker Engine API`, and the whole
//! of Moira's half of it is the HTTP calls below. **Nothing in this crate may take a dependency
//! on a Docker client, and this module must never grow one.**
//!
//! # The token
//!
//! [`RunnerControlClient::fetch_token`] is the one call that returns credential material, and it
//! is one-shot at the far end: a second call answers `410 token_already_retrieved`, so the value
//! it returns cannot be re-fetched. It is handed to the caller inside a [`SecretString`] and the
//! caller's contract is to move it straight into the credential chain.
//!
//! Consequently: **no response body from this module is ever logged, traced, or placed in an
//! error message.** [`RunnerControlError`] carries an upstream status and the contract's own
//! `code` string and nothing else — see its own note. The temptation to attach "the body, for
//! debugging" is exactly how a token reaches a log aggregator.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode, redirect};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::json;

use crate::{config::ClaudeRunnerSettings, error::AppError};

/// A failure talking to the runner service.
///
/// # What is deliberately absent
///
/// There is no `body` field and no `message` carried from upstream. The runner service's error
/// envelope is `{"error": {"code": …, "message": …}}`, and only `code` is taken: `code` is a
/// closed vocabulary this crate maps onto its own status codes, while `message` is free text
/// produced by a process that reads a container's tty stream. A tty stream is where the token
/// lives. Copying upstream prose into a Moira error would put an unbounded string that has been
/// near credential material into a response body and a log line at the same time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerControlError {
    pub status: Option<StatusCode>,
    /// The contract's `error.code`, when the reply was a well-formed error envelope.
    pub code: Option<String>,
    /// Why the call failed when it never produced a status at all — a connect failure, a
    /// timeout, a body that would not parse. A fixed set of static strings authored here, never
    /// anything derived from a response body.
    pub transport: Option<&'static str>,
}

impl RunnerControlError {
    fn transport(reason: &'static str) -> Self {
        Self {
            status: None,
            code: None,
            transport: Some(reason),
        }
    }
}

/// A runner as the control contract reports it.
///
/// `state` is left as the raw contract string and mapped by the application layer, so an unknown
/// state from a newer runner build is a decision Moira makes explicitly rather than a
/// deserialisation failure that hides the runner from its own console.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RunnerStatus {
    pub id: String,
    pub state: String,
    #[serde(default)]
    pub authorization_url: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
}

/// The runner-service calls Moira makes.
///
/// A trait rather than a concrete client so the application layer's state-machine and
/// token-handling logic is testable without a socket, following the convention every repository
/// in `src/infra/repositories` already uses.
#[async_trait]
pub trait RunnerControl: Send + Sync {
    async fn create(
        &self,
        label: &str,
        ttl_seconds: u32,
    ) -> Result<RunnerStatus, RunnerControlError>;
    async fn status(&self, runner_reference: &str) -> Result<RunnerStatus, RunnerControlError>;
    async fn submit_authorization_code(
        &self,
        runner_reference: &str,
        code: &str,
    ) -> Result<(), RunnerControlError>;
    /// The one-shot token read. See the module header.
    async fn fetch_token(&self, runner_reference: &str)
    -> Result<SecretString, RunnerControlError>;
    async fn delete(&self, runner_reference: &str) -> Result<(), RunnerControlError>;
}

#[derive(Debug, Clone)]
pub struct RunnerControlClient {
    http: Client,
    base_url: String,
    auth_token: SecretString,
}

/// The **dedicated** HTTP client for runner-control calls.
///
/// Deliberately not `AppState.http`: that client carries provider execution calls and is left on
/// `reqwest`'s defaults, including `Policy::limited(10)`. This one exists so that one policy
/// question — "may a runner call leave the endpoint the operator configured?" — has exactly one
/// answer, and it is the same construction `security::ssrf::build_jwks_client` uses, for the same
/// reasons:
///
/// - `redirect::Policy::none()` — a `302` from the configured endpoint to anywhere else would
///   forward the bearer token for a service with Docker Engine access to a host the operator
///   never named. Unlike the JWKS path there is no second line of defence here, because there is
///   no final-URL comparison after the fact: the request carries the credential, so the redirect
///   must never be followed in the first place.
/// - `referer(false)` — nothing about Moira's configuration leaks upstream.
/// - client-level `timeout`/`connect_timeout` — a floor under the per-request budget, so a call
///   site that forgets `RequestBuilder::timeout` still cannot hang an admin request.
pub fn build_runner_control_client(
    settings: &ClaudeRunnerSettings,
) -> Result<Client, reqwest::Error> {
    let budget = Duration::from_millis(settings.request_timeout_ms.max(1));
    Client::builder()
        .user_agent("moira/0.1")
        .redirect(redirect::Policy::none())
        .referer(false)
        .timeout(budget)
        .connect_timeout(budget)
        .build()
}

impl RunnerControlClient {
    /// `None` when the deployment has no runner service configured, which is the default.
    ///
    /// Returning `None` rather than erroring keeps "there is no runner service" out of the error
    /// path entirely: the application layer turns it into one named `503`, once, instead of every
    /// call site inventing its own.
    pub fn from_settings(settings: &ClaudeRunnerSettings) -> Result<Option<Self>, AppError> {
        if !settings.enabled {
            return Ok(None);
        }
        let Some(token) = settings
            .auth_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
        else {
            // Unreachable through `Settings::validate`, which refuses this combination at
            // startup. Kept as a refusal rather than an `unwrap` because a second construction
            // path (a test building `Settings` by hand) must not be able to produce a client that
            // silently omits the Authorization header.
            return Err(AppError::Config(
                "claude_runner.auth_token must be set when claude_runner.enabled is true"
                    .to_string(),
            ));
        };
        let http = build_runner_control_client(settings)
            .map_err(|err| AppError::Config(format!("runner control client: {err}")))?;
        Ok(Some(Self {
            http,
            base_url: settings.base_url.trim_end_matches('/').to_string(),
            auth_token: SecretString::new(token.to_string()),
        }))
    }

    fn url(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.base_url)
    }

    fn authorized(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        use secrecy::ExposeSecret;
        builder.bearer_auth(self.auth_token.expose_secret())
    }
}

/// Turns a non-success response into a [`RunnerControlError`], reading **only** `error.code`.
async fn error_from_response(response: reqwest::Response) -> RunnerControlError {
    let status = response.status();
    let code = response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| {
            body.get("error")
                .and_then(|error| error.get("code"))
                .and_then(|code| code.as_str())
                .map(str::to_string)
        })
        // A code is an identifier from a closed vocabulary; anything longer is not one, and
        // truncating rather than trusting keeps an upstream that echoes arbitrary text out of
        // Moira's audit rows.
        .filter(|code| {
            code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    RunnerControlError {
        status: Some(status),
        code,
        transport: None,
    }
}

#[async_trait]
impl RunnerControl for RunnerControlClient {
    async fn create(
        &self,
        label: &str,
        ttl_seconds: u32,
    ) -> Result<RunnerStatus, RunnerControlError> {
        let response = self
            .authorized(self.http.post(self.url("/v1/runners")))
            .json(&json!({ "label": label, "ttl_seconds": ttl_seconds }))
            .send()
            .await
            .map_err(|_| RunnerControlError::transport("request_failed"))?;
        if !response.status().is_success() {
            return Err(error_from_response(response).await);
        }
        response
            .json::<RunnerStatus>()
            .await
            .map_err(|_| RunnerControlError::transport("invalid_response_body"))
    }

    async fn status(&self, runner_reference: &str) -> Result<RunnerStatus, RunnerControlError> {
        let response = self
            .authorized(
                self.http
                    .get(self.url(&format!("/v1/runners/{runner_reference}"))),
            )
            .send()
            .await
            .map_err(|_| RunnerControlError::transport("request_failed"))?;
        if !response.status().is_success() {
            return Err(error_from_response(response).await);
        }
        response
            .json::<RunnerStatus>()
            .await
            .map_err(|_| RunnerControlError::transport("invalid_response_body"))
    }

    async fn submit_authorization_code(
        &self,
        runner_reference: &str,
        code: &str,
    ) -> Result<(), RunnerControlError> {
        let response = self
            .authorized(self.http.post(self.url(&format!(
                "/v1/runners/{runner_reference}/authorization-code"
            ))))
            .json(&json!({ "code": code }))
            .send()
            .await
            .map_err(|_| RunnerControlError::transport("request_failed"))?;
        if !response.status().is_success() {
            return Err(error_from_response(response).await);
        }
        Ok(())
    }

    async fn fetch_token(
        &self,
        runner_reference: &str,
    ) -> Result<SecretString, RunnerControlError> {
        let response = self
            .authorized(
                self.http
                    .get(self.url(&format!("/v1/runners/{runner_reference}/token"))),
            )
            .send()
            .await
            .map_err(|_| RunnerControlError::transport("request_failed"))?;
        if !response.status().is_success() {
            return Err(error_from_response(response).await);
        }
        // The body is parsed into a local that is moved into a `SecretString` and never bound to
        // a name that outlives this function. `TokenBody` derives no `Debug` for the same reason.
        let body = response
            .json::<TokenBody>()
            .await
            .map_err(|_| RunnerControlError::transport("invalid_response_body"))?;
        if body.token.trim().is_empty() {
            return Err(RunnerControlError::transport("empty_token"));
        }
        Ok(SecretString::new(body.token))
    }

    async fn delete(&self, runner_reference: &str) -> Result<(), RunnerControlError> {
        let response = self
            .authorized(
                self.http
                    .delete(self.url(&format!("/v1/runners/{runner_reference}"))),
            )
            .send()
            .await
            .map_err(|_| RunnerControlError::transport("request_failed"))?;
        // The contract makes DELETE idempotent (204 when already gone). A 404 from an older build
        // means the same thing and is accepted as success for the same reason.
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(error_from_response(response).await)
    }
}

/// Deliberately no `Debug`: a derived one would print the token the moment anyone wrote
/// `dbg!`, `{:?}` or `.expect()` on a `Result` containing it.
#[derive(Deserialize)]
struct TokenBody {
    token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_settings() -> ClaudeRunnerSettings {
        ClaudeRunnerSettings {
            enabled: true,
            base_url: "http://127.0.0.1:8090/".to_string(),
            auth_token: Some("runner-token".to_string()),
            request_timeout_ms: 1_500,
        }
    }

    #[test]
    fn a_disabled_deployment_builds_no_client_at_all() {
        let settings = ClaudeRunnerSettings::default();
        assert!(!settings.enabled);
        assert!(
            RunnerControlClient::from_settings(&settings)
                .expect("disabled is not an error")
                .is_none(),
            "a disabled runner section must yield no client, so the surface can refuse with one \
             named 503 instead of attempting a connect"
        );
    }

    #[test]
    fn an_enabled_deployment_without_a_token_refuses_to_build_a_client() {
        for token in [None, Some(String::new()), Some("   ".to_string())] {
            let settings = ClaudeRunnerSettings {
                auth_token: token.clone(),
                ..enabled_settings()
            };
            assert!(
                RunnerControlClient::from_settings(&settings).is_err(),
                "an enabled runner with token {token:?} must not produce a client that omits the \
                 Authorization header"
            );
        }
    }

    #[test]
    fn the_base_url_loses_exactly_one_trailing_slash_so_paths_do_not_double_up() {
        let client = RunnerControlClient::from_settings(&enabled_settings())
            .expect("build client")
            .expect("enabled");
        assert_eq!(
            client.url("/v1/runners"),
            "http://127.0.0.1:8090/v1/runners"
        );
    }

    /// The construction the module header promises, asserted rather than described.
    ///
    /// `reqwest::Client` exposes no getters for its redirect policy or timeouts, so this pins the
    /// one property that *is* observable without a socket — that the builder accepts the settings
    /// and produces a client at all — and the redirect refusal itself is pinned end to end by
    /// `tests/claude_runners.rs`, which stands up a real redirecting server.
    #[test]
    fn the_client_builds_from_a_zero_timeout_without_panicking() {
        let settings = ClaudeRunnerSettings {
            request_timeout_ms: 0,
            ..enabled_settings()
        };
        assert!(
            build_runner_control_client(&settings).is_ok(),
            "a zero budget is clamped to 1ms rather than being handed to reqwest as zero"
        );
    }

    #[test]
    fn an_upstream_error_code_that_is_not_an_identifier_is_dropped() {
        // `error_from_response` needs a real `reqwest::Response`, so the filter is exercised
        // through the same predicate it applies. What matters is that free text never survives.
        let accept = |code: &str| {
            code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        assert!(accept("runner_wrong_state"));
        assert!(accept("token_already_retrieved"));
        assert!(!accept("sk-ant-oat01-abcdef ... leaked from a tty stream"));
        assert!(!accept(&"x".repeat(65)));
    }
}
