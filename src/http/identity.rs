//! Admin identity claiming over HTTP (plan 07, module 11).
//!
//! Two routes, deliberately asymmetric in their authentication:
//!
//! * `GET /api/v1/admin/setup/claim-status` is **unauthenticated**, because its entire
//!   response is one boolean that an attacker could infer anyway from the fact that the
//!   instance is freshly deployed, and because a setup wizard must be able to ask "do I
//!   need to show the claim flow?" before any human holds a credential.
//! * `POST /api/v1/admin/setup/claim` accepts an `X-Moira-System-Key` and **nothing else** —
//!   not even a bearer JWT that verifies perfectly.
//!
//! # Why these live under `/api/v1/admin/` even though one of them is anonymous
//!
//! When `MOIRA_DOCS__EXPOSE_ADMIN` is false, `openapi::public_document` strips every path
//! starting `/api/v1/admin/`, so `claim-status` is absent from the *public* spec. That
//! affects spec visibility only and never routing: the route still serves unauthenticated
//! traffic, and plan 08's wizard consumes the endpoint rather than the published document.
//! Do **not** "fix" the spec omission by moving the path out from under the admin prefix —
//! that would also move it out of the admin-strip protection covering the other nine
//! operations this plan adds, and out of the admin body-limit and timeout layers.

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{AdminIdentityService, ClaimCredential, RequestContext},
    domain::{
        AdminIdentityPatchRequest, AdminIdentityRecord, AdminInviteCreateRequest,
        AdminInvitePreviewRequest, AdminInvitePreviewResponse, AdminInviteRecord,
        AdminInviteRedeemRequest, AdminInviteSecretResponse, ClaimAdminIdentityRequest,
        ListResponse, PageQuery, SetupClaimStatusResponse,
    },
    error::{AppError, ErrorResponse},
    security::{Actor, header_string},
};

use super::admin::{etag_headers, require_if_match};

#[utoipa::path(
    get, path = "/api/v1/admin/setup/claim-status", tag = "admin-setup",
    responses(
        (status = 200, description = "Whether an admin identity has been claimed. Intentionally returns a single boolean and nothing else - no count, timestamp, issuer, or subject - so that an unauthenticated caller learns nothing about the deployment beyond whether the setup wizard should be shown.", body = SetupClaimStatusResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    )
)]
pub async fn get_setup_claim_status(
    State(state): State<AppState>,
) -> Result<Json<SetupClaimStatusResponse>, AppError> {
    // No `HeaderMap`, no `Actor`, no `security(...)` annotation. The signature is the
    // documentation: there is nothing here to authenticate *against*, because the response
    // is one bit and carries no deployment detail.
    AdminIdentityService::new(&state)?
        .claim_status()
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/setup/claim", tag = "admin-setup",
    request_body = ClaimAdminIdentityRequest,
    description = "Grants Moira admin scope to a specific (issuer, subject). Requires an X-Moira-System-Key; a bare trusted-JWT bearer token is refused even if it verifies. `email` and `email_verified` are REQUIRED. The email domain allow-list is DENY-BY-DEFAULT: a claim is refused 403 unless an enabled auth-provider configuration governs the target issuer AND its `allowed_email_domains` explicitly contains the email's domain. An unconfigured or empty allow-list denies every claim, including the first one - there is no first-claim exemption and no bootstrap bypass. Configure the policy first via POST /api/v1/admin/auth/providers, then POST /api/v1/admin/auth/providers/{id}/enable.",
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response with status 200 instead of creating a second grant; without one, retrying an already-succeeded claim returns 409.")),
    responses(
        (status = 201, description = "Admin identity granted", body = AdminIdentityRecord),
        (status = 200, description = "Idempotent replay of a prior successful claim", body = AdminIdentityRecord),
        (status = 400, description = "unregistered_trusted_issuer, admin_claim_email_required, setup_token_not_supported, or invalid_request (malformed body)", body = ErrorResponse),
        (status = 401, description = "setup_claim_credential_required or an invalid system key", body = ErrorResponse),
        (status = 403, description = "admin_claim_email_not_verified or admin_claim_domain_not_allowed", body = ErrorResponse),
        (status = 409, description = "admin_identity_already_claimed, idempotency_conflict, or idempotency_in_progress", body = ErrorResponse),
        (status = 422, description = "invalid_request - the body is well-formed JSON but violates the schema, e.g. `email` or `email_verified` omitted - or scope_invalid", body = ErrorResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    ),
    security(("systemKeyAuth" = []))
)]
pub async fn claim_admin_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    // Not a bare `Json<T>`: with `email` and `email_verified` required (decision D5), an
    // omitted field is rejected by the extractor before the handler body runs, and axum's
    // default rejection is bare plain text with no `code` and no `message_key` - a
    // CONVENTIONS §4 violation on a brand-new endpoint.
    body: Result<Json<ClaimAdminIdentityRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<AdminIdentityRecord>), AppError> {
    let Json(request) = body.map_err(claim_body_rejection)?;
    let credential = resolve_claim_credential(&state, &headers, &request).await?;
    let ctx = RequestContext::from_headers(&headers);
    let (record, replayed) = AdminIdentityService::new(&state)?
        .claim(&ctx, credential, request)
        .await?;
    // The status code, not the notice, distinguishes fresh from replayed: a replay returns
    // the stored body verbatim, so both carry `moira.notice.admin_identity_claimed`.
    Ok((
        if replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(record),
    ))
}

/// Answers "may this caller submit a claim at all", and only that.
///
/// It deliberately performs **no policy evaluation** — whether the claim may *succeed* is
/// module 10's question, asked inside the service on every credential path. Keeping the two
/// apart is what guarantees the deny-by-default domain policy cannot be skipped by a caller
/// who merely presented a strong enough credential.
///
/// **This must never fall through to `state.auth.authenticate_admin`.** That method accepts
/// a bearer JWT, which is precisely the credential this endpoint exists to refuse.
async fn resolve_claim_credential(
    state: &AppState,
    headers: &HeaderMap,
    request: &ClaimAdminIdentityRequest,
) -> Result<ClaimCredential, AppError> {
    if let Some(raw_key) = header_string(headers, "x-moira-system-key") {
        let actor = state
            .auth
            .verify_system_key_only(state.pool()?, &raw_key)
            .await?;
        return Ok(ClaimCredential::SystemKey(actor));
    }

    // Decision D1: the one-time setup-token path is deferred. The field survives in the
    // schema so plan 08's generated client keeps typechecking, but a caller who populates
    // it is told so explicitly rather than being handed the generic "no credential"
    // answer - which would read as "my token was wrong" instead of "this deployment has
    // no token path". The service refuses it again as belt and braces.
    if request.setup_token.is_some() {
        return Err(AppError::coded(
            StatusCode::BAD_REQUEST,
            "setup_token_not_supported",
            "the one-time setup token path is not available on this deployment",
        ));
    }

    Err(AppError::coded(
        StatusCode::UNAUTHORIZED,
        "setup_claim_credential_required",
        "a system key is required to claim an admin identity",
    ))
}

/// Keeps a schema-violating body inside Moira's `ErrorResponse` envelope (CONVENTIONS §4)
/// instead of axum's bare plain-text rejection.
///
/// `rejection.status()` is preserved rather than flattened, so axum's own distinction
/// survives: 400 for malformed JSON or a missing content type, 422 for well-formed JSON
/// that violates the schema. `invalid_request` is an existing catalog key, so this adds no
/// key — it reuses the one whose description already fits ("the request cannot be parsed or
/// violates a basic contract rule").
///
/// **Scope discipline:** this mapping is applied to this handler only. Every pre-existing
/// admin handler takes a bare `Json<T>` and has the same §4 gap; fixing them all is a
/// repo-wide change that would violate this plan's pure-iteration constraint, and it is
/// recorded as a deferred follow-up instead.
fn claim_body_rejection(rejection: JsonRejection) -> AppError {
    AppError::coded(
        rejection.status(),
        "invalid_request",
        "the claim request body is malformed or does not match the required schema",
    )
}

// =======================================================================================
// Plan 09 wave 2 — admin invitations and grant administration.
//
// # Every path below stays under `/api/v1/admin/`
//
// Plan 09's body puts `admin-invites/{preview,redeem}` *outside* the admin prefix, citing
// "07's non-admin-credential path precedent". That precedent is the opposite one, and it
// is written into this file's header as a prohibition: moving a path out from under the
// prefix also moves it out of the admin-strip protection, the admin body limit and the
// admin timeout. `claim-status` is anonymous and stays under the prefix for exactly that
// reason. The prefix is about layers and spec visibility, not about scope gating.
// =======================================================================================

/// `authenticate_admin` plus the **verified** issuer, which the two ownership operations
/// need because ownership is row state keyed by `(issuer, subject)`.
///
/// Delegates to the same `AuthService` entry point `admin_actor` does, so there is one
/// transcription of "authenticate the admin plane".
async fn admin_actor_with_issuer(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Actor, Option<String>), AppError> {
    state
        .auth
        .authenticate_admin_with_issuer(state.pool()?, headers)
        .await
}

#[utoipa::path(
    post, path = "/api/v1/admin/admin-invites", tag = "admin-invites",
    request_body = AdminInviteCreateRequest,
    description = "Mints a single-use, time-limited, email- or domain-bound invitation token so an existing admin can onboard a colleague without the bootstrap system key. Scope moira:admins:invite. The token is returned EXACTLY ONCE, here; it is stored Argon2id-hashed with the deployment pepper and cannot be read back. There is no unbound 'anyone with the link' form, and the lifetime is capped server-side at 72 hours. An invitation authorises its holder to SUBMIT a redemption - it is never an exemption from the deny-by-default allowed_email_domains policy, which is enforced again at redemption.",
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response instead of minting a second invitation; the replayed body omits the token, because the token is shown exactly once.")),
    responses(
        (status = 201, description = "Invitation created. The `secret` field carries the token and is present only on this first response; a replay returns the same body with `secret` null and `secret_retrievable` false.", body = AdminInviteSecretResponse),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = 422, description = "admin_invite_expiry_too_long, or invalid_request for a constraint value that is not a usable address or bare domain", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_admin_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<AdminInviteCreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<AdminInviteSecretResponse>), AppError> {
    let Json(request) = body.map_err(invite_body_rejection)?;
    let (actor, issuer) = admin_actor_with_issuer(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let (response, _replayed) = AdminIdentityService::new(&state)?
        .create_invite(&actor, issuer.as_deref(), &ctx, request)
        .await?;
    // 201 whether fresh or replayed: the frozen contract documents one success status,
    // and a replay returns the stored body, so the two are indistinguishable anyway.
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get, path = "/api/v1/admin/admin-invites", tag = "admin-invites",
    params(PageQuery),
    responses(
        (status = 200, description = "Paginated invitations. No token value is ever returned after creation, and the record type has no field for one.", body = ListResponse<AdminInviteRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_admin_invites(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<AdminInviteRecord>>, AppError> {
    let actor = super::admin::admin_actor(&state, &headers).await?;
    AdminIdentityService::new(&state)?
        .list_invites(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get, path = "/api/v1/admin/admin-invites/{id}", tag = "admin-invites",
    params(("id" = Uuid, Path, description = "Invitation identifier")),
    responses(
        (status = 200, description = "Invitation. `expired` is derived from `expires_at` on read, because `status` deliberately has no 'expired' value - nothing sweeps for one.", body = AdminInviteRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or invite_not_found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_admin_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AdminInviteRecord>), AppError> {
    let actor = super::admin::admin_actor(&state, &headers).await?;
    let record = AdminIdentityService::new(&state)?
        .get_invite(&actor, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/admin-invites/{id}/revoke", tag = "admin-invites",
    description = "Withdraws a pending invitation. Scope moira:admins:invite. Distinct from expiry: a revocation is a deliberate withdrawal, and the invitee is told so.",
    params(
        ("id" = Uuid, Path, description = "Invitation identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response.")
    ),
    responses(
        (status = 200, description = "Invitation revoked", body = AdminInviteRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "invite_already_consumed, idempotency_conflict, or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Authentication, authorization, invite_not_found, or invite_revoked error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn revoke_admin_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AdminInviteRecord>), AppError> {
    let actor = super::admin::admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let (record, _replayed) = AdminIdentityService::new(&state)?
        .revoke_invite(&actor, &ctx, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post, path = "/api/v1/admin/admin-invites/preview", tag = "admin-invites",
    request_body = AdminInvitePreviewRequest,
    description = "Describes the invitation a token belongs to, so an invite page can render before its visitor has signed in. Deliberately UNAUTHENTICATED, and credentialed by the invitation token in the request BODY: the visitor is unauthenticated by construction - they open the link before signing in - so the credential a scope check would demand is the one signing in produces. POST rather than GET, and body rather than path or query, so the token cannot land in an access log, a proxy log, or a Referer chain. The response is confined to the invitation's own constraint and expiry: no inviter, no identifier, no policy, no deployment detail. A token that matches nothing is refused at the indexed prefix lookup, before any password hashing runs.",
    responses(
        (status = 200, description = "The invitation's constraint and expiry", body = AdminInvitePreviewResponse),
        (status = 403, description = "invite_expired or invite_revoked", body = ErrorResponse),
        (status = 404, description = "invite_not_found - no live invitation matches this token", body = ErrorResponse),
        (status = 409, description = "invite_already_consumed", body = ErrorResponse),
        (status = "4XX", description = "Malformed or schema-violating request body", body = ErrorResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    )
)]
pub async fn preview_admin_invite(
    State(state): State<AppState>,
    body: Result<Json<AdminInvitePreviewRequest>, JsonRejection>,
) -> Result<Json<AdminInvitePreviewResponse>, AppError> {
    // No `HeaderMap`, no `Actor`, no `security(...)` annotation. The signature is the
    // documentation, as it is for `get_setup_claim_status`: there is nothing here to
    // authenticate against, because the request body carries the credential.
    let Json(request) = body.map_err(invite_body_rejection)?;
    AdminIdentityService::new(&state)?
        .preview_invite(&request)
        .await
        .map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/admin/admin-invites/redeem", tag = "admin-invites",
    request_body = AdminInviteRedeemRequest,
    description = "Redeems an invitation into an admin_identities grant for the presenting identity. Requires BOTH the token in the body and a bearer JWT from a registered trusted issuer - the token says which invitation, the JWT says which (issuer, subject) the grant is for. A system key or consumer key is refused: neither carries an (issuer, subject) pair. The bearer token is verified for IDENTITY ONLY; no scope it asserts is read, and no existing grant is applied to it. `email` and `email_verified` are REQUIRED (plan 07 decision D5). Policy is enforced twice and reported separately: the invitation's own email/domain constraint yields invite_email_mismatch or invite_domain_mismatch, and the deny-by-default provider allowed_email_domains policy yields admin_claim_domain_not_allowed. An invitation grants NO exemption from the second. Every check runs before the transactional envelope, so a refused redemption does NOT consume the invitation and the same link still works once an operator widens the allow-list.",
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response instead of creating a second grant.")),
    responses(
        (status = 201, description = "Admin access granted and the invitation consumed", body = AdminIdentityRecord),
        (status = 403, description = "invite_expired, invite_revoked, invite_email_mismatch, invite_domain_mismatch, admin_claim_email_not_verified, or admin_claim_domain_not_allowed. On any of these the invitation is NOT consumed.", body = ErrorResponse),
        (status = 404, description = "invite_not_found", body = ErrorResponse),
        (status = 409, description = "invite_already_consumed, admin_identity_already_claimed, idempotency_conflict, or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request or authentication error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []))
)]
pub async fn redeem_admin_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<AdminInviteRedeemRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<AdminIdentityRecord>), AppError> {
    let Json(request) = body.map_err(invite_body_rejection)?;
    // `verify_trusted_jwt_identity`, never `authenticate_admin`: the redeemer holds no
    // grant yet — creating one is the point — and the narrow return type means no
    // token-asserted scope is even readable from the service below.
    let identity = state
        .auth
        .verify_trusted_jwt_identity(state.pool()?, &headers)
        .await?;
    let ctx = RequestContext::from_headers(&headers);
    let (record, replayed) = AdminIdentityService::new(&state)?
        .redeem_invite(&identity, &ctx, request)
        .await?;
    Ok((
        if replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(record),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/admin-identities", tag = "admin-identities",
    params(PageQuery),
    description = "Lists admin-identity grants. Scope moira:admins:read. Revoked grants are included: 'who used to hold admin' is what an operator auditing an incident needs, and omitting them would make this list disagree with the audit log.",
    responses(
        (status = 200, description = "Paginated admin identity grants", body = ListResponse<AdminIdentityRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_admin_identities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<AdminIdentityRecord>>, AppError> {
    let actor = super::admin::admin_actor(&state, &headers).await?;
    AdminIdentityService::new(&state)?
        .list_identities(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    patch, path = "/api/v1/admin/admin-identities/{id}", tag = "admin-identities",
    request_body = AdminIdentityPatchRequest,
    description = "Transfers ownership by setting or clearing `is_primary`. Ownership is ROW STATE, not a scope: Moira's authorization core grants a moira:admin-holding trusted-JWT actor every scope by implication with no per-scope opt-out, and every grant carries moira:admin by default, so a 'manage admins' scope would be held by every admin and could not express ownership at all. The caller must therefore itself be a primary admin - or present the bootstrap system key, which remains the documented break-glass path. Clearing the last active primary is refused with admin_identity_last_primary.",
    params(
        ("id" = Uuid, Path, description = "Admin identity identifier"),
        ("If-Match" = i64, Header, description = "Required current resource version"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response.")
    ),
    responses(
        (status = 200, description = "Ownership updated", body = AdminIdentityRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 403, description = "admin_identity_not_primary - the caller is not a primary admin", body = ErrorResponse),
        (status = 404, description = "admin_identity_not_found", body = ErrorResponse),
        (status = 409, description = "admin_identity_last_primary, admin_identity_already_revoked, resource_version_conflict, idempotency_conflict, or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request or authentication error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_admin_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<AdminIdentityPatchRequest>, JsonRejection>,
) -> Result<(HeaderMap, Json<AdminIdentityRecord>), AppError> {
    let Json(request) = body.map_err(invite_body_rejection)?;
    let (actor, issuer) = admin_actor_with_issuer(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let expected_version = require_if_match(&headers)?;
    let (record, _replayed) = AdminIdentityService::new(&state)?
        .set_identity_primary(
            &actor,
            issuer.as_deref(),
            &ctx,
            id,
            expected_version,
            request,
        )
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/admin-identities/{id}", tag = "admin-identities",
    description = "Revokes an admin-identity grant. This is plan 07's explicitly deferred revoke endpoint. The revocation is SOFT - status becomes 'revoked' and the row survives, so the (issuer, subject) uniqueness key keeps blocking a silent re-grant and the audit history stays intact - which is why the response carries the updated record rather than an empty 204. It does NOT reset setup_state.claimed: setup-required is a one-way transition, so revoking the last admin leaves system-key break-glass as the re-entry path rather than reopening the unauthenticated land-grab window. Requires a primary caller, and refuses to revoke the last active primary.",
    params(
        ("id" = Uuid, Path, description = "Admin identity identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the stored response.")
    ),
    responses(
        (status = 200, description = "Grant revoked", body = AdminIdentityRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 403, description = "admin_identity_not_primary - the caller is not a primary admin", body = ErrorResponse),
        (status = 404, description = "admin_identity_not_found", body = ErrorResponse),
        (status = 409, description = "admin_identity_already_revoked, admin_identity_last_primary, idempotency_conflict, or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request or authentication error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_admin_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<AdminIdentityRecord>), AppError> {
    let (actor, issuer) = admin_actor_with_issuer(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let (record, _replayed) = AdminIdentityService::new(&state)?
        .revoke_identity(&actor, issuer.as_deref(), &ctx, id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

/// The same envelope-preserving mapping [`claim_body_rejection`] performs, applied to the
/// bodies this wave adds.
///
/// It exists separately only so the message names the right operation; the code and the
/// status handling are identical, and both reuse the existing `invalid_request` key
/// rather than minting one.
fn invite_body_rejection(rejection: JsonRejection) -> AppError {
    AppError::coded(
        rejection.status(),
        "invalid_request",
        "the request body is malformed or does not match the required schema",
    )
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, extract::FromRequest, http::Request};

    use super::*;

    async fn rejection_for(content_type: Option<&str>, body: &str) -> JsonRejection {
        let mut builder = Request::builder().method("POST").uri("/");
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        let request = builder
            .body(Body::from(body.to_string()))
            .expect("build request");
        Json::<ClaimAdminIdentityRequest>::from_request(request, &())
            .await
            .expect_err("the body must be rejected")
    }

    /// The whole point of the `Result<Json<_>, JsonRejection>` extractor: an omitted
    /// required field must reach the client as Moira's coded envelope, not as axum's bare
    /// plain-text rejection with no `code` and no `message_key`.
    #[tokio::test]
    async fn claim_body_rejection_maps_to_the_invalid_request_code() {
        let rejection = rejection_for(
            Some("application/json"),
            r#"{"issuer":"https://issuer.example","subject":"sub-1"}"#,
        )
        .await;
        let status = rejection.status();
        let error = claim_body_rejection(rejection);

        assert_eq!(error.status(), status);
        // The code literal is what `AppError::message_key` derives
        // `moira.error.invalid_request` from (`src/error.rs`), so asserting on it asserts
        // on the key the client sees.
        assert!(
            error.to_string().starts_with("invalid_request:"),
            "unexpected error: {error}"
        );
    }

    /// Axum distinguishes "this is not JSON at all" from "this JSON does not match the
    /// schema" by status, and that distinction is worth preserving: a client can tell a
    /// transport/serialisation bug from a contract violation without parsing prose.
    #[tokio::test]
    async fn the_rejection_mapping_preserves_axums_own_status_distinction() {
        let malformed = rejection_for(Some("application/json"), "{not json").await;
        let schema_violating = rejection_for(
            Some("application/json"),
            r#"{"issuer":"https://issuer.example","subject":"sub-1","email":"a@b.test"}"#,
        )
        .await;

        assert_eq!(
            claim_body_rejection(malformed).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            claim_body_rejection(schema_violating).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    /// Both branches that refuse a claim outright emit catalogued codes rather than
    /// `AppError::Unauthorized`/`BadRequest`, whose generic `unauthorized`/`bad_request`
    /// codes would silently drop the specific key plan 08 binds to.
    #[tokio::test]
    async fn credential_resolution_refuses_an_uncredentialed_and_a_token_only_claim() {
        let state = AppState::new(crate::config::Settings::default(), None).expect("app state");
        let mut request = ClaimAdminIdentityRequest {
            issuer: "https://issuer.example".to_string(),
            subject: "sub-1".to_string(),
            email: "owner@example.com".to_string(),
            email_verified: true,
            scopes: vec!["moira:admin".to_string()],
            setup_token: None,
        };

        let missing = resolve_claim_credential(&state, &HeaderMap::new(), &request)
            .await
            .expect_err("no credential is refused");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert!(
            missing
                .to_string()
                .starts_with("setup_claim_credential_required:"),
            "unexpected error: {missing}"
        );

        request.setup_token = Some("moira_setup_whatever".to_string());
        let deferred = resolve_claim_credential(&state, &HeaderMap::new(), &request)
            .await
            .expect_err("the deferred token path is refused, not ignored");
        assert_eq!(deferred.status(), StatusCode::BAD_REQUEST);
        assert!(
            deferred
                .to_string()
                .starts_with("setup_token_not_supported:"),
            "unexpected error: {deferred}"
        );
    }
}
