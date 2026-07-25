//! Provider-credential administration, including the per-user credential lists.
//!
//! `rotate_credential` is the one mutation that already carries `expected_version` into the
//! command envelope (plan 04), which makes it the worked example plan 06b replicates across
//! the remaining If-Match sites. It moved here verbatim, that plumbing included.
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
            CREDENTIALS_CURSOR, PageRequest, USER_CREDENTIALS_CURSOR, admin_command_spec,
            audit_success, authorize_credential_record, authorize_credential_scope, command_hasher,
            load_credential_record, mask_credential_secret, paginate, require_active_row,
            schedule_runtime_cache_invalidation, success_audit, validate_credential_scope,
            validate_credential_secret,
        },
    },
    domain::{
        CredentialCreateRequest, CredentialPatchRequest, CredentialRecord, ExternalUserId,
        ListCursor, ListResponse, RotateCredentialRequest,
    },
    error::AppError,
    infra::{
        pg_rows::{credential_type_to_db, scope_type_to_db},
        repositories::{AdminRepository, PgAdminRepository},
    },
    security::{
        Actor, CredentialAadParts, ENVELOPE_VERSION_V1, SecretCipher, credential_aad,
        secret_fingerprint,
    },
};

pub(crate) struct CredentialAdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> CredentialAdminService<'a> {
    pub(crate) fn new(state: &'a AppState, repo: PgAdminRepository) -> Self {
        Self { state, repo }
    }

    pub(crate) async fn create_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: CredentialCreateRequest,
    ) -> Result<CredentialRecord, AppError> {
        self.state.authz.require(actor, "moira:credentials:write")?;
        let spec = admin_command_spec(ctx, actor, "credential.create", json!({}), &request)?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let cipher = self.state.cipher.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, |transaction| {
                Box::pin(async move {
                    require_active_row(transaction, "providers", request.provider_id, "provider")
                        .await?;
                    validate_credential_scope(&request)?;
                    authorize_credential_scope(&actor, &request.scope)?;
                    validate_credential_secret(&request.credential_type, &request.secret)?;
                    let id = Uuid::now_v7();
                    let plaintext = serde_json::to_vec(&request.secret).map_err(|err| {
                        AppError::BadRequest(format!("invalid credential secret: {err}"))
                    })?;
                    let aad = credential_aad(CredentialAadParts {
                        credential_id: id,
                        provider_id: request.provider_id,
                        credential_type: credential_type_to_db(&request.credential_type),
                        scope_type: scope_type_to_db(&request.scope.scope_type()),
                        external_tenant_id: request.scope.external_tenant_id(),
                        application_id: request.scope.application_id(),
                        external_user_id: request.scope.external_user_id(),
                        encryption_version: ENVELOPE_VERSION_V1,
                    });
                    let encrypted = cipher.encrypt(&plaintext, aad.as_bytes())?;
                    let fingerprint = secret_fingerprint(&plaintext);
                    let masked = mask_credential_secret(&request.secret);
                    let record = transaction
                        .create_credential(id, &request, &encrypted, &fingerprint, &masked)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "credential.create",
                            "provider_credential",
                            Some(record.id.to_string()),
                            json!({
                                "provider_id": record.provider_id,
                                "credential_type": record.credential_type,
                                "scope": record.scope,
                            }),
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

    pub(crate) async fn list_credentials(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<CredentialRecord>, AppError> {
        self.state.authz.require(actor, "moira:credentials:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_credentials(page.decode(CREDENTIALS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, CREDENTIALS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn list_user_credentials(
        &self,
        actor: &Actor,
        external_user_id: &str,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<CredentialRecord>, AppError> {
        self.state.authz.require(actor, "moira:credentials:read")?;
        ExternalUserId::parse(external_user_id.to_string())?;
        let page = page.into();
        let rows = self
            .repo
            .list_user_credentials(
                external_user_id,
                page.decode(USER_CREDENTIALS_CURSOR)?,
                page.limit(),
            )
            .await?;
        Ok(paginate(rows, &page, USER_CREDENTIALS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn get_credential(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<CredentialRecord, AppError> {
        self.state.authz.require(actor, "moira:credentials:read")?;
        let record = self.repo.get_credential(id).await?;
        authorize_credential_record(actor, &record)?;
        Ok(record)
    }

    pub(crate) async fn patch_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: CredentialPatchRequest,
    ) -> Result<CredentialRecord, AppError> {
        self.state.authz.require(actor, "moira:credentials:write")?;
        let existing = self.repo.get_credential(id).await?;
        authorize_credential_record(actor, &existing)?;
        if let Some(priority) = request.priority
            && priority < 0
        {
            return Err(AppError::BadRequest(
                "credential priority must be non-negative".to_string(),
            ));
        }
        let record = self.repo.patch_credential(id, &request).await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "credential.update",
            "provider_credential",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn rotate_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: RotateCredentialRequest,
    ) -> Result<CredentialRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:credentials:rotate")?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "credential.rotate",
            json!({ "credential_id": id }),
            &request,
        )?
        .with_expected_version(Some(expected_version));
        let actor = actor.clone();
        let ctx = ctx.clone();
        let cipher = self.state.cipher.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, |transaction| {
                Box::pin(async move {
                    let existing = load_credential_record(transaction, id).await?;
                    authorize_credential_record(&actor, &existing)?;
                    validate_credential_secret(&existing.credential_type, &request.secret)?;
                    let plaintext = serde_json::to_vec(&request.secret).map_err(|err| {
                        AppError::BadRequest(format!("invalid credential secret: {err}"))
                    })?;
                    let aad = credential_aad(CredentialAadParts {
                        credential_id: existing.id,
                        provider_id: existing.provider_id,
                        credential_type: credential_type_to_db(&existing.credential_type),
                        scope_type: scope_type_to_db(&existing.scope_type),
                        external_tenant_id: existing.external_tenant_id.as_deref(),
                        application_id: existing.application_id,
                        external_user_id: existing.external_user_id.as_deref(),
                        encryption_version: ENVELOPE_VERSION_V1,
                    });
                    let encrypted = cipher.encrypt(&plaintext, aad.as_bytes())?;
                    let fingerprint = secret_fingerprint(&plaintext);
                    let masked = mask_credential_secret(&request.secret);
                    let record = transaction
                        .rotate_credential(
                            id,
                            Some(expected_version),
                            &encrypted,
                            &fingerprint,
                            &masked,
                        )
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "credential.rotate",
                            "provider_credential",
                            Some(id.to_string()),
                            json!({}),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 200, Some(record.id.to_string()))
                })
            })
            .await?;
        if !outcome.replayed {
            schedule_runtime_cache_invalidation(self.state);
        }
        Ok(outcome.response)
    }

    pub(crate) async fn validate_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<CredentialRecord, AppError> {
        self.state.authz.require(actor, "moira:credentials:write")?;
        let stored = self.repo.load_credential_secret(id).await?;
        let aad = credential_aad(CredentialAadParts {
            credential_id: stored.record.id,
            provider_id: stored.record.provider_id,
            credential_type: credential_type_to_db(&stored.record.credential_type),
            scope_type: scope_type_to_db(&stored.record.scope_type),
            external_tenant_id: stored.record.external_tenant_id.as_deref(),
            application_id: stored.record.application_id,
            external_user_id: stored.record.external_user_id.as_deref(),
            encryption_version: stored.record.encryption_version,
        });
        let _plaintext = self
            .state
            .cipher
            .decrypt(&stored.encrypted, aad.as_bytes())?;
        let record = self.repo.mark_credential_validated(id).await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "credential.validate",
            "provider_credential",
            Some(id.to_string()),
            json!({ "provider_id": record.provider_id }),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn set_credential_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<CredentialRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:credentials:disable")?;
        let existing = self.repo.get_credential(id).await?;
        authorize_credential_record(actor, &existing)?;
        let status = if enabled { "active" } else { "disabled" };
        let record = self.repo.set_credential_status(id, status).await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            if enabled {
                "credential.enable"
            } else {
                "credential.disable"
            },
            "provider_credential",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn delete_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:credentials:delete")?;
        let existing = self.repo.get_credential(id).await?;
        authorize_credential_record(actor, &existing)?;
        self.repo.soft_delete_credential(id).await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "credential.delete",
            "provider_credential",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub(crate) async fn delete_user_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        external_user_id: &str,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:credentials:delete")?;
        ExternalUserId::parse(external_user_id.to_string())?;
        self.repo
            .soft_delete_user_credential(external_user_id, id)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "credential.delete",
            "provider_credential",
            Some(id.to_string()),
            json!({ "external_user_id": external_user_id }),
        )
        .await
    }
}
