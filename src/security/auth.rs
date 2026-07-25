use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{JwkSet, KeyAlgorithm},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::{
    config::{CallerAuthSettings, JwksFetchSettings, JwtAuthSettings},
    error::{AppError, current_request_id},
};

use super::{
    ApiKeyHasher,
    ssrf::{JwksFetchError, fetch_jwks_hardened},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    #[default]
    Anonymous,
    DevAdmin,
    SystemKey,
    ConsumerKey,
    TrustedJwt,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Actor {
    pub actor_type: ActorType,
    pub subject: Option<String>,
    pub tenant_id: Option<String>,
    pub application_id: Option<String>,
    pub external_user_id: Option<String>,
    pub external_tenant_id: Option<String>,
    pub internal_application_id: Option<Uuid>,
    pub delegated_subject: Option<String>,
    pub roles: Vec<String>,
    pub scopes: Vec<String>,
    pub trusted_jwt_issuer_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
}

/// How long a successfully fetched JWKS stays *fresh*. An expired entry is not
/// evicted: it is retained as a last-known-good fallback (see [`JwksCache::load`]).
const JWKS_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct AuthService {
    admin_settings: JwtAuthSettings,
    caller_settings: CallerAuthSettings,
    http: Client,
    key_hasher: ApiKeyHasher,
    jwks_cache: JwksCache,
}

#[derive(Debug, Clone)]
struct CachedJwks {
    jwks: JwkSet,
    expires_at: Instant,
}

/// The single JWKS cache shared by `AuthService`'s trusted-issuer path and by both
/// static authenticators.
///
/// Deliberately one struct, constructed once in `AppState` and cloned into all three
/// consumers, rather than three divergent copies: the static path previously had no
/// cache at all (a fetch per request in the worst case) and the trusted-issuer path
/// had a cache with no singleflight and no stale retention.
///
/// Keyed by JWKS **URL** rather than by issuer name, so changing an issuer's
/// `jwks_url` naturally invalidates instead of serving keys fetched from the old
/// endpoint, and so the static and trusted paths coalesce when they point at the
/// same endpoint.
#[derive(Debug, Clone)]
pub struct JwksCache {
    settings: JwksFetchSettings,
    entries: Arc<RwLock<HashMap<String, CachedJwks>>>,
    /// One async mutex per URL. Held across the upstream fetch so N concurrent
    /// cache misses for the same issuer collapse into a single upstream call.
    /// Bounded by the number of distinct admin-configured JWKS URLs.
    locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
}

/// How a JWKS was obtained. The caller needs the distinction because a *stale* result
/// still has to be audited even though authentication proceeds.
#[derive(Debug)]
enum JwksOutcome {
    /// Served from a fresh cache entry, or freshly fetched.
    Live(JwkSet),
    /// The refresh failed and the last-known-good entry was served instead.
    Stale {
        jwks: JwkSet,
        failure: Box<JwksFetchError>,
    },
}

impl JwksCache {
    pub fn new(settings: JwksFetchSettings) -> Self {
        Self {
            settings,
            entries: Arc::new(RwLock::new(HashMap::new())),
            locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn fresh(&self, url: &str) -> Option<JwkSet> {
        let guard = self.entries.read().await;
        guard
            .get(url)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.jwks.clone())
    }

    /// Any cached value, expired or not — the last-known-good fallback.
    async fn retained(&self, url: &str) -> Option<JwkSet> {
        let guard = self.entries.read().await;
        guard.get(url).map(|entry| entry.jwks.clone())
    }

    async fn lock_for(&self, url: &str) -> Arc<Mutex<()>> {
        if let Some(lock) = self.locks.read().await.get(url) {
            return lock.clone();
        }
        let mut guard = self.locks.write().await;
        guard
            .entry(url.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Singleflighted, SSRF-hardened JWKS load with last-known-good retention.
    ///
    /// 1. fast path on a fresh cache entry;
    /// 2. take the per-URL lock;
    /// 3. **re-check the cache under the lock** — a concurrent request may have
    ///    refreshed it while this one waited;
    /// 4. fetch only if still stale, then release.
    ///
    /// On failure an existing entry is served even if it expired past the TTL —
    /// a transient IdP outage must not break authentication. The error only
    /// propagates when nothing has ever been cached for this URL.
    async fn load(&self, http: &Client, url: &str) -> Result<JwksOutcome, JwksFetchError> {
        if let Some(jwks) = self.fresh(url).await {
            return Ok(JwksOutcome::Live(jwks));
        }

        let lock = self.lock_for(url).await;
        let _singleflight = lock.lock().await;

        if let Some(jwks) = self.fresh(url).await {
            return Ok(JwksOutcome::Live(jwks));
        }

        match fetch_jwks_hardened(http, url, &self.settings).await {
            Ok(jwks) => {
                self.entries.write().await.insert(
                    url.to_string(),
                    CachedJwks {
                        jwks: jwks.clone(),
                        expires_at: Instant::now() + JWKS_CACHE_TTL,
                    },
                );
                Ok(JwksOutcome::Live(jwks))
            }
            Err(failure) => match self.retained(url).await {
                Some(jwks) => {
                    tracing::warn!(
                        jwks_url = %url,
                        reason = failure.reason().as_str(),
                        detail = %failure.detail(),
                        "JWKS refresh failed; serving the last-known-good cached key set"
                    );
                    Ok(JwksOutcome::Stale {
                        jwks,
                        failure: Box::new(failure),
                    })
                }
                None => Err(failure),
            },
        }
    }

    /// Ages every cached entry past its TTL so a test can force the refresh path
    /// without waiting out `JWKS_CACHE_TTL`.
    #[cfg(test)]
    async fn expire_all(&self) {
        let stale = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        for entry in self.entries.write().await.values_mut() {
            entry.expires_at = stale;
        }
    }
}

#[derive(Debug, Clone)]
struct TrustedIssuerConfig {
    id: Uuid,
    issuer: String,
    jwks_url: String,
    expected_audiences: Vec<String>,
    allowed_algorithms: Vec<String>,
    subject_claim: String,
    user_id_claim: Option<String>,
    tenant_id_claim: Option<String>,
    application_id_claim: Option<String>,
    roles_claim: Option<String>,
    scopes_claim: Option<String>,
    delegated_user_claim: Option<String>,
    delegated_tenant_claim: Option<String>,
    clock_skew_seconds: i32,
    allow_delegation: bool,
}

#[derive(Debug, Clone)]
pub struct AdminAuthenticator {
    settings: JwtAuthSettings,
    http: Client,
    jwks_cache: JwksCache,
    /// Optional because `AppState` can be built without a database (unit-grade HTTP
    /// tests do exactly that). When absent, a JWKS rejection is still traced; only
    /// the audit row is skipped.
    pool: Option<PgPool>,
}

#[derive(Debug, Clone)]
pub struct CallerAuthenticator {
    settings: CallerAuthSettings,
    http: Client,
    jwks_cache: JwksCache,
    pool: Option<PgPool>,
}

#[derive(Debug, Clone, Deserialize)]
struct StaticClaims {
    sub: Option<String>,
    tenant_id: Option<String>,
    application_id: Option<String>,
    roles: Option<Vec<String>>,
    scope: Option<String>,
    scp: Option<Vec<String>>,
}

impl AuthService {
    pub fn new(
        admin_settings: JwtAuthSettings,
        caller_settings: CallerAuthSettings,
        http: Client,
        key_hasher: ApiKeyHasher,
        jwks_cache: JwksCache,
    ) -> Self {
        Self {
            admin_settings,
            caller_settings,
            http,
            key_hasher,
            jwks_cache,
        }
    }

    pub async fn authenticate_admin(
        &self,
        pool: &PgPool,
        headers: &HeaderMap,
    ) -> Result<Actor, AppError> {
        let token = optional_bearer_token(headers)?;
        let system_key = header_string(headers, "x-moira-system-key");
        let consumer_key = header_string(headers, "x-consumer-key");
        let supplied = usize::from(token.is_some())
            + usize::from(system_key.is_some())
            + usize::from(consumer_key.is_some());
        if supplied > 1 {
            return Err(AppError::Unauthorized(
                "conflicting authentication credentials".to_string(),
            ));
        }

        if let Some(token) = token {
            return self.authenticate_trusted_jwt(pool, token).await;
        }

        if let Some(raw_key) = system_key {
            return self.verify_api_key(pool, "system_api_keys", &raw_key).await;
        }

        if let Some(raw_key) = consumer_key {
            return self
                .verify_api_key(pool, "consumer_api_keys", &raw_key)
                .await;
        }

        if !self.admin_settings.enabled {
            return Ok(Actor {
                actor_type: ActorType::DevAdmin,
                subject: Some("dev-admin".to_string()),
                scopes: vec!["moira:admin".to_string(), "moira.admin".to_string()],
                ..Actor::default()
            });
        }

        Err(AppError::Unauthorized(
            "missing bearer token or api key".to_string(),
        ))
    }

    pub async fn authenticate_caller(
        &self,
        pool: &PgPool,
        headers: &HeaderMap,
    ) -> Result<Actor, AppError> {
        let token = optional_bearer_token(headers)?;
        let consumer_key = header_string(headers, "x-consumer-key");
        let system_key = header_string(headers, "x-moira-system-key");
        if system_key.is_some() && (consumer_key.is_some() || token.is_some()) {
            return Err(AppError::Unauthorized(
                "system key cannot be combined with public caller credentials".to_string(),
            ));
        }

        if let Some(raw_key) = system_key {
            return self.verify_api_key(pool, "system_api_keys", &raw_key).await;
        }

        if let (Some(raw_key), Some(token)) = (consumer_key.as_deref(), token) {
            let consumer = self
                .verify_api_key(pool, "consumer_api_keys", raw_key)
                .await?;
            let jwt = self.authenticate_trusted_jwt(pool, token).await?;
            return combine_consumer_and_jwt(consumer, jwt);
        }

        if let Some(raw_key) = consumer_key {
            return self
                .verify_api_key(pool, "consumer_api_keys", &raw_key)
                .await;
        }

        if let Some(token) = token {
            return self.authenticate_trusted_jwt(pool, token).await;
        }

        if self.caller_settings.dev_trust_headers {
            let actor = actor_from_trusted_headers(headers)?;
            if actor.subject.is_some()
                || actor.tenant_id.is_some()
                || actor.application_id.is_some()
            {
                return Ok(actor);
            }
        }

        if !self.caller_settings.enabled {
            return Ok(Actor::default());
        }

        Err(AppError::Unauthorized(
            "missing bearer token or consumer key".to_string(),
        ))
    }

    async fn verify_api_key(
        &self,
        pool: &PgPool,
        table: &str,
        raw_key: &str,
    ) -> Result<Actor, AppError> {
        let key_prefix = self.key_hasher.prefix(raw_key);
        let sql = match table {
            "system_api_keys" => {
                r#"
                select id, null::uuid as application_id, key_hash, scopes, status, expires_at
                from system_api_keys
                where key_prefix = $1
                  and deleted_at is null
                  and status = 'active'
                  and (expires_at is null or expires_at > now())
                "#
            }
            "consumer_api_keys" => {
                r#"
                select id, application_id, key_hash, scopes, status, expires_at
                from consumer_api_keys
                where key_prefix = $1
                  and deleted_at is null
                  and status = 'active'
                  and (expires_at is null or expires_at > now())
                "#
            }
            _ => return Err(AppError::Internal("unsupported api key table".to_string())),
        };

        let rows = sqlx::query(sql).bind(key_prefix).fetch_all(pool).await?;
        for row in rows {
            let key_hash: String = row.try_get("key_hash")?;
            if self.key_hasher.verify(raw_key, &key_hash)? {
                let id: Uuid = row.try_get("id")?;
                let application_id: Option<Uuid> = row.try_get("application_id")?;
                let scopes: Vec<String> = row.try_get("scopes")?;
                let update_sql = match table {
                    "system_api_keys" => {
                        "update system_api_keys set last_used_at = now(), updated_at = now() where id = $1"
                    }
                    "consumer_api_keys" => {
                        "update consumer_api_keys set last_used_at = now(), updated_at = now() where id = $1"
                    }
                    _ => unreachable!("table checked above"),
                };
                sqlx::query(update_sql).bind(id).execute(pool).await?;
                return Ok(Actor {
                    actor_type: if table == "system_api_keys" {
                        ActorType::SystemKey
                    } else {
                        ActorType::ConsumerKey
                    },
                    subject: Some(id.to_string()),
                    internal_application_id: application_id,
                    api_key_id: Some(id),
                    scopes,
                    ..Actor::default()
                });
            }
        }

        Err(AppError::Unauthorized("invalid api key".to_string()))
    }

    async fn authenticate_trusted_jwt(
        &self,
        pool: &PgPool,
        token: &str,
    ) -> Result<Actor, AppError> {
        let header = decode_header(token).map_err(|err| AppError::Unauthorized(err.to_string()))?;
        let key_id = header
            .kid
            .clone()
            .ok_or_else(|| AppError::Unauthorized("JWT kid header is required".to_string()))?;
        let unverified_claims = unverified_claims(token)?;
        let issuer = claim_string(&unverified_claims, "iss")
            .ok_or_else(|| AppError::Unauthorized("JWT iss claim is required".to_string()))?;
        let issuer_config = self.load_issuer(pool, &issuer).await?;
        let algorithm = algorithm_from_name(algorithm_name(header.alg))?;
        if !issuer_config
            .allowed_algorithms
            .iter()
            .any(|allowed| allowed == algorithm_name(algorithm))
        {
            return Err(AppError::Unauthorized(
                "JWT algorithm is not allowed for issuer".to_string(),
            ));
        }

        let jwks = self.jwks(pool, &issuer_config).await?;
        let jwk = jwks
            .find(&key_id)
            .ok_or_else(|| AppError::Unauthorized("no matching JWKS key".to_string()))?;
        let key = DecodingKey::from_jwk(jwk)
            .map_err(|err| AppError::Unauthorized(format!("invalid jwk: {err}")))?;

        let mut validation = Validation::new(algorithm);
        validation.leeway = issuer_config.clock_skew_seconds.max(0) as u64;
        validation.set_issuer(&[issuer_config.issuer.as_str()]);
        if issuer_config.expected_audiences.is_empty() {
            validation.validate_aud = false;
        } else {
            validation.set_audience(
                &issuer_config
                    .expected_audiences
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            );
        }

        let claims = decode::<Value>(token, &key, &validation)
            .map_err(|err| AppError::Unauthorized(err.to_string()))?
            .claims;
        actor_from_trusted_claims(&issuer_config, claims)
    }

    async fn load_issuer(
        &self,
        pool: &PgPool,
        issuer: &str,
    ) -> Result<TrustedIssuerConfig, AppError> {
        let row = sqlx::query(
            r#"
            select id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                   subject_claim, user_id_claim, tenant_id_claim, application_id_claim,
                   roles_claim, scopes_claim, delegated_user_claim, delegated_tenant_claim,
                   clock_skew_seconds, allow_delegation
            from trusted_jwt_issuers
            where issuer = $1
              and status = 'active'
              and deleted_at is null
            "#,
        )
        .bind(issuer)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("unknown or disabled JWT issuer".to_string()))?;

        Ok(TrustedIssuerConfig {
            id: row.try_get("id")?,
            issuer: row.try_get("issuer")?,
            jwks_url: row.try_get("jwks_url")?,
            expected_audiences: row.try_get("expected_audiences")?,
            allowed_algorithms: row.try_get("allowed_algorithms")?,
            subject_claim: row.try_get("subject_claim")?,
            user_id_claim: row.try_get("user_id_claim")?,
            tenant_id_claim: row.try_get("tenant_id_claim")?,
            application_id_claim: row.try_get("application_id_claim")?,
            roles_claim: row.try_get("roles_claim")?,
            scopes_claim: row.try_get("scopes_claim")?,
            delegated_user_claim: row.try_get("delegated_user_claim")?,
            delegated_tenant_claim: row.try_get("delegated_tenant_claim")?,
            clock_skew_seconds: row.try_get("clock_skew_seconds")?,
            allow_delegation: row.try_get("allow_delegation")?,
        })
    }

    async fn jwks(&self, pool: &PgPool, issuer: &TrustedIssuerConfig) -> Result<JwkSet, AppError> {
        resolve_jwks(
            &self.jwks_cache,
            &self.http,
            Some(pool),
            &issuer.jwks_url,
            Some(issuer.id),
        )
        .await
    }
}

/// Shared JWKS resolution for all three authentication paths.
///
/// Audits every rejection — including the ones that are survived by falling back to
/// the retained cache — and maps failures onto the deliberately generic
/// `unauthorized` error so the verification path is not an SSRF oracle.
async fn resolve_jwks(
    cache: &JwksCache,
    http: &Client,
    pool: Option<&PgPool>,
    jwks_url: &str,
    issuer_id: Option<Uuid>,
) -> Result<JwkSet, AppError> {
    match cache.load(http, jwks_url).await {
        Ok(JwksOutcome::Live(jwks)) => Ok(jwks),
        Ok(JwksOutcome::Stale { jwks, failure }) => {
            audit_jwks_rejection(pool, jwks_url, issuer_id, &failure, true).await;
            Ok(jwks)
        }
        Err(failure) => {
            audit_jwks_rejection(pool, jwks_url, issuer_id, &failure, false).await;
            Err(failure.into_unauthorized())
        }
    }
}

/// Records a JWKS fetch rejection in `audit_logs`.
///
/// The URL is safe to record (URLs are not secrets); the specific denial reason and
/// the resolved address stay server-side, which is precisely the split that keeps the
/// caller-visible error from leaking internal topology.
///
/// Mirrors `insert_audit_with_connection` in `src/infra/repositories/admin.rs`; the
/// insert is written directly here because the two static authenticators sit below
/// the repository layer and hold only a pool.
async fn audit_jwks_rejection(
    pool: Option<&PgPool>,
    jwks_url: &str,
    issuer_id: Option<Uuid>,
    failure: &JwksFetchError,
    served_stale_cache: bool,
) {
    tracing::warn!(
        jwks_url = %jwks_url,
        reason = failure.reason().as_str(),
        detail = %failure.detail(),
        served_stale_cache,
        "JWKS fetch rejected by the SSRF/resource policy"
    );

    let Some(pool) = pool else {
        return;
    };

    let metadata = json!({
        "jwks_url": jwks_url,
        "reason": failure.reason().as_str(),
        "served_stale_cache": served_stale_cache,
    });
    // `audit_logs.resource_id` is varchar(256); the untruncated URL is preserved in
    // the metadata document.
    let resource_id = issuer_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| truncate_for_column(jwks_url, 256));

    let insert = sqlx::query(
        r#"
        insert into audit_logs
            (request_id, actor_type, resource_type, resource_id, action, result, metadata)
        values ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(current_request_id())
    .bind("anonymous")
    .bind("jwt_issuer")
    .bind(resource_id)
    .bind("jwks_fetch")
    .bind("denied")
    .bind(metadata)
    .execute(pool)
    .await;

    if let Err(err) = insert {
        tracing::error!(error = %err, "failed to record the JWKS rejection audit entry");
    }
}

fn truncate_for_column(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

impl AdminAuthenticator {
    pub fn new(
        settings: JwtAuthSettings,
        http: Client,
        jwks_cache: JwksCache,
        pool: Option<PgPool>,
    ) -> Self {
        Self {
            settings,
            http,
            jwks_cache,
            pool,
        }
    }

    pub async fn require(&self, headers: &HeaderMap) -> Result<Actor, AppError> {
        if !self.settings.enabled {
            return Ok(Actor {
                actor_type: ActorType::DevAdmin,
                subject: Some("dev-admin".to_string()),
                scopes: vec!["moira.admin".to_string(), "moira:admin".to_string()],
                ..Actor::default()
            });
        }

        let token = bearer_token(headers)?;
        let actor = validate_static_jwt(
            &self.http,
            &self.jwks_cache,
            self.pool.as_ref(),
            token,
            self.settings.jwks_url.as_deref(),
            self.settings.issuer.as_deref(),
            self.settings.audience.as_deref(),
            self.settings.leeway_seconds,
        )
        .await?;

        if let Some(required) = &self.settings.required_scope {
            let scopes: HashSet<_> = actor.scopes.iter().map(String::as_str).collect();
            if !scopes.contains(required.as_str()) {
                return Err(AppError::Forbidden(format!(
                    "missing required admin scope {required}"
                )));
            }
        }

        Ok(actor)
    }
}

impl CallerAuthenticator {
    pub fn new(
        settings: CallerAuthSettings,
        http: Client,
        jwks_cache: JwksCache,
        pool: Option<PgPool>,
    ) -> Self {
        Self {
            settings,
            http,
            jwks_cache,
            pool,
        }
    }

    pub async fn identify(&self, headers: &HeaderMap) -> Result<Actor, AppError> {
        if self.settings.dev_trust_headers {
            let actor = actor_from_trusted_headers(headers)?;
            if actor.subject.is_some()
                || actor.tenant_id.is_some()
                || actor.application_id.is_some()
            {
                return Ok(actor);
            }
        }

        if !self.settings.enabled {
            return Ok(Actor::default());
        }

        validate_static_jwt(
            &self.http,
            &self.jwks_cache,
            self.pool.as_ref(),
            bearer_token(headers)?,
            self.settings.jwks_url.as_deref(),
            self.settings.issuer.as_deref(),
            self.settings.audience.as_deref(),
            self.settings.leeway_seconds,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn validate_static_jwt(
    http: &Client,
    jwks_cache: &JwksCache,
    pool: Option<&PgPool>,
    token: &str,
    jwks_url: Option<&str>,
    issuer: Option<&str>,
    audience: Option<&str>,
    leeway_seconds: u64,
) -> Result<Actor, AppError> {
    let jwks_url = jwks_url.ok_or_else(|| AppError::Config("jwks_url is required".to_string()))?;
    let header = decode_header(token).map_err(|err| AppError::Unauthorized(err.to_string()))?;
    let key_id = header
        .kid
        .ok_or_else(|| AppError::Unauthorized("JWT kid header is required".to_string()))?;
    // Previously an unhardened, uncached `http.get(jwks_url).json::<JwkSet>()` on
    // every request. Now it shares the singleflighted, SSRF-hardened cache with the
    // trusted-issuer path.
    let jwks = resolve_jwks(jwks_cache, http, pool, jwks_url, None).await?;
    let jwk = jwks
        .find(&key_id)
        .ok_or_else(|| AppError::Unauthorized("no matching JWKS key".to_string()))?;
    let key = DecodingKey::from_jwk(jwk)
        .map_err(|err| AppError::Unauthorized(format!("invalid jwk: {err}")))?;

    let mut validation = Validation::new(algorithm_from_jwk(jwk.common.key_algorithm));
    validation.leeway = leeway_seconds;
    if let Some(issuer) = issuer {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = audience {
        validation.set_audience(&[audience]);
    } else {
        validation.validate_aud = false;
    }

    let claims = decode::<StaticClaims>(token, &key, &validation)
        .map_err(|err| AppError::Unauthorized(err.to_string()))?
        .claims;
    actor_from_static_claims(claims)
}

fn actor_from_static_claims(claims: StaticClaims) -> Result<Actor, AppError> {
    let mut scopes = Vec::new();
    if let Some(scope) = claims.scope {
        scopes.extend(scope.split_whitespace().map(ToOwned::to_owned));
    }
    if let Some(scp) = claims.scp {
        scopes.extend(scp);
    }

    let subject = claims.sub.ok_or_else(|| {
        AppError::Unauthorized("trusted JWT subject claim is required".to_string())
    })?;
    let internal_application_id = parse_application_id(claims.application_id.as_deref())?;

    Ok(Actor {
        actor_type: ActorType::TrustedJwt,
        subject: Some(subject.clone()),
        tenant_id: claims.tenant_id.clone(),
        application_id: claims.application_id.clone(),
        external_user_id: Some(subject),
        external_tenant_id: claims.tenant_id,
        internal_application_id,
        roles: claims.roles.unwrap_or_default(),
        scopes,
        ..Actor::default()
    })
}

fn actor_from_trusted_claims(
    issuer: &TrustedIssuerConfig,
    claims: Value,
) -> Result<Actor, AppError> {
    let mut scopes = Vec::new();
    if let Some(claim_name) = issuer.scopes_claim.as_deref().or(Some("scope")) {
        scopes.extend(claim_strings(&claims, claim_name));
    }
    if issuer.scopes_claim.as_deref() != Some("scp") {
        scopes.extend(claim_strings(&claims, "scp"));
    }
    scopes.sort();
    scopes.dedup();

    let subject = claim_string(&claims, &issuer.subject_claim).ok_or_else(|| {
        AppError::Unauthorized(format!(
            "trusted JWT subject claim '{}' is required",
            issuer.subject_claim
        ))
    })?;
    let external_user_id = issuer
        .user_id_claim
        .as_deref()
        .and_then(|claim| claim_string(&claims, claim))
        .or_else(|| Some(subject.clone()));
    let external_tenant_id = issuer
        .tenant_id_claim
        .as_deref()
        .and_then(|claim| claim_string(&claims, claim));
    let application_id = match issuer.application_id_claim.as_deref() {
        Some(claim_name) => application_id_claim(&claims, claim_name)?,
        None => None,
    };
    let internal_application_id = parse_application_id(application_id.as_deref())?;
    let delegated_user = issuer
        .delegated_user_claim
        .as_deref()
        .and_then(|claim| claim_string(&claims, claim));
    let delegated_tenant = issuer
        .delegated_tenant_claim
        .as_deref()
        .and_then(|claim| claim_string(&claims, claim));

    if (delegated_user.is_some() || delegated_tenant.is_some())
        && (!issuer.allow_delegation
            || !scopes
                .iter()
                .any(|scope| scope == "moira:identity:delegate"))
    {
        return Err(AppError::Forbidden(
            "delegated identity requires issuer delegation and moira:identity:delegate".to_string(),
        ));
    }

    let mut roles = Vec::new();
    if let Some(claim_name) = issuer.roles_claim.as_deref() {
        roles.extend(claim_strings(&claims, claim_name));
    }

    Ok(Actor {
        actor_type: ActorType::TrustedJwt,
        subject: Some(subject),
        tenant_id: delegated_tenant.clone().or(external_tenant_id.clone()),
        application_id: application_id.clone(),
        external_user_id: delegated_user.clone().or(external_user_id),
        external_tenant_id: delegated_tenant.or(external_tenant_id),
        internal_application_id,
        delegated_subject: delegated_user,
        roles,
        scopes,
        trusted_jwt_issuer_id: Some(issuer.id),
        api_key_id: None,
    })
}

fn actor_from_trusted_headers(headers: &HeaderMap) -> Result<Actor, AppError> {
    let subject = header_string(headers, "x-moira-user-id");
    let tenant_id = header_string(headers, "x-moira-tenant-id");
    let application_id = header_string(headers, "x-moira-application-id");
    let internal_application_id = parse_application_id(application_id.as_deref())?;

    Ok(Actor {
        actor_type: ActorType::TrustedJwt,
        subject: subject.clone(),
        tenant_id: tenant_id.clone(),
        application_id,
        external_user_id: subject,
        external_tenant_id: tenant_id,
        internal_application_id,
        ..Actor::default()
    })
}

fn parse_application_id(value: Option<&str>) -> Result<Option<Uuid>, AppError> {
    value
        .map(|value| {
            Uuid::parse_str(value).map_err(|_| {
                AppError::Unauthorized(
                    "trusted JWT application identifier must be a valid UUID".to_string(),
                )
            })
        })
        .transpose()
}

fn application_id_claim(claims: &Value, claim_name: &str) -> Result<Option<String>, AppError> {
    match claims.get(claim_name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(AppError::Unauthorized(
            "trusted JWT application identifier must be a non-empty UUID string".to_string(),
        )),
    }
}

fn combine_consumer_and_jwt(consumer: Actor, jwt: Actor) -> Result<Actor, AppError> {
    let Some(application_id) = consumer.internal_application_id else {
        return Err(AppError::Unauthorized(
            "consumer key is not application-bound".to_string(),
        ));
    };
    if let Some(jwt_application_id) = jwt.internal_application_id
        && jwt_application_id != application_id
    {
        return Err(AppError::Unauthorized(
            "consumer key application conflicts with JWT application".to_string(),
        ));
    }

    let jwt_scopes: HashSet<_> = jwt.scopes.iter().map(String::as_str).collect();
    let mut scopes = consumer
        .scopes
        .iter()
        .filter(|scope| scope.as_str() != "moira:admin")
        .filter(|scope| jwt_scopes.contains(scope.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();

    Ok(Actor {
        actor_type: ActorType::ConsumerKey,
        subject: jwt.subject.clone(),
        tenant_id: jwt.tenant_id.clone(),
        application_id: Some(application_id.to_string()),
        external_user_id: jwt.external_user_id.clone().or(jwt.subject),
        external_tenant_id: jwt.external_tenant_id.clone().or(jwt.tenant_id),
        internal_application_id: Some(application_id),
        delegated_subject: jwt.delegated_subject,
        roles: jwt.roles,
        scopes,
        trusted_jwt_issuer_id: jwt.trusted_jwt_issuer_id,
        api_key_id: consumer.api_key_id,
    })
}

fn unverified_claims(token: &str) -> Result<Value, AppError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| AppError::Unauthorized("JWT payload segment is missing".to_string()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|err| AppError::Unauthorized(format!("invalid JWT payload: {err}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| AppError::Unauthorized(format!("invalid JWT claims: {err}")))
}

fn claim_string(claims: &Value, name: &str) -> Option<String> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn claim_strings(claims: &Value, name: &str) -> Vec<String> {
    match claims.get(name) {
        Some(Value::String(value)) => value
            .split_whitespace()
            .filter(|scope| !scope.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .filter(|scope| !scope.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn algorithm_name(algorithm: Algorithm) -> &'static str {
    match algorithm {
        Algorithm::HS256 => "HS256",
        Algorithm::HS384 => "HS384",
        Algorithm::HS512 => "HS512",
        Algorithm::ES256 => "ES256",
        Algorithm::ES384 => "ES384",
        Algorithm::RS256 => "RS256",
        Algorithm::RS384 => "RS384",
        Algorithm::RS512 => "RS512",
        Algorithm::PS256 => "PS256",
        Algorithm::PS384 => "PS384",
        Algorithm::PS512 => "PS512",
        Algorithm::EdDSA => "EdDSA",
    }
}

fn algorithm_from_name(name: &str) -> Result<Algorithm, AppError> {
    match name {
        "RS256" => Ok(Algorithm::RS256),
        "RS384" => Ok(Algorithm::RS384),
        "RS512" => Ok(Algorithm::RS512),
        "PS256" => Ok(Algorithm::PS256),
        "PS384" => Ok(Algorithm::PS384),
        "PS512" => Ok(Algorithm::PS512),
        "ES256" => Ok(Algorithm::ES256),
        "ES384" => Ok(Algorithm::ES384),
        "EdDSA" => Ok(Algorithm::EdDSA),
        _ => Err(AppError::Unauthorized(
            "JWT algorithm is not supported".to_string(),
        )),
    }
}

fn algorithm_from_jwk(algorithm: Option<KeyAlgorithm>) -> Algorithm {
    match algorithm {
        Some(KeyAlgorithm::RS256) | None => Algorithm::RS256,
        Some(KeyAlgorithm::RS384) => Algorithm::RS384,
        Some(KeyAlgorithm::RS512) => Algorithm::RS512,
        Some(KeyAlgorithm::PS256) => Algorithm::PS256,
        Some(KeyAlgorithm::PS384) => Algorithm::PS384,
        Some(KeyAlgorithm::PS512) => Algorithm::PS512,
        Some(KeyAlgorithm::ES256) => Algorithm::ES256,
        Some(KeyAlgorithm::ES384) => Algorithm::ES384,
        Some(KeyAlgorithm::EdDSA) => Algorithm::EdDSA,
        _ => Algorithm::RS256,
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AppError> {
    optional_bearer_token(headers)?
        .ok_or_else(|| AppError::Unauthorized("missing Authorization header".to_string()))
}

fn optional_bearer_token(headers: &HeaderMap) -> Result<Option<&str>, AppError> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::Unauthorized("invalid Authorization header".to_string()))?;
    value
        .strip_prefix("Bearer ")
        .map(Some)
        .ok_or_else(|| AppError::Unauthorized("Authorization must use Bearer token".to_string()))
}

fn header_string(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use serde_json::json;
    use tokio::sync::{Barrier, Notify};

    use super::{
        super::ssrf::test_stub::{EMPTY_JWKS, StubPlan, spawn},
        *,
    };

    /// JWKS fetch policy for the loopback stub: the dev override is what makes a
    /// `http://127.0.0.1` IdP reachable at all, which is itself the assertion that
    /// the production posture would refuse it.
    fn stub_settings() -> JwksFetchSettings {
        JwksFetchSettings {
            allow_insecure_dev_urls: true,
            timeout_ms: 2_000,
            ..JwksFetchSettings::default()
        }
    }

    #[tokio::test]
    async fn concurrent_cache_miss_loads_are_singleflighted_to_one_upstream_call() {
        const CONCURRENCY: usize = 8;

        let arrived = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let listening = arrived.notified();
        let stub = spawn(StubPlan {
            arrived: Some(arrived.clone()),
            hold: Some(release.clone()),
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;

        let cache = JwksCache::new(stub_settings());
        let http = Client::new();
        // The barrier is the acknowledgement gate (CONVENTIONS §3: no `sleep`): every
        // task is inside `load` before the stub is allowed to answer, so the seven
        // followers are genuinely queued behind the leader's per-URL lock.
        let gate = Arc::new(Barrier::new(CONCURRENCY + 1));

        let mut handles = Vec::with_capacity(CONCURRENCY);
        for _ in 0..CONCURRENCY {
            let cache = cache.clone();
            let http = http.clone();
            let url = stub.url.clone();
            let gate = gate.clone();
            handles.push(tokio::spawn(async move {
                gate.wait().await;
                cache.load(&http, &url).await.map(|_| ())
            }));
        }

        gate.wait().await;
        // Wait for the leader's request to reach the stub before releasing it.
        listening.await;
        release.notify_waiters();

        for handle in handles {
            handle
                .await
                .expect("no jwks load task may panic")
                .expect("every coalesced load must succeed");
        }

        assert_eq!(
            stub.hits.load(Ordering::SeqCst),
            1,
            "{CONCURRENCY} concurrent cache misses must collapse into one upstream fetch"
        );
    }

    #[tokio::test]
    async fn a_failed_refresh_serves_the_last_known_good_cached_jwks() {
        let stub = spawn(StubPlan {
            fail_after_first: true,
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;

        let cache = JwksCache::new(stub_settings());
        let http = Client::new();

        cache
            .load(&http, &stub.url)
            .await
            .expect("the first fetch must warm the cache");

        // Force the refresh path; every later stub response is a 500.
        cache.expire_all().await;

        let outcome = cache
            .load(&http, &stub.url)
            .await
            .expect("a failed refresh must not break authentication");
        assert!(
            matches!(outcome, JwksOutcome::Stale { .. }),
            "the retained entry must be served and the failure reported for auditing"
        );
        assert_eq!(stub.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_first_ever_fetch_failure_does_not_fail_open() {
        let stub = spawn(StubPlan {
            status_line: "HTTP/1.1 503 Service Unavailable",
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;

        let cache = JwksCache::new(stub_settings());
        let error = cache
            .load(&Client::new(), &stub.url)
            .await
            .expect_err("with no cached entry the failure must propagate");
        assert_eq!(error.reason().as_str(), "status");
    }

    #[tokio::test]
    async fn a_rejected_jwks_fetch_surfaces_as_a_generic_unauthorized() {
        let stub = spawn(StubPlan {
            content_type: "text/html",
            ..StubPlan::json("<html>redis</html>")
        })
        .await;

        let cache = JwksCache::new(stub_settings());
        // `pool` is `None` here, so the audit row is skipped and only the tracing
        // event fires — the error mapping is what this asserts.
        let error = resolve_jwks(&cache, &Client::new(), None, &stub.url, None)
            .await
            .expect_err("a rejected fetch must not authenticate the caller");

        assert!(matches!(error, AppError::Unauthorized(_)));
        let body = format!("{:?}", error.error_response(None));
        assert!(
            !body.contains("content_type") && !body.contains("text/html"),
            "the denial reason must not reach the caller: {body}"
        );
        assert!(
            !body.contains(&stub.url),
            "the JWKS URL must not reach the caller: {body}"
        );
    }

    #[test]
    fn actor_claims_merge_scope_formats() {
        let application_id = Uuid::now_v7();
        let application_id_text = application_id.to_string();
        let actor = actor_from_static_claims(StaticClaims {
            sub: Some("user-1".to_string()),
            tenant_id: Some("tenant-1".to_string()),
            application_id: Some(application_id_text.clone()),
            roles: Some(vec!["admin".to_string()]),
            scope: Some("read write".to_string()),
            scp: Some(vec!["moira.admin".to_string()]),
        })
        .unwrap();

        assert_eq!(actor.subject.as_deref(), Some("user-1"));
        assert_eq!(actor.tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(
            actor.application_id.as_deref(),
            Some(application_id_text.as_str())
        );
        assert_eq!(actor.internal_application_id, Some(application_id));
        assert_eq!(actor.scopes, vec!["read", "write", "moira.admin"]);
    }

    #[test]
    fn static_claims_reject_malformed_application_identifier() {
        let result = actor_from_static_claims(StaticClaims {
            sub: None,
            tenant_id: None,
            application_id: Some("not-a-uuid".to_string()),
            roles: None,
            scope: None,
            scp: None,
        });

        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[test]
    fn static_claims_require_subject() {
        let result = actor_from_static_claims(StaticClaims {
            sub: None,
            tenant_id: None,
            application_id: Some(Uuid::now_v7().to_string()),
            roles: None,
            scope: None,
            scp: None,
        });

        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    fn trusted_issuer_with_application_claim() -> TrustedIssuerConfig {
        TrustedIssuerConfig {
            id: Uuid::now_v7(),
            issuer: "https://issuer.example".to_string(),
            jwks_url: "https://issuer.example/jwks".to_string(),
            expected_audiences: Vec::new(),
            allowed_algorithms: vec!["RS256".to_string()],
            subject_claim: "sub".to_string(),
            user_id_claim: None,
            tenant_id_claim: None,
            application_id_claim: Some("application_id".to_string()),
            roles_claim: None,
            scopes_claim: Some("scope".to_string()),
            delegated_user_claim: None,
            delegated_tenant_claim: None,
            clock_skew_seconds: 60,
            allow_delegation: false,
        }
    }

    #[test]
    fn trusted_issuer_claims_reject_malformed_application_identifier() {
        let issuer = trusted_issuer_with_application_claim();
        let result = actor_from_trusted_claims(
            &issuer,
            json!({
                "sub": "user-1",
                "application_id": "not-a-uuid"
            }),
        );

        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[test]
    fn trusted_issuer_claims_reject_non_string_application_identifier() {
        let issuer = trusted_issuer_with_application_claim();
        let result = actor_from_trusted_claims(
            &issuer,
            json!({
                "sub": "user-1",
                "application_id": 42
            }),
        );

        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[test]
    fn trusted_issuer_claims_require_configured_subject() {
        let issuer = trusted_issuer_with_application_claim();
        let result = actor_from_trusted_claims(
            &issuer,
            json!({
                "application_id": Uuid::now_v7().to_string()
            }),
        );

        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[test]
    fn trusted_issuer_claims_allow_missing_application_for_admin_authentication() {
        let issuer = trusted_issuer_with_application_claim();
        let actor = actor_from_trusted_claims(&issuer, json!({ "sub": "admin-1" })).unwrap();

        assert_eq!(actor.actor_type, ActorType::TrustedJwt);
        assert_eq!(actor.internal_application_id, None);
    }

    #[test]
    fn delegated_identity_requires_explicit_scope() {
        let issuer = TrustedIssuerConfig {
            id: Uuid::now_v7(),
            issuer: "https://issuer.example".to_string(),
            jwks_url: "https://issuer.example/jwks".to_string(),
            expected_audiences: Vec::new(),
            allowed_algorithms: vec!["RS256".to_string()],
            subject_claim: "sub".to_string(),
            user_id_claim: Some("uid".to_string()),
            tenant_id_claim: Some("tid".to_string()),
            application_id_claim: None,
            roles_claim: None,
            scopes_claim: Some("scope".to_string()),
            delegated_user_claim: Some("delegated_user".to_string()),
            delegated_tenant_claim: None,
            clock_skew_seconds: 60,
            allow_delegation: true,
        };

        let denied = actor_from_trusted_claims(
            &issuer,
            json!({
                "sub": "svc",
                "uid": "svc",
                "delegated_user": "user-1",
                "scope": "moira:prompt"
            }),
        );
        assert!(denied.is_err());

        let allowed = actor_from_trusted_claims(
            &issuer,
            json!({
                "sub": "svc",
                "uid": "svc",
                "delegated_user": "user-1",
                "scope": "moira:identity:delegate moira:prompt"
            }),
        )
        .unwrap();
        assert_eq!(allowed.external_user_id.as_deref(), Some("user-1"));
        assert_eq!(allowed.delegated_subject.as_deref(), Some("user-1"));
    }

    #[test]
    fn claim_strings_accept_space_or_array_claims() {
        let claims = json!({
            "scope": "read write",
            "scp": ["admin"]
        });
        assert_eq!(claim_strings(&claims, "scope"), vec!["read", "write"]);
        assert_eq!(claim_strings(&claims, "scp"), vec!["admin"]);
    }

    #[test]
    fn consumer_jwt_combination_intersects_public_scopes_and_strips_admin() {
        let application_id = Uuid::now_v7();
        let consumer_key_id = Uuid::now_v7();
        let issuer_id = Uuid::now_v7();
        let combined = combine_consumer_and_jwt(
            Actor {
                actor_type: ActorType::ConsumerKey,
                internal_application_id: Some(application_id),
                scopes: vec![
                    "moira:admin".to_string(),
                    "moira:responses:create".to_string(),
                    "moira:models:read".to_string(),
                ],
                api_key_id: Some(consumer_key_id),
                ..Actor::default()
            },
            Actor {
                actor_type: ActorType::TrustedJwt,
                subject: Some("user-1".to_string()),
                external_tenant_id: Some("tenant-1".to_string()),
                internal_application_id: Some(application_id),
                scopes: vec![
                    "moira:responses:create".to_string(),
                    "moira:routes:read".to_string(),
                ],
                trusted_jwt_issuer_id: Some(issuer_id),
                ..Actor::default()
            },
        )
        .unwrap();

        assert_eq!(combined.actor_type, ActorType::ConsumerKey);
        assert_eq!(combined.internal_application_id, Some(application_id));
        assert_eq!(combined.api_key_id, Some(consumer_key_id));
        assert_eq!(combined.trusted_jwt_issuer_id, Some(issuer_id));
        assert_eq!(combined.external_user_id.as_deref(), Some("user-1"));
        assert_eq!(combined.external_tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(combined.scopes, vec!["moira:responses:create"]);
    }

    #[test]
    fn consumer_jwt_combination_rejects_application_conflict() {
        let err = combine_consumer_and_jwt(
            Actor {
                actor_type: ActorType::ConsumerKey,
                internal_application_id: Some(Uuid::now_v7()),
                scopes: vec!["moira:responses:create".to_string()],
                ..Actor::default()
            },
            Actor {
                actor_type: ActorType::TrustedJwt,
                internal_application_id: Some(Uuid::now_v7()),
                scopes: vec!["moira:responses:create".to_string()],
                ..Actor::default()
            },
        );

        assert!(err.is_err());
    }
}
