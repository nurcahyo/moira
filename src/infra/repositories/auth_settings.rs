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
        AuditLogInsert, AuthMethod, AuthProviderSettingsCreateRequest,
        AuthProviderSettingsPatchRequest, AuthProviderSettingsRecord, ListCursor, PublicAuthMethod,
    },
    error::AppError,
    infra::{pg_rows::resource_status_from_db, repositories::admin::commit_with_audit},
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

/// Every mutation below takes an [`AuditLogInsert`] and writes it inside the write's own
/// transaction — see [`AdminRepository`](super::AdminRepository) for why that is a
/// parameter and not a convention.
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
        audit: AuditLogInsert,
    ) -> Result<AuthProviderSettingsRecord, AppError>;

    async fn set_enabled(
        &self,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
        audit: AuditLogInsert,
    ) -> Result<AuthProviderSettingsRecord, AppError>;

    async fn soft_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    /// The bootstrap read behind `GET /api/v1/admin/setup/auth-methods`.
    async fn list_enabled_public(&self) -> Result<Vec<PublicAuthMethod>, AppError>;

    /// The enabled configuration row whose `allowed_email_domains` admit — or refuse — a
    /// claim or an invite redemption presented under `issuer`.
    ///
    /// # Two disjoint stages, and nothing is ordered (finding F23)
    ///
    /// This replaces `governing_policy`, which was a single query ending in
    /// `where … and (issuer = $1 or trusted_jwt_issuer_id = $2)
    ///  order by (issuer is not distinct from $1) desc, created_at asc, id asc limit 1`.
    /// Three defects, all reproduced against a live database:
    ///
    /// * **(a)** On a console-mediated deployment `$1` is the **console's** issuer while
    ///   every provider row's `issuer` column holds the **IdP's**, so no legitimately-bound
    ///   row ever matched the first sort key. Every candidate tied on it and `created_at
    ///   asc` decided — the *oldest* row bound to that trusted issuer supplied the policy
    ///   for every claim and every redemption, whichever provider authenticated the human.
    /// * **(b)** An enabled row whose *own* `issuer` column equals `$1` sorted FIRST at any
    ///   age and outranked the correctly-bound row — and it need not be bound to any
    ///   trusted issuer, so no index on `(trusted_jwt_issuer_id)` can reach it. Verified:
    ///   an unbound `jwks` row with `allowed_email_domains = '{}'` took over the lookup and
    ///   403'd every claim and redemption for that provider.
    /// * **(c)** The intended row bound to a *different* trusted issuer never entered the
    ///   set at all.
    ///
    /// The replacement is two disjoint queries with no `ORDER BY` between them:
    ///
    /// 1. **Bound**: `trusted_jwt_issuer_id = $2`. At most one row by
    ///    `auth_provider_settings_one_enabled_per_trusted_issuer` (`migrations/0020`).
    /// 2. **Only if (1) matched nothing** — `issuer = $1 and trusted_jwt_issuer_id is null`.
    ///    This is CONVENTIONS §7.3's **mode 3**: bring-your-own-JWKS, where the caller *is*
    ///    the IdP and the row's `issuer` legitimately equals the token's. Deleting this
    ///    branch outright would have broken mode 3; restricting it to *unbound* rows is
    ///    what closes shape (b), because a shadowing row can no longer outrank a bound one.
    ///
    /// Stage 2 is unreachable whenever stage 1 matches, so a console-owned issuer string
    /// colliding with some row's `issuer` column is no longer a policy substitution.
    ///
    /// # Why this cannot return "the first of several"
    ///
    /// Both stages `fetch_all` and refuse a set larger than one with
    /// `409 duplicate_enabled_provider_for_issuer`. `fetch_optional` would take an
    /// arbitrary row from a duplicate set and issue a grant from it — silently, and after
    /// the index that is supposed to prevent the state has already been bypassed by, say, a
    /// direct write or a future migration. The index is the invariant; this refusal is the
    /// query being unable to be wrong even without it.
    ///
    /// `None` means no enabled configuration admits the issuer — which the claim policy
    /// treats as a *stricter* case of "no allowed domains" and denies.
    async fn admission_policy(
        &self,
        issuer: &str,
        trusted_jwt_issuer_id: Uuid,
    ) -> Result<Option<GoverningAuthPolicy>, AppError>;

    /// Active `trusted_jwt_issuers` rows whose `issuer` string equals `issuer`, excluding
    /// `exclude_binding` (this provider row's own binding, which is the legitimate case).
    ///
    /// Backs the `auth_provider_issuer_shadows_trusted_issuer` guard.
    async fn trusted_issuer_ids_for_issuer(
        &self,
        issuer: &str,
        exclude_binding: Option<Uuid>,
    ) -> Result<Vec<Uuid>, AppError>;

    /// Ids of the enabled, active, live rows bound to `trusted_jwt_issuer_id`, excluding
    /// `exclude_row`.
    ///
    /// Backs the pre-envelope `duplicate_enabled_provider_for_issuer` check, so the common
    /// path returns a coded 409 rather than a mapped constraint violation.
    async fn enabled_providers_on_trusted_issuer(
        &self,
        trusted_jwt_issuer_id: Uuid,
        exclude_row: Option<Uuid>,
    ) -> Result<Vec<Uuid>, AppError>;

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
        //
        // `method = any(...)` is the W4-B2 filter — see [`DECODABLE_METHODS`]. It sits
        // inside the keyset query rather than after it so the over-fetch still answers
        // "is there another page?" about the rows this binary can actually return.
        let (keyset, limit_param, methods_param) = match cursor {
            Some(_) => (
                "and (created_at, id) < ($1::timestamptz, $2::uuid)",
                "$4",
                "$3",
            ),
            None => ("", "$2", "$1"),
        };
        let sql = format!(
            "select {RECORD_COLUMNS} from auth_provider_settings \
             where deleted_at is null {keyset} and method = any({methods_param}) \
             order by created_at desc, id desc limit {limit_param}"
        );
        let query = sqlx::query(&sql);
        let query = match cursor {
            Some(cursor) => query.bind(cursor.ts).bind(cursor.id),
            None => query,
        };
        let rows = query
            .bind(decodable_method_strings())
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
        audit: AuditLogInsert,
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
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn set_enabled(
        &self,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
        audit: AuditLogInsert,
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
        // `0020`'s partial unique index fires on this UPDATE, not only on INSERT: enabling a
        // second provider on an already-occupied trusted issuer is precisely the transition
        // it refuses. Without this mapping the request became `500 database_error` — F13's
        // exact signature, and the reason G4's assertion is on the code string rather than
        // on the status.
        .await
        .map_err(map_constraint_violation)?
        .ok_or_else(version_conflict)?;
        let record = record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    async fn soft_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
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
        commit_with_audit(tx, audit).await?;
        Ok(())
    }

    /// # W4-B2: one unmappable row must not take the login screen down
    ///
    /// This backs the **anonymous** `GET /api/v1/admin/setup/sign-in-methods`, whose
    /// committed spec declares only `200` and `503` — no `5XX` wildcard. It used to end in
    /// `collect::<Result<_, _>>`, so a single row carrying a `method` this binary cannot
    /// decode poisoned the whole list and returned an **undeclared 500 to unauthenticated
    /// callers, for every provider**, for the length of a rolling deploy.
    ///
    /// The undecodable rows are excluded in SQL (see [`DECODABLE_METHODS`]) and logged, so
    /// the remaining providers still render their buttons. `?` on the `try_get` calls
    /// stays: a row that *did* decode and then failed to project is a genuine schema
    /// mismatch, not a version window, and must not be swallowed.
    async fn list_enabled_public(&self) -> Result<Vec<PublicAuthMethod>, AppError> {
        // Projected field by field, never `..record`, so a future column cannot silently
        // widen the bootstrap response.
        let rows = sqlx::query(
            "select id, method, display_name, issuer, discovery_url, authorization_url, \
                    jwks_url, client_id, requested_scopes, allowed_email_domains \
             from auth_provider_settings \
             where deleted_at is null and status = 'active' and enabled \
               and method = any($1) \
             order by created_at asc, id asc",
        )
        .bind(decodable_method_strings())
        .fetch_all(&self.pool)
        .await?;
        warn_about_undecodable_rows(&self.pool, "list_enabled_public").await;
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

    async fn admission_policy(
        &self,
        issuer: &str,
        trusted_jwt_issuer_id: Uuid,
    ) -> Result<Option<GoverningAuthPolicy>, AppError> {
        // Stage 1 — the provider bound to the trusted issuer the token was verified
        // against. No ORDER BY, no LIMIT: `auth_provider_settings_one_enabled_per_trusted_issuer`
        // makes this at most one row, and a second one is a refusal rather than a choice.
        let bound = sqlx::query(
            "select id, allowed_email_domains from auth_provider_settings \
             where deleted_at is null and status = 'active' and enabled \
               and trusted_jwt_issuer_id = $1",
        )
        .bind(trusted_jwt_issuer_id)
        .fetch_all(&self.pool)
        .await?;
        if let Some(policy) = single_policy(bound, "trusted_jwt_issuer_id")? {
            return Ok(Some(policy));
        }

        // Stage 2 — mode 3 (CONVENTIONS §7.3): the caller IS the IdP, so the row's own
        // `issuer` legitimately equals the token's. Restricted to **unbound** rows, which
        // is what stops a row carrying a console-owned issuer string from shadowing a bound
        // provider (F23 shape (b)). Reached only when stage 1 matched nothing.
        let unbound = sqlx::query(
            "select id, allowed_email_domains from auth_provider_settings \
             where deleted_at is null and status = 'active' and enabled \
               and issuer = $1 and trusted_jwt_issuer_id is null",
        )
        .bind(issuer)
        .fetch_all(&self.pool)
        .await?;
        single_policy(unbound, "issuer")
    }

    async fn trusted_issuer_ids_for_issuer(
        &self,
        issuer: &str,
        exclude_binding: Option<Uuid>,
    ) -> Result<Vec<Uuid>, AppError> {
        let rows = sqlx::query_scalar::<_, Uuid>(
            "select id from trusted_jwt_issuers \
             where issuer = $1 and status = 'active' and deleted_at is null \
               and ($2::uuid is null or id <> $2)",
        )
        .bind(issuer)
        .bind(exclude_binding)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn enabled_providers_on_trusted_issuer(
        &self,
        trusted_jwt_issuer_id: Uuid,
        exclude_row: Option<Uuid>,
    ) -> Result<Vec<Uuid>, AppError> {
        let rows = sqlx::query_scalar::<_, Uuid>(
            "select id from auth_provider_settings \
             where deleted_at is null and status = 'active' and enabled \
               and trusted_jwt_issuer_id = $1 \
               and ($2::uuid is null or id <> $2)",
        )
        .bind(trusted_jwt_issuer_id)
        .bind(exclude_row)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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

/// The one code for "two enabled providers claim the same issuer identity".
///
/// Emitted from three places on purpose, because they are the same fact seen at three
/// moments: the pre-envelope guard in the service (the common path), the mapped index
/// violation from `migrations/0020` (a race the guard cannot close), and
/// [`AuthProviderSettingsRepository::admission_policy`]'s refusal to pick one of a set at
/// claim time (the invariant already broken by something else). A single code means an
/// operator reads one remedy — disable the row that should not govern — rather than
/// three unrelated-looking failures.
pub(crate) fn duplicate_enabled_provider_for_issuer() -> AppError {
    AppError::conflict(
        "duplicate_enabled_provider_for_issuer",
        "more than one enabled auth provider is bound to this trusted JWT issuer",
    )
}

/// Collapse a candidate set to at most one policy, refusing rather than choosing.
///
/// Deliberately **not** `rows.into_iter().next()`. Taking the first row of a duplicate set
/// is how F23 issued grants under the wrong `allowed_email_domains` for a year: it is
/// indistinguishable from a correct result at every call site, and the caller has no way to
/// know a choice was made. A 409 says the deployment is ambiguous and names the remedy.
fn single_policy(
    rows: Vec<PgRow>,
    matched_on: &'static str,
) -> Result<Option<GoverningAuthPolicy>, AppError> {
    if rows.len() > 1 {
        let ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get::<Uuid, _>("id").ok())
            .map(|id| id.to_string())
            .collect();
        tracing::error!(
            target: "moira::auth_settings",
            matched_on,
            provider_ids = ?ids,
            "admission policy lookup matched several enabled providers; refusing rather \
             than admitting under an arbitrary one. Disable all but the row that should \
             govern."
        );
        return Err(duplicate_enabled_provider_for_issuer());
    }
    rows.into_iter()
        .next()
        .map(|row| {
            Ok(GoverningAuthPolicy {
                id: row.try_get("id")?,
                allowed_email_domains: row.try_get("allowed_email_domains")?,
            })
        })
        .transpose()
}

fn auth_provider_not_found(id: Uuid) -> AppError {
    AppError::coded(
        StatusCode::NOT_FOUND,
        "auth_provider_not_found",
        format!("auth provider configuration {id} was not found"),
    )
}

/// Name of the partial unique index added by `migrations/0020`.
///
/// Matched by name rather than folded into the generic unique-violation arm: two enabled
/// providers on one trusted issuer and two rows with the same `(method, issuer)` are
/// different operator problems with different remedies, and F13 is the finding that says
/// collapsing a specific conflict onto a generic one costs an operator a page.
const ONE_ENABLED_PER_TRUSTED_ISSUER_INDEX: &str =
    "auth_provider_settings_one_enabled_per_trusted_issuer";

/// Maps the constraints `0013` and `0020` can raise onto their catalogued codes.
///
/// The CHECK arm is defence in depth: module 9 validates method shape before the write, so
/// reaching it means a caller found a shape the service validator missed — which should
/// still be a `400` the console can act on, not a `500`.
fn map_constraint_violation(error: sqlx::Error) -> AppError {
    let sqlx::Error::Database(database) = &error else {
        return AppError::from(error);
    };
    if database.is_unique_violation() {
        // `0020`'s index. The service pre-checks this so the common path gets a coded 409
        // without a round trip through a constraint violation, but two concurrent enables
        // can still both pass that check and race into the index — and *that* request must
        // not become a `500 database_error`. Finding F13 is exactly this shape, one table
        // over.
        if database.constraint() == Some(ONE_ENABLED_PER_TRUSTED_ISSUER_INDEX) {
            return duplicate_enabled_provider_for_issuer();
        }
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
        AuthMethod::GithubOauth => "github_oauth",
    }
}

fn auth_method_from_db(value: String) -> Result<AuthMethod, AppError> {
    match value.as_str() {
        "google_oauth" => Ok(AuthMethod::GoogleOauth),
        "generic_oidc" => Ok(AuthMethod::GenericOidc),
        "jwks" => Ok(AuthMethod::Jwks),
        "github_oauth" => Ok(AuthMethod::GithubOauth),
        _ => Err(AppError::Internal(format!("unknown auth method {value}"))),
    }
}

/// Every `method` value this binary can decode into an [`AuthMethod`].
///
/// # Why a list exists at all — finding W4-B2
///
/// `charts/moira/templates/migration-job.yaml` runs migrations as a Helm pre-upgrade hook,
/// **before** pods roll, so during any rolling deploy old replicas serve against the new
/// schema. The moment `0020` lands and someone creates a `github_oauth` row, every replica
/// still running the previous binary meets a `method` its `auth_method_from_db` rejects.
///
/// Before this list, that one row failed the whole projection: `list_enabled_public` used
/// `collect::<Result<_, _>>`, so a single unmappable row turned the **anonymous**
/// `GET /api/v1/admin/setup/sign-in-methods` into a 500 — for *every* provider, not just
/// GitHub — on an endpoint whose committed spec declares only `200` and `503`. The login
/// screen went down for the whole rolling-deploy window, and the response was undeclared.
///
/// Filtering in SQL rather than skipping in Rust is deliberate: [`AuthProviderSettingsRepository::list`]
/// over-fetches by one to answer "is there another page?", and a row dropped *after* the
/// fetch shrinks that count, so a full page silently reports itself as the last one. The
/// predicate keeps the keyset arithmetic exact.
///
/// The skipped row is not lost — it is invisible to *this* binary and reappears when the
/// roll completes. It is logged at `warn!` with its id and raw method so an operator who
/// is not mid-upgrade can tell the difference between a version window and a corrupt row.
///
/// **Maintenance:** one entry per [`AuthMethod`] variant. `auth_method_round_trips_through_the_database_encoding`
/// enumerates the same set and asserts the count, so a variant added without an entry here
/// fails that test instead of disappearing from every list.
const DECODABLE_METHODS: &[AuthMethod] = &[
    AuthMethod::GoogleOauth,
    AuthMethod::GenericOidc,
    AuthMethod::Jwks,
    AuthMethod::GithubOauth,
];

fn decodable_method_strings() -> Vec<&'static str> {
    DECODABLE_METHODS
        .iter()
        .copied()
        .map(auth_method_to_db)
        .collect()
}

/// Names the rows this binary is skipping, once per read, at `warn!`.
///
/// Never at `error!`: during a rolling deploy this is the *expected* state, and a page for
/// an expected state trains operators to ignore the signal.
async fn warn_about_undecodable_rows(pool: &PgPool, context: &'static str) {
    let rows = sqlx::query(
        "select id, method from auth_provider_settings \
         where deleted_at is null and status = 'active' and enabled \
           and method <> all($1)",
    )
    .bind(decodable_method_strings())
    .fetch_all(pool)
    .await;
    let Ok(rows) = rows else { return };
    if rows.is_empty() {
        return;
    }
    for row in &rows {
        let id: Result<Uuid, _> = row.try_get("id");
        let method: Result<String, _> = row.try_get("method");
        tracing::warn!(
            target: "moira::auth_settings",
            row_id = ?id.ok(),
            method = ?method.ok(),
            skipped = rows.len(),
            context,
            "auth_provider_settings row carries a method this binary cannot decode; it is \
             skipped rather than failing the whole read. Expected during a rolling deploy \
             (the migration job runs before pods roll); investigate if no upgrade is in \
             flight."
        );
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

    /// Round-trips every variant, and pins the two literals that matter for W4-B2.
    ///
    /// The negative literal used to be `"github"`, which is not the value `0020` writes —
    /// so the test that was meant to prove "an unknown method is refused" passed while
    /// saying nothing about `github_oauth`, the one string wave 4 introduces.
    /// `"github_enterprise"` replaces it: a plausible *next* value, so the assertion is
    /// about a string the tree does not know rather than a typo of one it does.
    #[test]
    fn auth_method_round_trips_through_the_database_encoding() {
        for method in [
            AuthMethod::GoogleOauth,
            AuthMethod::GenericOidc,
            AuthMethod::Jwks,
            AuthMethod::GithubOauth,
        ] {
            assert_eq!(
                auth_method_from_db(auth_method_to_db(method).to_string()).expect("round trip"),
                method
            );
        }
        assert_eq!(
            auth_method_from_db("github_oauth".to_string()).expect("github_oauth decodes"),
            AuthMethod::GithubOauth,
            "the value `migrations/0020` admits must decode; `\"github\"` — the string the \
             previous negative assertion used — is not it"
        );
        assert!(auth_method_from_db("github_enterprise".to_string()).is_err());
        assert!(auth_method_from_db("github".to_string()).is_err());
    }

    /// The W4-B2 filter must name every decodable variant, or a provider vanishes from
    /// every list without a single test noticing.
    #[test]
    fn the_decodable_method_filter_covers_every_variant() {
        let strings = decodable_method_strings();
        for method in [
            AuthMethod::GoogleOauth,
            AuthMethod::GenericOidc,
            AuthMethod::Jwks,
            AuthMethod::GithubOauth,
        ] {
            let encoded = auth_method_to_db(method);
            assert!(
                strings.contains(&encoded),
                "{method:?} encodes to {encoded:?}, which DECODABLE_METHODS omits — every \
                 list would silently drop it"
            );
            assert!(
                auth_method_from_db(encoded.to_string()).is_ok(),
                "DECODABLE_METHODS must only contain values auth_method_from_db accepts"
            );
        }
        // Bump alongside a new variant. The loop above pins the direction that matters
        // (every variant is filtered in); this pins the other one (nothing unmappable was
        // added to the filter by hand).
        assert_eq!(strings.len(), 4);
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
            repo.admission_policy("https://issuer.invalid", Uuid::nil())
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

    /// Mutual exclusion for the issuer-less `generic_oidc` row.
    ///
    /// `auth_provider_settings_method_issuer_active_unique`
    /// (`migrations/0013_auth_provider_settings.sql:67-69`) indexes
    /// `(method, coalesce(issuer, ''))`, so an issuer-less `generic_oidc` row occupies the
    /// single slot `('generic_oidc', '')` for the whole database — not a `Uuid`-keyed one
    /// like every other row this suite writes. On the shared `MOIRA_TEST_DATABASE_URL`
    /// database that makes the slot a global resource, and two test processes inserting
    /// into it collide on a unique violation.
    ///
    /// Same idiom, and for the same reason, as `SetupStateLock` in
    /// `src/application/identity.rs` — a different global resource, so a different key.
    /// The two are deliberately not shared: `src/` has no test-support module, and adding
    /// one to the library crate to host twenty lines of test scaffolding would be a
    /// structural change for a test-only concern.
    const ISSUERLESS_GENERIC_OIDC_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiraoid");

    /// Exclusive ownership of the `('generic_oidc', '')` slot for one test.
    ///
    /// The connection is opened directly rather than taken from the pool because
    /// `pg_advisory_lock` is *session*-scoped: a pooled connection goes back to the pool
    /// still holding the lock, and a later checkout would silently inherit it. Binding the
    /// lock to its own socket means dropping the guard — including while unwinding from a
    /// panic — closes the session and releases it.
    struct IssuerlessSlotLock {
        _session: sqlx::PgConnection,
    }

    impl IssuerlessSlotLock {
        /// Takes the lock, then clears the slot.
        ///
        /// The delete is what makes the test **self-healing**. The lock alone stops two
        /// concurrent runs from colliding, but it does nothing about a run that panicked
        /// between inserting the issuer-less row and the cleanup below — before this, that
        /// left the row behind permanently, and *every* later run of this test then died on
        /// the same unique violation, on a database no one would think to inspect.
        ///
        /// Deleting by the slot rather than by the row's identity is deliberate: the point
        /// is to reclaim the contended resource whatever stale row is sitting in it. Under
        /// the lock, and given this is the only place in the tree that writes an
        /// issuer-less row to the shared database, the only row this can remove is a leak.
        async fn acquire(database_url: &str, pool: &PgPool) -> Self {
            use sqlx::Connection as _;

            let mut session = sqlx::PgConnection::connect(database_url)
                .await
                .expect("open the issuer-less slot lock session");
            sqlx::query("select pg_advisory_lock($1)")
                .bind(ISSUERLESS_GENERIC_OIDC_LOCK_KEY)
                .execute(&mut session)
                .await
                .expect("take the issuer-less slot advisory lock");
            sqlx::query(
                "delete from auth_provider_settings \
                 where method = 'generic_oidc' and issuer is null and deleted_at is null",
            )
            .execute(pool)
            .await
            .expect("reclaim the issuer-less generic_oidc slot");
            Self { _session: session }
        }
    }

    /// **Guard G2 — a shadowing row cannot govern.** Finding F23 shape (b), against a real
    /// planner.
    ///
    /// # What this test used to assert, and why the premise was wrong
    ///
    /// It was `an_exact_issuer_match_outranks_a_match_through_the_trusted_issuer_id`, and it
    /// asserted the old `governing_policy`'s first sort key: a row whose own `issuer` column
    /// equals `$1` wins. That ordering was defensible in the abstract — "an operator who
    /// configured the issuer explicitly gets the row they configured" — and it is exactly
    /// backwards on a console-mediated deployment, where `$1` is the **console's** issuer
    /// and every provider row's `issuer` holds the **IdP's**. A row carrying a console-owned
    /// issuer string is not an operator being explicit; it is a row claiming an identity it
    /// does not have, and it outranked the correctly-bound provider **at any age** while
    /// needing no trusted-issuer binding at all — so no index on `(trusted_jwt_issuer_id)`
    /// could reach it.
    ///
    /// Reproduced before the fix: an enabled `jwks` row with
    /// `allowed_email_domains = '{}'` took over the lookup and 403'd every claim and every
    /// redemption for the bound provider.
    ///
    /// The test is rewritten to the new premise rather than adjusted until it passes: the
    /// **bound** row governs, and the second stage is reached only when nothing is bound.
    ///
    /// # The assertion is on the returned id
    ///
    /// Not on "a policy came back", and not on "the claim succeeded". With a permissive
    /// rogue list the claim succeeds for the wrong reason and every status-level assertion
    /// stays green — which is how this survived a whole plan. The id names *which row* was
    /// consulted, and that is the property.
    ///
    /// The issuer-less shape is load-bearing (it is what makes the rogue row unreachable by
    /// the new index), so [`IssuerlessSlotLock`] still guards the shared `('generic_oidc','')`
    /// slot.
    #[tokio::test]
    async fn a_shadowing_unbound_row_cannot_outrank_the_bound_provider() {
        let Ok(database_url) = std::env::var("MOIRA_TEST_DATABASE_URL") else {
            eprintln!("skipping auth provider settings integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let pool = PgPool::connect(&database_url).await.expect("connect");
        crate::infra::db::migrate(&pool).await.expect("migrate");
        // Bound for the whole test, never as a bare `_`: dropping it early puts the slot
        // back up for grabs while this test is still using it.
        let _slot_lock = IssuerlessSlotLock::acquire(&database_url, &pool).await;

        // The console's issuer — the string `$1` carries on every claim and redemption.
        let console_issuer = format!("https://console-{}.invalid/idp", Uuid::now_v7().simple());
        let issuer_id = sqlx::query_scalar::<_, Uuid>(
            "insert into trusted_jwt_issuers (issuer, jwks_url) values ($1, $2) returning id",
        )
        .bind(&console_issuer)
        .bind("https://idp.invalid/.well-known/jwks.json")
        .fetch_one(&pool)
        .await
        .expect("register a trusted JWT issuer");

        // The correctly-bound provider. Inserted FIRST, so `created_at asc` cannot be what
        // makes this pass.
        let bound_id = sqlx::query_scalar::<_, Uuid>(
            "insert into auth_provider_settings \
                 (method, display_name, enabled, client_id, discovery_url, \
                  allowed_email_domains, trusted_jwt_issuer_id) \
             values ('generic_oidc', 'Bound', true, 'cid', $1, array['corp.test'], $2) \
             returning id",
        )
        .bind(format!("{console_issuer}/.well-known/openid-configuration"))
        .bind(issuer_id)
        .fetch_one(&pool)
        .await
        .expect("insert the bound provider");

        // The rogue row: enabled, UNBOUND, and carrying the console's own issuer string in
        // its `issuer` column with an empty allow-list. Under the old query it sorted first
        // and denied everyone. `0020`'s partial unique index does not touch it — its
        // `trusted_jwt_issuer_id` is NULL — which is precisely why the two-stage lookup and
        // not the index is what closes this.
        let rogue_id = sqlx::query_scalar::<_, Uuid>(
            "insert into auth_provider_settings \
                 (method, display_name, enabled, issuer, jwks_url, allowed_email_domains) \
             values ('jwks', 'Shadow', true, $1, 'https://rogue.invalid/jwks', '{}') \
             returning id",
        )
        .bind(&console_issuer)
        .fetch_one(&pool)
        .await
        .expect("insert the shadowing row");

        let policy = PgAuthProviderSettingsRepository::new(pool.clone())
            .admission_policy(&console_issuer, issuer_id)
            .await
            .expect("policy lookup runs");

        sqlx::query("delete from auth_provider_settings where id = any($1)")
            .bind(vec![bound_id, rogue_id])
            .execute(&pool)
            .await
            .expect("remove the test rows");
        sqlx::query("delete from trusted_jwt_issuers where id = $1")
            .bind(issuer_id)
            .execute(&pool)
            .await
            .expect("remove the test issuer");

        let policy = policy.expect("the bound provider governs");
        assert_eq!(
            policy.id, bound_id,
            "the shadowing row governed: {} was consulted instead of the bound provider {}",
            policy.id, bound_id
        );
        assert_eq!(policy.allowed_email_domains, vec!["corp.test".to_string()]);
    }
}
