use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandIdempotency, AdminCommandMutation, AdminCommandRunner, AdminCommandSpec,
        RequestContext,
    },
    domain::{
        ApiKeyRecord, ApiKeyRotateRequest, ApiKeySecretResponse, ApplicationCreateRequest,
        ApplicationPatchRequest, ApplicationRecord, ApplicationSlug, AuditLogInsert,
        AuditLogRecord, AuditResult, ConsumerKeyCreateRequest, CredentialCreateRequest,
        CredentialPatchRequest, CredentialRecord, CredentialScope, CredentialSecret,
        ExternalApplicationId, ExternalTenantId, ExternalUserId, ListResponse,
        ProviderCreateRequest, ProviderModelCreateRequest, ProviderModelPatchRequest,
        ProviderModelRecord, ProviderPatchRequest, ProviderRecord, RotateCredentialRequest,
        SystemKeyCreateRequest, TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest,
        TrustedJwtIssuerRecord,
    },
    error::AppError,
    infra::{
        pg_rows::{credential_record_from_row, credential_type_to_db, scope_type_to_db},
        repositories::{
            AdminRepository, KeyMaterial, PgAdminCommandTransaction, PgAdminRepository,
        },
    },
    security::{
        Actor, AuthorizationService, CredentialAadParts, ENVELOPE_VERSION_V1, IdempotencyHasher,
        SecretCipher, credential_aad, secret_fingerprint,
    },
};

pub struct AdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> AdminService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        Ok(Self {
            repo: PgAdminRepository::new(state.pool()?.clone()),
            state,
        })
    }

    /// The keyed hasher every admin command ledger write goes through (plan 03, P1-1).
    fn command_hasher(&self) -> IdempotencyHasher {
        self.state.idempotency_hasher.clone()
    }

    pub async fn create_application(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
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

    pub async fn list_applications(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<ApplicationRecord>, AppError> {
        self.state.authz.require(actor, "moira:applications:read")?;
        self.repo
            .list_applications(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_application(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApplicationRecord, AppError> {
        self.state.authz.require(actor, "moira:applications:read")?;
        self.repo.get_application(id).await
    }

    pub async fn patch_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
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
        let record = self.repo.patch_application(id, &request).await?;
        self.audit_success(
            actor,
            ctx,
            "application.update",
            "application",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn delete_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:applications:delete")?;
        self.repo.soft_delete_application(id).await?;
        self.audit_success(
            actor,
            ctx,
            "application.delete",
            "application",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn set_application_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<ApplicationRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:applications:write")?;
        let record = self
            .repo
            .set_application_status(id, if enabled { "active" } else { "disabled" })
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
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
        )
        .await?;
        Ok(record)
    }

    pub async fn create_provider(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
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
            self.schedule_runtime_cache_invalidation();
        }
        Ok(outcome.response)
    }

    pub async fn list_providers(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<ProviderRecord>, AppError> {
        self.state.authz.require(actor, "moira:providers:read")?;
        self.repo.list_providers(limit).await.map(ListResponse::new)
    }

    pub async fn get_provider(&self, actor: &Actor, id: Uuid) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:read")?;
        self.repo.get_provider(id).await
    }

    pub async fn patch_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: ProviderPatchRequest,
    ) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:write")?;
        if let Some(display_name) = &request.display_name {
            require_non_empty("display_name", display_name)?;
        }
        let base_url = match request.base_url.as_deref() {
            Some(value) => Some(Some(self.validate_provider_base_url(value)?)),
            None => None,
        };
        let record = self.repo.patch_provider(id, &request, base_url).await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
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

    pub async fn delete_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:providers:delete")?;
        self.repo.soft_delete_provider(id).await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
            actor,
            ctx,
            "provider.delete",
            "provider",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn set_provider_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<ProviderRecord, AppError> {
        self.state.authz.require(actor, "moira:providers:write")?;
        let record = self
            .repo
            .set_provider_status(id, if enabled { "active" } else { "disabled" })
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
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

    pub async fn create_provider_model(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
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
            self.schedule_runtime_cache_invalidation();
        }
        Ok(outcome.response)
    }

    pub async fn list_provider_models(
        &self,
        actor: &Actor,
        provider_id: Uuid,
        limit: i64,
    ) -> Result<ListResponse<ProviderModelRecord>, AppError> {
        self.state.authz.require(actor, "moira:providers:read")?;
        self.repo
            .list_provider_models(provider_id, limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn patch_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        request: ProviderModelPatchRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:write")?;
        let record = self.repo.patch_provider_model(model_id, &request).await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
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

    pub async fn get_provider_model(
        &self,
        actor: &Actor,
        model_id: Uuid,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:read")?;
        self.repo.get_provider_model(model_id).await
    }

    pub async fn delete_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:models:delete")?;
        self.repo.soft_delete_provider_model(model_id).await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
            actor,
            ctx,
            "provider_model.delete",
            "provider_model",
            Some(model_id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn set_provider_model_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        enabled: bool,
    ) -> Result<ProviderModelRecord, AppError> {
        self.state.authz.require(actor, "moira:models:write")?;
        let record = self
            .repo
            .set_provider_model_status(model_id, if enabled { "active" } else { "disabled" })
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit_success(
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

    pub async fn create_credential(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
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
            self.schedule_runtime_cache_invalidation();
        }
        Ok(outcome.response)
    }

    pub async fn list_credentials(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<CredentialRecord>, AppError> {
        self.state.authz.require(actor, "moira:credentials:read")?;
        self.repo
            .list_credentials(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn list_user_credentials(
        &self,
        actor: &Actor,
        external_user_id: &str,
        limit: i64,
    ) -> Result<ListResponse<CredentialRecord>, AppError> {
        self.state.authz.require(actor, "moira:credentials:read")?;
        ExternalUserId::parse(external_user_id.to_string())?;
        self.repo
            .list_user_credentials(external_user_id, limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_credential(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<CredentialRecord, AppError> {
        self.state.authz.require(actor, "moira:credentials:read")?;
        let record = self.repo.get_credential(id).await?;
        authorize_credential_record(actor, &record)?;
        Ok(record)
    }

    pub async fn patch_credential(
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
        self.audit_success(
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

    pub async fn rotate_credential(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
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
            self.schedule_runtime_cache_invalidation();
        }
        Ok(outcome.response)
    }

    pub async fn validate_credential(
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
        self.audit_success(
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

    pub async fn set_credential_enabled(
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
        self.audit_success(
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

    pub async fn delete_credential(
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
        self.audit_success(
            actor,
            ctx,
            "credential.delete",
            "provider_credential",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn delete_user_credential(
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
        self.audit_success(
            actor,
            ctx,
            "credential.delete",
            "provider_credential",
            Some(id.to_string()),
            json!({ "external_user_id": external_user_id }),
        )
        .await
    }

    pub async fn create_system_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SystemKeyCreateRequest,
    ) -> Result<ApiKeySecretResponse, AppError> {
        self.state.authz.require(actor, "moira:system-keys:write")?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "system_key.create",
            json!({ "key_type": "system" }),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let authz = self.state.authz.clone();
        let key_hasher = self.state.key_hasher.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    let mut request = request;
                    request.scopes = validate_key_request(
                        &actor,
                        &authz,
                        &request.display_name,
                        &request.scopes,
                    )?;
                    let generated = key_hasher.generate("moira_sys")?;
                    let record = transaction
                        .create_system_key(
                            Uuid::now_v7(),
                            &request,
                            KeyMaterial {
                                key_prefix: &generated.key_prefix,
                                key_hash: &generated.key_hash,
                                fingerprint: &generated.fingerprint,
                                pepper_version: &generated.pepper_version,
                            },
                        )
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "system_key.create",
                            "system_api_key",
                            Some(record.id.to_string()),
                            json!({ "scopes": record.scopes }),
                        ))
                        .await?;
                    let response = ApiKeySecretResponse {
                        resource: record.clone(),
                        secret: Some(generated.raw_key),
                        secret_retrievable: true,
                    };
                    AdminCommandMutation::with_replay_response(
                        response,
                        sanitized_key_response(&record),
                        201,
                        Some(record.id.to_string()),
                    )
                })
            })
            .await?;
        Ok(outcome.response)
    }

    pub async fn list_system_keys(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<ApiKeyRecord>, AppError> {
        self.state.authz.require(actor, "moira:system-keys:read")?;
        self.repo
            .list_system_keys(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_system_key(&self, actor: &Actor, id: Uuid) -> Result<ApiKeyRecord, AppError> {
        self.state.authz.require(actor, "moira:system-keys:read")?;
        self.repo.get_system_key(id).await
    }

    pub async fn create_consumer_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ConsumerKeyCreateRequest,
    ) -> Result<ApiKeySecretResponse, AppError> {
        self.state
            .authz
            .require(actor, "moira:consumer-keys:write")?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "consumer_key.create",
            json!({
                "key_type": "consumer",
                "application_id": request.application_id,
            }),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let authz = self.state.authz.clone();
        let key_hasher = self.state.key_hasher.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    let mut request = request;
                    request.scopes = validate_key_request(
                        &actor,
                        &authz,
                        &request.display_name,
                        &request.scopes,
                    )?;
                    require_active_row(
                        transaction,
                        "applications",
                        request.application_id,
                        "application",
                    )
                    .await?;
                    let generated = key_hasher.generate("moira_cons")?;
                    let record = transaction
                        .create_consumer_key(
                            Uuid::now_v7(),
                            request.application_id,
                            &request.display_name,
                            &request.scopes,
                            request.expires_at,
                            KeyMaterial {
                                key_prefix: &generated.key_prefix,
                                key_hash: &generated.key_hash,
                                fingerprint: &generated.fingerprint,
                                pepper_version: &generated.pepper_version,
                            },
                        )
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "consumer_key.create",
                            "consumer_api_key",
                            Some(record.id.to_string()),
                            json!({
                                "application_id": record.application_id,
                                "scopes": record.scopes,
                            }),
                        ))
                        .await?;
                    let response = ApiKeySecretResponse {
                        resource: record.clone(),
                        secret: Some(generated.raw_key),
                        secret_retrievable: true,
                    };
                    AdminCommandMutation::with_replay_response(
                        response,
                        sanitized_key_response(&record),
                        201,
                        Some(record.id.to_string()),
                    )
                })
            })
            .await?;
        Ok(outcome.response)
    }

    pub async fn list_consumer_keys(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<ApiKeyRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:consumer-keys:read")?;
        self.repo
            .list_consumer_keys(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_consumer_key(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApiKeyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:consumer-keys:read")?;
        self.repo.get_consumer_key(id).await
    }

    pub async fn rotate_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        table: &str,
        namespace: &str,
        id: Uuid,
        request: ApiKeyRotateRequest,
    ) -> Result<ApiKeySecretResponse, AppError> {
        let scope = if table == "system_api_keys" {
            "moira:system-keys:rotate"
        } else {
            "moira:consumer-keys:rotate"
        };
        self.state.authz.require(actor, scope)?;
        let operation = if table == "system_api_keys" {
            "system_key.rotate"
        } else {
            "consumer_key.rotate"
        };
        let spec = admin_command_spec(
            ctx,
            actor,
            operation,
            json!({ "key_type": table, "key_id": id }),
            &request,
        )?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let key_hasher = self.state.key_hasher.clone();
        let table = table.to_string();
        let namespace = namespace.to_string();
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    let generated = key_hasher.generate(&namespace)?;
                    let record = transaction
                        .rotate_key(
                            &table,
                            id,
                            KeyMaterial {
                                key_prefix: &generated.key_prefix,
                                key_hash: &generated.key_hash,
                                fingerprint: &generated.fingerprint,
                                pepper_version: &generated.pepper_version,
                            },
                        )
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "api_key.rotate",
                            &table,
                            Some(id.to_string()),
                            json!({}),
                        ))
                        .await?;
                    let response = ApiKeySecretResponse {
                        resource: record.clone(),
                        secret: Some(generated.raw_key),
                        secret_retrievable: true,
                    };
                    AdminCommandMutation::with_replay_response(
                        response,
                        sanitized_key_response(&record),
                        200,
                        Some(record.id.to_string()),
                    )
                })
            })
            .await?;
        Ok(outcome.response)
    }

    pub async fn revoke_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        table: &str,
        id: Uuid,
    ) -> Result<ApiKeyRecord, AppError> {
        let scope = if table == "system_api_keys" {
            "moira:system-keys:revoke"
        } else {
            "moira:consumer-keys:revoke"
        };
        self.state.authz.require(actor, scope)?;
        let record = self.repo.revoke_key(table, id).await?;
        self.audit_success(
            actor,
            ctx,
            "api_key.revoke",
            table,
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn delete_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        table: &str,
        id: Uuid,
    ) -> Result<(), AppError> {
        let scope = if table == "system_api_keys" {
            "moira:system-keys:revoke"
        } else {
            "moira:consumer-keys:revoke"
        };
        self.state.authz.require(actor, scope)?;
        self.repo.soft_delete_key(table, id).await?;
        self.audit_success(
            actor,
            ctx,
            "api_key.delete",
            table,
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn create_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: TrustedJwtIssuerCreateRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        let spec = admin_command_spec(ctx, actor, "jwt_issuer.create", json!({}), &request)?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), self.command_hasher())
            .execute(spec, |transaction| {
                Box::pin(async move {
                    validate_https_url("jwks_url", &request.jwks_url)?;
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

    pub async fn list_trusted_jwt_issuers(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<TrustedJwtIssuerRecord>, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:read")?;
        self.repo
            .list_trusted_jwt_issuers(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:read")?;
        self.repo.get_trusted_jwt_issuer(id).await
    }

    pub async fn patch_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: TrustedJwtIssuerPatchRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        if let Some(jwks_url) = &request.jwks_url {
            validate_https_url("jwks_url", jwks_url)?;
        }
        if let Some(allowed) = &request.allowed_algorithms {
            validate_jwt_algorithm_list(allowed)?;
        }
        let record = self.repo.patch_trusted_jwt_issuer(id, &request).await?;
        self.audit_success(
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

    pub async fn set_trusted_jwt_issuer_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:jwt-issuers:delete")?;
        let status = if enabled { "active" } else { "disabled" };
        let record = self.repo.set_trusted_jwt_issuer_status(id, status).await?;
        self.audit_success(
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

    pub async fn refresh_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        let issuer = self.repo.get_trusted_jwt_issuer(id).await?;
        self.state
            .http
            .get(&issuer.jwks_url)
            .send()
            .await?
            .error_for_status()?;
        let record = self.repo.touch_trusted_jwt_issuer(id).await?;
        self.audit_success(
            actor,
            ctx,
            "jwt_issuer.refresh_jwks",
            "trusted_jwt_issuer",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn delete_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:jwt-issuers:write")?;
        self.repo.soft_delete_trusted_jwt_issuer(id).await?;
        self.audit_success(
            actor,
            ctx,
            "jwt_issuer.delete",
            "trusted_jwt_issuer",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn list_audit_logs(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<AuditLogRecord>, AppError> {
        self.state.authz.require(actor, "moira:audit:read")?;
        self.repo
            .list_audit_logs(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_audit_log(&self, actor: &Actor, id: Uuid) -> Result<AuditLogRecord, AppError> {
        self.state.authz.require(actor, "moira:audit:read")?;
        self.repo.get_audit_log(id).await
    }

    async fn audit_success(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        action: &str,
        resource_type: &str,
        resource_id: Option<String>,
        metadata: Value,
    ) -> Result<(), AppError> {
        self.repo
            .insert_audit(AuditLogInsert {
                request_id: Some(ctx.request_id.clone()),
                actor_type: Some(format!("{:?}", actor.actor_type).to_ascii_lowercase()),
                actor_subject: actor.subject.clone(),
                delegated_subject: actor.delegated_subject.clone(),
                external_user_id: actor.external_user_id.clone(),
                external_tenant_id: actor.external_tenant_id.clone(),
                application_id: actor.internal_application_id,
                resource_type: resource_type.to_string(),
                resource_id,
                action: action.to_string(),
                result: AuditResult::Success,
                source_ip: ctx.source_ip,
                user_agent: ctx.user_agent.clone(),
                metadata,
            })
            .await
    }

    fn validate_provider_base_url(&self, value: &str) -> Result<String, AppError> {
        validate_provider_base_url(
            value,
            self.state
                .settings
                .provider_security
                .allow_private_provider_urls,
            self.state
                .settings
                .provider_security
                .allow_http_provider_urls,
        )
    }

    fn schedule_runtime_cache_invalidation(&self) {
        let cache = self.state.runtime_cache.clone();
        std::mem::drop(tokio::spawn(async move {
            cache.invalidate_all().await;
        }));
    }
}

fn validate_application_identifiers(
    external_application_id: Option<&str>,
    application_slug: Option<&str>,
) -> Result<(), AppError> {
    if external_application_id.is_none() && application_slug.is_none() {
        return Err(AppError::BadRequest(
            "external_application_id or application_slug is required".to_string(),
        ));
    }
    if let Some(value) = external_application_id {
        ExternalApplicationId::parse(value.to_string())?;
    }
    if let Some(value) = application_slug {
        ApplicationSlug::parse(value.to_string())?;
    }
    Ok(())
}

/// The single definition of the actor fingerprint used by the **admin-command envelope**
/// (`AdminCommandRunner` / `AdminCommandSpec`), covering the ten admin commands and the RAG
/// write commands in `crate::application::conversation`.
///
/// It is part of the `idempotency_records` unique index and of the advisory-lock key, so a
/// second, divergent copy would silently break cross-actor replay isolation. `pub(crate)`
/// exists so `crate::application::conversation` can reuse this exact formula
/// (`plans/02b-idempotency-replay.md` §5); do not copy the body anywhere.
///
/// **Warning — this is not the only fingerprint formula writing to `idempotency_records`.**
/// `crate::application::runtime_admin` holds a separate, *weaker* private copy (its own
/// `actor_fingerprint`, `src/application/runtime_admin.rs`) which hashes only
/// `{actor_type, subject, api_key_id}`. It omits the seven identity fields this function
/// includes — `trusted_jwt_issuer_id`, `internal_application_id`, `application_id`,
/// `tenant_id`, `external_user_id`, `external_tenant_id`, `delegated_subject` — yet it
/// feeds the **same** ledger table for the runtime-policy routes (`route.create`,
/// `routing_policy.create`, `agent_profile.create`, `provider_runtime_policy.*`).
/// Consequence, measured: two actors differing only in trusted-JWT issuer or in tenant
/// produce *different* fingerprints here but *identical* fingerprints there, so those
/// routes do not isolate replay across issuers or tenants.
///
/// That is known, pre-existing debt, deliberately out of scope for plan 02b ("No change to
/// `runtime_admin.rs`'s idempotency scheme") and deferred to
/// `plans/06-architecture-test-hygiene.md`, which owns unifying the two schemes. Do not
/// assume a fingerprint found in `idempotency_records` was produced by this function.
pub(crate) fn actor_fingerprint(actor: &Actor) -> String {
    let identity = serde_json::to_vec(&(
        actor.actor_type,
        &actor.subject,
        actor.api_key_id,
        actor.trusted_jwt_issuer_id,
        actor.internal_application_id,
        &actor.application_id,
        &actor.tenant_id,
        &actor.external_user_id,
        &actor.external_tenant_id,
        &actor.delegated_subject,
    ))
    .expect("actor identity fields are serializable");
    secret_fingerprint(&identity)
}

fn admin_command_spec<T: Serialize>(
    ctx: &RequestContext,
    actor: &Actor,
    operation: &str,
    path: Value,
    request: &T,
) -> Result<AdminCommandSpec, AppError> {
    AdminCommandSpec::new(operation, path, request).map(|spec| {
        spec.with_idempotency(
            ctx.idempotency_key
                .as_ref()
                .map(|key| AdminCommandIdempotency {
                    key: key.clone(),
                    actor_fingerprint: actor_fingerprint(actor),
                }),
        )
    })
}

fn success_audit(
    actor: &Actor,
    ctx: &RequestContext,
    action: &str,
    resource_type: &str,
    resource_id: Option<String>,
    metadata: Value,
) -> AuditLogInsert {
    AuditLogInsert {
        request_id: Some(ctx.request_id.clone()),
        actor_type: Some(format!("{:?}", actor.actor_type).to_ascii_lowercase()),
        actor_subject: actor.subject.clone(),
        delegated_subject: actor.delegated_subject.clone(),
        external_user_id: actor.external_user_id.clone(),
        external_tenant_id: actor.external_tenant_id.clone(),
        application_id: actor.internal_application_id,
        resource_type: resource_type.to_string(),
        resource_id,
        action: action.to_string(),
        result: AuditResult::Success,
        source_ip: ctx.source_ip,
        user_agent: ctx.user_agent.clone(),
        metadata,
    }
}

async fn require_active_row(
    transaction: &mut PgAdminCommandTransaction,
    table: &str,
    id: Uuid,
    resource_name: &str,
) -> Result<(), AppError> {
    let query = match table {
        "applications" => "select 1 from applications where id = $1 and deleted_at is null",
        "providers" => "select 1 from providers where id = $1 and deleted_at is null",
        _ => {
            return Err(AppError::Internal(format!(
                "unsupported admin command resource table {table}"
            )));
        }
    };
    let exists = sqlx::query_scalar::<_, i32>(query)
        .bind(id)
        .fetch_optional(transaction.connection())
        .await?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound(format!("{resource_name} {id}")))
    }
}

async fn load_credential_record(
    transaction: &mut PgAdminCommandTransaction,
    id: Uuid,
) -> Result<CredentialRecord, AppError> {
    let row = sqlx::query(
        r#"
        select id, provider_id, credential_type, scope_type, external_tenant_id,
               application_id, external_user_id, encryption_algorithm,
               encryption_version, secret_fingerprint, masked_secret, status,
               priority, expires_at, last_validated_at, last_used_at, metadata,
               created_at, updated_at, deleted_at, version, display_name
        from provider_credentials
        where id = $1 and deleted_at is null
        "#,
    )
    .bind(id)
    .fetch_optional(transaction.connection())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
    credential_record_from_row(&row)
}

fn sanitized_key_response(resource: &ApiKeyRecord) -> ApiKeySecretResponse {
    ApiKeySecretResponse {
        resource: resource.clone(),
        secret: None,
        secret_retrievable: false,
    }
}

fn validate_credential_scope(request: &CredentialCreateRequest) -> Result<(), AppError> {
    if request.priority < 0 {
        return Err(AppError::BadRequest(
            "credential priority must be non-negative".to_string(),
        ));
    }
    match &request.scope {
        CredentialScope::Global => {}
        CredentialScope::Tenant { external_tenant_id } => {
            ExternalTenantId::parse(external_tenant_id.clone())?;
        }
        CredentialScope::Application {
            external_tenant_id, ..
        } => {
            if let Some(value) = external_tenant_id {
                ExternalTenantId::parse(value.clone())?;
            }
        }
        CredentialScope::User {
            external_user_id,
            external_tenant_id,
            ..
        } => {
            ExternalUserId::parse(external_user_id.clone())?;
            if let Some(value) = external_tenant_id {
                ExternalTenantId::parse(value.clone())?;
            }
        }
    }
    Ok(())
}

fn authorize_credential_scope(actor: &Actor, scope: &CredentialScope) -> Result<(), AppError> {
    let Some(bound_app) = actor.internal_application_id else {
        return Ok(());
    };
    let Some(scope_app) = scope.application_id() else {
        return Err(AppError::Forbidden(
            "consumer principals may manage only application-bound credentials".to_string(),
        ));
    };
    if scope_app == bound_app {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "consumer principal cannot manage credentials for another application".to_string(),
        ))
    }
}

fn authorize_credential_record(actor: &Actor, record: &CredentialRecord) -> Result<(), AppError> {
    let Some(bound_app) = actor.internal_application_id else {
        return Ok(());
    };
    if record.application_id == Some(bound_app) {
        Ok(())
    } else {
        Err(AppError::NotFound(format!(
            "provider credential {}",
            record.id
        )))
    }
}

fn validate_credential_secret(
    credential_type: &crate::domain::CredentialType,
    secret: &CredentialSecret,
) -> Result<(), AppError> {
    match (credential_type, secret) {
        (crate::domain::CredentialType::ApiKey, CredentialSecret::ApiKey { api_key }) => {
            require_non_empty("api_key", api_key)
        }
        (crate::domain::CredentialType::Oauth2, CredentialSecret::OAuth2 { access_token, .. }) => {
            require_non_empty("access_token", access_token)
        }
        (
            crate::domain::CredentialType::BearerToken,
            CredentialSecret::BearerToken { bearer_token },
        ) => require_non_empty("bearer_token", bearer_token),
        (
            crate::domain::CredentialType::BasicAuth,
            CredentialSecret::BasicAuth { username, password },
        ) => {
            require_non_empty("username", username)?;
            require_non_empty("password", password)
        }
        (
            crate::domain::CredentialType::CustomHeaders,
            CredentialSecret::CustomHeaders { headers },
        ) => validate_custom_headers(headers),
        (
            crate::domain::CredentialType::AzureOpenAi,
            CredentialSecret::AzureOpenAi { api_key, .. },
        ) => require_non_empty("api_key", api_key),
        (
            crate::domain::CredentialType::ServiceAccount,
            CredentialSecret::ServiceAccount { payload },
        ) if payload.is_object() => Ok(()),
        _ => Err(AppError::BadRequest(
            "credential secret payload does not match credential_type".to_string(),
        )),
    }
}

fn validate_custom_headers(headers: &serde_json::Map<String, Value>) -> Result<(), AppError> {
    if headers.len() > 32 {
        return Err(AppError::BadRequest(
            "custom header count exceeds 32".to_string(),
        ));
    }
    for (name, value) in headers {
        let normalized = name.to_ascii_lowercase();
        if DANGEROUS_HEADERS.contains(&normalized.as_str()) {
            return Err(AppError::BadRequest(format!(
                "custom header {name} is not allowed"
            )));
        }
        if name.chars().any(|ch| ch.is_ascii_control() || ch == ':') {
            return Err(AppError::BadRequest(format!(
                "custom header {name} has an invalid name"
            )));
        }
        let Some(value) = value.as_str() else {
            return Err(AppError::BadRequest(format!(
                "custom header {name} must be a string"
            )));
        };
        if value.chars().any(char::is_control) {
            return Err(AppError::BadRequest(format!(
                "custom header {name} contains control characters"
            )));
        }
    }
    Ok(())
}

const DANGEROUS_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
    "proxy-authenticate",
    "upgrade",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "authorization",
    "cookie",
    "set-cookie",
];

fn mask_credential_secret(secret: &CredentialSecret) -> String {
    match secret {
        CredentialSecret::ApiKey { api_key } => crate::security::mask_plain_secret(api_key),
        CredentialSecret::BearerToken { bearer_token } => {
            crate::security::mask_plain_secret(bearer_token)
        }
        CredentialSecret::AzureOpenAi { api_key, .. } => {
            crate::security::mask_plain_secret(api_key)
        }
        CredentialSecret::OAuth2 { access_token, .. } => {
            crate::security::mask_plain_secret(access_token)
        }
        CredentialSecret::BasicAuth { username, .. } => format!("{username}:****"),
        CredentialSecret::CustomHeaders { .. } | CredentialSecret::ServiceAccount { .. } => {
            "structured-secret".to_string()
        }
    }
}

fn validate_key_request(
    actor: &Actor,
    authz: &AuthorizationService,
    display_name: &str,
    scopes: &[String],
) -> Result<Vec<String>, AppError> {
    require_non_empty("display_name", display_name)?;
    if scopes.is_empty() {
        return Err(AppError::BadRequest(
            "api key scopes must not be empty".to_string(),
        ));
    }
    let normalized = AuthorizationService::normalize_scopes(scopes)?;
    if !authz.can_grant(actor, &normalized) {
        return Err(AppError::coded(
            axum::http::StatusCode::FORBIDDEN,
            "system_key_scope_escalation",
            "principal cannot mint scopes broader than its effective scopes",
        ));
    }
    Ok(normalized)
}

fn require_non_empty(label: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        Err(AppError::BadRequest(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}

fn validate_jwt_algorithm_list(values: &[String]) -> Result<(), AppError> {
    if values.is_empty() {
        return Err(AppError::BadRequest(
            "allowed_algorithms must not be empty".to_string(),
        ));
    }
    for value in values {
        if !matches!(
            value.as_str(),
            "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512" | "ES256" | "ES384" | "EdDSA"
        ) {
            return Err(AppError::BadRequest(format!(
                "unsupported JWT algorithm {value}"
            )));
        }
    }
    Ok(())
}

fn validate_https_url(label: &str, value: &str) -> Result<(), AppError> {
    let parsed = url::Url::parse(value)
        .map_err(|err| AppError::BadRequest(format!("invalid {label}: {err}")))?;
    if parsed.scheme() != "https" {
        return Err(AppError::BadRequest(format!("{label} must use https")));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::BadRequest(format!("{label} must include a host")));
    }
    Ok(())
}

pub fn validate_provider_base_url(
    value: &str,
    allow_private: bool,
    allow_http: bool,
) -> Result<String, AppError> {
    let trimmed = value.trim().trim_end_matches('/').to_string();
    let parsed = url::Url::parse(&trimmed)
        .map_err(|err| AppError::BadRequest(format!("invalid provider base_url: {err}")))?;
    match parsed.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => {
            return Err(AppError::BadRequest(
                "provider base_url must use https unless HTTP is explicitly allowed".to_string(),
            ));
        }
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(AppError::BadRequest(
            "provider base_url must not contain credentials".to_string(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("provider base_url must include a host".to_string()))?;
    if is_cloud_metadata_host(host) {
        return Err(AppError::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "provider_url_not_allowed",
            "provider base_url targets a cloud metadata endpoint",
        ));
    }
    if !allow_private && (is_private_host(host) || resolves_to_forbidden_ip(host, &parsed)) {
        return Err(AppError::BadRequest(
            "provider base_url host is private, loopback, link-local, or otherwise unsafe"
                .to_string(),
        ));
    }
    Ok(trimmed)
}

fn is_private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(is_forbidden_ip)
}

fn resolves_to_forbidden_ip(host: &str, parsed: &url::Url) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return false;
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    (host, port)
        .to_socket_addrs()
        .map(|addresses| addresses.map(|address| address.ip()).any(is_forbidden_ip))
        .unwrap_or(false)
}

fn is_cloud_metadata_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "169.254.169.254"
            | "metadata.google.internal"
            | "metadata"
            | "instance-data"
            | "100.100.100.200"
    )
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip == Ipv4Addr::new(0, 0, 0, 0)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || is_ipv6_unicast_link_local(ip)
                || ip.is_multicast()
        }
    }
}

fn is_ipv6_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CredentialScope, CredentialType};

    #[test]
    fn credential_scope_validation_matches_contract() {
        let mut request = CredentialCreateRequest {
            provider_id: Uuid::now_v7(),
            credential_type: CredentialType::ApiKey,
            scope: CredentialScope::Global,
            secret: CredentialSecret::ApiKey {
                api_key: "sk-test".to_string(),
            },
            display_name: None,
            priority: 100,
            expires_at: None,
            metadata: Value::Object(Default::default()),
        };
        assert!(validate_credential_scope(&request).is_ok());

        request.scope = CredentialScope::User {
            external_user_id: String::new(),
            application_id: None,
            external_tenant_id: None,
        };
        assert!(validate_credential_scope(&request).is_err());

        request.scope = CredentialScope::User {
            external_user_id: "user-1".to_string(),
            application_id: Some(Uuid::now_v7()),
            external_tenant_id: None,
        };
        assert!(validate_credential_scope(&request).is_ok());
    }

    #[test]
    fn dangerous_custom_headers_are_rejected() {
        let mut headers = serde_json::Map::new();
        headers.insert("Authorization".to_string(), json!("Bearer secret"));
        assert!(validate_custom_headers(&headers).is_err());

        let mut safe = serde_json::Map::new();
        safe.insert("X-Project-Key".to_string(), json!("value"));
        assert!(validate_custom_headers(&safe).is_ok());
    }

    #[test]
    fn provider_url_policy_blocks_private_by_default() {
        assert!(validate_provider_base_url("https://api.example.com", false, false).is_ok());
        assert!(validate_provider_base_url("http://api.example.com", false, false).is_err());
        assert!(validate_provider_base_url("http://127.0.0.1:8000", false, true).is_err());
        assert!(validate_provider_base_url("http://127.0.0.1:8000", true, true).is_ok());
    }

    #[test]
    fn actor_fingerprint_is_shared_by_admin_and_conversation_commands() {
        // Asserts exactly one thing: `conversation_command_spec` embeds the fingerprint
        // produced by *this* module's `actor_fingerprint`, byte for byte, rather than
        // computing its own (plans/02b-idempotency-replay.md §5).
        //
        // It does NOT verify that the fingerprint depends on the actor at all. Both sides
        // call the same function, so this assertion still passes if that function is
        // degraded to return a constant — a mutation that reduced `actor_fingerprint` to a
        // fixed string survived this test. It compares admin against conversation, nothing
        // more; read "same function", never "correct function".
        //
        // Real actor-isolation coverage lives in
        // `different_actors_with_the_same_key_do_not_replay_each_others_responses`
        // (tests/rag_idempotency_replay.rs), which drives two distinct admin actors through
        // one idempotency key and requires two independent resources. That test does kill
        // the constant-fingerprint mutation; this one does not.
        use crate::{application::conversation::conversation_command_spec, security::ActorType};

        let actor = Actor {
            actor_type: ActorType::SystemKey,
            subject: Some("system-actor".to_string()),
            ..Actor::default()
        };
        let ctx = RequestContext {
            request_id: "req-test".to_string(),
            source_ip: None,
            user_agent: None,
            idempotency_key: Some("replay-key".to_string()),
        };

        let direct = actor_fingerprint(&actor);

        let spec =
            conversation_command_spec(&ctx, &actor, "rag.collection.create", json!({}), &json!({}))
                .unwrap();

        assert!(
            format!("{spec:?}").contains(&format!("actor_fingerprint: {direct:?}")),
            "conversation_command_spec must embed the exact fingerprint produced by \
             admin::actor_fingerprint, not a divergent copy"
        );
    }
}
