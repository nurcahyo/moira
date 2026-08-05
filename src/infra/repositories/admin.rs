use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::{
    domain::{
        ApiKeyRecord, ApplicationCreateRequest, ApplicationPatchRequest, ApplicationRecord,
        AuditLogInsert, AuditLogRecord, CredentialCreateRequest, CredentialPatchRequest,
        CredentialRecord, IdempotencyRecord, ListCursor, ProviderCreateRequest,
        ProviderModelCreateRequest, ProviderModelPatchRequest, ProviderModelRecord,
        ProviderPatchRequest, ProviderRecord, SystemKeyCreateRequest,
        TrustedJwtIssuerCreateRequest, TrustedJwtIssuerPatchRequest, TrustedJwtIssuerRecord,
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

use super::keyset::{KeysetTail, bind_cursor, over_fetch_limit};

#[derive(Debug, Clone)]
pub struct PgAdminRepository {
    pool: PgPool,
}

pub struct PgAdminCommandTransaction {
    transaction: Transaction<'static, Postgres>,
}

/// A claim on the idempotency ledger.
///
/// Every hash it carries is precomputed by the application layer; this repository never
/// learns how any of them is derived, and — since plan 03 finding F3 — it no longer
/// *compares* them either. It looks a claim up, sweeps expired rows, and either inserts or
/// hands the existing row back for the application layer to verify, which is what plan 03's
/// Detailed Implementation item 6 asks for ("prefer verifying in the application layer").
///
/// `legacy_key_hash` exists because the switch to keyed hashing (P1-1) changed the index
/// key as well as the compared digest, so a row written before the switch is unreachable by
/// the new key hash alone. The caller passes `None` to close that dual-read window, which
/// also removes the extra lookup it costs on every claim. `idempotency.accept_legacy_hashes`
/// is the setting that is *meant* to drive that choice, but no production construction site
/// wires it into `AdminCommandRunner` yet, so on this path the window is currently always
/// open regardless of configuration — tracked in `TODO.md` and issue #125.
///
/// `legacy_actor_fingerprint` exists for the analogous reason on the other index column:
/// peppering the actor fingerprint changed its spelling, so a claim must be able to address
/// a pre-deploy row under either one. It is **not** governed by the same switch — the caller
/// populates it unconditionally, because the two windows opened at different deploys and
/// close independently. Both legacy values are read-only.
#[derive(Debug, Clone)]
pub struct AdminIdempotencyClaim {
    pub record_id: Uuid,
    /// The key hash written for a fresh claim and tried first on lookup.
    pub key_hash: String,
    /// The pre-switch key hash, tried only when `key_hash` misses. Never written.
    pub legacy_key_hash: Option<String>,
    /// The peppered fingerprint written for a fresh claim and tried first on lookup.
    pub actor_fingerprint: String,
    /// The pre-pepper, unkeyed fingerprint. Read-only, never written.
    pub legacy_actor_fingerprint: Option<String>,
    pub operation: String,
    /// The body digest written for a fresh claim.
    pub request_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum AdminIdempotencyClaimOutcome {
    /// No row existed; this claim now owns the ledger entry.
    Acquired,
    /// A row already exists for this key. Whether it is a legitimate replay or a
    /// same-key-different-body conflict is the application layer's call, because deciding
    /// it requires recomputing a keyed digest.
    Existing(IdempotencyRecord),
}

#[derive(Debug, Clone)]
pub struct StoredCredentialSecret {
    pub record: CredentialRecord,
    pub encrypted: EncryptedSecret,
}

/// # Pagination contract for every `list_*` method below (plan 04, P1-4)
///
/// Each takes the already-decoded keyset `cursor` for *its own* sort key plus the caller's
/// page `limit`, and returns **up to `limit + 1`** rows in that query's existing sort
/// order. The extra row is deliberate — see [`over_fetch_limit`]. Trimming it off,
/// computing `has_more` and encoding `next_cursor` belong to the application layer
/// (`src/application/admin.rs`), which is what keeps this SQL simple and keeps one
/// convention across all nine lists.
///
/// A repository method therefore never returns `has_more` and never encodes a cursor; it
/// only knows how to seek and how to over-fetch by one.
///
/// # Every mutation carries its `audit` row, and the write commits it
///
/// A method that changes state takes an [`AuditLogInsert`] and writes it **on the same
/// connection, inside the same transaction** as the change itself. It is a required
/// parameter rather than a convention because the alternative had already gone wrong:
/// thirty-six admin mutations used to write the row afterwards through
/// `PgAdminRepository::insert_audit`, which acquires a *second* pooled connection. The two
/// statements then commit separately, and any failure between them — a `22001` from an
/// over-long `x-request-id`, a pool timeout, a dropped request future, a killed pod —
/// leaves the administrative change committed with no record of it.
///
/// Only that direction was ever reachable, and it is the serious one. The audit row is
/// written *after* the row lock and version check, so a `409` or `404` still writes
/// nothing.
///
/// Do not add a mutating method here without an `audit` parameter, and do not restore a
/// caller-side `insert_audit` on a success path. `audit_denied`
/// (`src/application/admin/shared.rs`) is the one deliberate exception: it records a
/// refusal, so there is no write for it to be atomic with, and it is swallowed on purpose.
/// Pinned end to end by `tests/admin_audit_atomicity.rs`.
#[async_trait]
pub trait AdminRepository {
    async fn create_application(
        &self,
        id: Uuid,
        request: &ApplicationCreateRequest,
    ) -> Result<ApplicationRecord, AppError>;
    async fn list_applications(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ApplicationRecord>, AppError>;
    async fn get_application(&self, id: Uuid) -> Result<ApplicationRecord, AppError>;
    /// `expected_version` is the caller's `If-Match`. Implementations must compare it against a
    /// row locked inside the same transaction as the write — never against a version read on a
    /// separate connection, which is the lost update this parameter exists to close. A mismatch
    /// is `409 resource_version_conflict`; an absent row stays `404`.
    async fn patch_application(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &ApplicationPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<ApplicationRecord, AppError>;
    async fn set_application_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<ApplicationRecord, AppError>;
    async fn soft_delete_application(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn create_provider(
        &self,
        id: Uuid,
        request: &ProviderCreateRequest,
        normalized_base_url: Option<String>,
    ) -> Result<ProviderRecord, AppError>;
    async fn list_providers(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ProviderRecord>, AppError>;
    async fn get_provider(&self, id: Uuid) -> Result<ProviderRecord, AppError>;
    async fn patch_provider(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &ProviderPatchRequest,
        normalized_base_url: Option<Option<String>>,
        audit: AuditLogInsert,
    ) -> Result<ProviderRecord, AppError>;
    async fn set_provider_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<ProviderRecord, AppError>;
    async fn soft_delete_provider(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn create_provider_model(
        &self,
        id: Uuid,
        provider_id: Uuid,
        request: &ProviderModelCreateRequest,
    ) -> Result<ProviderModelRecord, AppError>;
    async fn list_provider_models(
        &self,
        provider_id: Uuid,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ProviderModelRecord>, AppError>;
    async fn patch_provider_model(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &ProviderModelPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<ProviderModelRecord, AppError>;
    async fn get_provider_model(&self, id: Uuid) -> Result<ProviderModelRecord, AppError>;
    async fn set_provider_model_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<ProviderModelRecord, AppError>;
    async fn soft_delete_provider_model(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn create_credential(
        &self,
        id: Uuid,
        request: &CredentialCreateRequest,
        encrypted: &EncryptedSecret,
        fingerprint: &str,
        masked: &str,
    ) -> Result<CredentialRecord, AppError>;
    async fn list_credentials(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<CredentialRecord>, AppError>;
    async fn list_user_credentials(
        &self,
        external_user_id: &str,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<CredentialRecord>, AppError>;
    async fn get_credential(&self, id: Uuid) -> Result<CredentialRecord, AppError>;
    async fn load_credential_secret(&self, id: Uuid) -> Result<StoredCredentialSecret, AppError>;
    async fn patch_credential(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &CredentialPatchRequest,
        audit: AuditLogInsert,
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
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<CredentialRecord, AppError>;
    async fn mark_credential_validated(
        &self,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<CredentialRecord, AppError>;
    async fn soft_delete_credential(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;
    async fn soft_delete_user_credential(
        &self,
        external_user_id: &str,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
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
    async fn list_system_keys(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ApiKeyRecord>, AppError>;
    async fn list_consumer_keys(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ApiKeyRecord>, AppError>;
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
    async fn revoke_key(
        &self,
        table: &str,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<ApiKeyRecord, AppError>;
    async fn soft_delete_key(
        &self,
        table: &str,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn create_trusted_jwt_issuer(
        &self,
        id: Uuid,
        request: &TrustedJwtIssuerCreateRequest,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn list_trusted_jwt_issuers(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<TrustedJwtIssuerRecord>, AppError>;
    async fn get_trusted_jwt_issuer(&self, id: Uuid) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn patch_trusted_jwt_issuer(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &TrustedJwtIssuerPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn set_trusted_jwt_issuer_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn touch_trusted_jwt_issuer(
        &self,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<TrustedJwtIssuerRecord, AppError>;
    async fn soft_delete_trusted_jwt_issuer(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;
    /// How many live `admin_identities` grants were made through this trusted JWT issuer.
    ///
    /// Backs the `trusted_issuer_has_active_grants` refusal on delete and disable. Both
    /// paths are **soft** (`status = 'deleted'` / `'disabled'`, `deleted_at` set), so
    /// `admin_identities`' foreign key never fires — while `load_issuer` filters
    /// `deleted_at is null`, so every grant made through the issuer stops resolving. One
    /// button silently revokes every admin who signs in through that issuer, with no
    /// warning and no error naming the cause.
    async fn count_active_grants_for_trusted_issuer(&self, id: Uuid) -> Result<i64, AppError>;

    async fn insert_audit(&self, insert: AuditLogInsert) -> Result<(), AppError>;
    /// Note the sort key: `audit_logs` orders by `occurred_at`, not `created_at` (it has
    /// no `created_at` column). Same cursor shape, different column.
    async fn list_audit_logs(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AuditLogRecord>, AppError>;
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

    pub async fn begin_admin_command(&self) -> Result<PgAdminCommandTransaction, AppError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("set transaction isolation level read committed")
            .execute(&mut *transaction)
            .await?;
        Ok(PgAdminCommandTransaction { transaction })
    }
}

impl PgAdminCommandTransaction {
    pub fn connection(&mut self) -> &mut PgConnection {
        &mut self.transaction
    }

    pub async fn create_application(
        &mut self,
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
        .fetch_one(self.connection())
        .await?;
        application_record_from_row(&row)
    }

    pub async fn create_provider(
        &mut self,
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
        .fetch_one(self.connection())
        .await?;
        provider_record_from_row(&row)
    }

    pub async fn create_provider_model(
        &mut self,
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
        .fetch_one(self.connection())
        .await?;
        provider_model_record_from_row(&row)
    }

    pub async fn create_credential(
        &mut self,
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
        .fetch_one(self.connection())
        .await?;
        credential_record_from_row(&row)
    }

    pub async fn rotate_credential(
        &mut self,
        id: Uuid,
        expected_version: Option<i64>,
        encrypted: &EncryptedSecret,
        fingerprint: &str,
        masked: &str,
    ) -> Result<CredentialRecord, AppError> {
        let current_version = sqlx::query_scalar::<_, i64>(
            "select version from provider_credentials where id = $1 and deleted_at is null for update",
        )
        .bind(id)
        .fetch_optional(self.connection())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        if expected_version.is_some_and(|expected| expected != current_version) {
            return Err(AppError::conflict(
                "resource_version_conflict",
                "credential version does not match If-Match",
            ));
        }

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
        .fetch_one(self.connection())
        .await?;
        credential_record_from_row(&row)
    }

    pub async fn create_system_key(
        &mut self,
        id: Uuid,
        request: &SystemKeyCreateRequest,
        material: KeyMaterial<'_>,
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
        .bind(material.key_prefix)
        .bind(material.key_hash)
        .bind(material.fingerprint)
        .bind(material.pepper_version)
        .bind(&request.scopes)
        .bind(request.expires_at)
        .fetch_one(self.connection())
        .await?;
        api_key_record_from_row(&row)
    }

    pub async fn create_consumer_key(
        &mut self,
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
        .fetch_one(self.connection())
        .await?;
        api_key_record_from_row(&row)
    }

    pub async fn rotate_key(
        &mut self,
        table: &str,
        id: Uuid,
        material: KeyMaterial<'_>,
    ) -> Result<ApiKeyRecord, AppError> {
        let sql = key_rotation_sql(table)?;
        let row = sqlx::query(sql)
            .bind(id)
            .bind(material.key_prefix)
            .bind(material.key_hash)
            .bind(material.fingerprint)
            .bind(material.pepper_version)
            .fetch_optional(self.connection())
            .await?
            .ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
        api_key_record_from_row(&row)
    }

    pub async fn create_trusted_jwt_issuer(
        &mut self,
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
        .fetch_one(self.connection())
        .await
        .map_err(duplicate_trusted_jwt_issuer_on_unique_violation)?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    pub async fn claim_idempotency(
        &mut self,
        claim: &AdminIdempotencyClaim,
    ) -> Result<AdminIdempotencyClaimOutcome, AppError> {
        // NOTE(rolling deploy): this key is derived from the *current* fingerprint, which
        // changed spelling when the fingerprint was peppered. Two instances on opposite sides
        // of a rolling deploy therefore derive different keys for the same actor and key, so
        // they do not exclude each other for the duration of the rollout. The unique index
        // still bounds the damage, but the two also insert at different index points, so a
        // duplicate execution is possible in that window. Keying the lock on a
        // migration-stable value fixes it and is tracked in `TODO.md` — it is a separate
        // concern from the dual-read below, changes behaviour this suite pins explicitly, and
        // wants its own change and its own test rather than riding along here.
        let lock_key =
            advisory_lock_key(&claim.key_hash, &claim.actor_fingerprint, &claim.operation);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let acquired = sqlx::query_scalar::<_, bool>("select pg_try_advisory_xact_lock($1)")
                .bind(lock_key)
                .fetch_one(self.connection())
                .await?;
            if acquired {
                break;
            }
            if Instant::now() >= deadline {
                return Err(AppError::conflict(
                    "idempotency_in_progress",
                    "another request with this Idempotency-Key is still in progress",
                ));
            }
            sleep(Duration::from_millis(20)).await;
        }

        // Every position in the unique index this claim can reach is swept: an expired
        // pre-switch row must not be resurrected as a replay by the legacy lookups below.
        // Both index columns have been redefined under deployed rows — the key hash by the
        // HMAC switch (plan 03, P1-1) and the fingerprint by the pepper — so the sweep is
        // the cross product of the spellings of each.
        let mut sweep_key_hashes = vec![claim.key_hash.clone()];
        sweep_key_hashes.extend(claim.legacy_key_hash.clone());
        let mut sweep_fingerprints = vec![claim.actor_fingerprint.clone()];
        sweep_fingerprints.extend(claim.legacy_actor_fingerprint.clone());
        sqlx::query(
            r#"
            delete from idempotency_records
            where idempotency_key_hash = any($1)
              and actor_fingerprint = any($2)
              and operation = $3
              and expires_at <= now()
            "#,
        )
        .bind(sweep_key_hashes)
        .bind(&sweep_fingerprints)
        .bind(&claim.operation)
        .execute(self.connection())
        .await?;

        // Dual lookup on both index columns: the current spelling of each is tried first, so
        // a post-deploy row always wins and a legacy hit is only ever *read*. The two windows
        // close independently, so the query count falls in two steps rather than one: closing
        // the key-hash window (`legacy_key_hash: None`, plan 03 finding F4) drops the inner
        // probe, and retiring the fingerprint window drops the outer one. Only when both are
        // closed does this collapse back to a single query.
        //
        // The advisory lock above is keyed on the *current* pair, which is identical for
        // every concurrent request carrying the same Idempotency-Key and actor — so within
        // one fingerprint spelling every branch below stays serialized against its peers.
        // Across a rolling deploy that spelling differs; see the NOTE on the lock key.
        let mut existing = None;
        'sweep: for fingerprint in [
            Some(&claim.actor_fingerprint),
            claim.legacy_actor_fingerprint.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for key_hash in [Some(&claim.key_hash), claim.legacy_key_hash.as_ref()]
                .into_iter()
                .flatten()
            {
                existing = self
                    .load_idempotency(key_hash, fingerprint, &claim.operation)
                    .await?;
                if existing.is_some() {
                    break 'sweep;
                }
            }
        }

        if let Some(record) = existing {
            // Deliberately no digest comparison here. Deciding whether this row describes
            // the same request means recomputing a keyed HMAC and comparing it in constant
            // time, which is hashing policy and belongs to the application layer
            // (plan 03 finding F3). Returning the row leaves this transaction open, so a
            // conflict raised upstream still rolls the sweep above back, exactly as before.
            return Ok(AdminIdempotencyClaimOutcome::Existing(record));
        }

        sqlx::query(
            r#"
            insert into idempotency_records
                (id, idempotency_key_hash, actor_fingerprint, operation, request_hash,
                 response_status, response_body, resource_id, expires_at)
            values ($1, $2, $3, $4, $5, null, null, null, $6)
            "#,
        )
        .bind(claim.record_id)
        .bind(&claim.key_hash)
        .bind(&claim.actor_fingerprint)
        .bind(&claim.operation)
        .bind(&claim.request_hash)
        .bind(claim.expires_at)
        .execute(self.connection())
        .await?;
        Ok(AdminIdempotencyClaimOutcome::Acquired)
    }

    pub async fn begin_command_savepoint(&mut self) -> Result<(), AppError> {
        sqlx::query("savepoint admin_command_mutation")
            .execute(self.connection())
            .await?;
        Ok(())
    }

    pub async fn release_command_savepoint(&mut self) -> Result<(), AppError> {
        sqlx::query("release savepoint admin_command_mutation")
            .execute(self.connection())
            .await?;
        Ok(())
    }

    pub async fn rollback_command_savepoint(&mut self) -> Result<(), AppError> {
        sqlx::query("rollback to savepoint admin_command_mutation")
            .execute(self.connection())
            .await?;
        Ok(())
    }

    pub async fn finalize_idempotency(
        &mut self,
        record_id: Uuid,
        status: i32,
        response_body: &serde_json::Value,
        resource_id: Option<&str>,
    ) -> Result<(), AppError> {
        let result = sqlx::query(
            r#"
            update idempotency_records
            set response_status = $2,
                response_body = $3,
                resource_id = $4
            where id = $1
              and response_status is null
              and response_body is null
            "#,
        )
        .bind(record_id)
        .bind(status)
        .bind(response_body)
        .bind(resource_id)
        .execute(self.connection())
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::Internal(
                "idempotency claim could not be finalized".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn insert_audit(&mut self, insert: AuditLogInsert) -> Result<(), AppError> {
        insert_audit_with_connection(self.connection(), insert).await
    }

    pub async fn commit(self) -> Result<(), AppError> {
        self.transaction.commit().await?;
        Ok(())
    }

    pub async fn rollback(self) -> Result<(), AppError> {
        self.transaction.rollback().await?;
        Ok(())
    }

    async fn load_idempotency(
        &mut self,
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
            "#,
        )
        .bind(key_hash)
        .bind(actor_fingerprint)
        .bind(operation)
        .fetch_optional(self.connection())
        .await?;
        row.map(|row| idempotency_record_from_row(&row)).transpose()
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

    async fn list_applications(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ApplicationRecord>, AppError> {
        let tail = KeysetTail::new("created_at", cursor.as_ref(), 1);
        let sql = format!(
            r#"
            select id, external_application_id, application_slug, display_name, status,
                   metadata, created_at, updated_at, deleted_at, version
            from applications
            where deleted_at is null
            {}
            {}
            "#,
            tail.and_clause(),
            tail.order_and_limit,
        );
        let rows = bind_cursor(sqlx::query(&sql), cursor.as_ref())
            .bind(over_fetch_limit(limit))
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
        expected_version: i64,
        request: &ApplicationPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<ApplicationRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            APPLICATION_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("application {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update applications
            set external_application_id = coalesce($2, external_application_id),
                application_slug = coalesce($3, application_slug),
                display_name = coalesce($4, display_name),
                updated_at = now()
                , metadata = coalesce($5, metadata)
            where id = $1 and deleted_at is null and version = $6
            returning id, external_application_id, application_slug, display_name, status,
                      metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.external_application_id)
        .bind(&request.application_slug)
        .bind(&request.display_name)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = application_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn set_application_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<ApplicationRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            APPLICATION_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("application {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update applications
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, external_application_id, application_slug, display_name, status,
                      metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = application_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete_application(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            APPLICATION_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("application {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update applications set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
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

    async fn list_providers(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ProviderRecord>, AppError> {
        let tail = KeysetTail::new("created_at", cursor.as_ref(), 1);
        let sql = format!(
            r#"
            select id, provider_type, display_name, base_url, status, metadata,
                   created_at, updated_at, deleted_at, version
            from providers
            where deleted_at is null
            {}
            {}
            "#,
            tail.and_clause(),
            tail.order_and_limit,
        );
        let rows = bind_cursor(sqlx::query(&sql), cursor.as_ref())
            .bind(over_fetch_limit(limit))
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
        expected_version: i64,
        request: &ProviderPatchRequest,
        normalized_base_url: Option<Option<String>>,
        audit: AuditLogInsert,
    ) -> Result<ProviderRecord, AppError> {
        let update_base_url = normalized_base_url.is_some();
        let base_url = normalized_base_url.flatten();
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            PROVIDER_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update providers
            set display_name = coalesce($2, display_name),
                base_url = case when $3 then $4 else base_url end,
                metadata = coalesce($5, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null and version = $6
            returning id, provider_type, display_name, base_url, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(update_base_url)
        .bind(&base_url)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = provider_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn set_provider_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<ProviderRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            PROVIDER_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update providers
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, provider_type, display_name, base_url, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = provider_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete_provider(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            PROVIDER_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update providers set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
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
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ProviderModelRecord>, AppError> {
        // `$1` is `provider_id`, so the cursor (if any) starts at `$2`.
        let tail = KeysetTail::new("created_at", cursor.as_ref(), 2);
        let sql = format!(
            r#"
            select id, provider_id, model_key, display_name, capabilities, status,
                   created_at, updated_at, deleted_at, version
            from provider_models
            where provider_id = $1 and deleted_at is null
            {}
            {}
            "#,
            tail.and_clause(),
            tail.order_and_limit,
        );
        let rows = bind_cursor(sqlx::query(&sql).bind(provider_id), cursor.as_ref())
            .bind(over_fetch_limit(limit))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(provider_model_record_from_row).collect()
    }

    async fn patch_provider_model(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &ProviderModelPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<ProviderModelRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            PROVIDER_MODEL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider model {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update provider_models
            set display_name = coalesce($2, display_name),
                capabilities = coalesce($3, capabilities),
                updated_at = now()
            where id = $1 and deleted_at is null and version = $4
            returning id, provider_id, model_key, display_name, capabilities, status,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.capabilities)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = provider_model_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
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
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<ProviderModelRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            PROVIDER_MODEL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider model {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update provider_models
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, provider_id, model_key, display_name, capabilities, status,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = provider_model_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete_provider_model(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            PROVIDER_MODEL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider model {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update provider_models set status = 'disabled', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
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

    async fn list_credentials(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<CredentialRecord>, AppError> {
        let tail = KeysetTail::new("created_at", cursor.as_ref(), 1);
        // `credential_select_sql` already emits `where deleted_at is null`.
        let sql = credential_select_sql(&format!("{} {}", tail.and_clause(), tail.order_and_limit));
        let rows = bind_cursor(sqlx::query(&sql), cursor.as_ref())
            .bind(over_fetch_limit(limit))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(credential_record_from_row).collect()
    }

    async fn list_user_credentials(
        &self,
        external_user_id: &str,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<CredentialRecord>, AppError> {
        // `$1` is `external_user_id`, so the cursor (if any) starts at `$2`.
        let tail = KeysetTail::new("created_at", cursor.as_ref(), 2);
        let sql = credential_select_sql(&format!(
            "and external_user_id = $1 {} {}",
            tail.and_clause(),
            tail.order_and_limit
        ));
        let rows = bind_cursor(sqlx::query(&sql).bind(external_user_id), cursor.as_ref())
            .bind(over_fetch_limit(limit))
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
        expected_version: i64,
        request: &CredentialPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<CredentialRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            CREDENTIAL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider credential {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update provider_credentials
            set display_name = coalesce($2, display_name),
                priority = coalesce($3, priority),
                expires_at = coalesce($4, expires_at),
                metadata = coalesce($5, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null and version = $6
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
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = credential_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
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
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<CredentialRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            CREDENTIAL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider credential {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update provider_credentials
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, provider_id, credential_type, scope_type, external_tenant_id,
                      application_id, external_user_id, encryption_algorithm,
                      encryption_version, secret_fingerprint, masked_secret, status,
                      priority, expires_at, last_validated_at, last_used_at, metadata,
                      created_at, updated_at, deleted_at, version, display_name
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = credential_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn mark_credential_validated(
        &self,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<CredentialRecord, AppError> {
        // A transaction for one `UPDATE`, so the audit row shares its fate. Every write on
        // this trait carries its own audit row for exactly this reason; see the trait docs.
        let mut tx = self.pool.begin().await?;
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
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        let record = credential_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete_credential(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            CREDENTIAL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("provider credential {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update provider_credentials set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
    }

    async fn soft_delete_user_credential(
        &self,
        external_user_id: &str,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        // Scoped by `external_user_id` as well as `id`: a credential owned by a different user
        // must stay a 404 here, exactly as it was before the lock was introduced, rather than
        // becoming a version conflict that confirms the row exists.
        let current_version = sqlx::query_scalar::<_, i64>(
            "select version from provider_credentials where id = $1 and external_user_id = $2 and deleted_at is null for update",
        )
        .bind(id)
        .bind(external_user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider credential {id}")))?;
        if current_version != expected_version {
            return Err(version_conflict());
        }
        let result = sqlx::query(
            "update provider_credentials set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and external_user_id = $2 and deleted_at is null and version = $3",
        )
        .bind(id)
        .bind(external_user_id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
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

    async fn list_system_keys(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ApiKeyRecord>, AppError> {
        list_keys(&self.pool, "system_api_keys", false, cursor, limit).await
    }

    async fn list_consumer_keys(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ApiKeyRecord>, AppError> {
        list_keys(&self.pool, "consumer_api_keys", true, cursor, limit).await
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

    async fn revoke_key(
        &self,
        table: &str,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<ApiKeyRecord, AppError> {
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
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(sql)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
        let record = api_key_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete_key(
        &self,
        table: &str,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let sql = match table {
            "system_api_keys" => {
                "update system_api_keys set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null"
            }
            "consumer_api_keys" => {
                "update consumer_api_keys set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null"
            }
            _ => return Err(AppError::Internal("unsupported api key table".to_string())),
        };
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(sql).bind(id).execute(&mut *tx).await?;
        ensure_affected(result.rows_affected(), format!("api key {id}"))?;
        commit_with_audit(tx, audit).await?;
        Ok(())
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
        .await
        .map_err(duplicate_trusted_jwt_issuer_on_unique_violation)?;
        crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)
    }

    async fn list_trusted_jwt_issuers(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<TrustedJwtIssuerRecord>, AppError> {
        let tail = KeysetTail::new("created_at", cursor.as_ref(), 1);
        // `trusted_issuer_select_sql` emits no `where`, so this call site owns it.
        let sql = trusted_issuer_select_sql(&format!(
            "where deleted_at is null {} {}",
            tail.and_clause(),
            tail.order_and_limit
        ));
        let rows = bind_cursor(sqlx::query(&sql), cursor.as_ref())
            .bind(over_fetch_limit(limit))
            .fetch_all(&self.pool)
            .await?;
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
        expected_version: i64,
        request: &TrustedJwtIssuerPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            TRUSTED_JWT_ISSUER_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("trusted JWT issuer {id}"),
        )
        .await?;
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
            where id = $1 and deleted_at is null and version = $15
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
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn set_trusted_jwt_issuer_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            TRUSTED_JWT_ISSUER_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("trusted JWT issuer {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update trusted_jwt_issuers
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                      subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                      roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                      clock_skew_seconds, allow_delegation, status, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn touch_trusted_jwt_issuer(
        &self,
        id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<TrustedJwtIssuerRecord, AppError> {
        let mut tx = self.pool.begin().await?;
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
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("trusted JWT issuer {id}")))?;
        let record = crate::infra::pg_rows::trusted_jwt_issuer_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete_trusted_jwt_issuer(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            TRUSTED_JWT_ISSUER_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("trusted JWT issuer {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update trusted_jwt_issuers set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
    }

    async fn count_active_grants_for_trusted_issuer(&self, id: Uuid) -> Result<i64, AppError> {
        // The same predicate `authenticate_admin`'s grant lookup uses: a revoked or
        // soft-deleted grant already authorises nobody, so it is not a reason to refuse.
        let count = sqlx::query_scalar::<_, i64>(
            "select count(*) from admin_identities \
             where trusted_jwt_issuer_id = $1 and deleted_at is null and status = 'active'",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn insert_audit(&self, insert: AuditLogInsert) -> Result<(), AppError> {
        let mut connection = self.pool.acquire().await?;
        insert_audit_with_connection(&mut connection, insert).await
    }

    async fn list_audit_logs(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AuditLogRecord>, AppError> {
        // The one admin list whose sort key is not `created_at`, and the one with no
        // `where` clause of its own — so the keyset predicate has to introduce it.
        let tail = KeysetTail::new("occurred_at", cursor.as_ref(), 1);
        let sql = format!(
            r#"
            select id, occurred_at, request_id, actor_type, actor_subject, delegated_subject,
                   external_user_id, external_tenant_id, application_id, resource_type,
                   resource_id, action, result, source_ip::text as source_ip, user_agent, metadata
            from audit_logs
            {}
            {}
            "#,
            tail.where_clause(),
            tail.order_and_limit,
        );
        let rows = bind_cursor(sqlx::query(&sql), cursor.as_ref())
            .bind(over_fetch_limit(limit))
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

        row.map(|row| idempotency_record_from_row(&row)).transpose()
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

fn advisory_lock_key(key_hash: &str, actor_fingerprint: &str, operation: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(key_hash.as_bytes());
    hasher.update([0]);
    hasher.update(actor_fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(operation.as_bytes());
    let digest = hasher.finalize();
    i64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"))
}

fn key_rotation_sql(table: &str) -> Result<&'static str, AppError> {
    match table {
        "system_api_keys" => Ok(r#"
            update system_api_keys
            set key_prefix = $2, key_hash = $3, fingerprint = $4,
                pepper_version = $5, status = 'active', revoked_at = null, updated_at = now()
            where id = $1 and deleted_at is null
            returning id, null::uuid as application_id, display_name, key_prefix, fingerprint,
                      pepper_version, scopes, status, expires_at, last_used_at, created_at,
                      updated_at, revoked_at
            "#),
        "consumer_api_keys" => Ok(r#"
            update consumer_api_keys
            set key_prefix = $2, key_hash = $3, fingerprint = $4,
                pepper_version = $5, status = 'active', revoked_at = null, updated_at = now()
            where id = $1 and deleted_at is null
            returning id, application_id, display_name, key_prefix, fingerprint,
                      pepper_version, scopes, status, expires_at, last_used_at, created_at,
                      updated_at, revoked_at
            "#),
        _ => Err(AppError::Internal("unsupported api key table".to_string())),
    }
}

fn idempotency_record_from_row(row: &sqlx::postgres::PgRow) -> Result<IdempotencyRecord, AppError> {
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

/// Writes `audit` on `tx`'s own connection and then commits **that same transaction**.
///
/// # Why this consumes the transaction
///
/// The bug this closes was not a missing audit row; it was an audit row written *after* the
/// write had already committed, on a second pooled connection. Two statements, two commits,
/// and any failure between them leaves an administrative change with no record of it.
///
/// Taking `tx` **by value** is what makes that arrangement hard to write down again. There
/// is no `insert_audit; … ; commit` sequence a later edit can quietly reorder: the insert and
/// the commit are one operation, and once it returns the transaction is gone. Reintroducing
/// the divergence now requires committing by hand *and* acquiring a second connection —
/// visible, deliberate lines, not a moved one.
///
/// `pub(crate)` because [`super::runtime`] and [`super::auth_settings`] end their writes
/// with it too.
pub(crate) async fn commit_with_audit(
    mut transaction: sqlx::Transaction<'static, sqlx::Postgres>,
    audit: AuditLogInsert,
) -> Result<(), AppError> {
    insert_audit_with_connection(&mut transaction, audit).await?;
    transaction.commit().await?;
    Ok(())
}

async fn insert_audit_with_connection(
    connection: &mut PgConnection,
    insert: AuditLogInsert,
) -> Result<(), AppError> {
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
    .execute(connection)
    .await?;
    Ok(())
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
    cursor: Option<ListCursor>,
    limit: i64,
) -> Result<Vec<ApiKeyRecord>, AppError> {
    // The `(table, has_application)` match is what keeps the interpolated table name a
    // closed set of literals rather than caller input.
    let projection = match (table, has_application) {
        ("system_api_keys", false) => "null::uuid as application_id",
        ("consumer_api_keys", true) => "application_id",
        _ => return Err(AppError::Internal("unsupported api key table".to_string())),
    };

    let tail = KeysetTail::new("created_at", cursor.as_ref(), 1);
    let sql = format!(
        r#"
        select id, {projection}, display_name, key_prefix, fingerprint,
               pepper_version, scopes, status, expires_at, last_used_at, created_at,
               updated_at, revoked_at
        from {table}
        where deleted_at is null
        {}
        {}
        "#,
        tail.and_clause(),
        tail.order_and_limit,
    );
    let rows = bind_cursor(sqlx::query(&sql), cursor.as_ref())
        .bind(over_fetch_limit(limit))
        .fetch_all(pool)
        .await?;
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

/// The `409` that `trusted_jwt_issuers_issuer_active_unique` produces (finding F13).
///
/// Registering an issuer that already exists used to fall through to `AppError::Sqlx` and
/// reach the caller as **`500 database_error`** — alone among Moira's uniqueness conflicts;
/// `auth_provider_settings` has mapped its equivalent to `duplicate_auth_provider` since
/// `0013`, and `admin_identities` maps its own to `admin_identity_already_claimed`.
///
/// The consequence was not cosmetic. A console recovering from a half-finished
/// registration — the issuer row landed, the step after it did not — cannot adopt the
/// existing issuer by catching a `409` when the `409` never arrives, so it has to
/// list-then-adopt instead. And a `500` is indistinguishable from an outage: the operator
/// is paged for a request that was simply a duplicate.
///
/// `issuer` is the only unique index on live `trusted_jwt_issuers` rows
/// (`0003_security_foundation.sql`), so any unique violation reaching this insert is that
/// one, and matching on the class rather than on the constraint name is exact here.
fn duplicate_trusted_jwt_issuer_on_unique_violation(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(database) if database.is_unique_violation() => AppError::conflict(
            "duplicate_trusted_jwt_issuer",
            "a trusted JWT issuer is already registered for this issuer",
        ),
        _ => AppError::from(error),
    }
}

/// The `409` a stale `If-Match` produces. Identical code and message to the ones the HTTP
/// layer used to emit from its own pre-read comparison, so moving the check down here is
/// invisible on the wire.
fn version_conflict() -> AppError {
    AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    )
}

/// The row-lock statements paired with `lock_and_match_version`, one per versioned admin
/// entity. Each takes the row's id as `$1` and every one of them ends in `for update`; a
/// variant that does not is a silent reopening of the write window.
const APPLICATION_VERSION_FOR_UPDATE: &str =
    "select version from applications where id = $1 and deleted_at is null for update";
const PROVIDER_VERSION_FOR_UPDATE: &str =
    "select version from providers where id = $1 and deleted_at is null for update";
const PROVIDER_MODEL_VERSION_FOR_UPDATE: &str =
    "select version from provider_models where id = $1 and deleted_at is null for update";
const CREDENTIAL_VERSION_FOR_UPDATE: &str =
    "select version from provider_credentials where id = $1 and deleted_at is null for update";
const TRUSTED_JWT_ISSUER_VERSION_FOR_UPDATE: &str =
    "select version from trusted_jwt_issuers where id = $1 and deleted_at is null for update";

/// Evaluate the caller's `If-Match` expectation where it is actually safe: on a row already
/// locked by `select … for update`, inside the same transaction as the write that follows.
///
/// `select_version_sql` must select `version` for the target row and end in `for update`, with
/// `$1` bound to `id`. Returns the locked version so the caller can carry `and version = $N`
/// on the write itself.
///
/// The absent-row branch stays `NotFound` and only a genuine mismatch becomes `409`. Folding
/// the predicate into the existing `UPDATE` without this pre-read would collapse both onto the
/// update's own zero-row branch and turn a stale `If-Match` into a `404`.
async fn lock_and_match_version(
    conn: &mut sqlx::PgConnection,
    select_version_sql: &str,
    id: Uuid,
    expected_version: i64,
    resource: String,
) -> Result<i64, AppError> {
    let current_version = sqlx::query_scalar::<_, i64>(select_version_sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::NotFound(resource))?;
    if current_version != expected_version {
        return Err(version_conflict());
    }
    Ok(current_version)
}
