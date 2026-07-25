use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminService, PublicExecutionService, RequestContext, RuntimeAdminService, SetupService,
        execute_diagnostic,
    },
    domain::{
        AgentProfileCreateRequest, AgentProfilePatchRequest, AgentProfileRecord, ApiKeyRecord,
        ApiKeyRotateRequest, ApiKeySecretResponse, ApplicationCreateRequest,
        ApplicationExecutionPolicyPutRequest, ApplicationExecutionPolicyRecord,
        ApplicationPatchRequest, ApplicationRecord, AuditLogRecord, ConsumerKeyCreateRequest,
        CredentialCreateRequest, CredentialPatchRequest, CredentialRecord, CredentialScope,
        DiagnosticExecutionRequest, DiagnosticExecutionResponse, ListResponse, PageQuery,
        ProviderCreateRequest, ProviderModelCreateRequest, ProviderModelPatchRequest,
        ProviderModelRecord, ProviderPatchRequest, ProviderRecord, ProviderRuntimePolicyPutRequest,
        ProviderRuntimePolicyRecord, RotateCredentialRequest, RouteDefinitionCreateRequest,
        RouteDefinitionPatchRequest, RouteDefinitionRecord, RoutingPolicyCreateRequest,
        RoutingPolicyPatchRequest, RoutingPolicyRecord, SetupStatusResponse,
        SystemKeyCreateRequest, TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest,
        TrustedJwtIssuerRecord,
    },
    error::{AppError, ErrorResponse},
};

#[utoipa::path(
    get, path = "/api/v1/admin/setup/status", tag = "admin-setup",
    responses(
        (status = 200, description = "Current structural setup readiness", body = SetupStatusResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Caller type or scope is not permitted", body = ErrorResponse),
        (status = 500, description = "Setup inspection failed", body = ErrorResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []))
)]
pub async fn get_setup_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SetupStatusResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    SetupService::new(&state)?.status(&actor).await.map(Json)
}

async fn admin_actor(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::security::Actor, AppError> {
    state.auth.authenticate_admin(state.pool()?, headers).await
}

fn etag_headers(version: i64) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("\"{version}\"")) {
        headers.insert(header::ETAG, value);
    }
    headers
}

fn require_if_match(headers: &HeaderMap) -> Result<i64, AppError> {
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
    value
        .trim()
        .trim_matches('"')
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("If-Match header is invalid".to_string()))
}

fn optional_if_match(headers: &HeaderMap) -> Result<Option<i64>, AppError> {
    match headers.get(header::IF_MATCH) {
        Some(_) => require_if_match(headers).map(Some),
        None => Ok(None),
    }
}

#[utoipa::path(
    post, path = "/api/v1/admin/applications", tag = "admin-applications",
    request_body = ApplicationCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Application created", body = ApplicationRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ApplicationCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<ApplicationRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_application(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/applications", tag = "admin-applications",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated applications", body = ListResponse<ApplicationRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_applications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<ApplicationRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_applications(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/applications/{id}", tag = "admin-applications",
    params(("id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Application", body = ApplicationRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ApplicationRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AdminService::new(&state)?
        .get_application(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/applications/{id}", tag = "admin-applications",
    request_body = ApplicationPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Application identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Application updated", body = ApplicationRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ApplicationPatchRequest>,
) -> Result<(HeaderMap, Json<ApplicationRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_application(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/applications/{id}", tag = "admin-applications",
    params(
        ("id" = Uuid, Path, description = "Application identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Application deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_application(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/applications/{id}/enable", tag = "admin-applications",
    params(
        ("id" = Uuid, Path, description = "Application identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Application enabled", body = ApplicationRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ApplicationRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_application_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/applications/{id}/disable", tag = "admin-applications",
    params(
        ("id" = Uuid, Path, description = "Application identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Application disabled", body = ApplicationRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ApplicationRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_application_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/applications/{id}/execution-policy", tag = "admin-policies",
    params(("id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Application execution policy", body = ApplicationExecutionPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_application_execution_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ApplicationExecutionPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = PublicExecutionService::new(&state)?
        .get_application_execution_policy(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put, path = "/api/v1/admin/applications/{id}/execution-policy", tag = "admin-policies",
    request_body = ApplicationExecutionPolicyPutRequest,
    params(
        ("id" = Uuid, Path, description = "Application identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 200, description = "Application execution policy updated", body = ApplicationExecutionPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn put_application_execution_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ApplicationExecutionPolicyPutRequest>,
) -> Result<(HeaderMap, Json<ApplicationExecutionPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = PublicExecutionService::new(&state)?
        .put_application_execution_policy(
            &actor,
            &ctx,
            id,
            Some(require_if_match(&headers)?),
            request,
        )
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/providers", tag = "admin-providers",
    request_body = ProviderCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Provider created", body = ProviderRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProviderCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<ProviderRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_provider(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/providers", tag = "admin-providers",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated providers", body = ListResponse<ProviderRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<ProviderRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_providers(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/providers/{id}", tag = "admin-providers",
    params(("id" = Uuid, Path, description = "Provider identifier")),
    responses(
        (status = 200, description = "Provider", body = ProviderRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AdminService::new(&state)?.get_provider(&actor, id).await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/providers/{id}", tag = "admin-providers",
    request_body = ProviderPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Provider identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Provider updated", body = ProviderRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ProviderPatchRequest>,
) -> Result<(HeaderMap, Json<ProviderRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_provider(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/providers/{id}", tag = "admin-providers",
    params(
        ("id" = Uuid, Path, description = "Provider identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Provider deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_provider(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/providers/{id}/enable", tag = "admin-providers",
    params(
        ("id" = Uuid, Path, description = "Provider identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Provider enabled", body = ProviderRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_provider_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/providers/{id}/disable", tag = "admin-providers",
    params(
        ("id" = Uuid, Path, description = "Provider identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Provider disabled", body = ProviderRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_provider_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/providers/{provider_id}/models", tag = "admin-provider-models",
    request_body = ProviderModelCreateRequest,
    params(
        ("provider_id" = Uuid, Path, description = "Provider identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 201, description = "Provider model created", body = ProviderModelRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_provider_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<Uuid>,
    Json(request): Json<ProviderModelCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<ProviderModelRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_provider_model(&actor, &ctx, provider_id, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/providers/{provider_id}/models", tag = "admin-provider-models",
    params(
        ("provider_id" = Uuid, Path, description = "Provider identifier"),
        PageQuery
    ),
    responses(
        (status = 200, description = "Paginated provider models", body = ListResponse<ProviderModelRecord>),
        (status = "4XX", description = "Query, authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_provider_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<Uuid>,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<ProviderModelRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_provider_models(&actor, provider_id, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    patch, path = "/api/v1/admin/provider-models/{id}", tag = "admin-provider-models",
    request_body = ProviderModelPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Provider model identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Provider model updated", body = ProviderModelRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_provider_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<Uuid>,
    Json(request): Json<ProviderModelPatchRequest>,
) -> Result<(HeaderMap, Json<ProviderModelRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_provider_model(&actor, &ctx, model_id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/provider-models/{id}", tag = "admin-provider-models",
    params(("id" = Uuid, Path, description = "Provider model identifier")),
    responses(
        (status = 200, description = "Provider model", body = ProviderModelRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_provider_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderModelRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AdminService::new(&state)?
        .get_provider_model(&actor, model_id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/provider-models/{id}", tag = "admin-provider-models",
    params(
        ("id" = Uuid, Path, description = "Provider model identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Provider model deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_provider_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_provider_model(&actor, &ctx, model_id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/provider-models/{id}/enable", tag = "admin-provider-models",
    params(
        ("id" = Uuid, Path, description = "Provider model identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Provider model enabled", body = ProviderModelRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_provider_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderModelRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_provider_model_enabled(&actor, &ctx, model_id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/provider-models/{id}/disable", tag = "admin-provider-models",
    params(
        ("id" = Uuid, Path, description = "Provider model identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Provider model disabled", body = ProviderModelRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_provider_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderModelRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_provider_model_enabled(&actor, &ctx, model_id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/provider-credentials", tag = "admin-credentials",
    request_body = CredentialCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Credential created; plaintext secret is never returned", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CredentialCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<CredentialRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_credential(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/provider-credentials", tag = "admin-credentials",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated provider credentials without plaintext secrets", body = ListResponse<CredentialRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<CredentialRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_credentials(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/provider-credentials/{id}", tag = "admin-credentials",
    params(("id" = Uuid, Path, description = "Credential identifier")),
    responses(
        (status = 200, description = "Credential metadata without plaintext secret", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<CredentialRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AdminService::new(&state)?
        .get_credential(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/provider-credentials/{id}", tag = "admin-credentials",
    request_body = CredentialPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Credential identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Credential metadata updated", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<CredentialPatchRequest>,
) -> Result<(HeaderMap, Json<CredentialRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_credential(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/provider-credentials/{id}", tag = "admin-credentials",
    params(
        ("id" = Uuid, Path, description = "Credential identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Credential deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_credential(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/provider-credentials/{id}/rotate", tag = "admin-credentials",
    request_body = RotateCredentialRequest,
    params(
        ("id" = Uuid, Path, description = "Credential identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 200, description = "Credential rotated; plaintext secret is never returned", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn rotate_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<RotateCredentialRequest>,
) -> Result<(HeaderMap, Json<CredentialRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let expected_version = require_if_match(&headers)?;
    let record = AdminService::new(&state)?
        .rotate_credential(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/provider-credentials/{id}/enable", tag = "admin-credentials",
    params(
        ("id" = Uuid, Path, description = "Credential identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Credential enabled", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<CredentialRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_credential_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/provider-credentials/{id}/disable", tag = "admin-credentials",
    params(
        ("id" = Uuid, Path, description = "Credential identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Credential disabled", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<CredentialRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_credential_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put, path = "/api/v1/admin/users/{external_user_id}/provider-credentials/{id}", tag = "admin-credentials",
    request_body = CredentialCreateRequest,
    params(
        ("external_user_id" = String, Path, description = "External user identifier"),
        ("id" = Uuid, Path, description = "Provider identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 201, description = "User credential created or replaced", body = CredentialRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn upsert_user_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((external_user_id, provider_id)): Path<(String, Uuid)>,
    Json(mut request): Json<CredentialCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<CredentialRecord>), AppError> {
    request.provider_id = provider_id;
    request.scope = CredentialScope::User {
        external_user_id,
        application_id: request.scope.application_id(),
        external_tenant_id: request.scope.external_tenant_id().map(ToOwned::to_owned),
    };
    create_credential(State(state), headers, Json(request)).await
}

#[utoipa::path(
    get, path = "/api/v1/admin/users/{external_user_id}/provider-credentials", tag = "admin-credentials",
    params(
        ("external_user_id" = String, Path, description = "External user identifier"),
        PageQuery
    ),
    responses(
        (status = 200, description = "User credentials without plaintext secrets", body = ListResponse<CredentialRecord>),
        (status = "4XX", description = "Query, authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_user_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(external_user_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<CredentialRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_user_credentials(&actor, &external_user_id, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    delete, path = "/api/v1/admin/users/{external_user_id}/provider-credentials/{id}", tag = "admin-credentials",
    params(
        ("external_user_id" = String, Path, description = "External user identifier"),
        ("id" = Uuid, Path, description = "Credential identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "User credential deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_user_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((external_user_id, credential_id)): Path<(String, Uuid)>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_user_credential(
            &actor,
            &ctx,
            &external_user_id,
            credential_id,
            expected_version,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/system-keys", tag = "admin-api-keys",
    request_body = SystemKeyCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "System key created; replay returns secret_retrievable false and no raw secret", body = ApiKeySecretResponse),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_system_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SystemKeyCreateRequest>,
) -> Result<(StatusCode, Json<ApiKeySecretResponse>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_system_key(&actor, &ctx, request)
        .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/system-keys", tag = "admin-api-keys",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated system key metadata without raw keys", body = ListResponse<ApiKeyRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_system_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<ApiKeyRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_system_keys(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/system-keys/{id}", tag = "admin-api-keys",
    params(("id" = Uuid, Path, description = "System key identifier")),
    responses(
        (status = 200, description = "System key metadata without raw key", body = ApiKeyRecord),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_system_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKeyRecord>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .get_system_key(&actor, id)
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/system-keys/{id}/rotate", tag = "admin-api-keys",
    params(
        ("id" = Uuid, Path, description = "System key identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 200, description = "System key rotated; replay returns secret_retrievable false and no raw secret", body = ApiKeySecretResponse),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn rotate_system_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKeySecretResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AdminService::new(&state)?
        .rotate_key(
            &actor,
            &ctx,
            "system_api_keys",
            "moira_sys",
            id,
            ApiKeyRotateRequest::default(),
        )
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/system-keys/{id}/revoke", tag = "admin-api-keys",
    params(("id" = Uuid, Path, description = "System key identifier")),
    responses(
        (status = 200, description = "System key revoked", body = ApiKeyRecord),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn revoke_system_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKeyRecord>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AdminService::new(&state)?
        .revoke_key(&actor, &ctx, "system_api_keys", id)
        .await
        .map(Json)
}

#[utoipa::path(
    delete, path = "/api/v1/admin/system-keys/{id}", tag = "admin-api-keys",
    params(("id" = Uuid, Path, description = "System key identifier")),
    responses(
        (status = 204, description = "System key deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_system_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AdminService::new(&state)?
        .delete_key(&actor, &ctx, "system_api_keys", id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/consumer-keys", tag = "admin-api-keys",
    request_body = ConsumerKeyCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Consumer key created; replay returns secret_retrievable false and no raw secret", body = ApiKeySecretResponse),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_consumer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ConsumerKeyCreateRequest>,
) -> Result<(StatusCode, Json<ApiKeySecretResponse>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_consumer_key(&actor, &ctx, request)
        .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/consumer-keys", tag = "admin-api-keys",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated consumer key metadata without raw keys", body = ListResponse<ApiKeyRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_consumer_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<ApiKeyRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_consumer_keys(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/consumer-keys/{id}", tag = "admin-api-keys",
    params(("id" = Uuid, Path, description = "Consumer key identifier")),
    responses(
        (status = 200, description = "Consumer key metadata without raw key", body = ApiKeyRecord),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_consumer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKeyRecord>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .get_consumer_key(&actor, id)
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/consumer-keys/{id}/rotate", tag = "admin-api-keys",
    params(
        ("id" = Uuid, Path, description = "Consumer key identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 200, description = "Consumer key rotated; replay returns secret_retrievable false and no raw secret", body = ApiKeySecretResponse),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn rotate_consumer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKeySecretResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AdminService::new(&state)?
        .rotate_key(
            &actor,
            &ctx,
            "consumer_api_keys",
            "moira_cons",
            id,
            ApiKeyRotateRequest::default(),
        )
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/consumer-keys/{id}/revoke", tag = "admin-api-keys",
    params(("id" = Uuid, Path, description = "Consumer key identifier")),
    responses(
        (status = 200, description = "Consumer key revoked", body = ApiKeyRecord),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn revoke_consumer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKeyRecord>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AdminService::new(&state)?
        .revoke_key(&actor, &ctx, "consumer_api_keys", id)
        .await
        .map(Json)
}

#[utoipa::path(
    delete, path = "/api/v1/admin/consumer-keys/{id}", tag = "admin-api-keys",
    params(("id" = Uuid, Path, description = "Consumer key identifier")),
    responses(
        (status = 204, description = "Consumer key deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_consumer_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    AdminService::new(&state)?
        .delete_key(&actor, &ctx, "consumer_api_keys", id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/jwt-issuers", tag = "admin-jwt-issuers",
    request_body = TrustedJwtIssuerCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Trusted JWT issuer created", body = TrustedJwtIssuerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or JWKS error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TrustedJwtIssuerCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<TrustedJwtIssuerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .create_trusted_jwt_issuer(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/jwt-issuers", tag = "admin-jwt-issuers",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated trusted JWT issuers", body = ListResponse<TrustedJwtIssuerRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_trusted_jwt_issuers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<TrustedJwtIssuerRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_trusted_jwt_issuers(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/jwt-issuers/{id}", tag = "admin-jwt-issuers",
    params(("id" = Uuid, Path, description = "Trusted JWT issuer identifier")),
    responses(
        (status = 200, description = "Trusted JWT issuer", body = TrustedJwtIssuerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<TrustedJwtIssuerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AdminService::new(&state)?
        .get_trusted_jwt_issuer(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/jwt-issuers/{id}", tag = "admin-jwt-issuers",
    request_body = TrustedJwtIssuerPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Trusted JWT issuer identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Trusted JWT issuer updated", body = TrustedJwtIssuerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or JWKS error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<TrustedJwtIssuerPatchRequest>,
) -> Result<(HeaderMap, Json<TrustedJwtIssuerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_trusted_jwt_issuer(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/jwt-issuers/{id}", tag = "admin-jwt-issuers",
    params(
        ("id" = Uuid, Path, description = "Trusted JWT issuer identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Trusted JWT issuer deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_trusted_jwt_issuer(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/jwt-issuers/{id}/refresh-jwks", tag = "admin-jwt-issuers",
    params(("id" = Uuid, Path, description = "Trusted JWT issuer identifier")),
    responses(
        (status = 200, description = "JWKS refreshed", body = TrustedJwtIssuerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or JWKS error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn refresh_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<TrustedJwtIssuerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = AdminService::new(&state)?
        .refresh_trusted_jwt_issuer(&actor, &ctx, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/jwt-issuers/{id}/enable", tag = "admin-jwt-issuers",
    params(
        ("id" = Uuid, Path, description = "Trusted JWT issuer identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Trusted JWT issuer enabled", body = TrustedJwtIssuerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<TrustedJwtIssuerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_trusted_jwt_issuer_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/jwt-issuers/{id}/disable", tag = "admin-jwt-issuers",
    params(
        ("id" = Uuid, Path, description = "Trusted JWT issuer identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Trusted JWT issuer disabled", body = TrustedJwtIssuerRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_trusted_jwt_issuer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<TrustedJwtIssuerRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_trusted_jwt_issuer_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/audit-events", tag = "admin-audit",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated immutable audit events", body = ListResponse<AuditLogRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_audit_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<AuditLogRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .list_audit_logs(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/audit-events/{id}", tag = "admin-audit",
    params(("id" = Uuid, Path, description = "Audit event identifier")),
    responses(
        (status = 200, description = "Immutable audit event", body = AuditLogRecord),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_audit_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<AuditLogRecord>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AdminService::new(&state)?
        .get_audit_log(&actor, id)
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/routes", tag = "admin-routes",
    request_body = RouteDefinitionCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Route definition created", body = RouteDefinitionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_route_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RouteDefinitionCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<RouteDefinitionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = RuntimeAdminService::new(&state)?
        .create_route_definition(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/routes", tag = "admin-routes",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated route definitions", body = ListResponse<RouteDefinitionRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_route_definitions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<RouteDefinitionRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    RuntimeAdminService::new(&state)?
        .list_route_definitions(&actor, query.cursor.as_deref(), query.limit())
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/routes/{id}", tag = "admin-routes",
    params(("id" = Uuid, Path, description = "Route definition identifier")),
    responses(
        (status = 200, description = "Route definition", body = RouteDefinitionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_route_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RouteDefinitionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = RuntimeAdminService::new(&state)?
        .get_route_definition(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/routes/{id}", tag = "admin-routes",
    request_body = RouteDefinitionPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Route definition identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Route definition updated", body = RouteDefinitionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_route_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<RouteDefinitionPatchRequest>,
) -> Result<(HeaderMap, Json<RouteDefinitionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_route_definition(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/routes/{id}", tag = "admin-routes",
    params(
        ("id" = Uuid, Path, description = "Route definition identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Route definition deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_route_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_route_definition(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/routes/{id}/enable", tag = "admin-routes",
    params(
        ("id" = Uuid, Path, description = "Route definition identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Route definition enabled", body = RouteDefinitionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_route_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RouteDefinitionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_route_definition_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/routes/{id}/disable", tag = "admin-routes",
    params(
        ("id" = Uuid, Path, description = "Route definition identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Route definition disabled", body = RouteDefinitionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_route_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RouteDefinitionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_route_definition_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/routing-policies", tag = "admin-routing-policies",
    request_body = RoutingPolicyCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Routing policy created", body = RoutingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_routing_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RoutingPolicyCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<RoutingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = RuntimeAdminService::new(&state)?
        .create_routing_policy(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/routing-policies", tag = "admin-routing-policies",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated routing policies", body = ListResponse<RoutingPolicyRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_routing_policies(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<RoutingPolicyRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    RuntimeAdminService::new(&state)?
        .list_routing_policies(&actor, query.cursor.as_deref(), query.limit())
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/routing-policies/{id}", tag = "admin-routing-policies",
    params(("id" = Uuid, Path, description = "Routing policy identifier")),
    responses(
        (status = 200, description = "Routing policy", body = RoutingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_routing_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RoutingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = RuntimeAdminService::new(&state)?
        .get_routing_policy(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/routing-policies/{id}", tag = "admin-routing-policies",
    request_body = RoutingPolicyPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Routing policy identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Routing policy updated", body = RoutingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_routing_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<RoutingPolicyPatchRequest>,
) -> Result<(HeaderMap, Json<RoutingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_routing_policy(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/routing-policies/{id}", tag = "admin-routing-policies",
    params(
        ("id" = Uuid, Path, description = "Routing policy identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Routing policy deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_routing_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_routing_policy(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/routing-policies/{id}/enable", tag = "admin-routing-policies",
    params(
        ("id" = Uuid, Path, description = "Routing policy identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Routing policy enabled", body = RoutingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_routing_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RoutingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_routing_policy_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/routing-policies/{id}/disable", tag = "admin-routing-policies",
    params(
        ("id" = Uuid, Path, description = "Routing policy identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Routing policy disabled", body = RoutingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_routing_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RoutingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_routing_policy_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/agent-profiles", tag = "admin-agent-profiles",
    request_body = AgentProfileCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")),
    responses(
        (status = 201, description = "Agent profile created", body = AgentProfileRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AgentProfileCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<AgentProfileRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = RuntimeAdminService::new(&state)?
        .create_agent_profile(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/agent-profiles", tag = "admin-agent-profiles",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated agent profiles", body = ListResponse<AgentProfileRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_agent_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<AgentProfileRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    RuntimeAdminService::new(&state)?
        .list_agent_profiles(&actor, query.cursor.as_deref(), query.limit())
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/agent-profiles/{id}", tag = "admin-agent-profiles",
    params(("id" = Uuid, Path, description = "Agent profile identifier")),
    responses(
        (status = 200, description = "Agent profile", body = AgentProfileRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AgentProfileRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = RuntimeAdminService::new(&state)?
        .get_agent_profile(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/agent-profiles/{id}", tag = "admin-agent-profiles",
    request_body = AgentProfilePatchRequest,
    params(
        ("id" = Uuid, Path, description = "Agent profile identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Agent profile updated", body = AgentProfileRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<AgentProfilePatchRequest>,
) -> Result<(HeaderMap, Json<AgentProfileRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch_agent_profile(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/agent-profiles/{id}", tag = "admin-agent-profiles",
    params(
        ("id" = Uuid, Path, description = "Agent profile identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Agent profile deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service
        .delete_agent_profile(&actor, &ctx, id, expected_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/agent-profiles/{id}/enable", tag = "admin-agent-profiles",
    params(
        ("id" = Uuid, Path, description = "Agent profile identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Agent profile enabled", body = AgentProfileRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AgentProfileRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_agent_profile_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/agent-profiles/{id}/disable", tag = "admin-agent-profiles",
    params(
        ("id" = Uuid, Path, description = "Agent profile identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Agent profile disabled", body = AgentProfileRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AgentProfileRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = RuntimeAdminService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_agent_profile_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/providers/{provider_id}/runtime-policy", tag = "admin-runtime",
    params(("provider_id" = Uuid, Path, description = "Provider identifier")),
    responses(
        (status = 200, description = "Provider runtime policy", body = ProviderRuntimePolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_provider_runtime_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ProviderRuntimePolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = RuntimeAdminService::new(&state)?
        .get_provider_runtime_policy(&actor, provider_id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put, path = "/api/v1/admin/providers/{provider_id}/runtime-policy", tag = "admin-runtime",
    request_body = ProviderRuntimePolicyPutRequest,
    params(
        ("provider_id" = Uuid, Path, description = "Provider identifier"),
        ("If-Match" = Option<i64>, Header, description = "Optional current resource version"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")
    ),
    responses(
        (status = 200, description = "Provider runtime policy updated", body = ProviderRuntimePolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn put_provider_runtime_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(provider_id): Path<Uuid>,
    Json(request): Json<ProviderRuntimePolicyPutRequest>,
) -> Result<(HeaderMap, Json<ProviderRuntimePolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = RuntimeAdminService::new(&state)?
        .put_provider_runtime_policy(
            &actor,
            &ctx,
            provider_id,
            optional_if_match(&headers)?,
            request,
        )
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/runtime/diagnose", tag = "admin-runtime",
    request_body = DiagnosticExecutionRequest,
    responses(
        (status = 200, description = "Runtime diagnostic result", body = DiagnosticExecutionResponse),
        (status = "4XX", description = "Endpoint disabled, request, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Provider, infrastructure, or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn diagnose_runtime(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DiagnosticExecutionRequest>,
) -> Result<Json<DiagnosticExecutionResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    execute_diagnostic(state, &actor, &ctx, request)
        .await
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CredentialType;
    use axum::http::Uri;

    /// Finding P2-9. `PageQuery` carries `#[serde(deny_unknown_fields)]`
    /// (`src/domain/admin.rs`), and every admin list handler takes it through
    /// `Query<PageQuery>` — but nothing asserted the attribute actually
    /// rejected anything, so removing it would have been silent. This goes
    /// through the real `Query` extractor rather than `serde_urlencoded`
    /// directly, because the extractor is what the routes use.
    #[test]
    fn page_query_rejects_a_field_absent_from_the_struct() {
        let uri: Uri = "/api/v1/admin/applications?not_a_real_field=1"
            .parse()
            .expect("test URI");
        let rejection = Query::<PageQuery>::try_from_uri(&uri)
            .expect_err("a field absent from PageQuery must be rejected");
        let described = rejection.to_string();
        assert!(
            described.contains("not_a_real_field"),
            "the rejection must name the offending field, got: {described}"
        );

        // The control: a field that *is* on the struct still parses, so the
        // assertion above is about the unknown field and not about the
        // extractor rejecting every query string it sees.
        let accepted: Uri = "/api/v1/admin/applications?limit=7"
            .parse()
            .expect("test URI");
        let query = Query::<PageQuery>::try_from_uri(&accepted).expect("a known field must parse");
        assert_eq!(query.0.limit, Some(7));
    }

    /// The companion to the rejection above: `deny_unknown_fields` is a
    /// *shape* check, not a relevance check. All 26 fields parse on every
    /// list route, including ones that endpoint has no use for. Pinned here
    /// so the nuance recorded on `PageQuery`(P2-9) cannot quietly change.
    #[test]
    fn page_query_accepts_a_defined_field_that_the_endpoint_ignores() {
        let uri: Uri = "/api/v1/admin/applications?credential_type=api_key"
            .parse()
            .expect("test URI");
        let query = Query::<PageQuery>::try_from_uri(&uri)
            .expect("a field defined on PageQuery must parse on any list route");
        assert_eq!(query.0.credential_type, Some(CredentialType::ApiKey));
    }
}
