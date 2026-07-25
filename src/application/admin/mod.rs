//! `AdminService` — the stable facade over the per-context admin services.
//!
//! The handlers in `src/http/admin.rs`, `bootstrap_system_key` in `src/main.rs`, and the
//! integration fixtures all call `AdminService` by name. Splitting the former 2,436-line
//! `admin.rs` into bounded contexts therefore had to leave that surface untouched: every
//! one of the 46 methods below keeps its original name, parameter list, and return type,
//! and does nothing but forward to the sub-service that owns it.
//!
//! Ownership map:
//!
//! | Module | Service | Methods |
//! |---|---|---|
//! | [`applications`] | `ApplicationAdminService` | 6 |
//! | [`providers`] | `ProviderAdminService` | 12 (providers + provider models) |
//! | [`credentials`] | `CredentialAdminService` | 10 |
//! | [`keys`] | `ApiKeyAdminService` | 9 (system + consumer, sharing the table-generic trio) |
//! | [`jwt_issuers`] | `JwtIssuerAdminService` | 7 |
//! | [`audit`] | `AuditAdminService` | 2 |
//!
//! [`shared`] holds what belongs to no single context: the pagination machinery, the
//! idempotency/audit envelope helpers, the request validators, and the cursor scopes.
//!
//! The idempotency envelope itself — `AdminCommandRunner`, `admin_command_spec`, and the
//! `claim_idempotency` → `finalize_idempotency` sequence — was deliberately **not** touched
//! by the split. Each sub-service calls it exactly as the single impl block did.

mod applications;
mod audit;
mod credentials;
mod jwt_issuers;
mod keys;
mod providers;
pub(crate) mod shared;

use uuid::Uuid;

use crate::{
    app::AppState,
    application::RequestContext,
    domain::{
        ApiKeyRecord, ApiKeyRotateRequest, ApiKeySecretResponse, ApplicationCreateRequest,
        ApplicationPatchRequest, ApplicationRecord, AuditLogRecord, ConsumerKeyCreateRequest,
        CredentialCreateRequest, CredentialPatchRequest, CredentialRecord, ListResponse,
        ProviderCreateRequest, ProviderModelCreateRequest, ProviderModelPatchRequest,
        ProviderModelRecord, ProviderPatchRequest, ProviderRecord, RotateCredentialRequest,
        SystemKeyCreateRequest, TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest,
        TrustedJwtIssuerRecord,
    },
    error::AppError,
    infra::repositories::PgAdminRepository,
    security::Actor,
};

use applications::ApplicationAdminService;
use audit::AuditAdminService;
use credentials::CredentialAdminService;
use jwt_issuers::JwtIssuerAdminService;
use keys::ApiKeyAdminService;
use providers::ProviderAdminService;

pub use shared::PageRequest;

/// Re-exported so `crate::application::conversation` keeps resolving
/// `crate::application::admin::actor_fingerprint` — it must reuse this exact formula, since
/// the fingerprint is part of the `idempotency_records` unique index and of the advisory
/// lock key.
pub(crate) use shared::actor_fingerprint;

pub struct AdminService<'a> {
    applications: ApplicationAdminService<'a>,
    providers: ProviderAdminService<'a>,
    credentials: CredentialAdminService<'a>,
    keys: ApiKeyAdminService<'a>,
    jwt_issuers: JwtIssuerAdminService<'a>,
    audit: AuditAdminService<'a>,
}

impl<'a> AdminService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        // One pool lookup and one repository, cloned into each sub-service, exactly as the
        // single-struct version did.
        let repo = PgAdminRepository::new(state.pool()?.clone());
        Ok(Self {
            applications: ApplicationAdminService::new(state, repo.clone()),
            providers: ProviderAdminService::new(state, repo.clone()),
            credentials: CredentialAdminService::new(state, repo.clone()),
            keys: ApiKeyAdminService::new(state, repo.clone()),
            jwt_issuers: JwtIssuerAdminService::new(state, repo.clone()),
            audit: AuditAdminService::new(state, repo),
        })
    }

    pub async fn create_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ApplicationCreateRequest,
    ) -> Result<ApplicationRecord, AppError> {
        self.applications
            .create_application(actor, ctx, request)
            .await
    }

    pub async fn list_applications(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ApplicationRecord>, AppError> {
        self.applications.list_applications(actor, page).await
    }

    pub async fn get_application(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApplicationRecord, AppError> {
        self.applications.get_application(actor, id).await
    }

    pub async fn patch_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: ApplicationPatchRequest,
    ) -> Result<ApplicationRecord, AppError> {
        self.applications
            .patch_application(actor, ctx, id, request)
            .await
    }

    pub async fn delete_application(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.applications.delete_application(actor, ctx, id).await
    }

    pub async fn set_application_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<ApplicationRecord, AppError> {
        self.applications
            .set_application_enabled(actor, ctx, id, enabled)
            .await
    }

    pub async fn create_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ProviderCreateRequest,
    ) -> Result<ProviderRecord, AppError> {
        self.providers.create_provider(actor, ctx, request).await
    }

    pub async fn list_providers(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ProviderRecord>, AppError> {
        self.providers.list_providers(actor, page).await
    }

    pub async fn get_provider(&self, actor: &Actor, id: Uuid) -> Result<ProviderRecord, AppError> {
        self.providers.get_provider(actor, id).await
    }

    pub async fn patch_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: ProviderPatchRequest,
    ) -> Result<ProviderRecord, AppError> {
        self.providers.patch_provider(actor, ctx, id, request).await
    }

    pub async fn delete_provider(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.providers.delete_provider(actor, ctx, id).await
    }

    pub async fn set_provider_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<ProviderRecord, AppError> {
        self.providers
            .set_provider_enabled(actor, ctx, id, enabled)
            .await
    }

    pub async fn create_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        provider_id: Uuid,
        request: ProviderModelCreateRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        self.providers
            .create_provider_model(actor, ctx, provider_id, request)
            .await
    }

    pub async fn list_provider_models(
        &self,
        actor: &Actor,
        provider_id: Uuid,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ProviderModelRecord>, AppError> {
        self.providers
            .list_provider_models(actor, provider_id, page)
            .await
    }

    pub async fn patch_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        request: ProviderModelPatchRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        self.providers
            .patch_provider_model(actor, ctx, model_id, request)
            .await
    }

    pub async fn get_provider_model(
        &self,
        actor: &Actor,
        model_id: Uuid,
    ) -> Result<ProviderModelRecord, AppError> {
        self.providers.get_provider_model(actor, model_id).await
    }

    pub async fn delete_provider_model(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
    ) -> Result<(), AppError> {
        self.providers
            .delete_provider_model(actor, ctx, model_id)
            .await
    }

    pub async fn set_provider_model_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        model_id: Uuid,
        enabled: bool,
    ) -> Result<ProviderModelRecord, AppError> {
        self.providers
            .set_provider_model_enabled(actor, ctx, model_id, enabled)
            .await
    }

    pub async fn create_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: CredentialCreateRequest,
    ) -> Result<CredentialRecord, AppError> {
        self.credentials
            .create_credential(actor, ctx, request)
            .await
    }

    pub async fn list_credentials(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<CredentialRecord>, AppError> {
        self.credentials.list_credentials(actor, page).await
    }

    pub async fn list_user_credentials(
        &self,
        actor: &Actor,
        external_user_id: &str,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<CredentialRecord>, AppError> {
        self.credentials
            .list_user_credentials(actor, external_user_id, page)
            .await
    }

    pub async fn get_credential(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<CredentialRecord, AppError> {
        self.credentials.get_credential(actor, id).await
    }

    pub async fn patch_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: CredentialPatchRequest,
    ) -> Result<CredentialRecord, AppError> {
        self.credentials
            .patch_credential(actor, ctx, id, request)
            .await
    }

    pub async fn rotate_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: RotateCredentialRequest,
    ) -> Result<CredentialRecord, AppError> {
        self.credentials
            .rotate_credential(actor, ctx, id, expected_version, request)
            .await
    }

    pub async fn validate_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<CredentialRecord, AppError> {
        self.credentials.validate_credential(actor, ctx, id).await
    }

    pub async fn set_credential_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<CredentialRecord, AppError> {
        self.credentials
            .set_credential_enabled(actor, ctx, id, enabled)
            .await
    }

    pub async fn delete_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.credentials.delete_credential(actor, ctx, id).await
    }

    pub async fn delete_user_credential(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        external_user_id: &str,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.credentials
            .delete_user_credential(actor, ctx, external_user_id, id)
            .await
    }

    pub async fn create_system_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SystemKeyCreateRequest,
    ) -> Result<ApiKeySecretResponse, AppError> {
        self.keys.create_system_key(actor, ctx, request).await
    }

    pub async fn list_system_keys(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ApiKeyRecord>, AppError> {
        self.keys.list_system_keys(actor, page).await
    }

    pub async fn get_system_key(&self, actor: &Actor, id: Uuid) -> Result<ApiKeyRecord, AppError> {
        self.keys.get_system_key(actor, id).await
    }

    pub async fn create_consumer_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ConsumerKeyCreateRequest,
    ) -> Result<ApiKeySecretResponse, AppError> {
        self.keys.create_consumer_key(actor, ctx, request).await
    }

    pub async fn list_consumer_keys(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ApiKeyRecord>, AppError> {
        self.keys.list_consumer_keys(actor, page).await
    }

    pub async fn get_consumer_key(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApiKeyRecord, AppError> {
        self.keys.get_consumer_key(actor, id).await
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
        self.keys
            .rotate_key(actor, ctx, table, namespace, id, request)
            .await
    }

    pub async fn revoke_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        table: &str,
        id: Uuid,
    ) -> Result<ApiKeyRecord, AppError> {
        self.keys.revoke_key(actor, ctx, table, id).await
    }

    pub async fn delete_key(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        table: &str,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.keys.delete_key(actor, ctx, table, id).await
    }

    pub async fn create_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: TrustedJwtIssuerCreateRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.jwt_issuers
            .create_trusted_jwt_issuer(actor, ctx, request)
            .await
    }

    pub async fn list_trusted_jwt_issuers(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<TrustedJwtIssuerRecord>, AppError> {
        self.jwt_issuers.list_trusted_jwt_issuers(actor, page).await
    }

    pub async fn get_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.jwt_issuers.get_trusted_jwt_issuer(actor, id).await
    }

    pub async fn patch_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: TrustedJwtIssuerPatchRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.jwt_issuers
            .patch_trusted_jwt_issuer(actor, ctx, id, request)
            .await
    }

    pub async fn set_trusted_jwt_issuer_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.jwt_issuers
            .set_trusted_jwt_issuer_enabled(actor, ctx, id, enabled)
            .await
    }

    pub async fn refresh_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        self.jwt_issuers
            .refresh_trusted_jwt_issuer(actor, ctx, id)
            .await
    }

    pub async fn delete_trusted_jwt_issuer(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.jwt_issuers
            .delete_trusted_jwt_issuer(actor, ctx, id)
            .await
    }

    pub async fn list_audit_logs(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<AuditLogRecord>, AppError> {
        self.audit.list_audit_logs(actor, page).await
    }

    pub async fn get_audit_log(&self, actor: &Actor, id: Uuid) -> Result<AuditLogRecord, AppError> {
        self.audit.get_audit_log(actor, id).await
    }
}
