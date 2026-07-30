//! Runtime auth-provider settings over HTTP (plan 07, module 12).
//!
//! Nine handlers: `GET /api/v1/admin/setup/auth-methods`, its anonymous narrow sibling
//! `GET /api/v1/admin/setup/sign-in-methods` (finding F15), plus the seven
//! `/api/v1/admin/auth/providers…` operations. They follow the trusted-JWT-issuer handlers
//! in [`crate::http::admin`] as their template — the same `security(...)` triple, the same
//! `params(PageQuery)` on list, the same `etag_headers` / `require_if_match` helpers, the
//! same `Idempotency-Key` documentation on create.
//!
//! # Decision D7: there is no `rotate-secret` operation, and there must never be one
//!
//! **No route, handler, DTO or `#[utoipa::path]` for
//! `POST /api/v1/admin/auth/providers/{id}/rotate-secret` may appear in this file or in the
//! router.** Moira stores no OAuth client secret, so there is nothing here to rotate; the
//! console owns the secret in its own `console_auth` database. Its absence is part of the
//! frozen contract plans 08 and 09 bind to, which is why it is asserted by a test rather
//! than merely left unwritten.
//!
//! # The version check happens inside the transaction
//!
//! Each mutating handler reads `require_if_match` and passes the expected version *down*
//! into the repository, which evaluates it under `select … for update`. A read-then-compare
//! in the handler would reopen the TOCTOU window plan 06b closed across all 33 admin
//! mutation sites.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{AuthProviderSettingsService, RequestContext},
    domain::{
        AuthProviderSettingsCreateRequest, AuthProviderSettingsPatchRequest,
        AuthProviderSettingsRecord, ListResponse, PageQuery, SetupAuthMethodsResponse,
        SetupSignInMethodsResponse,
    },
    error::{AppError, ErrorResponse},
};

use super::admin::{admin_actor, etag_headers, require_if_match};

#[utoipa::path(
    get, path = "/api/v1/admin/setup/sign-in-methods", tag = "admin-setup",
    description = "Lists the enabled methods a human can sign in with, so a login screen can render its buttons. Deliberately UNAUTHENTICATED: the credential a caller would present here is the one signing in produces, so requiring it makes the sign-in screen unrenderable on a deployment whose bootstrap system key has been removed. Every field returned is one the browser already transmits or receives during the sign-in it is about to start - client_id, issuer, authorization_url and requested_scopes all appear in the OAuth authorization URL. It carries NO allowed_email_domains: that is the deny-by-default admin-claim policy and stays behind GET /api/v1/admin/setup/auth-methods, which remains authenticated. Methods of kind jwks are omitted; they are machine token verification, not a sign-in button.",
    responses(
        (status = 200, description = "Enabled sign-in methods, narrowed to what a login screen renders", body = SetupSignInMethodsResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    )
)]
pub async fn get_setup_sign_in_methods(
    State(state): State<AppState>,
) -> Result<Json<SetupSignInMethodsResponse>, AppError> {
    // No `HeaderMap`, no `Actor`, no `security(...)` annotation — the same shape as
    // `get_setup_claim_status`, and for the same reason. The signature is the documentation:
    // there is nothing here to authenticate *against*, because the response is confined to
    // what the sign-in flow itself puts in front of the browser.
    AuthProviderSettingsService::new(&state)?
        .public_sign_in_methods()
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/setup/auth-methods", tag = "admin-setup",
    responses(
        (status = 200, description = "Enabled auth methods and their non-secret configuration. Authenticated on purpose: the response includes allowed_email_domains - the deny-by-default admin-claim policy - alongside the issuer and client id, and that is reconnaissance-worthy even though Moira stores no client secret. Plan 08's console calls this server-side with the system key it already holds. A login screen that only needs to render sign-in buttons must call the anonymous GET /api/v1/admin/setup/sign-in-methods instead; it returns a strictly narrower projection with no policy in it.", body = SetupAuthMethodsResponse),
        (status = 401, description = "Authentication failed", body = ErrorResponse),
        (status = 403, description = "Caller type or scope is not permitted", body = ErrorResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []))
)]
pub async fn get_setup_auth_methods(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SetupAuthMethodsResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AuthProviderSettingsService::new(&state)?
        .setup_auth_methods(&actor)
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/auth/providers", tag = "admin-auth-settings",
    request_body = AuthProviderSettingsCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response instead of creating a second configuration.")),
    responses(
        (status = 201, description = "Auth provider configuration created. The body carries non-secret configuration only; Moira accepts and stores no OAuth client secret.", body = AuthProviderSettingsRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "duplicate_auth_provider, idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_auth_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthProviderSettingsCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<AuthProviderSettingsRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    // The replay flag is deliberately unused here: the frozen contract documents 201 for
    // this operation, fresh or replayed, and a replay returns the stored body verbatim so
    // the two are indistinguishable to the client anyway. Only the claim endpoint
    // distinguishes them, because plan 08's wizard renders a different message.
    let (record, _replayed) = AuthProviderSettingsService::new(&state)?
        .create(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/auth/providers", tag = "admin-auth-settings",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated auth provider configurations", body = ListResponse<AuthProviderSettingsRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_auth_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<AuthProviderSettingsRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    AuthProviderSettingsService::new(&state)?
        .list(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/auth/providers/{id}", tag = "admin-auth-settings",
    params(("id" = Uuid, Path, description = "Auth provider configuration identifier")),
    responses(
        (status = 200, description = "Auth provider configuration. `client_id` is always returned: it is the non-secret anchor the console fingerprints to detect configuration drift against the client secret it stores itself.", body = AuthProviderSettingsRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_auth_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AuthProviderSettingsRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = AuthProviderSettingsService::new(&state)?
        .get(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/auth/providers/{id}", tag = "admin-auth-settings",
    request_body = AuthProviderSettingsPatchRequest,
    params(
        ("id" = Uuid, Path, description = "Auth provider configuration identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Auth provider configuration updated. `issuer` and `client_id` are ordinary mutable configuration: Moira binds no secret to them, so changing either is not a conflict.", body = AuthProviderSettingsRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_auth_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<AuthProviderSettingsPatchRequest>,
) -> Result<(HeaderMap, Json<AuthProviderSettingsRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AuthProviderSettingsService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .patch(&actor, &ctx, id, expected_version, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/auth/providers/{id}", tag = "admin-auth-settings",
    params(
        ("id" = Uuid, Path, description = "Auth provider configuration identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 204, description = "Auth provider configuration deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_auth_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AuthProviderSettingsService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    service.delete(&actor, &ctx, id, expected_version).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/admin/auth/providers/{id}/enable", tag = "admin-auth-settings",
    params(
        ("id" = Uuid, Path, description = "Auth provider configuration identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Auth provider configuration enabled. Refused with auth_provider_method_config_incomplete if the method's required non-secret configuration is missing; Moira never checks for a client secret, which it does not store.", body = AuthProviderSettingsRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_auth_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AuthProviderSettingsRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AuthProviderSettingsService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_enabled(&actor, &ctx, id, expected_version, true)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/auth/providers/{id}/disable", tag = "admin-auth-settings",
    params(
        ("id" = Uuid, Path, description = "Auth provider configuration identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version")
    ),
    responses(
        (status = 200, description = "Auth provider configuration disabled", body = AuthProviderSettingsRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_auth_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AuthProviderSettingsRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let service = AuthProviderSettingsService::new(&state)?;
    let expected_version = require_if_match(&headers)?;
    let record = service
        .set_enabled(&actor, &ctx, id, expected_version, false)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}
