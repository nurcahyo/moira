use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        ApiKeyRecord, ApplicationCreateRequest, ApplicationPatchRequest, ApplicationRecord,
        AuditLogInsert, AuditLogRecord, CredentialCreateRequest, CredentialPatchRequest,
        CredentialRecord, IdempotencyRecord, ProviderCreateRequest, ProviderModelCreateRequest,
        ProviderModelPatchRequest, ProviderModelRecord, ProviderPatchRequest, ProviderRecord,
        SystemKeyCreateRequest, TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest,
        TrustedJwtIssuerRecord,
    },
    error::AppError,
    infra::pg_rows::{
        api_key_record_from_row, application_record_from_row, audit_log_record_from_row,
        audit_result_to_db, credential_record_from_row, credential_type_to_db,
        provider_model_record_from_row, provider_record_from_row, provider_type_to_db,
        scope_type_to_db,
    },
    security::EncryptedSecret,
};

#[derive(Debug, Clone)]
pub struct PgAdminRepository {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct StoredCredentialSecret {
    pub record: CredentialRecord,
    pub encrypted: EncryptedSecret,
}

#[async_trait]
pub trait AdminRepository {
    async fn create_application(
        &self,
        id: Uuid,
        request: &ApplicationCreateRequest,
    ) -> Result<ApplicationRecord, AppError>;
    async fn list_applications(&self, limit: i64) -> Result<Vec<ApplicationRecord>, AppError>;
    async fn get_application(&self, id: Uuid) -> Result<ApplicationRecord, AppError>;
    async fn patch_application(
        &self,
        id: Uuid,
        request: &ApplicationPatchRequest,
    ) -> Result<ApplicationRecord, AppError>;
    async fn set_application_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<ApplicationRecord, AppError>;
    async fn soft_delete_application(&self, id: Uuid) -> Result<(), AppError>;

    async fn create_provider(
        &self,
        id: Uuid,
        request: &ProviderCreateRequest,
        normalized_base_url: Option<String>,
    ) -> Result<ProviderRecord, AppError>;
    async fn list_providers(&self, limit: i64) -> Result<Vec<ProviderRecord>, AppError>;
    async fn get_provider(&self, id: Uuid) -> Result<ProviderRecord, AppError>;
    async fn patch_provider(
        &self,
        id: Uuid,
        request: &ProviderPatchRequest,
        normalized_base_url: Option<Option<String>>,
    ) -> Result<ProviderRecord, AppError>;
    async fn set_provider_status(&self, id: Uuid, status: &str)
    -> Result<ProviderRecord, AppError>;
    async fn soft_delete_provider(&self, id: Uuid) -> Result<(), AppError>;

    async fn create_provider_model(
        &self,
        id: Uuid,
        provider_id: Uuid,
        request: &ProviderModelCreateRequest,
    ) -> Result<ProviderModelRecord, AppError>;
    async fn list_provider_models(
        &self,
        provider_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ProviderModelRecord>, AppError>;
    async fn patch_provider_model(
        &self,
        id: Uuid,
        request: &ProviderModelPatchRequest,
    ) -> Result<ProviderModelRecord, AppError>;
    async fn get_provider_model(&self, id: Uuid) -> Result<ProviderModelRecord, AppError>;
    async fn set_provider_model_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<ProviderModelRecord, AppError>;
    async fn soft_delete_provider_model(&self, id: Uuid) -> Result<(), AppError>;

    async fn create_credential(
        &self,
        id: Uuid,
        request: &CredentialCreateRequest,
        encrypted: &EncryptedSecret,
        fingerprint: &str,
        masked: &str,
    ) -> Result<CredentialRecord, AppError>;
    async fn list_credentials(&self, limit: i64) -> Result<Vec<CredentialRecord>, AppError>;
    async fn list_user_credentials(
        &self,
        external_user_id: &str,
        limit: i64,
    ) -> Result<Vec<CredentialRecord>, AppError>;
    async fn get_credential(&self, id: Uuid) -> Result<CredentialRecord, AppError>;
    async fn load_credential_secret(&self, id: Uuid) -> Result<StoredCredentialSecret, AppError>;
    async fn patch_credential(
        &self,
        id: Uuid,
        request: &CredentialPatchRequest,
    ) -> Result<CredentialRecord, AppError>;
    async fn rotate_credential(
        &self,
        id: Uuid,
        encrypted: &EncryptedSecret,
        fingerprint: &str,
        masked: &str,
    ) -> Result<CredentialRecord, AppError>;
    async fn set_credential_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<CredentialRecord, AppError>;
    async fn mark_credential_validated(&self, id: Uuid) -> Result<CredentialRecord, AppError>;
    async fn soft_delete_credential(&self, id: Uuid) -> Result<(), AppError>;
    async fn soft_delete_user_credential(
        &self,
        external_user_id: &str,
        id: Uuid,
    ) -> Result<(), AppError>;

    async fn create_system_key(
        &self,
        id: Uuid,
        request: &SystemKeyCreateRequest,
        key_prefix: &str,
        key_hash: &str,
        fingerprint: &str,
        pepper_version: &str,
    ) -> Result<ApiKeyRecord, AppError>;
    async fn create_consumer_key(
        &self,
        id: Uuid,
        application_id: Uuid,
        display_name: &str,
        scopes: &[String],
        expires_at: Option<DateTime<Utc>>,
        material: KeyMaterial<'_>,
    ) -> Result<ApiKeyRecord, AppError>;
    async fn list_system_keys(&self, limit: i64) -> Result<Vec<ApiKeyRecord>, AppError>;
    async fn list_consumer_keys(&self, limit: i64) -> Result<Vec<ApiKeyRecord>, AppError>;
    async fn get_system_key(&self, id: Uuid) -> Result<ApiKeyRecord, AppError>;
    async fn get_consumer_key(&self, id: Uuid) -> Result<ApiKeyRecord, AppError>;
    async fn rotate_key(
        &self,
        table: &str,
        id: Uuid,
        key_prefix: &str,
        key_hash: &str,
        fingerprint: &str,
        pepper_version: &str,
    ) -> Result<ApiKeyRecord, AppError>;
    async fn revoke_key(&self, table: &str, id: Uuid) -> Result<ApiKeyRecord, AppError>;
    async fn soft_delete_key(&self, table: &str, id: Uuid) -> Result<(), AppError>;

    async fn create_trusted_jwt_issuer(
        &self,
        id: Uuid,
        request: &TrustedJwtIssuerCreateRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn list_trusted_jwt_issuers(
        &self,
        limit: i64,
    ) -> Result<Vec<TrustedJwtIssuerRecord>, AppError>;
    async fn get_trusted_jwt_issuer(&self, id: Uuid) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn patch_trusted_jwt_issuer(
        &self,
        id: Uuid,
        request: &TrustedJwtIssuerPatchRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn set_trusted_jwt_issuer_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn touch_trusted_jwt_issuer(&self, id: Uuid) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn soft_delete_trusted_jwt_issuer(&self, id: Uuid) -> Result<(), AppError>;

    async fn insert_audit(&self, insert: AuditLogInsert) -> Result<(), AppError>;
    async fn list_audit_logs(&self, limit: i64) -> Result<Vec<AuditLogRecord>, AppError>;
    async fn get_audit_log(&self, id: Uuid) -> Result<AuditLogRecord, AppError>;
    async fn get_idempotency_record(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
    ) -> Result<Option<IdempotencyRecord>, AppError>;
    async fn put_idempotency_record(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyRecord, AppError>;
}

#[derive(Debug, Clone, Copy)]
pub struct KeyMaterial<'a> {
    pub key_prefix: &'a str,
    pub key_hash: &'a str,
    pub fingerprint: &'a str,
    pub pepper_version: &'a str,
}

impl PgAdminRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AdminRepository for PgAdminRepository {
    async fn create_application(
        &self,
        id: Uuid,
        request: &ApplicationCreateRequest,
    ) -> Result<ApplicationRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into applications
                (id, external_application_id, application_slug, display_name, metadata)
            values ($1, $2, $3, $4, $5)
            returning id, external_application_id, application_slug, display_name, status,
                      metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.external_application_id)
        .bind(&request.application_slug)
        .bind(&request.display_name)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        application_record_from_row(&row)
    }

    async fn list_applications(&self, limit: i64) -> Result<Vec<ApplicationRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, external_application_id, application_slug, display_name, status,
                   metadata, created_at, updated_at, deleted_at, version
            from applications
            where deleted_at is null
            order by created_at desc
            limit $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(application_record_from_row).collect()
    }

    async fn get_application(&self, id: Uuid) -> Result<ApplicationRecord, AppError> {
        let row = sqlx::query(
            r#"
            select id, external_application_id, application_slug, display_name, status,
                   metadata, created_at, updated_at, deleted_at, version
            from applications
            where id = $1 and deleted_at is null
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("application {id}")))?;
        application_record_from_row(&row)
    }

    async fn patch_application(
        &self,
        id: Uuid,
        request: &ApplicationPatchRequest,
    ) -> Result<ApplicationRecord, AppError> {
        let row = sqlx::query(
            r#"
            update applications
            set external_application_id = coalesce($2, external_application_id),
                application_slug = coalesce($3, application_slug),
                display_name = coalesce($4, display_name),
                updated_at = now()
                , metadata = coalesce($5, metadata)
            where id = $1 and deleted_at is null
            returning id, external_application_id, application_slug, display_name, status,
                      metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.external_application_id)
        .bind(&request.application_slug)
        .bind(&request.display_name)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("application {id}")))?;
        application_record_from_row(&row)
    }

    async fn set_application_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<ApplicationRecord, AppError> {
        let row = sqlx::query(
            r#"
            update applications
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, external_application_id, application_slug, display_name, status,
                      metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("application {id}")))?;
        application_record_from_row(&row)
    }

    async fn soft_delete_application(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update applications set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("application {id}"))
    }

    async fn create_provider(
        &self,
        id: Uuid,
        request: &ProviderCreateRequest,
        normalized_base_url: Option<String>,
    ) -> Result<ProviderRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into providers
                (id, provider_type, display_name, base_url, metadata)
            values ($1, $2, $3, $4, $5)
            returning id, provider_type, display_name, base_url, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(provider_type_to_db(&request.provider_type))
        .bind(&request.display_name)
        .bind(&normalized_base_url)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        provider_record_from_row(&row)
    }

    async fn list_providers(&self, limit: i64) -> Result<Vec<ProviderRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, provider_type, display_name, base_url, status, metadata,
                   created_at, updated_at, deleted_at, version
            from providers
            where deleted_at is null
            order by created_at desc
            limit $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(provider_record_from_row).collect()
    }

    async fn get_provider(&self, id: Uuid) -> Result<ProviderRecord, AppError> {
        let row = sqlx::query(
            r#"
            select id, provider_type, display_name, base_url, status, metadata,
                   created_at, updated_at, deleted_at, version
            from providers
            where id = $1 and deleted_at is null
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider {id}")))?;
        provider_record_from_row(&row)
    }

    async fn patch_provider(
        &self,
        id: Uuid,
        request: &ProviderPatchRequest,
        normalized_base_url: Option<Option<String>>,
    ) -> Result<ProviderRecord, AppError> {
        let update_base_url = normalized_base_url.is_some();
        let base_url = normalized_base_url.flatten();
        let row = sqlx::query(
            r#"
            update providers
            set display_name = coalesce($2, display_name),
                base_url = case when $3 then $4 else base_url end,
                metadata = coalesce($5, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_type, display_name, base_url, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(update_base_url)
        .bind(&base_url)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider {id}")))?;
        provider_record_from_row(&row)
    }

    async fn set_provider_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<ProviderRecord, AppError> {
        let row = sqlx::query(
            r#"
            update providers
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_type, display_name, base_url, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider {id}")))?;
        provider_record_from_row(&row)
    }

    async fn soft_delete_provider(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update providers set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("provider {id}"))
    }

    async fn create_provider_model(
        &self,
        id: Uuid,
        provider_id: Uuid,
        request: &ProviderModelCreateRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into provider_models
                (id, provider_id, model_key, display_name, capabilities)
            values ($1, $2, $3, $4, $5)
            returning id, provider_id, model_key, display_name, capabilities, status,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(provider_id)
        .bind(&request.model_key)
        .bind(&request.display_name)
        .bind(&request.capabilities)
        .fetch_one(&self.pool)
        .await?;
        provider_model_record_from_row(&row)
    }

    async fn list_provider_models(
        &self,
        provider_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ProviderModelRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, provider_id, model_key, display_name, capabilities, status,
                   created_at, updated_at, deleted_at, version
            from provider_models
            where provider_id = $1 and deleted_at is null
            order by created_at desc
            limit $2
            "#,
        )
        .bind(provider_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(provider_model_record_from_row).collect()
    }

    async fn patch_provider_model(
        &self,
        id: Uuid,
        request: &ProviderModelPatchRequest,
    ) -> Result<ProviderModelRecord, AppError> {
        let row = sqlx::query(
            r#"
            update provider_models
            set display_name = coalesce($2, display_name),
                capabilities = coalesce($3, capabilities),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_id, model_key, display_name, capabilities, status,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.capabilities)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider model {id}")))?;
        provider_model_record_from_row(&row)
    }

    async fn get_provider_model(&self, id: Uuid) -> Result<ProviderModelRecord, AppError> {
        let row = sqlx::query(
            r#"
            select id, provider_id, model_key, display_name, capabilities, status,
                   created_at, updated_at, deleted_at, version
            from provider_models
            where id = $1 and deleted_at is null
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider model {id}")))?;
        provider_model_record_from_row(&row)
    }

    async fn set_provider_model_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<ProviderModelRecord, AppError> {
        let row = sqlx::query(
            r#"
            update provider_models
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_id, model_key, display_name, capabilities, status,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider model {id}")))?;
        provider_model_record_from_row(&row)
    }

    async fn soft_delete_provider_model(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update provider_models set status = 'disabled', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("provider model {id}"))
    }

    async fn create_credential(
        &self,
        id: Uuid,
        request: &CredentialCreateRequest,
        encrypted: &EncryptedSecret,
        fingerprint: &str,
        masked: &str,
    ) -> Result<CredentialRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into provider_credentials
                (id, provider_id, credential_type, scope_type, external_tenant_id,
                 application_id, external_user_id, encrypted_payload, encryption_algorithm,
                 encryption_version, encrypted_data_key, nonce, secret_fingerprint,
                 masked_secret, priority, expires_at, metadata, display_name)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
            returning id, provider_id, credential_type, scope_type, external_tenant_id,
                      application_id, external_user_id, encryption_algorithm,
                      encryption_version, secret_fingerprint, masked_secret, status,
                      priority, expires_at, last_validated_at, last_used_at, metadata,
                      created_at, updated_at, deleted_at, version, display_name
            "#,
        )
        .bind(id)
        .bind(request.provider_id)
        .bind(credential_type_to_db(&request.credential_type))
        .bind(scope_type_to_db(&request.scope.scope_type()))
        .bind(request.scope.external_tenant_id())
        .bind(request.scope.application_id())
        .bind(request.scope.external_user_id())
        .bind(&encrypted.ciphertext)
        .bind(&encrypted.algorithm)
        .bind(encrypted.version)
        .bind(&encrypted.encrypted_data_key)
        .bind(&encrypted.nonce)
        .bind(fingerprint)
        .bind(masked)
        .bind(request.priority)
        .bind(request.expires_at)
        .bind(&request.metadata)
        .bind(&request.display_name)
        .fetch_one(&self.pool)
        .await?;
        credential_record_from_row(&row)
    }

    async fn list_credentials(&self, limit: i64) -> Result<Vec<CredentialRecord>, AppError> {
        let sql = credential_select_sql("order by created_at desc limit $1");
        let rows = sqlx::query(&sql).bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(credential_record_from_row).collect()
    }

    async fn list_user_credentials(
        &self,
        external_user_id: &str,
        limit: i64,
    ) -> Result<Vec<CredentialRecord>, AppError> {
        let sql =
            credential_select_sql("and external_user_id = $1 order by created_at desc limit $2");
        let rows = sqlx::query(&sql)
            .bind(external_user_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(credential_record_from_row).collect()
    }

    async fn get_credential(&self, id: Uuid) -> Result<CredentialRecord, AppError> {
        let sql = credential_select_sql("and id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        credential_record_from_row(&row)
    }

    async fn load_credential_secret(&self, id: Uuid) -> Result<StoredCredentialSecret, AppError> {
        let row = sqlx::query(
            r#"
            select id, provider_id, credential_type, scope_type, external_tenant_id,
                   application_id, external_user_id, encryption_algorithm, encryption_version,
                   secret_fingerprint, masked_secret, status, priority, expires_at,
                   last_validated_at, last_used_at, metadata, created_at, updated_at,
                   deleted_at, encrypted_payload, encrypted_data_key, nonce, version,
                   display_name
            from provider_credentials
            where id = $1 and deleted_at is null
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        let record = credential_record_from_row(&row)?;
        Ok(StoredCredentialSecret {
            encrypted: EncryptedSecret {
                algorithm: record.encryption_algorithm.clone(),
                version: record.encryption_version,
                key_id: String::new(),
                encrypted_data_key: row.try_get("encrypted_data_key")?,
                nonce: row.try_get("nonce")?,
                ciphertext: row.try_get("encrypted_payload")?,
            },
            record,
        })
    }

    async fn patch_credential(
        &self,
        id: Uuid,
        request: &CredentialPatchRequest,
    ) -> Result<CredentialRecord, AppError> {
        let row = sqlx::query(
            r#"
            update provider_credentials
            set display_name = coalesce($2, display_name),
                priority = coalesce($3, priority),
                expires_at = coalesce($4, expires_at),
                metadata = coalesce($5, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_id, credential_type, scope_type, external_tenant_id,
                      application_id, external_user_id, encryption_algorithm,
                      encryption_version, secret_fingerprint, masked_secret, status,
                      priority, expires_at, last_validated_at, last_used_at, metadata,
                      created_at, updated_at, deleted_at, version, display_name
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(request.priority)
        .bind(request.expires_at)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        credential_record_from_row(&row)
    }

    async fn rotate_credential(
        &self,
        id: Uuid,
        encrypted: &EncryptedSecret,
        fingerprint: &str,
        masked: &str,
    ) -> Result<CredentialRecord, AppError> {
        let row = sqlx::query(
            r#"
            update provider_credentials
            set encrypted_payload = $2,
                encryption_algorithm = $3,
                encryption_version = $4,
                encrypted_data_key = $5,
                nonce = $6,
                secret_fingerprint = $7,
                masked_secret = $8,
                status = 'active',
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_id, credential_type, scope_type, external_tenant_id,
                      application_id, external_user_id, encryption_algorithm,
                      encryption_version, secret_fingerprint, masked_secret, status,
                      priority, expires_at, last_validated_at, last_used_at, metadata,
                      created_at, updated_at, deleted_at, version, display_name
            "#,
        )
        .bind(id)
        .bind(&encrypted.ciphertext)
        .bind(&encrypted.algorithm)
        .bind(encrypted.version)
        .bind(&encrypted.encrypted_data_key)
        .bind(&encrypted.nonce)
        .bind(fingerprint)
        .bind(masked)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        credential_record_from_row(&row)
    }

    async fn set_credential_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<CredentialRecord, AppError> {
        let row = sqlx::query(
            r#"
            update provider_credentials
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_id, credential_type, scope_type, external_tenant_id,
                      application_id, external_user_id, encryption_algorithm,
                      encryption_version, secret_fingerprint, masked_secret, status,
                      priority, expires_at, last_validated_at, last_used_at, metadata,
                      created_at, updated_at, deleted_at, version, display_name
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        credential_record_from_row(&row)
    }

    async fn mark_credential_validated(&self, id: Uuid) -> Result<CredentialRecord, AppError> {
        let row = sqlx::query(
            r#"
            update provider_credentials
            set last_validated_at = now(), status = 'active', updated_at = now()
            where id = $1 and deleted_at is null
            returning id, provider_id, credential_type, scope_type, external_tenant_id,
                      application_id, external_user_id, encryption_algorithm,
                      encryption_version, secret_fingerprint, masked_secret, status,
                      priority, expires_at, last_validated_at, last_used_at, metadata,
                      created_at, updated_at, deleted_at, version, display_name
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        credential_record_from_row(&row)
    }

    async fn soft_delete_credential(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update provider_credentials set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("provider credential {id}"))
    }

    async fn soft_delete_user_credential(
        &self,
        external_user_id: &str,
        id: Uuid,
    ) -> Result<(), AppError> {
        let result = sqlx::query(
            "update provider_credentials set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and external_user_id = $2 and deleted_at is null",
        )
        .bind(id)
        .bind(external_user_id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("provider credential {id}"))
    }

    async fn create_system_key(
        &self,
        id: Uuid,
        request: &SystemKeyCreateRequest,
        key_prefix: &str,
        key_hash: &str,
        fingerprint: &str,
        pepper_version: &str,
    ) -> Result<ApiKeyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into system_api_keys
                (id, display_name, key_prefix, key_hash, fingerprint, pepper_version, scopes, expires_at)
            values ($1, $2, $3, $4, $5, $6, $7, $8)
            returning id, null::uuid as application_id, display_name, key_prefix, fingerprint,
                      pepper_version, scopes, status, expires_at, last_used_at, created_at,
                      updated_at, revoked_at
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(key_prefix)
        .bind(key_hash)
        .bind(fingerprint)
        .bind(pepper_version)
        .bind(&request.scopes)
        .bind(request.expires_at)
        .fetch_one(&self.pool)
        .await?;
        api_key_record_from_row(&row)
    }

    async fn create_consumer_key(
        &self,
        id: Uuid,
        application_id: Uuid,
        display_name: &str,
        scopes: &[String],
        expires_at: Option<DateTime<Utc>>,
        material: KeyMaterial<'_>,
    ) -> Result<ApiKeyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into consumer_api_keys
                (id, application_id, display_name, key_prefix, key_hash, fingerprint, pepper_version, scopes, expires_at)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            returning id, application_id, display_name, key_prefix, fingerprint,
                      pepper_version, scopes, status, expires_at, last_used_at, created_at,
                      updated_at, revoked_at
            "#,
        )
        .bind(id)
        .bind(application_id)
        .bind(display_name)
        .bind(material.key_prefix)
        .bind(material.key_hash)
        .bind(material.fingerprint)
        .bind(material.pepper_version)
        .bind(scopes)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await?;
        api_key_record_from_row(&row)
    }

    async fn list_system_keys(&self, limit: i64) -> Result<Vec<ApiKeyRecord>, AppError> {
        list_keys(&self.pool, "system_api_keys", false, limit).await
    }

    async fn list_consumer_keys(&self, limit: i64) -> Result<Vec<ApiKeyRecord>, AppError> {
        list_keys(&self.pool, "consumer_api_keys", true, limit).await
    }

    async fn get_system_key(&self, id: Uuid) -> Result<ApiKeyRecord, AppError> {
        get_key(&self.pool, "system_api_keys", false, id).await
    }

    async fn get_consumer_key(&self, id: Uuid) -> Result<ApiKeyRecord, AppError> {
        get_key(&self.pool, "consumer_api_keys", true, id).await
    }

    async fn rotate_key(
        &self,
        table: &str,
        id: Uuid,
        key_prefix: &str,
        key_hash: &str,
        fingerprint: &str,
        pepper_version: &str,
    ) -> Result<ApiKeyRecord, AppError> {
        let (sql, app_col) = match table {
            "system_api_keys" => (
                r#"
                update system_api_keys
                set key_prefix = $2, key_hash = $3, fingerprint = $4,
                    pepper_version = $5, status = 'active', revoked_at = null, updated_at = now()
                where id = $1 and deleted_at is null
                returning id, null::uuid as application_id, display_name, key_prefix, fingerprint,
                          pepper_version, scopes, status, expires_at, last_used_at, created_at,
                          updated_at, revoked_at
                "#,
                false,
            ),
            "consumer_api_keys" => (
                r#"
                update consumer_api_keys
                set key_prefix = $2, key_hash = $3, fingerprint = $4,
                    pepper_version = $5, status = 'active', revoked_at = null, updated_at = now()
                where id = $1 and deleted_at is null
                returning id, application_id, display_name, key_prefix, fingerprint,
                          pepper_version, scopes, status, expires_at, last_used_at, created_at,
                          updated_at, revoked_at
                "#,
                true,
            ),
            _ => return Err(AppError::Internal("unsupported api key table".to_string())),
        };
        let row = sqlx::query(sql)
            .bind(id)
            .bind(key_prefix)
            .bind(key_hash)
            .bind(fingerprint)
            .bind(pepper_version)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
        let _ = app_col;
        api_key_record_from_row(&row)
    }

    async fn revoke_key(&self, table: &str, id: Uuid) -> Result<ApiKeyRecord, AppError> {
        let sql = match table {
            "system_api_keys" => {
                r#"
                update system_api_keys
                set status = 'revoked', revoked_at = now(), updated_at = now()
                where id = $1 and deleted_at is null
                returning id, null::uuid as application_id, display_name, key_prefix, fingerprint,
                          pepper_version, scopes, status, expires_at, last_used_at, created_at,
                          updated_at, revoked_at
                "#
            }
            "consumer_api_keys" => {
                r#"
                update consumer_api_keys
                set status = 'revoked', revoked_at = now(), updated_at = now()
                where id = $1 and deleted_at is null
                returning id, application_id, display_name, key_prefix, fingerprint,
                          pepper_version, scopes, status, expires_at, last_used_at, created_at,
                          updated_at, revoked_at
                "#
            }
            _ => return Err(AppError::Internal("unsupported api key table".to_string())),
        };
        let row = sqlx::query(sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
        api_key_record_from_row(&row)
    }

    async fn soft_delete_key(&self, table: &str, id: Uuid) -> Result<(), AppError> {
        let sql = match table {
            "system_api_keys" => {
                "update system_api_keys set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null"
            }
            "consumer_api_keys" => {
                "update consumer_api_keys set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null"
            }
            _ => return Err(AppError::Internal("unsupported api key table".to_string())),
        };
        let result = sqlx::query(sql).bind(id).execute(&self.pool).await?;
        ensure_affected(result.rows_affected(), format!("api key {id}"))
    }

    async fn create_trusted_jwt_issuer(
        &self,
        id: Uuid,
        request: &TrustedJwtIssuerCreateRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into trusted_jwt_issuers
                (id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                 subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                 roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                 clock_skew_seconds, allow_delegation)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            returning id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                      subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                      roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                      clock_skew_seconds, allow_delegation, status, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.issuer)
        .bind(&request.jwks_url)
        .bind(&request.expected_audiences)
        .bind(&request.allowed_algorithms)
        .bind(&request.subject_claim)
        .bind(&request.user_id_claim)
        .bind(&request.tenant_id_claim)
        .bind(&request.application_id_claim)
        .bind(&request.roles_claim)
        .bind(&request.scopes_claim)
        .bind(&request.delegated_user_claim)
        .bind(&request.delegated_tenant_claim)
        .bind(request.clock_skew_seconds)
        .bind(request.allow_delegation)
        .fetch_one(&self.pool)
        .await?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    async fn list_trusted_jwt_issuers(
        &self,
        limit: i64,
    ) -> Result<Vec<TrustedJwtIssuerRecord>, AppError> {
        let sql =
            trusted_issuer_select_sql("where deleted_at is null order by created_at desc limit $1");
        let rows = sqlx::query(&sql).bind(limit).fetch_all(&self.pool).await?;
        rows.iter()
            .map(crate::infra::pg_rows::trusted_jwt_issuer_record_from_row)
            .collect()
    }

    async fn get_trusted_jwt_issuer(&self, id: Uuid) -> Result<TrustedJwtIssuerRecord, AppError> {
        let sql = trusted_issuer_select_sql("where id = $1 and deleted_at is null");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("trusted JWT issuer {id}")))?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    async fn patch_trusted_jwt_issuer(
        &self,
        id: Uuid,
        request: &TrustedJwtIssuerPatchRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        let row = sqlx::query(
            r#"
            update trusted_jwt_issuers
            set jwks_url = coalesce($2, jwks_url),
                expected_audiences = coalesce($3, expected_audiences),
                allowed_algorithms = coalesce($4, allowed_algorithms),
                subject_claim = coalesce($5, subject_claim),
                user_id_claim = coalesce($6, user_id_claim),
                tenant_id_claim = coalesce($7, tenant_id_claim),
                application_id_claim = coalesce($8, application_id_claim),
                roles_claim = coalesce($9, roles_claim),
                scopes_claim = coalesce($10, scopes_claim),
                delegated_user_claim = coalesce($11, delegated_user_claim),
                delegated_tenant_claim = coalesce($12, delegated_tenant_claim),
                clock_skew_seconds = coalesce($13, clock_skew_seconds),
                allow_delegation = coalesce($14, allow_delegation),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                      subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                      roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                      clock_skew_seconds, allow_delegation, status, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.jwks_url)
        .bind(&request.expected_audiences)
        .bind(&request.allowed_algorithms)
        .bind(&request.subject_claim)
        .bind(&request.user_id_claim)
        .bind(&request.tenant_id_claim)
        .bind(&request.application_id_claim)
        .bind(&request.roles_claim)
        .bind(&request.scopes_claim)
        .bind(&request.delegated_user_claim)
        .bind(&request.delegated_tenant_claim)
        .bind(request.clock_skew_seconds)
        .bind(request.allow_delegation)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("trusted JWT issuer {id}")))?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    async fn set_trusted_jwt_issuer_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        let row = sqlx::query(
            r#"
            update trusted_jwt_issuers
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                      subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                      roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                      clock_skew_seconds, allow_delegation, status, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("trusted JWT issuer {id}")))?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    async fn touch_trusted_jwt_issuer(&self, id: Uuid) -> Result<TrustedJwtIssuerRecord, AppError> {
        let row = sqlx::query(
            r#"
            update trusted_jwt_issuers
            set updated_at = now()
            where id = $1 and deleted_at is null
            returning id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                      subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                      roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                      clock_skew_seconds, allow_delegation, status, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("trusted JWT issuer {id}")))?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    async fn soft_delete_trusted_jwt_issuer(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update trusted_jwt_issuers set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("trusted JWT issuer {id}"))
    }

    async fn insert_audit(&self, insert: AuditLogInsert) -> Result<(), AppError> {
        let source_ip = insert.source_ip.map(|ip| ip.to_string());
        sqlx::query(
            r#"
            insert into audit_logs
                (request_id, actor_type, actor_subject, delegated_subject,
                 external_user_id, external_tenant_id, application_id, resource_type,
                 resource_id, action, result, source_ip, user_agent, metadata)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12::inet, $13, $14)
            "#,
        )
        .bind(&insert.request_id)
        .bind(&insert.actor_type)
        .bind(&insert.actor_subject)
        .bind(&insert.delegated_subject)
        .bind(&insert.external_user_id)
        .bind(&insert.external_tenant_id)
        .bind(insert.application_id)
        .bind(&insert.resource_type)
        .bind(&insert.resource_id)
        .bind(&insert.action)
        .bind(audit_result_to_db(&insert.result))
        .bind(&source_ip)
        .bind(&insert.user_agent)
        .bind(&insert.metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_audit_logs(&self, limit: i64) -> Result<Vec<AuditLogRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, occurred_at, request_id, actor_type, actor_subject, delegated_subject,
                   external_user_id, external_tenant_id, application_id, resource_type,
                   resource_id, action, result, source_ip::text as source_ip, user_agent, metadata
            from audit_logs
            order by occurred_at desc
            limit $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(audit_log_record_from_row).collect()
    }

    async fn get_audit_log(&self, id: Uuid) -> Result<AuditLogRecord, AppError> {
        let row = sqlx::query(
            r#"
            select id, occurred_at, request_id, actor_type, actor_subject, delegated_subject,
                   external_user_id, external_tenant_id, application_id, resource_type,
                   resource_id, action, result, source_ip::text as source_ip, user_agent, metadata
            from audit_logs
            where id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("audit event {id}")))?;
        audit_log_record_from_row(&row)
    }

    async fn get_idempotency_record(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
    ) -> Result<Option<IdempotencyRecord>, AppError> {
        let row = sqlx::query(
            r#"
            select id, idempotency_key_hash, actor_fingerprint, operation, request_hash,
                   response_status, response_body, resource_id, expires_at
            from idempotency_records
            where idempotency_key_hash = $1
              and actor_fingerprint = $2
              and operation = $3
              and expires_at > now()
            "#,
        )
        .bind(key_hash)
        .bind(actor_fingerprint)
        .bind(operation)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| {
            Ok(IdempotencyRecord {
                id: row.try_get("id")?,
                idempotency_key_hash: row.try_get("idempotency_key_hash")?,
                actor_fingerprint: row.try_get("actor_fingerprint")?,
                operation: row.try_get("operation")?,
                request_hash: row.try_get("request_hash")?,
                response_status: row.try_get("response_status")?,
                response_body: row.try_get("response_body")?,
                resource_id: row.try_get("resource_id")?,
                expires_at: row.try_get("expires_at")?,
            })
        })
        .transpose()
    }

    async fn put_idempotency_record(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into idempotency_records
                (id, idempotency_key_hash, actor_fingerprint, operation, request_hash,
                 response_status, response_body, resource_id, expires_at)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            on conflict (idempotency_key_hash, actor_fingerprint, operation)
            do update set response_status = excluded.response_status,
                          response_body = excluded.response_body,
                          resource_id = excluded.resource_id
            returning id, idempotency_key_hash, actor_fingerprint, operation, request_hash,
                      response_status, response_body, resource_id, expires_at
            "#,
        )
        .bind(record.id)
        .bind(&record.idempotency_key_hash)
        .bind(&record.actor_fingerprint)
        .bind(&record.operation)
        .bind(&record.request_hash)
        .bind(record.response_status)
        .bind(&record.response_body)
        .bind(&record.resource_id)
        .bind(record.expires_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(IdempotencyRecord {
            id: row.try_get("id")?,
            idempotency_key_hash: row.try_get("idempotency_key_hash")?,
            actor_fingerprint: row.try_get("actor_fingerprint")?,
            operation: row.try_get("operation")?,
            request_hash: row.try_get("request_hash")?,
            response_status: row.try_get("response_status")?,
            response_body: row.try_get("response_body")?,
            resource_id: row.try_get("resource_id")?,
            expires_at: row.try_get("expires_at")?,
        })
    }
}

fn credential_select_sql(suffix: &str) -> String {
    format!(
        r#"
        select id, provider_id, credential_type, scope_type, external_tenant_id,
               application_id, external_user_id, encryption_algorithm,
               encryption_version, secret_fingerprint, masked_secret, status,
               priority, expires_at, last_validated_at, last_used_at, metadata,
               created_at, updated_at, deleted_at, version, display_name
        from provider_credentials
        where deleted_at is null
        {suffix}
        "#
    )
}

fn trusted_issuer_select_sql(suffix: &str) -> String {
    format!(
        r#"
        select id, issuer, jwks_url, expected_audiences, allowed_algorithms,
               subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
               roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
               clock_skew_seconds, allow_delegation, status, created_at, updated_at, deleted_at,
               version
        from trusted_jwt_issuers
        {suffix}
        "#
    )
}

async fn list_keys(
    pool: &PgPool,
    table: &str,
    has_application: bool,
    limit: i64,
) -> Result<Vec<ApiKeyRecord>, AppError> {
    let sql = match (table, has_application) {
        ("system_api_keys", false) => {
            r#"
            select id, null::uuid as application_id, display_name, key_prefix, fingerprint,
                   pepper_version, scopes, status, expires_at, last_used_at, created_at,
                   updated_at, revoked_at
            from system_api_keys
            where deleted_at is null
            order by created_at desc
            limit $1
            "#
        }
        ("consumer_api_keys", true) => {
            r#"
            select id, application_id, display_name, key_prefix, fingerprint,
                   pepper_version, scopes, status, expires_at, last_used_at, created_at,
                   updated_at, revoked_at
            from consumer_api_keys
            where deleted_at is null
            order by created_at desc
            limit $1
            "#
        }
        _ => return Err(AppError::Internal("unsupported api key table".to_string())),
    };
    let rows = sqlx::query(sql).bind(limit).fetch_all(pool).await?;
    rows.iter().map(api_key_record_from_row).collect()
}

async fn get_key(
    pool: &PgPool,
    table: &str,
    has_application: bool,
    id: Uuid,
) -> Result<ApiKeyRecord, AppError> {
    let sql = match (table, has_application) {
        ("system_api_keys", false) => {
            r#"
            select id, null::uuid as application_id, display_name, key_prefix, fingerprint,
                   pepper_version, scopes, status, expires_at, last_used_at, created_at,
                   updated_at, revoked_at
            from system_api_keys
            where id = $1 and deleted_at is null
            "#
        }
        ("consumer_api_keys", true) => {
            r#"
            select id, application_id, display_name, key_prefix, fingerprint,
                   pepper_version, scopes, status, expires_at, last_used_at, created_at,
                   updated_at, revoked_at
            from consumer_api_keys
            where id = $1 and deleted_at is null
            "#
        }
        _ => return Err(AppError::Internal("unsupported api key table".to_string())),
    };
    let row = sqlx::query(sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
    api_key_record_from_row(&row)
}

fn ensure_affected(rows_affected: u64, resource: String) -> Result<(), AppError> {
    if rows_affected == 0 {
        Err(AppError::NotFound(resource))
    } else {
        Ok(())
    }
}
