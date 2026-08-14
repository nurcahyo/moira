use std::{convert::Infallible, time::Duration};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::{Stream, StreamExt};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{PublicExecutionService, RequestContext},
    domain::{
        ExecutionQuery, ListResponse, OpenAiResponseCompatRequest, PublicCapabilities,
        PublicExecutionSummary, PublicListQuery, PublicModelResource, PublicResponse,
        PublicResponseRequest, PublicRouteResource, PublicSseEnvelope, PublicUsageRecord,
        UsageQuery,
    },
    error::{AppError, ErrorResponse},
};

async fn public_actor(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::security::Actor, AppError> {
    state.auth.authenticate_caller(state.pool()?, headers).await
}

#[utoipa::path(
    post,
    path = "/api/v1/responses",
    tag = "responses",
    request_body = PublicResponseRequest,
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key for non-streaming response creation"),
        ("X-Request-Id" = Option<String>, Header, description = "Optional caller-provided correlation identifier")
    ),
    responses(
        (
            status = 200,
            description = "Response completed",
            body = PublicResponse,
            headers(("X-Request-Id" = String, description = "Request correlation identifier"))
        ),
        (status = 400, description = "Malformed request", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        // A reference on the resolution chain — route, agent profile, model, credential —
        // that does not resolve. None of these is named by the caller, so this is a
        // configuration answer rather than a "you asked for a thing that isn't there":
        // `route_not_found`, `agent_profile_not_found`, `model_not_found`,
        // `no_eligible_model`, `credential_not_found`, per `failure_http_status`.
        //
        // Undeclared until now. `route_not_found` has returned `404` here since long
        // before #79 added `agent_profile_not_found` to the same family, and
        // `tests/openapi_drift.rs` could never have caught it: it compares the served
        // document to the committed one, and both were generated from this same
        // annotation. A status a handler returns but never declares is invisible to
        // every generated client, which is what makes this the one gap in the set worth
        // fixing rather than only recording.
        (status = 404, description = "A route, agent profile, model or credential on the resolution chain does not exist", body = ErrorResponse),
        (status = 409, description = "Idempotency or execution conflict", body = ErrorResponse),
        (status = 422, description = "Request violates execution policy", body = ErrorResponse),
        (status = 429, description = "Rate limit exceeded", body = ErrorResponse),
        (status = 502, description = "Upstream provider failed", body = ErrorResponse),
        (status = 503, description = "Required infrastructure is unavailable", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn create_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PublicResponseRequest>,
) -> Result<(HeaderMap, Json<PublicResponse>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = PublicExecutionService::new(&state)?
        .create_response(&actor, &ctx, request)
        .await?;
    state.metrics.record_public_response_created();
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/responses/stream",
    tag = "responses",
    request_body = PublicResponseRequest,
    params(
        ("X-Request-Id" = Option<String>, Header, description = "Optional caller-provided correlation identifier")
    ),
    responses(
        (
            status = 200,
            description = "Live server-sent event stream ending with exactly one response.completed, response.failed, or response.cancelled event; Idempotency-Key is not supported",
            body = PublicSseEnvelope,
            content_type = "text/event-stream",
            headers(("X-Request-Id" = String, description = "Request correlation identifier"))
        ),
        (status = 400, description = "Malformed request", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        // No `404` and no `409` here, and the asymmetry with `POST /api/v1/responses` is
        // real rather than an oversight. Execution failures on this operation do not
        // reach the caller as a status at all: the stream has already opened `200` by the
        // time the resolution chain is walked, so the refusal arrives as the terminal
        // `response.failed` event carrying the same code. Pinned by
        // `guards_the_public_stream_refuses_a_dangling_agent_profile_in_its_terminal_event`
        // in `tests/agent_profile_wire.rs`, which asserts `200` on the transport and reads
        // the code out of the last event.
        //
        // The statuses that remain below are the ones raised *before* the stream opens —
        // authentication, authorization, the streaming-disabled policy check, the rate
        // limit, and request validation.
        (status = 422, description = "Request violates execution policy", body = ErrorResponse),
        (status = 429, description = "Rate limit exceeded", body = ErrorResponse),
        (status = 502, description = "Upstream provider failed", body = ErrorResponse),
        (status = 503, description = "Required infrastructure is unavailable", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn stream_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PublicResponseRequest>,
) -> Result<
    (
        HeaderMap,
        Sse<impl Stream<Item = Result<Event, Infallible>>>,
    ),
    AppError,
> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let request_id = ctx.request_id.clone();
    let heartbeat_seconds = state.settings.public_api.heartbeat_seconds;
    let stream = PublicExecutionService::new(&state)?
        .stream_response(actor, ctx, request)
        .await?
        .map(|result| Ok::<_, Infallible>(sse_event(result)));
    state.metrics.record_public_stream_started();
    Ok((
        sse_headers(&request_id),
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(heartbeat_seconds.max(1)))
                .text("heartbeat"),
        ),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/responses/{response_id}",
    tag = "responses",
    params(("response_id" = String, Path, description = "Public response identifier")),
    responses(
        (
            status = 200,
            description = "Stored response",
            body = PublicResponse,
            headers(("X-Request-Id" = String, description = "Request correlation identifier"))
        ),
        (status = 400, description = "Invalid response identifier", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 404, description = "Response was not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn get_response(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(response_id): Path<String>,
) -> Result<(HeaderMap, Json<PublicResponse>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let id = parse_public_id(&response_id, "resp")?;
    let response = PublicExecutionService::new(&state)?
        .get_response(&actor, &ctx, id)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/executions/{execution_id}",
    tag = "executions",
    params(("execution_id" = String, Path, description = "Public execution identifier")),
    responses(
        (status = 200, description = "Execution summary", body = PublicExecutionSummary),
        (status = 400, description = "Invalid execution identifier", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 404, description = "Execution was not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn get_execution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(execution_id): Path<String>,
) -> Result<(HeaderMap, Json<PublicExecutionSummary>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let id = parse_public_id(&execution_id, "exec")?;
    let response = PublicExecutionService::new(&state)?
        .get_execution(&actor, &ctx, id)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/executions",
    tag = "executions",
    params(ExecutionQuery),
    responses(
        (status = 200, description = "Paginated execution summaries", body = ListResponse<PublicExecutionSummary>),
        (status = 400, description = "Invalid query", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn list_executions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ExecutionQuery>,
) -> Result<(HeaderMap, Json<ListResponse<PublicExecutionSummary>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = PublicExecutionService::new(&state)?
        .list_executions(&actor, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/usage",
    tag = "usage",
    params(UsageQuery),
    responses(
        (status = 200, description = "Paginated usage records", body = ListResponse<PublicUsageRecord>),
        (status = 400, description = "Invalid query", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn list_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageQuery>,
) -> Result<(HeaderMap, Json<ListResponse<PublicUsageRecord>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = PublicExecutionService::new(&state)?
        .list_usage(&actor, &ctx, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/models",
    tag = "discovery",
    params(PublicListQuery),
    responses(
        (status = 200, description = "Models available to the caller", body = ListResponse<PublicModelResource>),
        (status = 400, description = "Invalid query or pagination cursor", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PublicListQuery>,
) -> Result<(HeaderMap, Json<ListResponse<PublicModelResource>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = PublicExecutionService::new(&state)?
        .list_models(&actor, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/routes",
    tag = "discovery",
    params(PublicListQuery),
    responses(
        (status = 200, description = "Routes available to the caller", body = ListResponse<PublicRouteResource>),
        (status = 400, description = "Invalid query or pagination cursor", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn list_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PublicListQuery>,
) -> Result<(HeaderMap, Json<ListResponse<PublicRouteResource>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = PublicExecutionService::new(&state)?
        .list_routes(&actor, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/capabilities",
    tag = "discovery",
    responses(
        (status = 200, description = "Capabilities and limits for the caller", body = PublicCapabilities),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<PublicCapabilities>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = PublicExecutionService::new(&state)?
        .capabilities(&actor)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(response)))
}

#[utoipa::path(
    post,
    path = "/v1/responses",
    tag = "compatibility",
    description = "OpenAI Responses compatibility adapter. `text.format` is honoured: `text` maps to the native `response_format`, `json_schema` carries the caller's schema through, and `json_object` is refused with 422 `unsupported_request_option` because Moira would constrain the output to the empty object rather than to free-form JSON. Any `text` key Moira does not honour — including `verbosity` — is refused rather than ignored. Honouring a schema makes the request a structured-output request, so it becomes subject to the application's `structured_output_enabled` policy and the model's `structured_output` capability. This endpoint carries no route field and always resolves through the default route. See docs/openai-compatibility.md.",
    request_body = OpenAiResponseCompatRequest,
    responses(
        (
            status = 200,
            description = "OpenAI-compatible JSON response or live server-sent event stream ending with exactly one terminal response event",
            content(
                (PublicResponse = "application/json"),
                (PublicSseEnvelope = "text/event-stream")
            )
        ),
        (status = 400, description = "Malformed or unsupported compatibility request", body = ErrorResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Operation is not permitted", body = ErrorResponse),
        // Issue #157, item 2. One status, two unrelated meanings, and the description used to
        // name only the first: the handler raises it directly when
        // `openai_responses_compat_enabled` is off, *and* `failure_http_status` raises it for
        // every unresolved reference on the resolution chain. Since #79 that family is
        // `route_not_found`, `agent_profile_not_found`, `model_not_found`, `no_eligible_model`
        // and `credential_not_found` — none of them named by the caller, since this endpoint
        // carries no route field at all. A caller who reads only "the endpoint is disabled"
        // has no way to tell an off switch from a misconfigured default route.
        (status = 404, description = "Compatibility endpoint is disabled, or a route, agent profile, model or credential on the resolution chain does not exist", body = ErrorResponse),
        // Issue #157, item 2. `AgentProfileDisabled` maps to `CONFLICT` in
        // `failure_http_status`, which this endpoint shares by routing through
        // `PublicExecutionService` exactly as the native path does. Confirmed over the wire
        // rather than by reading — `compat_response_format` is a translation layer the native
        // path does not have, so the shared-code argument was an argument, not evidence — by
        // `compat_refuses_a_disabled_agent_profile_with_409_exactly_as_the_native_path_does`
        // in `tests/openai_compat_text_format.rs`, which drives a real disabled profile on the
        // default route through `POST /v1/responses` and reads the status off the socket.
        (status = 409, description = "Execution conflict; the resolved route names a disabled agent profile", body = ErrorResponse),
        (status = 422, description = "Request violates execution policy, or carries a `text.format` Moira will not honour", body = ErrorResponse),
        (status = 429, description = "Rate limit exceeded", body = ErrorResponse),
        (status = 502, description = "Upstream provider failed", body = ErrorResponse),
        // Issue #157, item 2. Declared on both native response operations and absent here for
        // no stated reason. Pinned by `compat_answers_503_when_the_database_is_unavailable`,
        // which drives the endpoint over a poolless `AppState` — `public_actor` asks for
        // `state.pool()` before anything else and `AppError::DatabaseUnavailable` is a `503`.
        (status = 503, description = "Required infrastructure is unavailable", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(
        ("bearerAuth" = []),
        ("systemKeyAuth" = []),
        ("consumerKeyAuth" = [])
    )
)]
pub async fn openai_responses_compat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OpenAiResponseCompatRequest>,
) -> Result<Response, AppError> {
    if !state.settings.public_api.openai_responses_compat_enabled {
        return Err(AppError::NotFound("compatibility endpoint".to_string()));
    }
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = PublicExecutionService::new(&state)?;
    let stream_requested = request.stream;
    let public_request = service.openai_compat_to_public(request)?;
    if stream_requested {
        let request_id = ctx.request_id.clone();
        let heartbeat_seconds = state.settings.public_api.heartbeat_seconds;
        let stream = service
            .stream_response(actor, ctx, public_request)
            .await?
            .map(|result| Ok::<_, Infallible>(sse_event(result)));
        state.metrics.record_public_stream_started();
        Ok((
            sse_headers(&request_id),
            Sse::new(stream).keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(heartbeat_seconds.max(1)))
                    .text("heartbeat"),
            ),
        )
            .into_response())
    } else {
        let response = service
            .create_response(&actor, &ctx, public_request)
            .await?;
        state.metrics.record_public_response_created();
        Ok((json_headers(&ctx.request_id), Json(response)).into_response())
    }
}

fn sse_event(result: Result<crate::domain::PublicSseEnvelope, AppError>) -> Event {
    match result {
        Ok(envelope) => Event::default()
            .event(envelope.event_type.clone())
            .id(envelope.sequence.to_string())
            .data(serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string())),
        Err(error) => Event::default()
            .event("response.failed")
            .data(serde_json::json!({ "error": { "code": "internal_error", "message": error.to_string() } }).to_string()),
    }
}

fn json_headers(request_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(header::HeaderName::from_static("x-request-id"), value);
    }
    headers
}

fn sse_headers(request_id: &str) -> HeaderMap {
    let mut headers = json_headers(request_id);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store"),
    );
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    headers
}

fn parse_public_id(value: &str, prefix: &str) -> Result<Uuid, AppError> {
    let expected = format!("{prefix}_");
    let raw = value
        .strip_prefix(&expected)
        .ok_or_else(|| AppError::BadRequest(format!("{prefix} id has invalid prefix")))?;
    Uuid::parse_str(raw).map_err(|_| AppError::BadRequest(format!("{prefix} id is invalid")))
}
