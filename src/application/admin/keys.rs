//! System- and consumer-API-key administration.
//!
//! Both key families live in one service because `rotate_key`, `revoke_key`, and `delete_key`
//! are table-generic — each takes a `table: &str` selecting `system_api_keys` or
//! `consumer_api_keys`. Splitting the families would force those three to be duplicated or
//! awkwardly shared.
//!
//! This module owns the three `expose_secret()` call sites that read the freshly generated
//! plaintext key. `GeneratedApiKey::raw_key` is a `SecretString`, so these are the only places
//! the plaintext becomes a `String`, and each one feeds it straight into the once-only
//! `ApiKeySecretResponse` envelope.
//!
//! Constructed and owned by [`crate::application::AdminService`], which re-exposes every
//! method below under its original name and signature.

use secrecy::ExposeSecret;
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandMutation, AdminCommandRunner, RequestContext,
        admin::shared::{
            CONSUMER_KEYS_CURSOR, PageRequest, SYSTEM_KEYS_CURSOR, admin_command_spec,
            command_hasher, paginate, require_active_row, sanitized_key_response, success_audit,
            validate_key_request,
        },
    },
    domain::{
        ApiKeyRecord, ApiKeyRotateRequest, ApiKeySecretResponse, ConsumerKeyCreateRequest,
        ListCursor, ListResponse, SystemKeyCreateRequest,
    },
    error::AppError,
    infra::repositories::{AdminRepository, KeyMaterial, PgAdminRepository},
    security::Actor,
};

pub(crate) struct ApiKeyAdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> ApiKeyAdminService<'a> {
    pub(crate) fn new(state: &'a AppState, repo: PgAdminRepository) -> Self {
        Self { state, repo }
    }

    pub(crate) async fn create_system_key(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
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
                        secret: Some(generated.raw_key.expose_secret().to_string()),
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

    pub(crate) async fn list_system_keys(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ApiKeyRecord>, AppError> {
        self.state.authz.require(actor, "moira:system-keys:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_system_keys(page.decode(SYSTEM_KEYS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, SYSTEM_KEYS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn get_system_key(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApiKeyRecord, AppError> {
        self.state.authz.require(actor, "moira:system-keys:read")?;
        self.repo.get_system_key(id).await
    }

    pub(crate) async fn create_consumer_key(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
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
                        secret: Some(generated.raw_key.expose_secret().to_string()),
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

    pub(crate) async fn list_consumer_keys(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ApiKeyRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:consumer-keys:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_consumer_keys(page.decode(CONSUMER_KEYS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, CONSUMER_KEYS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub(crate) async fn get_consumer_key(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<ApiKeyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:consumer-keys:read")?;
        self.repo.get_consumer_key(id).await
    }

    pub(crate) async fn rotate_key(
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
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
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
                        secret: Some(generated.raw_key.expose_secret().to_string()),
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

    pub(crate) async fn revoke_key(
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
        let record = self
            .repo
            .revoke_key(
                table,
                id,
                success_audit(
                    actor,
                    ctx,
                    "api_key.revoke",
                    table,
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await?;
        Ok(record)
    }

    pub(crate) async fn delete_key(
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
        self.repo
            .soft_delete_key(
                table,
                id,
                success_audit(
                    actor,
                    ctx,
                    "api_key.delete",
                    table,
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await?;
        Ok(())
    }
}
