//! Admin HTTP handlers for the agent platform (issue #214, plan 12 §3/§5): skills CRUD.
//!
//! A new module rather than more lines in the already-large `src/http/admin.rs`, per the plan's
//! build-speed rule (new code in new files). It reuses `admin.rs`'s admin-plane wrapper
//! (`admin_actor`) and the shared `etag_headers`/`require_if_match` helpers so this surface gates
//! on the exact same authentication and `If-Match` contract every other admin handler uses.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{AgentPlatformService, RequestContext},
    domain::{
        ListResponse, PageQuery, SkillBulkEnableRequest, SkillBulkEnableResponse,
        SkillCreateRequest, SkillPatchRequest, SkillRecord,
    },
    error::{AppError, ErrorResponse},
};

use super::admin::{admin_actor, etag_headers, require_if_match};

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
