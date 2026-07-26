//! Runtime auth-provider settings persistence (plan 07, module 6).
//!
//! Backs `migrations/0013_auth_provider_settings.sql`. Which auth methods a deployment
//! offers, and with what policy, is runtime configuration owned by Moira's database
//! (CONVENTIONS §7.2) — the same place providers, models, routing and credentials already
//! live.
//!
//! # Decision D7: there is no secret on this table, and therefore none in this file
//!
//! There is **no `rotate_secret` method, no `EncryptedSecret` parameter, and no
//! `load_secret`-style read-back anywhere below.** The INSERT/SELECT column lists contain
//! only the non-secret columns of `0013`, which is precisely why this repository is
//! materially simpler than the credential repository it was once modelled on: there is no
//! envelope to map.
//!
//! Adding a secret column plus a read-back here would break the invariant D7 exists to
//! preserve — that a decrypted secret never crosses a network boundary. It is not a
//! follow-up; it is a prohibition. If plan 08 needs an OAuth client secret server-side it
//! reads it from its own `console_auth` database.
//!
//! # Optimistic concurrency
//!
//! Every versioned mutation locks its row with `select … for update` and compares the
//! caller's `If-Match` **inside the same transaction as the write**, mirroring
//! `lock_and_match_version` in [`super::admin`]. A version read on a separate connection is
//! the lost update that shape exists to close.

use async_trait::async_trait;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::{
    domain::{
        AuthMethod, AuthProviderSettingsCreateRequest, AuthProviderSettingsPatchRequest,
        AuthProviderSettingsRecord, ListCursor, PublicAuthMethod,
    },
    error::AppError,
    infra::pg_rows::resource_status_from_db,
};

/// The subset of an `auth_provider_settings` row that the admin-identity claim policy
/// needs (plan 07, module 10).
///
/// Deliberately narrow: the policy decision depends on the allow-list and nothing else,
/// and a wider struct would invite the evaluation to start consulting fields that are not
/// part of the documented rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoverningAuthPolicy {
    pub id: Uuid,
    /// Deny-by-default: an empty list refuses every claim. There is no "empty means
    /// unrestricted" reading anywhere in this plan.
    pub allowed_email_domains: Vec<String>,
}

#[async_trait]
pub trait AuthProviderSettingsRepository: Send + Sync {
    /// Inserts a configuration row **inside the caller's transaction**, so the write sits
    /// under `AdminCommandRunner`'s idempotency savepoint.
    async fn create(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
        request: &AuthProviderSettingsCreateRequest,
    ) -> Result<AuthProviderSettingsRecord, AppError>;

    /// Over-fetches by one, exactly like every other admin list: the extra row answers
    /// "is there another page?" and is trimmed by the application layer.
    async fn list(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AuthProviderSettingsRecord>, AppError>;

    async fn get(&self, id: Uuid) -> Result<AuthProviderSettingsRecord, AppError>;

    async fn patch(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &AuthProviderSettingsPatchRequest,
    ) -> Result<AuthProviderSettingsRecord, AppError>;

    async fn set_enabled(
        &self,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<AuthProviderSettingsRecord, AppError>;

    async fn soft_delete(&self, id: Uuid, expected_version: i64) -> Result<(), AppError>;

    /// The bootstrap read behind `GET /api/v1/admin/setup/auth-methods`.
    async fn list_enabled_public(&self) -> Result<Vec<PublicAuthMethod>, AppError>;

    /// The enabled configuration row governing `issuer`, if any.
    ///
    /// A direct `issuer` match wins over a match through `trusted_jwt_issuer_id`, so an
    /// operator who configured the issuer explicitly gets the row they configured. The
    /// tie-break is written `is not distinct from` rather than `=`: `issuer` is nullable,
    /// `null = $1` is `NULL`, and `order by … desc` puts nulls **first** in Postgres — so
    /// the obvious spelling would rank a row with no issuer *above* an exact match.
    /// `None` means no enabled configuration governs the issuer — which module 10 treats
    /// as a *stricter* case of "no allowed domains" and denies.
    async fn governing_policy(
        &self,
        issuer: &str,
        trusted_jwt_issuer_id: Uuid,
    ) -> Result<Option<GoverningAuthPolicy>, AppError>;

    /// The `scopes_claim` mapping of a trusted JWT issuer, for module 7d's
    /// no-self-asserted-scopes rule.
    ///
    /// An issuer that is not registered and active is `400 unregistered_trusted_issuer` —
    /// the same condition, and the same code, as naming one on the claim path.
    async fn trusted_issuer_scopes_claim(&self, id: Uuid) -> Result<Option<String>, AppError>;
}

#[derive(Debug, Clone)]
pub struct PgAuthProviderSettingsRepository {
    pool: PgPool,
}

impl PgAuthProviderSettingsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Every column of the table except `deleted_at`, which is a predicate rather than a
/// payload. There is deliberately no secret column in this list, and there is none on the
/// table to add (D7).
const RECORD_COLUMNS: &str = "id, method, display_name, enabled, issuer, discovery_url, \
                              authorization_url, token_url, userinfo_url, jwks_url, client_id, \
                              requested_scopes, allowed_email_domains, allowed_algorithms, \
                              expected_audiences, redirect_uris, trusted_jwt_issuer_id, metadata, \
                              status, created_at, updated_at, version";

/// Paired with [`lock_auth_provider_version`]; ends in `for update`, and a variant that
/// does not is a silent reopening of the write window.
const AUTH_PROVIDER_VERSION_FOR_UPDATE: &str =
    "select version from auth_provider_settings where id = $1 and deleted_at is null for update";

#[async_trait]
impl AuthProviderSettingsRepository for PgAuthProviderSettingsRepository {
    async fn create(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
        request: &AuthProviderSettingsCreateRequest,
    ) -> Result<AuthProviderSettingsRecord, AppError> {
        let row = sqlx::query(&format!(
            r#"
            insert into auth_provider_settings
                (id, method, display_name, enabled, issuer, discovery_url, authorization_url,
                 token_url, userinfo_url, jwks_url, client_id, requested_scopes,
                 allowed_email_domains, allowed_algorithms, expected_audiences, redirect_uris,
                 trusted_jwt_issuer_id, metadata)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                    $18)
            returning {RECORD_COLUMNS}
            "#
        ))
        .bind(id)
        .bind(auth_method_to_db(request.method))
        .bind(&request.display_name)
        .bind(request.enabled)
        .bind(&request.issuer)
        .bind(&request.discovery_url)
        .bind(&request.authorization_url)
        .bind(&request.token_url)
        .bind(&request.userinfo_url)
        .bind(&request.jwks_url)
        .bind(&request.client_id)
        .bind(&request.requested_scopes)
        .bind(&request.allowed_email_domains)
        .bind(&request.allowed_algorithms)
        .bind(&request.expected_audiences)
        .bind(&request.redirect_uris)
        .bind(request.trusted_jwt_issuer_id)
        .bind(&request.metadata)
        .fetch_one(conn)
        .await
        .map_err(map_constraint_violation)?;
        record_from_row(&row)
    }

    async fn list(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AuthProviderSettingsRecord>, AppError> {
        // Descending `(created_at, id)` keyset, strictly less-than, over-fetching by one:
        // the same contract every other admin list follows.
        let (keyset, limit_param) = match cursor {
            Some(_) => ("and (created_at, id) < ($1::timestamptz, $2::uuid)", "$3"),
            None => ("", "$1"),
        };
        let sql = format!(
            "select {RECORD_COLUMNS} from auth_provider_settings \
             where deleted_at is null {keyset} \
             order by created_at desc, id desc limit {limit_param}"
        );
        let query = sqlx::query(&sql);
        let query = match cursor {
            Some(cursor) => query.bind(cursor.ts).bind(cursor.id),
            None => query,
        };
        let rows = query
            .bind(limit.saturating_add(1))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(record_from_row).collect()
    }

    async fn get(&self, id: Uuid) -> Result<AuthProviderSettingsRecord, AppError> {
        let row = sqlx::query(&format!(
            "select {RECORD_COLUMNS} from auth_provider_settings \
             where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| auth_provider_not_found(id))?;
        record_from_row(&row)
    }

    async fn patch(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &AuthProviderSettingsPatchRequest,
    ) -> Result<AuthProviderSettingsRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_auth_provider_version(&mut tx, id, expected_version).await?;
        let row = sqlx::query(&format!(
            r#"
            update auth_provider_settings
            set display_name = coalesce($2, display_name),
                enabled = coalesce($3, enabled),
                issuer = coalesce($4, issuer),
                discovery_url = coalesce($5, discovery_url),
                authorization_url = coalesce($6, authorization_url),
                token_url = coalesce($7, token_url),
                userinfo_url = coalesce($8, userinfo_url),
                jwks_url = coalesce($9, jwks_url),
                client_id = coalesce($10, client_id),
                requested_scopes = coalesce($11, requested_scopes),
                allowed_email_domains = coalesce($12, allowed_email_domains),
                allowed_algorithms = coalesce($13, allowed_algorithms),
                expected_audiences = coalesce($14, expected_audiences),
                redirect_uris = coalesce($15, redirect_uris),
                trusted_jwt_issuer_id = coalesce($16, trusted_jwt_issuer_id),
                metadata = coalesce($17, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null and version = $18
            returning {RECORD_COLUMNS}
            "#
        ))
        .bind(id)
        .bind(&request.display_name)
        .bind(request.enabled)
        .bind(&request.issuer)
        .bind(&request.discovery_url)
        .bind(&request.authorization_url)
        .bind(&request.token_url)
        .bind(&request.userinfo_url)
        .bind(&request.jwks_url)
        .bind(&request.client_id)
        .bind(&request.requested_scopes)
        .bind(&request.allowed_email_domains)
        .bind(&request.allowed_algorithms)
        .bind(&request.expected_audiences)
        .bind(&request.redirect_uris)
        .bind(request.trusted_jwt_issuer_id)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_constraint_violation)?
        .ok_or_else(version_conflict)?;
        let record = record_from_row(&row)?;
        tx.commit().await?;
        Ok(record)
    }

    async fn set_enabled(
        &self,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<AuthProviderSettingsRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_auth_provider_version(&mut tx, id, expected_version).await?;
        let row = sqlx::query(&format!(
            r#"
            update auth_provider_settings
            set enabled = $2, updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning {RECORD_COLUMNS}
            "#
        ))
        .bind(id)
        .bind(enabled)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = record_from_row(&row)?;
        tx.commit().await?;
        Ok(record)
    }

    async fn soft_delete(&self, id: Uuid, expected_version: i64) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_auth_provider_version(&mut tx, id, expected_version).await?;
        let result = sqlx::query(
            "update auth_provider_settings \
             set status = 'deleted', enabled = false, deleted_at = now(), updated_at = now() \
             where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        tx.commit().await?;
        Ok(())
    }

    async fn list_enabled_public(&self) -> Result<Vec<PublicAuthMethod>, AppError> {
        // Projected field by field, never `..record`, so a future column cannot silently
        // widen the bootstrap response.
        let rows = sqlx::query(
            "select id, method, display_name, issuer, discovery_url, authorization_url, \
                    jwks_url, client_id, requested_scopes, allowed_email_domains \
             from auth_provider_settings \
             where deleted_at is null and status = 'active' and enabled \
             order by created_at asc, id asc",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(PublicAuthMethod {
                    id: row.try_get("id")?,
                    method: auth_method_from_db(row.try_get::<String, _>("method")?)?,
                    display_name: row.try_get("display_name")?,
                    issuer: row.try_get("issuer")?,
                    discovery_url: row.try_get("discovery_url")?,
                    authorization_url: row.try_get("authorization_url")?,
                    jwks_url: row.try_get("jwks_url")?,
                    client_id: row.try_get("client_id")?,
                    requested_scopes: row.try_get("requested_scopes")?,
                    allowed_email_domains: row.try_get("allowed_email_domains")?,
                })
            })
            .collect()
    }

    async fn governing_policy(
        &self,
        issuer: &str,
        trusted_jwt_issuer_id: Uuid,
    ) -> Result<Option<GoverningAuthPolicy>, AppError> {
        let row = sqlx::query(
            "select id, allowed_email_domains from auth_provider_settings \
             where deleted_at is null and status = 'active' and enabled \
               and (issuer = $1 or trusted_jwt_issuer_id = $2) \
             order by (issuer is not distinct from $1) desc, created_at asc, id asc \
             limit 1",
        )
        .bind(issuer)
        .bind(trusted_jwt_issuer_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(GoverningAuthPolicy {
                id: row.try_get("id")?,
                allowed_email_domains: row.try_get("allowed_email_domains")?,
            })
        })
        .transpose()
    }

    async fn trusted_issuer_scopes_claim(&self, id: Uuid) -> Result<Option<String>, AppError> {
        let row = sqlx::query(
            "select scopes_claim from trusted_jwt_issuers \
             where id = $1 and status = 'active' and deleted_at is null",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            AppError::coded(
                StatusCode::BAD_REQUEST,
                "unregistered_trusted_issuer",
                "the linked trusted JWT issuer is not registered and active",
            )
        })?;
        Ok(row.try_get("scopes_claim")?)
    }
}

/// Evaluate the caller's `If-Match` on a row already locked by `select … for update`,
/// inside the same transaction as the write that follows.
///
/// The absent-row branch stays `404 auth_provider_not_found` and only a genuine mismatch
/// becomes `409`. Folding the predicate into the `UPDATE` without this pre-read would
/// collapse both onto the update's zero-row branch and turn a stale `If-Match` into a 404.
async fn lock_auth_provider_version(
    conn: &mut PgConnection,
    id: Uuid,
    expected_version: i64,
) -> Result<i64, AppError> {
    let current_version = sqlx::query_scalar::<_, i64>(AUTH_PROVIDER_VERSION_FOR_UPDATE)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| auth_provider_not_found(id))?;
    if current_version != expected_version {
        return Err(version_conflict());
    }
    Ok(current_version)
}

fn version_conflict() -> AppError {
    AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    )
}

fn auth_provider_not_found(id: Uuid) -> AppError {
    AppError::coded(
        StatusCode::NOT_FOUND,
        "auth_provider_not_found",
        format!("auth provider configuration {id} was not found"),
    )
}

/// Maps the two constraints `0013` can raise onto their catalogued codes.
///
/// The CHECK arm is defence in depth: module 9 validates method shape before the write, so
/// reaching it means a caller found a shape the service validator missed — which should
/// still be a `400` the console can act on, not a `500`.
fn map_constraint_violation(error: sqlx::Error) -> AppError {
    let sqlx::Error::Database(database) = &error else {
        return AppError::from(error);
    };
    if database.is_unique_violation() {
        return AppError::conflict(
            "duplicate_auth_provider",
            "an auth provider is already configured for this method and issuer",
        );
    }
    if database.is_check_violation() {
        return AppError::coded(
            StatusCode::BAD_REQUEST,
            "auth_provider_method_config_incomplete",
            "the auth provider configuration is incomplete for this method",
        );
    }
    AppError::from(error)
}

fn record_from_row(row: &PgRow) -> Result<AuthProviderSettingsRecord, AppError> {
    Ok(AuthProviderSettingsRecord {
        id: row.try_get("id")?,
        method: auth_method_from_db(row.try_get::<String, _>("method")?)?,
        display_name: row.try_get("display_name")?,
        enabled: row.try_get("enabled")?,
        issuer: row.try_get("issuer")?,
        discovery_url: row.try_get("discovery_url")?,
        authorization_url: row.try_get("authorization_url")?,
        token_url: row.try_get("token_url")?,
        userinfo_url: row.try_get("userinfo_url")?,
        jwks_url: row.try_get("jwks_url")?,
        client_id: row.try_get("client_id")?,
        requested_scopes: row.try_get("requested_scopes")?,
        allowed_email_domains: row.try_get("allowed_email_domains")?,
        allowed_algorithms: row.try_get("allowed_algorithms")?,
        expected_audiences: row.try_get("expected_audiences")?,
        redirect_uris: row.try_get("redirect_uris")?,
        trusted_jwt_issuer_id: row.try_get("trusted_jwt_issuer_id")?,
        metadata: row.try_get::<Value, _>("metadata")?,
        status: resource_status_from_db(row.try_get::<String, _>("status")?)?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: row.try_get::<DateTime<Utc>, _>("updated_at")?,
        version: row.try_get("version")?,
    })
}

fn auth_method_to_db(method: AuthMethod) -> &'static str {
    match method {
        AuthMethod::GoogleOauth => "google_oauth",
        AuthMethod::GenericOidc => "generic_oidc",
        AuthMethod::Jwks => "jwks",
    }
}

fn auth_method_from_db(value: String) -> Result<AuthMethod, AppError> {
    match value.as_str() {
        "google_oauth" => Ok(AuthMethod::GoogleOauth),
        "generic_oidc" => Ok(AuthMethod::GenericOidc),
        "jwks" => Ok(AuthMethod::Jwks),
        _ => Err(AppError::Internal(format!("unknown auth method {value}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn auth_provider_settings_repository_trait_is_object_safe() {
        fn assert_object_safe(_: Arc<dyn AuthProviderSettingsRepository>) {}
        let _ = assert_object_safe;
    }

    /// D7 is a schema fact, and this is the guard that keeps it one: if a secret column is
    /// ever added to `auth_provider_settings`, the projection that would carry it off the
    /// row fails here first.
    #[test]
    fn no_projection_in_this_repository_selects_secret_material() {
        for forbidden in [
            "client_secret",
            "encrypted_payload",
            "encrypted_data_key",
            "encryption_version",
            "secret_fingerprint",
            "masked_secret",
            "nonce",
        ] {
            assert!(
                !RECORD_COLUMNS.contains(forbidden),
                "the record projection must not select {forbidden} — decision D7 means no \
                 secret exists on this table"
            );
        }
    }

    #[test]
    fn the_version_lock_statement_actually_locks() {
        assert!(AUTH_PROVIDER_VERSION_FOR_UPDATE.ends_with("for update"));
    }

    #[test]
    fn auth_method_round_trips_through_the_database_encoding() {
        for method in [
            AuthMethod::GoogleOauth,
            AuthMethod::GenericOidc,
            AuthMethod::Jwks,
        ] {
            assert_eq!(
                auth_method_from_db(auth_method_to_db(method).to_string()).expect("round trip"),
                method
            );
        }
        assert!(auth_method_from_db("github".to_string()).is_err());
    }

    #[test]
    fn only_the_two_expected_constraints_are_translated() {
        assert!(matches!(
            map_constraint_violation(sqlx::Error::RowNotFound),
            AppError::Sqlx(_)
        ));
    }

    #[tokio::test]
    async fn migrated_database_supports_the_auth_settings_reads() {
        let Ok(database_url) = std::env::var("MOIRA_TEST_DATABASE_URL") else {
            eprintln!("skipping auth provider settings integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let pool = PgPool::connect(&database_url).await.expect("connect");
        crate::infra::db::migrate(&pool).await.expect("migrate");
        let repo = PgAuthProviderSettingsRepository::new(pool);

        repo.list(None, 10).await.expect("list runs");
        repo.list_enabled_public()
            .await
            .expect("bootstrap projection runs");
        assert!(
            repo.governing_policy("https://issuer.invalid", Uuid::nil())
                .await
                .expect("policy lookup runs")
                .is_none()
        );
        let error = repo
            .get(Uuid::nil())
            .await
            .expect_err("a missing configuration is a 404");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert!(
            error.to_string().contains("auth_provider_not_found"),
            "unexpected error: {error}"
        );
    }

    /// The `order by … desc` tie-break, exercised against a real planner.
    ///
    /// Written `=` instead of `is not distinct from`, this returns the *issuer-less* row:
    /// `null = $1` is `NULL`, and a descending sort puts nulls first in Postgres. The bug
    /// is invisible in a single-row deployment and silently applies the wrong allow-list in
    /// a multi-provider one, which is exactly the shape of defect a unit test cannot see.
    #[tokio::test]
    async fn an_exact_issuer_match_outranks_a_match_through_the_trusted_issuer_id() {
        let Ok(database_url) = std::env::var("MOIRA_TEST_DATABASE_URL") else {
            eprintln!("skipping auth provider settings integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let pool = PgPool::connect(&database_url).await.expect("connect");
        crate::infra::db::migrate(&pool).await.expect("migrate");

        let issuer = format!("https://governing-{}.invalid", Uuid::now_v7().simple());
        let issuer_id = sqlx::query_scalar::<_, Uuid>(
            "insert into trusted_jwt_issuers (issuer, jwks_url) values ($1, $2) returning id",
        )
        .bind(&issuer)
        .bind("https://idp.invalid/.well-known/jwks.json")
        .fetch_one(&pool)
        .await
        .expect("register a trusted JWT issuer");
        // Inserted first, so `created_at asc` would also prefer it if the tie-break failed.
        sqlx::query(
            "insert into auth_provider_settings \
                 (method, display_name, enabled, client_id, discovery_url, \
                  allowed_email_domains, trusted_jwt_issuer_id) \
             values ('generic_oidc', 'By issuer id', true, 'cid', $1, array['wrong.example'], $2)",
        )
        .bind(format!("{issuer}/.well-known/openid-configuration"))
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("insert the issuer-less row");
        sqlx::query(
            "insert into auth_provider_settings \
                 (method, display_name, enabled, issuer, client_id, allowed_email_domains) \
             values ('google_oauth', 'By issuer', true, $1, 'cid', array['right.example'])",
        )
        .bind(&issuer)
        .execute(&pool)
        .await
        .expect("insert the exact-issuer row");

        let policy = PgAuthProviderSettingsRepository::new(pool.clone())
            .governing_policy(&issuer, issuer_id)
            .await
            .expect("policy lookup runs");

        sqlx::query(
            "delete from auth_provider_settings where issuer = $1 or trusted_jwt_issuer_id = $2",
        )
        .bind(&issuer)
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("remove the test rows");
        sqlx::query("delete from trusted_jwt_issuers where id = $1")
            .bind(issuer_id)
            .execute(&pool)
            .await
            .expect("remove the test issuer");

        assert_eq!(
            policy
                .expect("a governing row exists")
                .allowed_email_domains,
            vec!["right.example".to_string()]
        );
    }
}
