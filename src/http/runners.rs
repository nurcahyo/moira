//! The `/api/v1/admin/runners` surface (issue #275, workstream R2 of #272).
//!
//! Handlers stay thin, exactly like `src/http/admin.rs`: authenticate through the shared
//! [`admin_actor`] wrapper, build a [`RequestContext`], delegate to
//! [`ClaudeRunnerService`], map the result. **Every authorization check lives in the
//! application layer**, not here — see `src/application/admin/credentials.rs` for the precedent
//! and `src/application/runners.rs` for these.
//!
//! # No response on this surface can carry a token
//!
//! Every handler below returns [`ClaudeRunnerRecord`], `ListResponse<ClaudeRunnerRecord>`, or
//! nothing. That record has no token-shaped field and a unit test in `src/domain/runners.rs`
//! asserts the generated schema still has none. Adding a handler here that returns anything else
//! is how that property would be lost.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{ClaudeRunnerService, RequestContext},
    domain::{
        ClaudeRunnerAuthorizationCodeRequest, ClaudeRunnerFinalizeRequest,
        ClaudeRunnerProvisionRequest, ClaudeRunnerRecord, ListResponse, PageQuery,
    },
    error::{AppError, ErrorResponse},
    http::admin::{admin_actor, etag_headers, require_if_match},
};

#[utoipa::path(
    post, path = "/api/v1/admin/runners", tag = "admin-runners",
    request_body = ClaudeRunnerProvisionRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Runner provisioned. `scope` fixes whose Claude account this runner is for and cannot be changed afterwards — it is sealed into the resulting credential's AAD. Absent means the platform-wide (`global`) account; a tenant connecting its own subscription sends `{\"type\": \"tenant\", \"external_tenant_id\": \"…\"}`, and credential resolution then prefers that credential over the platform one for that tenant", body = ClaudeRunnerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict, idempotency_in_progress, or duplicate_runner_label", body = ErrorResponse),
        (status = 503, description = "runner_service_disabled, runner_service_unavailable, or runner_service_unauthorized", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn provision_runner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ClaudeRunnerProvisionRequest>,
) -> Result<(StatusCode, HeaderMap, Json<ClaudeRunnerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ClaudeRunnerService::new(&state)?
        .provision(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/runners", tag = "admin-runners",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated runners, answered from Moira's mirror without contacting the runner service", body = ListResponse<ClaudeRunnerRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_runners(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<ClaudeRunnerRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    ClaudeRunnerService::new(&state)?
        .list(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/runners/{id}", tag = "admin-runners",
    params(("id" = Uuid, Path, description = "Runner identifier")),
    responses(
        (status = 200, description = "Runner. A runner that is not yet in a terminal state is refreshed from the runner service first, which is what surfaces authorization_url; that refresh writes, so the ETag advances on a poll", body = ClaudeRunnerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 503, description = "runner_service_unavailable or runner_service_unauthorized", body = ErrorResponse),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_runner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ClaudeRunnerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ClaudeRunnerService::new(&state)?.get(&actor, id).await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/runners/{id}/authorization-code", tag = "admin-runners",
    request_body = ClaudeRunnerAuthorizationCodeRequest,
    params(("id" = Uuid, Path, description = "Runner identifier")),
    responses(
        (status = 200, description = "Authorization code forwarded to the runner; the code itself is never stored, logged, or echoed back", body = ClaudeRunnerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "runner_wrong_state", body = ErrorResponse),
        (status = 503, description = "runner_service_disabled, runner_service_unavailable, or runner_service_unauthorized", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn submit_runner_authorization_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ClaudeRunnerAuthorizationCodeRequest>,
) -> Result<(HeaderMap, Json<ClaudeRunnerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ClaudeRunnerService::new(&state)?
        .submit_authorization_code(&actor, &ctx, id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/runners/{id}/finalize", tag = "admin-runners",
    request_body = ClaudeRunnerFinalizeRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key, forwarded to the credential-create envelope")),
    params(("id" = Uuid, Path, description = "Runner identifier")),
    responses(
        (status = 200, description = "Token stored as a provider credential at the runner's own stored scope, and the runner linked. The response carries credential_id only — the token itself never leaves the Moira process and is not returned, logged, or traced. The request body has no scope field and one is rejected: the scope is fixed at provisioning time because it is part of the credential's AAD", body = ClaudeRunnerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "runner_wrong_state or runner_token_unavailable", body = ErrorResponse),
        (status = 503, description = "runner_service_disabled, runner_service_unavailable, or runner_service_unauthorized", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn finalize_runner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ClaudeRunnerFinalizeRequest>,
) -> Result<(HeaderMap, Json<ClaudeRunnerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ClaudeRunnerService::new(&state)?
        .finalize(&actor, &ctx, id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/runners/{id}", tag = "admin-runners",
    params(
        ("id" = Uuid, Path, description = "Runner identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Runner container removed and the mirror row soft-deleted. Any credential the runner produced is deliberately left in place"),
        (status = 503, description = "runner_service_unavailable or runner_service_unauthorized", body = ErrorResponse),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_runner(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = ClaudeRunnerService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service.delete(&actor, &ctx, id, expected_version).await?;
    Ok(StatusCode::NO_CONTENT)
}
