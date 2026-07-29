//! Provider and provider-model administration.
//!
//! Providers and provider models share one service on purpose: a provider model always
//! resolves through its parent provider, and both paths share `require_active_row` plus the
//! same base-URL policy. Splitting them would duplicate that shared validation for no gain.
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
            PROVIDER_MODELS_CURSOR, PROVIDERS_CURSOR, PageRequest, admin_command_spec,
            audit_success, command_hasher, paginate, require_active_row, require_non_empty,
            schedule_runtime_cache_invalidation, success_audit, validate_provider_base_url,
            validate_provider_base_url_with_settings,
        },
    },
    domain::{
        ListCursor, ListResponse, ProviderCreateRequest, ProviderModelCreateRequest,
        ProviderModelPatchRequest, ProviderModelRecord, ProviderPatchRequest, ProviderRecord,
    },
    error::AppError,
    infra::repositories::{AdminRepository, PgAdminRepository},
    security::Actor,
};

pub(crate) struct ProviderAdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> ProviderAdminService<'a> {
    pub(crate) fn new(state: &'a AppState, repo: PgAdminRepository) -> Self {
        Self { state, repo }
    }

    pub(crate) async fn create_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ProviderCreateRequest,
    ) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:write")?;
        let spec = admin_command_spec(ctx, actor, "provider.create", json!({}), &request)?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let allow_private_provider_urls = self
            .state
            .settings
            .provider_security
            .allow_private_provider_urls;
        let allow_http_provider_urls = self
            .state
            .settings
            .provider_security
            .allow_http_provider_urls;
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, |transaction| {
                Box::pin(async move {
                    require_non_empty("display_name", &request.display_name)?;
                    let base_url = match request.base_url.as_deref() {
                        Some(value) => Some(validate_provider_base_url(
                            value,
                            allow_private_provider_urls,
                            allow_http_provider_urls,
                        )?),
                        None => None,
                    };
                    let record = transaction
                        .create_provider(Uuid::now_v7(), &request, base_url)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "provider.create",
                            "provider",
                            Some(record.id.to_string()),
                            json!({ "provider_type": record.provider_type }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.to_string()))
                })
            })
            .await?;
        if !outcome.replayed {
            schedule_runtime_cache_invalidation(self.state);
        }
        Ok(outcome.response)
    }

    pub(crate) async fn list_providers(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ProviderRecord>, AppError> {
        self.state.authz.require(actor, "moira:providers:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_providers(page.decode(PROVIDERS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, PROVIDERS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn get_provider(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:read")?;
        self.repo.get_provider(id).await
    }

    pub(crate) async fn patch_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: ProviderPatchRequest,
    ) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:write")?;
        if let Some(display_name) = &request.display_name {
            require_non_empty("display_name", display_name)?;
        }
        let base_url = match request.base_url.as_deref() {
            Some(value) => Some(Some(validate_provider_base_url_with_settings(
                self.state, value,
            )?)),
            None => None,
        };
        let record = self
            .repo
            .patch_provider(id, expected_version, &request, base_url)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "provider.update",
            "provider",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn delete_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:providers:delete")?;
        self.repo.soft_delete_provider(id, expected_version).await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "provider.delete",
            "provider",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub(crate) async fn set_provider_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:write")?;
        let record = self
            .repo
            .set_provider_status(
                id,
                expected_version,
                if enabled { "active" } else { "disabled" },
            )
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            if enabled {
                "provider.enable"
            } else {
                "provider.disable"
            },
            "provider",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn create_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        provider_id: Uuid,
        request: ProviderModelCreateRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:write")?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "provider_model.create",
            json!({ "provider_id": provider_id }),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, |transaction| {
                Box::pin(async move {
                    require_non_empty("model_key", &request.model_key)?;
                    require_active_row(transaction, "providers", provider_id, "provider").await?;
                    let record = transaction
                        .create_provider_model(Uuid::now_v7(), provider_id, &request)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "provider_model.create",
                            "provider_model",
                            Some(record.id.to_string()),
                            json!({ "provider_id": provider_id }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.to_string()))
                })
            })
            .await?;
        if !outcome.replayed {
            schedule_runtime_cache_invalidation(self.state);
        }
        Ok(outcome.response)
    }

    pub(crate) async fn list_provider_models(
        &self,
        actor: &Actor,
        provider_id: Uuid,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ProviderModelRecord>, AppError> {
        self.state.authz.require(actor, "moira:providers:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_provider_models(
                provider_id,
                page.decode(PROVIDER_MODELS_CURSOR)?,
                page.limit(),
            )
            .await?;
        Ok(paginate(rows, &page, PROVIDER_MODELS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn patch_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        expected_version: i64,
        request: ProviderModelPatchRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:write")?;
        let record = self
            .repo
            .patch_provider_model(model_id, expected_version, &request)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "provider_model.update",
            "provider_model",
            Some(model_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn get_provider_model(
        &self,
        actor: &Actor,
        model_id: Uuid,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:read")?;
        self.repo.get_provider_model(model_id).await
    }

    pub(crate) async fn delete_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:models:delete")?;
        self.repo
            .soft_delete_provider_model(model_id, expected_version)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "provider_model.delete",
            "provider_model",
            Some(model_id.to_string()),
            json!({}),
        )
        .await
    }

    pub(crate) async fn set_provider_model_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:write")?;
        let record = self
            .repo
            .set_provider_model_status(
                model_id,
                expected_version,
                if enabled { "active" } else { "disabled" },
            )
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            if enabled {
                "provider_model.enable"
            } else {
                "provider_model.disable"
            },
            "provider_model",
            Some(model_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }
}
