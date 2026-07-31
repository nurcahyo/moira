//! Application administration: create, list, read, patch, delete, enable/disable.
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
            APPLICATIONS_CURSOR, PageRequest, admin_command_spec, command_hasher, paginate,
            require_non_empty, success_audit, validate_application_identifiers,
        },
    },
    domain::{
        ApplicationCreateRequest, ApplicationPatchRequest, ApplicationRecord, ListCursor,
        ListResponse,
    },
    error::AppError,
    infra::repositories::{AdminRepository, PgAdminRepository},
    security::Actor,
};

pub(crate) struct ApplicationAdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> ApplicationAdminService<'a> {
    pub(crate) fn new(state: &'a AppState, repo: PgAdminRepository) -> Self {
        Self { state, repo }
    }

    pub(crate) async fn create_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ApplicationCreateRequest,
    ) -> Result<ApplicationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:applications:write")?;
        let spec = admin_command_spec(ctx, actor, "application.create", json!({}), &request)?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, |transaction| {
                Box::pin(async move {
                    validate_application_identifiers(
                        request.external_application_id.as_deref(),
                        request.application_slug.as_deref(),
                    )?;
                    require_non_empty("display_name", &request.display_name)?;
                    let record = transaction
                        .create_application(Uuid::now_v7(), &request)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "application.create",
                            "application",
                            Some(record.id.to_string()),
                            json!({
                                "external_application_id": record.external_application_id,
                                "application_slug": record.application_slug,
                            }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.to_string()))
                })
            })
            .await?;
        Ok(outcome.response)
    }

    pub(crate) async fn list_applications(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ApplicationRecord>, AppError> {
        self.state.authz.require(actor, "moira:applications:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_applications(page.decode(APPLICATIONS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, APPLICATIONS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn get_application(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApplicationRecord, AppError> {
        self.state.authz.require(actor, "moira:applications:read")?;
        self.repo.get_application(id).await
    }

    pub(crate) async fn patch_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: ApplicationPatchRequest,
    ) -> Result<ApplicationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:applications:write")?;
        if request.external_application_id.is_some() || request.application_slug.is_some() {
            validate_application_identifiers(
                request.external_application_id.as_deref(),
                request.application_slug.as_deref(),
            )?;
        }
        if let Some(display_name) = &request.display_name {
            require_non_empty("display_name", display_name)?;
        }
        let record = self
            .repo
            .patch_application(
                id,
                expected_version,
                &request,
                success_audit(
                    actor,
                    ctx,
                    "application.update",
                    "application",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await?;
        Ok(record)
    }

    pub(crate) async fn delete_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:applications:delete")?;
        self.repo
            .soft_delete_application(
                id,
                expected_version,
                success_audit(
                    actor,
                    ctx,
                    "application.delete",
                    "application",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn set_application_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<ApplicationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:applications:write")?;
        let record = self
            .repo
            .set_application_status(
                id,
                expected_version,
                if enabled { "active" } else { "disabled" },
                success_audit(
                    actor,
                    ctx,
                    if enabled {
                        "application.enable"
                    } else {
                        "application.disable"
                    },
                    "application",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        Ok(record)
    }
}
