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
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};

use crate::{
    app::AppState,
    application::{AdminIdentityService, ClaimCredential, RequestContext},
    domain::{AdminIdentityRecord, ClaimAdminIdentityRequest, SetupClaimStatusResponse},
    error::{AppError, ErrorResponse},
    security::header_string,
};

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
