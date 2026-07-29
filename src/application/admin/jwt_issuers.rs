//! Trusted JWT issuer administration, including the JWKS refresh path.
//!
//! Constructed and owned by [`crate::application::AdminService`], which re-exposes every
//! method below under its original name and signature.

use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandMutation, AdminCommandRunner, RequestContext,
        admin::shared::{
            PageRequest, TRUSTED_JWT_ISSUERS_CURSOR, admin_command_spec, audit_denied,
            audit_success, command_hasher, paginate, reject_denied_jwks_url, require_non_empty,
            success_audit, validate_jwt_algorithm_list,
        },
    },
    domain::{
        ListCursor, ListResponse, TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest,
        TrustedJwtIssuerRecord,
    },
    error::AppError,
    infra::repositories::{AdminRepository, PgAdminRepository},
    security::Actor,
};

pub(crate) struct JwtIssuerAdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> JwtIssuerAdminService<'a> {
    pub(crate) fn new(state: &'a AppState, repo: PgAdminRepository) -> Self {
        Self { state, repo }
    }

    pub(crate) async fn create_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: TrustedJwtIssuerCreateRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        // Deliberately *before* the command runner: validation performs DNS, which must
        // not be held inside a database transaction, and a denied URL must never reach
        // the idempotency ledger, let alone `trusted_jwt_issuers`.
        reject_denied_jwks_url(self.state, &request.jwks_url).await?;
        let spec = admin_command_spec(ctx, actor, "jwt_issuer.create", json!({}), &request)?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, |transaction| {
                Box::pin(async move {
                    validate_jwt_algorithm_list(&request.allowed_algorithms)?;
                    require_non_empty("issuer", &request.issuer)?;
                    let record = transaction
                        .create_trusted_jwt_issuer(Uuid::now_v7(), &request)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "jwt_issuer.create",
                            "trusted_jwt_issuer",
                            Some(record.id.to_string()),
                            json!({ "issuer": record.issuer }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.to_string()))
                })
            })
            .await?;
        Ok(outcome.response)
    }

    pub(crate) async fn list_trusted_jwt_issuers(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<TrustedJwtIssuerRecord>, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_trusted_jwt_issuers(page.decode(TRUSTED_JWT_ISSUERS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, TRUSTED_JWT_ISSUERS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn get_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:read")?;
        self.repo.get_trusted_jwt_issuer(id).await
    }

    pub(crate) async fn patch_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: TrustedJwtIssuerPatchRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        if let Some(jwks_url) = &request.jwks_url {
            reject_denied_jwks_url(self.state, jwks_url).await?;
        }
        if let Some(allowed) = &request.allowed_algorithms {
            validate_jwt_algorithm_list(allowed)?;
        }
        let record = self
            .repo
            .patch_trusted_jwt_issuer(id, expected_version, &request)
            .await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "jwt_issuer.update",
            "trusted_jwt_issuer",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn set_trusted_jwt_issuer_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:jwt-issuers:delete")?;
        let status = if enabled { "active" } else { "disabled" };
        let record = self
            .repo
            .set_trusted_jwt_issuer_status(id, expected_version, status)
            .await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            if enabled {
                "jwt_issuer.enable"
            } else {
                "jwt_issuer.disable"
            },
            "trusted_jwt_issuer",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn refresh_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        let issuer = self.repo.get_trusted_jwt_issuer(id).await?;

        // Plan 03 / P1-2, third fetch site. This used to be a bare
        // `state.http.get(jwks_url).send().await?.error_for_status()?`: no URL
        // validation, no timeout, no size cap, no content-type check, redirects
        // followed, and the body discarded so the cache was never warmed.
        //
        // `error_for_status()?` also made it a **status oracle** — the upstream's status
        // came straight back to the caller, so an ordinary admin credential could sweep
        // the pod's internal network for open ports by reading the response code.
        // Every outcome now collapses to the one catalogued `jwks_url_rejected` shape,
        // exactly as `into_unauthorized` protects the verification path; the reason
        // survives only in the server-side log below.
        let keys = match self.state.jwks_cache.refresh(&issuer.jwks_url).await {
            Ok(jwks) => jwks.keys.len(),
            Err(failure) => {
                tracing::warn!(
                    issuer_id = %id,
                    jwks_url = %issuer.jwks_url,
                    reason = failure.reason().as_str(),
                    detail = %failure.detail(),
                    "admin jwks refresh rejected by the SSRF/resource policy"
                );
                audit_denied(
                    &self.repo,
                    actor,
                    ctx,
                    "jwt_issuer.refresh_jwks",
                    "trusted_jwt_issuer",
                    Some(id.to_string()),
                    json!({
                        "jwks_url": issuer.jwks_url,
                        "reason": failure.reason().as_str(),
                    }),
                )
                .await;
                return Err(failure.into_registration_error());
            }
        };

        let record = self.repo.touch_trusted_jwt_issuer(id).await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "jwt_issuer.refresh_jwks",
            "trusted_jwt_issuer",
            Some(id.to_string()),
            json!({ "keys": keys }),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn delete_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        self.repo
            .soft_delete_trusted_jwt_issuer(id, expected_version)
            .await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "jwt_issuer.delete",
            "trusted_jwt_issuer",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }
}
