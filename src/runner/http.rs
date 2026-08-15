//! The Axum surface: the six routes of the frozen control contract, bearer authentication,
//! and the error envelope.
//!
//! # The envelope is deliberately not Moira's
//!
//! Moira's `ErrorResponse` carries `code`, `message_key`, `message`, `message_args`,
//! `request_id` and `details`, because its consumer is a human-facing console with an i18n
//! layer. This service's consumer is the Moira process itself, over loopback, and the frozen
//! contract specifies `{"error": {"code", "message"}}` — nothing more. Widening it to match
//! `AppError` would change a contract another workstream is implementing against in parallel;
//! narrowing `AppError` to match this would damage the API. So they stay separate, and Moira
//! maps these codes onto its own i18n keys on its side of the boundary, which is where the
//! human-facing text belongs anyway.
//!
//! # Every response is `cache-control: no-store`
//!
//! Including the error ones and including `/healthz`. One of these responses contains a live
//! OAuth token and another contains an authorization URL with a PKCE challenge in it; a
//! blanket header is the only version of this rule that cannot be forgotten on a route added
//! later.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequest, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    engine::ContainerEngine,
    service::{RunnerService, ServiceError},
    state::{RunnerState, RunnerView},
};

// ---------------------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------------------

/// The frozen contract's error codes. Every failure this service can produce is one of these.
pub mod codes {
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const RUNNER_NOT_FOUND: &str = "runner_not_found";
    pub const RUNNER_WRONG_STATE: &str = "runner_wrong_state";
    pub const TOKEN_ALREADY_RETRIEVED: &str = "token_already_retrieved";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const DOCKER_UNAVAILABLE: &str = "docker_unavailable";
    pub const RUNNER_FAILED: &str = "runner_failed";
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

/// An error on its way out of a handler.
#[derive(Debug)]
pub struct RunnerError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl RunnerError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn unauthorized() -> Self {
        // One message for every authentication failure. Distinguishing "no header" from
        // "wrong token" tells a prober which half it got right, and there is nothing a
        // legitimate caller can do with the difference.
        Self::new(
            StatusCode::UNAUTHORIZED,
            codes::UNAUTHORIZED,
            "a valid bearer token is required",
        )
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, codes::INVALID_REQUEST, message)
    }
}

impl From<ServiceError> for RunnerError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::NotFound => Self::new(
                StatusCode::NOT_FOUND,
                codes::RUNNER_NOT_FOUND,
                "no runner with that id",
            ),
            ServiceError::WrongState { current } => Self::new(
                StatusCode::CONFLICT,
                codes::RUNNER_WRONG_STATE,
                format!("runner is {current}"),
            ),
            ServiceError::TokenAlreadyRetrieved => Self::new(
                StatusCode::GONE,
                codes::TOKEN_ALREADY_RETRIEVED,
                "this runner's token has already been retrieved",
            ),
            ServiceError::Invalid(message) => Self::invalid(message),
            // The daemon's own message is not echoed: it can name host paths, socket
            // locations and container ids, none of which the caller needs and all of which
            // describe the host this service is protecting.
            ServiceError::DockerUnavailable(reason) => {
                tracing::warn!(%reason, "docker engine is unavailable");
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    codes::DOCKER_UNAVAILABLE,
                    "the container engine is unavailable",
                )
            }
            ServiceError::Failed(reason) => {
                tracing::warn!(%reason, "runner operation failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    codes::RUNNER_FAILED,
                    "the runner could not complete the operation",
                )
            }
        }
    }
}

impl IntoResponse for RunnerError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code.to_string(),
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

/// `axum::Json` with the contract's error envelope on a rejection.
///
/// The stock extractor answers a malformed body with a plain-text 400/422, which would be the
/// one response shape in this service that does not match the contract — and it is the shape
/// a caller hits first while integrating.
pub struct ContractJson<T>(pub T);

impl<S, T> FromRequest<S> for ContractJson<T>
where
    Json<T>: FromRequest<S, Rejection = axum::extract::rejection::JsonRejection>,
    S: Send + Sync,
{
    type Rejection = RunnerError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(RunnerError::invalid(rejection.body_text())),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRunnerRequest {
    pub label: String,
    /// Absent means [`super::config::RunnerConfig::default_ttl`].
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct CreateRunnerResponse {
    pub id: Uuid,
    pub state: RunnerState,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerStatusResponse {
    pub id: Uuid,
    pub state: RunnerState,
    pub authorization_url: Option<String>,
    pub expires_at: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthorizationCodeRequest {
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct AuthorizationCodeResponse {
    pub state: RunnerState,
}

/// The one response body that carries a credential.
///
/// No `Debug`: deriving it on a struct that holds a token is exactly how the value ends up in
/// a log line, and this type has no debugging value that would justify the risk.
#[derive(Serialize)]
pub struct TokenResponse {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub docker: &'static str,
}

fn rfc3339(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl From<RunnerView> for RunnerStatusResponse {
    fn from(view: RunnerView) -> Self {
        Self {
            id: view.id,
            state: view.state,
            authorization_url: view.authorization_url,
            expires_at: rfc3339(view.expires_at),
            error_code: view.error_code.map(str::to_string),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------------------

/// Shared handler state.
///
/// `Clone` is hand-written rather than derived: `#[derive(Clone)]` on a generic struct adds a
/// `E: Clone` bound, and `ContainerEngine` implementors are held behind an `Arc` precisely
/// because they are not `Clone`.
pub struct RunnerApi<E: ContainerEngine> {
    service: Arc<RunnerService<E>>,
}

impl<E: ContainerEngine> Clone for RunnerApi<E> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
        }
    }
}

/// Builds the whole control surface.
///
/// `/healthz` sits outside the authenticated group by design: it reports only liveness and
/// whether the daemon answers, and a health probe that needs a credential is a health probe
/// that gets disabled.
pub fn router<E: ContainerEngine>(service: Arc<RunnerService<E>>) -> Router {
    let api = RunnerApi { service };

    let authenticated = Router::new()
        .route("/v1/runners", post(create_runner::<E>))
        .route("/v1/runners/{id}", get(get_runner::<E>))
        .route("/v1/runners/{id}", delete(delete_runner::<E>))
        .route(
            "/v1/runners/{id}/authorization-code",
            post(submit_authorization_code::<E>),
        )
        .route("/v1/runners/{id}/token", get(take_token::<E>))
        .route_layer(middleware::from_fn_with_state(
            api.clone(),
            require_control_token::<E>,
        ));

    Router::new()
        .route("/healthz", get(healthz::<E>))
        .merge(authenticated)
        .layer(middleware::from_fn(no_store))
        .with_state(api)
}

/// Applies `cache-control: no-store` to every response, including errors and `/healthz`.
async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Bearer authentication.
///
/// The comparison is constant-time ([`super::token::ControlToken::matches`]). The scheme
/// match is ASCII-case-insensitive because RFC 7235 says the scheme token is
/// case-insensitive, and a caller sending `bearer` is a caller who read the RFC rather than
/// this file.
async fn require_control_token<E: ContainerEngine>(
    State(api): State<RunnerApi<E>>,
    request: Request,
    next: Next,
) -> Result<Response, RunnerError> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| token.trim())
                .filter(|token| !token.is_empty())
        })
        .ok_or_else(RunnerError::unauthorized)?;

    if !api.service.config().control_token.matches(presented) {
        return Err(RunnerError::unauthorized());
    }
    Ok(next.run(request).await)
}

/// Parses the `{id}` path segment.
///
/// A malformed UUID is `400 invalid_request`, not `404 runner_not_found`: the caller sent
/// something that is not an id at all, and reporting "not found" would suggest that fixing
/// the id is a matter of creating the runner.
fn parse_id(raw: &str) -> Result<Uuid, RunnerError> {
    raw.parse::<Uuid>()
        .map_err(|_| RunnerError::invalid("id must be a UUID"))
}

async fn healthz<E: ContainerEngine>(State(api): State<RunnerApi<E>>) -> Response {
    let docker = if api.service.docker_reachable().await {
        "reachable"
    } else {
        "unreachable"
    };
    // 200 either way: this is the liveness probe for *this* process. Reporting 503 because
    // the daemon is restarting would take the control plane out of rotation for a condition
    // it reports perfectly well in the body.
    Json(HealthResponse {
        status: "ok",
        docker,
    })
    .into_response()
}

async fn create_runner<E: ContainerEngine>(
    State(api): State<RunnerApi<E>>,
    ContractJson(request): ContractJson<CreateRunnerRequest>,
) -> Result<Response, RunnerError> {
    let view = api
        .service
        .create(&request.label, request.ttl_seconds)
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateRunnerResponse {
            id: view.id,
            state: view.state,
            expires_at: rfc3339(view.expires_at),
        }),
    )
        .into_response())
}

async fn get_runner<E: ContainerEngine>(
    State(api): State<RunnerApi<E>>,
    Path(id): Path<String>,
) -> Result<Response, RunnerError> {
    let view = api.service.view(parse_id(&id)?).await?;
    Ok(Json(RunnerStatusResponse::from(view)).into_response())
}

async fn submit_authorization_code<E: ContainerEngine>(
    State(api): State<RunnerApi<E>>,
    Path(id): Path<String>,
    ContractJson(request): ContractJson<AuthorizationCodeRequest>,
) -> Result<Response, RunnerError> {
    let view = api
        .service
        .submit_code(parse_id(&id)?, &request.code)
        .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(AuthorizationCodeResponse { state: view.state }),
    )
        .into_response())
}

async fn take_token<E: ContainerEngine>(
    State(api): State<RunnerApi<E>>,
    Path(id): Path<String>,
) -> Result<Response, RunnerError> {
    let token = api.service.take_token(parse_id(&id)?).await?;
    Ok(Json(TokenResponse {
        token: token.expose().to_string(),
    })
    .into_response())
}

async fn delete_runner<E: ContainerEngine>(
    State(api): State<RunnerApi<E>>,
    Path(id): Path<String>,
) -> Result<Response, RunnerError> {
    api.service.delete(parse_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_error_maps_onto_a_contract_code_and_status() {
        let cases: Vec<(ServiceError, StatusCode, &str)> = vec![
            (
                ServiceError::NotFound,
                StatusCode::NOT_FOUND,
                codes::RUNNER_NOT_FOUND,
            ),
            (
                ServiceError::WrongState {
                    current: RunnerState::Provisioning,
                },
                StatusCode::CONFLICT,
                codes::RUNNER_WRONG_STATE,
            ),
            (
                ServiceError::TokenAlreadyRetrieved,
                StatusCode::GONE,
                codes::TOKEN_ALREADY_RETRIEVED,
            ),
            (
                ServiceError::Invalid("bad".to_string()),
                StatusCode::BAD_REQUEST,
                codes::INVALID_REQUEST,
            ),
            (
                ServiceError::DockerUnavailable("socket".to_string()),
                StatusCode::SERVICE_UNAVAILABLE,
                codes::DOCKER_UNAVAILABLE,
            ),
            (
                ServiceError::Failed("boom".to_string()),
                StatusCode::INTERNAL_SERVER_ERROR,
                codes::RUNNER_FAILED,
            ),
        ];

        for (error, status, code) in cases {
            let mapped = RunnerError::from(error);
            assert_eq!(mapped.status, status);
            assert_eq!(mapped.code, code);
        }
    }

    /// The daemon's own text describes the host this service exists to protect — socket
    /// paths, container ids, sometimes a mount table. It is logged, never returned.
    #[test]
    fn daemon_and_internal_messages_are_not_echoed_to_the_caller() {
        let docker = RunnerError::from(ServiceError::DockerUnavailable(
            "permission denied while trying to connect to /var/run/docker.sock".to_string(),
        ));
        assert!(!docker.message.contains("docker.sock"));

        let failed = RunnerError::from(ServiceError::Failed(
            "container 3f2a1b died with signal 9".to_string(),
        ));
        assert!(!failed.message.contains("3f2a1b"));
    }

    #[test]
    fn states_serialise_in_the_contract_spelling() {
        for (state, expected) in [
            (RunnerState::Provisioning, "\"provisioning\""),
            (
                RunnerState::AwaitingAuthorization,
                "\"awaiting_authorization\"",
            ),
            (RunnerState::Exchanging, "\"exchanging\""),
            (RunnerState::Ready, "\"ready\""),
            (RunnerState::Failed, "\"failed\""),
            (RunnerState::Expired, "\"expired\""),
        ] {
            assert_eq!(serde_json::to_string(&state).expect("serialise"), expected);
        }
    }

    #[test]
    fn a_malformed_id_is_invalid_request_rather_than_not_found() {
        let error = parse_id("not-a-uuid").expect_err("must be refused");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, codes::INVALID_REQUEST);
    }

    #[test]
    fn the_unauthorized_message_does_not_say_which_half_was_wrong() {
        let error = RunnerError::unauthorized();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);
        // No "missing header" / "invalid token" distinction: it tells a prober which half
        // it got right and helps nobody legitimate.
        assert_eq!(error.message, "a valid bearer token is required");
    }
}
