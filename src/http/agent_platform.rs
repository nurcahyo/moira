//! Admin HTTP handlers for the agent platform (issue #214, plan 12 §3/§5): skills CRUD, the
//! OpenAPI import pipeline, and `skill_http_executors` CRUD (workstream H).
//!
//! A new module rather than more lines in the already-large `src/http/admin.rs`, per the plan's
//! build-speed rule (new code in new files). It reuses `admin.rs`'s admin-plane wrapper
//! (`admin_actor`) and the shared `etag_headers`/`require_if_match` helpers so this surface gates
//! on the exact same authentication and `If-Match` contract every other admin handler uses —
//! except for the executor endpoints, which cannot: `skill_http_executors` has no `version`
//! column, so they use [`executor_etag_headers`]/[`require_executor_if_match`] instead. See
//! `domain::SkillHttpExecutorRecord`'s doc comment for why.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{AgentPlatformService, RequestContext},
    domain::{
        ListResponse, PageQuery, SkillBulkEnableRequest, SkillBulkEnableResponse,
        SkillCreateRequest, SkillHttpExecutorPatchRequest, SkillHttpExecutorRecord,
        SkillImportRequest, SkillImportResponse, SkillPatchRequest, SkillRecord,
    },
    error::{AppError, ErrorResponse},
};

use super::admin::{admin_actor, etag_headers, require_if_match};

/// `skill_http_executors` carries `updated_at` but no `version` (see the module docs), so
/// this resource's ETag is a quoted RFC 3339 timestamp at microsecond precision — the same
/// precision Postgres `timestamptz` stores — rather than the integer every other versioned
/// admin resource uses.
fn executor_etag_headers(updated_at: DateTime<Utc>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let value = updated_at.to_rfc3339_opts(SecondsFormat::Micros, true);
    if let Ok(header_value) = HeaderValue::from_str(&format!("\"{value}\"")) {
        headers.insert(header::ETAG, header_value);
    }
    headers
}

/// [`require_if_match`]'s timestamp-based twin — see [`executor_etag_headers`].
fn require_executor_if_match(headers: &HeaderMap) -> Result<DateTime<Utc>, AppError> {
    let value = headers
        .get(header::IF_MATCH)
        .ok_or_else(|| {
            AppError::coded(
                StatusCode::BAD_REQUEST,
                "if_match_required",
                "If-Match header is required",
            )
        })?
        .to_str()
        .map_err(|_| AppError::BadRequest("If-Match header is invalid".to_string()))?;
    DateTime::parse_from_rfc3339(value.trim().trim_matches('"'))
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| AppError::BadRequest("If-Match header is invalid".to_string()))
}

#[utoipa::path(
    post, path = "/api/v1/admin/skills", tag = "admin-skills",
    request_body = SkillCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Skill created", body = SkillRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SkillCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<SkillRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AgentPlatformService::new(&state)?
        .create_skill(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/skills", tag = "admin-skills",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated skills", body = ListResponse<SkillRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<SkillRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AgentPlatformService::new(&state)?
        .list_skills(&actor, query.cursor.as_deref(), query.limit())
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/skills/{id}", tag = "admin-skills",
    params(("id" = Uuid, Path, description = "Skill identifier")),
    responses(
        (status = 200, description = "Skill", body = SkillRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<SkillRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AgentPlatformService::new(&state)?
        .get_skill(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/skills/{id}", tag = "admin-skills",
    request_body = SkillPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Skill identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Skill updated", body = SkillRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<SkillPatchRequest>,
) -> Result<(HeaderMap, Json<SkillRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AgentPlatformService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_skill(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/skills/{id}", tag = "admin-skills",
    params(
        ("id" = Uuid, Path, description = "Skill identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Skill deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AgentPlatformService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_skill(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/skills/{id}/enable", tag = "admin-skills",
    params(
        ("id" = Uuid, Path, description = "Skill identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Skill enabled", body = SkillRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<SkillRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AgentPlatformService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_skill_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/skills/{id}/disable", tag = "admin-skills",
    params(
        ("id" = Uuid, Path, description = "Skill identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Skill disabled", body = SkillRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<SkillRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AgentPlatformService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_skill_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/skills/bulk-enable", tag = "admin-skills",
    request_body = SkillBulkEnableRequest,
    responses(
        (status = 200, description = "Skills enabled", body = SkillBulkEnableResponse),
        (status = "4XX", description = "Request, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn bulk_enable_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SkillBulkEnableRequest>,
) -> Result<Json<SkillBulkEnableResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AgentPlatformService::new(&state)?
        .bulk_enable_skills(&actor, &ctx, request)
        .await
        .map(Json)
}

/// Issue #237 (plan 12 §5) — the OpenAPI import pipeline: parses `request.document`, caps it
/// at 300 operations (§5 decision 23), SSRF-validates the server URL, and creates one `draft`
/// skill plus one HTTP executor per operation. Live execution (an actual `HttpSkillTool`
/// calling the executor) is deferred to the rig tool loop, #84 — this endpoint only ever
/// creates disabled rows for an operator to review and enable via F's
/// `/enable`/`/bulk-enable`.
#[utoipa::path(
    post, path = "/api/v1/admin/skills/import", tag = "admin-skills",
    request_body = SkillImportRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Skills and HTTP executors imported as drafts", body = SkillImportResponse),
        (status = "4XX", description = "Request, authentication, authorization, or SSRF-blocked-host error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn import_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SkillImportRequest>,
) -> Result<(StatusCode, Json<SkillImportResponse>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let response = AgentPlatformService::new(&state)?
        .import_skills(&actor, &ctx, request)
        .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/skill-executors", tag = "admin-skills",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated skill HTTP executors", body = ListResponse<SkillHttpExecutorRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_skill_executors(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<SkillHttpExecutorRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AgentPlatformService::new(&state)?
        .list_executors(&actor, query.cursor.as_deref(), query.limit())
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/skills/{id}/executor", tag = "admin-skills",
    params(("id" = Uuid, Path, description = "Skill identifier")),
    responses(
        (status = 200, description = "The skill's HTTP executor", body = SkillHttpExecutorRecord, headers(("ETag" = String, description = "Quoted RFC 3339 updated_at, this resource's If-Match basis"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_skill_executor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<SkillHttpExecutorRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AgentPlatformService::new(&state)?
        .get_executor(&actor, id)
        .await?;
    Ok((executor_etag_headers(record.updated_at), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/skills/{id}/executor", tag = "admin-skills",
    request_body = SkillHttpExecutorPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Skill identifier"),
        ("If-Match" = String, Header, description = "Required current updated_at (quoted RFC 3339)")
    ),
    responses(
        (status = 200, description = "HTTP executor updated", body = SkillHttpExecutorRecord, headers(("ETag" = String, description = "Quoted RFC 3339 updated_at, this resource's If-Match basis"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, not-found, or SSRF-blocked-host error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_skill_executor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<SkillHttpExecutorPatchRequest>,
) -> Result<(HeaderMap, Json<SkillHttpExecutorRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AgentPlatformService::new(&state)?;
    let expected_updated_at = require_executor_if_match(&headers)?;
    let record = service
        .patch_executor(&actor, &ctx, id, expected_updated_at, request)
        .await?;
    Ok((executor_etag_headers(record.updated_at), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/skills/{id}/executor", tag = "admin-skills",
    params(
        ("id" = Uuid, Path, description = "Skill identifier"),
        ("If-Match" = String, Header, description = "Required current updated_at (quoted RFC 3339)")
    ),
    responses(
        (status = 204, description = "HTTP executor deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_skill_executor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AgentPlatformService::new(&state)?;
    let expected_updated_at = require_executor_if_match(&headers)?;
    service
        .delete_executor(&actor, &ctx, id, expected_updated_at)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
