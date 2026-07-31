//! Surface shared by every admin sub-service.
//!
//! Nothing here belongs to one bounded context. It is the pagination machinery every list
//! goes through, the idempotency/audit envelope helpers every mutation goes through, the
//! request-validation functions, and the small set of helpers that used to be private
//! methods on the single `AdminService` impl.
//!
//! # Why the ex-methods are free functions
//!
//! `command_hasher`, `reject_denied_jwks_url`, `audit_denied`,
//! `validate_provider_base_url_with_settings`, and `schedule_runtime_cache_invalidation`
//! were private methods on `AdminService`. Six sub-services now need overlapping subsets of
//! them. Taking `&AppState` / `&PgAdminRepository` as an argument keeps every sub-service
//! holding the same two plain fields, so the bodies that moved kept their `self.state` and
//! `self.repo` expressions untouched, and there is no shared base type or `Deref` to reason
//! about at the call site.
//!
//! # There is no longer an `audit_success` here, and that is the point
//!
//! This file used to hold two near-identical names: [`success_audit`], which *builds* an
//! [`AuditLogInsert`], and `audit_success`, which *wrote* one directly through the
//! repository on a second pooled connection — after the write it described had already
//! committed on a different one. Thirty-six admin mutations went through that second form
//! and could therefore lose their audit row while keeping the write.
//!
//! `audit_success` is **deleted**. Every write method on `AdminRepository`,
//! `AuthProviderSettingsRepository` and `RuntimeRepository` now takes an `AuditLogInsert`
//! and writes it inside its own transaction, so the only way to produce a `Success` audit
//! row for an admin mutation is to hand it to the write. Re-adding a repository-level
//! `insert_audit` call on a mutation path re-opens the divergence; `audit_denied` below is
//! the one deliberate exception, and it records something that did **not** happen.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{AdminCommandIdempotency, AdminCommandSpec, RequestContext},
    domain::{
        ApiKeyRecord, ApiKeySecretResponse, ApplicationSlug, AuditLogInsert, AuditResult,
        CredentialCreateRequest, CredentialRecord, CredentialScope, CredentialSecret, CursorScope,
        ExternalApplicationId, ExternalTenantId, ExternalUserId, ListCursor, ListResponse,
        PageQuery,
    },
    error::AppError,
    infra::{
        pg_rows::credential_record_from_row,
        repositories::{AdminRepository, PgAdminCommandTransaction, PgAdminRepository},
    },
    security::{
        Actor, AuthorizationService, IdempotencyHasher, secret_fingerprint, validate_jwks_url,
    },
};

/// Cursor scopes for the nine admin lists (plan 04, P1-4).
///
/// Each label is mixed into the cursor's integrity tag but never stored inside it, so a
/// cursor minted by one list fails closed with `400 invalid_cursor` on another rather than
/// paging through an unrelated table's key space. Encode and decode must be given the same
/// const, which is why each list below names exactly one and uses it for both.
///
/// Two of these lists are additionally narrowed by a path/query parameter
/// (`list_provider_models` by `provider_id`, `list_user_credentials` by
/// `external_user_id`), and the scope label does not capture that parameter. Reusing one
/// user's cursor on another user's list therefore decodes successfully — and is harmless:
/// the `where` clause still restricts the query to the rows that caller is authorised to
/// see, so the cursor can only seek to an offset inside that same authorised set. It is a
/// position, not a capability.
pub(crate) const APPLICATIONS_CURSOR: CursorScope = CursorScope::new("admin.applications");
pub(crate) const PROVIDERS_CURSOR: CursorScope = CursorScope::new("admin.providers");
pub(crate) const PROVIDER_MODELS_CURSOR: CursorScope = CursorScope::new("admin.provider_models");
pub(crate) const CREDENTIALS_CURSOR: CursorScope = CursorScope::new("admin.credentials");
pub(crate) const USER_CREDENTIALS_CURSOR: CursorScope = CursorScope::new("admin.user_credentials");
pub(crate) const SYSTEM_KEYS_CURSOR: CursorScope = CursorScope::new("admin.system_keys");
pub(crate) const CONSUMER_KEYS_CURSOR: CursorScope = CursorScope::new("admin.consumer_keys");
pub(crate) const TRUSTED_JWT_ISSUERS_CURSOR: CursorScope =
    CursorScope::new("admin.trusted_jwt_issuers");
pub(crate) const AUDIT_LOGS_CURSOR: CursorScope = CursorScope::new("admin.audit_logs");

/// What a paginated admin list needs from its caller: how many rows, and where to resume.
///
/// # There is exactly one way to build one, deliberately
///
/// [`From<&PageQuery>`] is the only constructor. It carries both the `limit` *and* the
/// `cursor` query parameter through to the service, so a handler cannot obtain a
/// `PageRequest` without having decided what to do about the cursor.
///
/// An earlier revision also had a `From<i64>` bridge that hardcoded `cursor: None`, so a
/// handler passing a bare `query.limit()` still type-checked while silently dropping the
/// caller's cursor. All nine admin lists shipped that way and compiled cleanly. The bridge
/// is gone: passing a limit alone is now a compile error, which is the only mechanism that
/// actually keeps the cursor wired end-to-end.
///
/// Decoding lives here rather than in the HTTP layer deliberately: the handlers stay thin,
/// and there is exactly one place that knows which [`CursorScope`] belongs to which list.
#[derive(Debug, Clone)]
pub struct PageRequest {
    limit: i64,
    cursor: Option<String>,
}

impl PageRequest {
    /// Rows the caller asked for, clamped. The repository fetches one more than this.
    pub fn limit(&self) -> i64 {
        self.limit
    }

    /// The raw, still-encoded cursor exactly as the client sent it.
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// Decodes the cursor for `scope`, or `Ok(None)` for a first page.
    ///
    /// Returns `400 invalid_cursor` for anything malformed, tampered with, or issued for a
    /// different list. Callers run this *after* the authorization check and *before*
    /// touching the database, so an unauthorized caller learns nothing about cursor
    /// validity and a bad cursor never reaches Postgres.
    pub(crate) fn decode(&self, scope: CursorScope) -> Result<Option<ListCursor>, AppError> {
        ListCursor::decode_optional(self.cursor(), scope)
    }
}

impl From<&PageQuery> for PageRequest {
    fn from(query: &PageQuery) -> Self {
        Self {
            limit: query.limit(),
            cursor: query.cursor.clone(),
        }
    }
}

/// Turns the repository's over-fetched rows into a real [`ListResponse`].
///
/// The repository returns up to `limit + 1` rows; that extra row exists only to answer
/// "is there another page?" without a second `count(*)`. Here it is counted, then dropped.
///
/// `key` extracts the sort key of a row — whichever timestamp column that list orders by,
/// paired with `id`. It is passed in rather than derived because the nine lists do not
/// share a record type and `audit_logs` does not even sort on the same column.
pub(crate) fn paginate<T>(
    mut rows: Vec<T>,
    page: &PageRequest,
    scope: CursorScope,
    key: impl Fn(&T) -> ListCursor,
) -> ListResponse<T> {
    let limit = usize::try_from(page.limit()).unwrap_or(0);
    let has_more = rows.len() > limit;
    rows.truncate(limit);

    // `next_cursor` encodes the last row *actually returned*, never the over-fetched one.
    // Encoding the extra row would start the next page one record too far along and drop
    // exactly one row at every page boundary.
    let next_cursor = rows
        .last()
        .filter(|_| has_more)
        .map(|row| key(row).encode(scope));

    // Built through `ListResponse::new` and then filled in, rather than as a struct
    // literal: `Pagination` lives in a private `domain` submodule and is not re-exported,
    // so its name is not reachable from here. Adding a `ListResponse::paginated`
    // constructor belongs to whoever owns `src/domain/`, not to this file.
    let mut response = ListResponse::new(rows);
    response.pagination.has_more = has_more;
    response.pagination.next_cursor = next_cursor;
    response
}

/// The keyed hasher every admin command ledger write goes through (plan 03, P1-1).
pub(crate) fn command_hasher(state: &AppState) -> IdempotencyHasher {
    state.idempotency_hasher.clone()
}

/// Registration-time `jwks_url` gate (plan 03, P1-2).
///
/// The same [`validate_jwks_url`] the verification path runs, applied where the
/// value **enters** the system. Without this a `https://169.254.169.254/…` issuer is
/// persisted happily and only fails much later, per caller, as an opaque `401` — the
/// row sitting in `trusted_jwt_issuers` is itself the finding.
///
/// A scheme-and-host check is *not* a substitute: `https://169.254.169.254/` passes
/// it, which is exactly how this regressed.
///
/// **`Resolution`/`Timeout` are deliberately not fatal here.** Whether a hostname
/// resolves *right now* is an availability fact, not a security one: an IdP with a
/// briefly unreachable nameserver — or a name that only resolves inside the cluster
/// the workload will eventually run in — must still be registrable, and a config API
/// that fails on transient DNS is worse than useless. Nothing is weakened by this:
/// a name that resolves into denied space is refused as `IpRange` here, and *every*
/// fetch re-runs the full validation before a single byte is read.
pub(crate) async fn reject_denied_jwks_url(
    state: &AppState,
    jwks_url: &str,
) -> Result<(), AppError> {
    use crate::security::JwksDenialReason;

    let Err(failure) = validate_jwks_url(jwks_url, &state.settings.auth.jwks).await else {
        return Ok(());
    };

    if matches!(
        failure.reason(),
        JwksDenialReason::Resolution | JwksDenialReason::Timeout
    ) {
        tracing::warn!(
            jwks_url = %jwks_url,
            reason = failure.reason().as_str(),
            detail = %failure.detail(),
            "accepted a trusted JWT issuer jwks_url that could not be resolved at \
             registration time; the address-range check will run again on every fetch"
        );
        return Ok(());
    }

    // Reason and resolved address stay server-side; the admin gets the single
    // catalogued `jwks_url_rejected` shape.
    tracing::warn!(
        jwks_url = %jwks_url,
        reason = failure.reason().as_str(),
        detail = %failure.detail(),
        "rejected a trusted JWT issuer registration whose jwks_url is not permitted"
    );
    Err(failure.into_registration_error())
}

/// Records a denial without ever changing the caller-visible outcome.
///
/// A failed audit insert is logged and swallowed on purpose: if it propagated, the
/// admin's response would vary with database state, reintroducing exactly the
/// distinguishable-outcome problem the denial path exists to remove.
pub(crate) async fn audit_denied(
    repo: &PgAdminRepository,
    actor: &Actor,
    ctx: &RequestContext,
    action: &str,
    resource_type: &str,
    resource_id: Option<String>,
    metadata: Value,
) {
    let recorded = repo
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
            result: AuditResult::Denied,
            source_ip: ctx.source_ip,
            user_agent: ctx.user_agent.clone(),
            metadata,
        })
        .await;
    if let Err(err) = recorded {
        tracing::error!(error = %err, action, "failed to record an admin denial audit entry");
    }
}

/// [`validate_provider_base_url`] with this deployment's private/HTTP policy applied.
pub(crate) fn validate_provider_base_url_with_settings(
    state: &AppState,
    value: &str,
) -> Result<String, AppError> {
    validate_provider_base_url(
        value,
        state.settings.provider_security.allow_private_provider_urls,
        state.settings.provider_security.allow_http_provider_urls,
    )
}

pub(crate) fn schedule_runtime_cache_invalidation(state: &AppState) {
    let cache = state.runtime_cache.clone();
    std::mem::drop(tokio::spawn(async move {
        cache.invalidate_all().await;
    }));
}

pub(crate) fn validate_application_identifiers(
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

/// **The** actor fingerprint. One formula, crate-wide, for every writer of
/// `idempotency_records` (plan 06, Module 16 / P2-15).
///
/// The fingerprint is the actor half of the unique index
/// `(idempotency_key_hash, actor_fingerprint, operation)`
/// (`migrations/0003_security_foundation.sql:360-361`) and an input to
/// `advisory_lock_key` (`src/infra/repositories/admin.rs`). It is what makes an
/// `Idempotency-Key` scoped to the caller who issued it: two actors that hash to the same
/// value can replay each other's stored responses. Every identity field omitted from it is
/// therefore a cross-actor replay hole, not a missed optimisation.
///
/// Until plan 06 there were three formulas writing that one index — this 10-field one, a
/// 3-field one in `crate::application::runtime_admin`, and a 4-field one in
/// `crate::application::public`. The two weaker copies made `route.create`,
/// `routing_policy.create`, `agent_profile.create`, `provider_runtime_policy.*` and
/// `response.create` blind to the caller's issuer and tenant. They are gone; what survives
/// of them is a **read-only** legacy value in each of those two modules
/// (`legacy_actor_fingerprint`, `legacy_public_actor_fingerprint`), used solely to keep
/// pre-deploy ledger rows replayable, never written. See `plans/06-…` §16.3.
///
/// ## Why each field is in it
///
/// | Field | Why it must discriminate |
/// |---|---|
/// | `actor_type` | A system key, a consumer key, a trusted JWT and a dev admin are different principals even when every other field coincides. |
/// | `subject` | The caller's own identity inside its authentication scheme. |
/// | `api_key_id` | *Which* credential authenticated. A revoked-and-reissued key must not inherit the old key's ledger. |
/// | `trusted_jwt_issuer_id` | Two issuers can mint the same `sub`. Without this, one tenant's IdP replays another's responses — the headline P2-15 hole. |
/// | `internal_application_id` | Moira's own `applications` row; the authorization boundary most resources hang off. |
/// | `application_id` | The caller-**asserted** application claim (a string), distinct from the resolved internal UUID above. An unresolved claim must not collide with a resolved one. |
/// | `tenant_id` | The tenant claim as presented. |
/// | `external_tenant_id` | The second, independently-populated tenant channel; multi-tenant isolation is exactly what replay scoping is for. |
/// | `external_user_id` | The end user the call is made for. |
/// | `delegated_subject` | On-behalf-of delegation: the delegate and the delegator are different actors for replay purposes. |
///
/// Serialised as a `serde_json` tuple rather than a `format!` string so no field can be
/// made ambiguous by a separator character appearing inside another field's value, then
/// reduced by `secret_fingerprint` so the ledger stores no caller identity in the clear.
///
/// `pub(crate)` exists so `crate::application::{conversation, runtime_admin, public}` reuse
/// this exact formula; do not copy the body anywhere.
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

pub(crate) fn admin_command_spec<T: Serialize>(
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

pub(crate) fn success_audit(
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

pub(crate) async fn require_active_row(
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

pub(crate) async fn load_credential_record(
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

pub(crate) fn sanitized_key_response(resource: &ApiKeyRecord) -> ApiKeySecretResponse {
    ApiKeySecretResponse {
        resource: resource.clone(),
        secret: None,
        secret_retrievable: false,
    }
}

pub(crate) fn validate_credential_scope(request: &CredentialCreateRequest) -> Result<(), AppError> {
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

pub(crate) fn authorize_credential_scope(
    actor: &Actor,
    scope: &CredentialScope,
) -> Result<(), AppError> {
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

pub(crate) fn authorize_credential_record(
    actor: &Actor,
    record: &CredentialRecord,
) -> Result<(), AppError> {
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

pub(crate) fn validate_credential_secret(
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

pub(crate) fn mask_credential_secret(secret: &CredentialSecret) -> String {
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

pub(crate) fn validate_key_request(
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

pub(crate) fn require_non_empty(label: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        Err(AppError::BadRequest(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_jwt_algorithm_list(values: &[String]) -> Result<(), AppError> {
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
    use serde_json::json;

    /// A stand-in row: the page-assembly logic only ever touches a row's sort key, so the
    /// tests do not need any of the nine real record types.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row {
        created_at: chrono::DateTime<chrono::Utc>,
        id: Uuid,
    }

    const TEST_SCOPE: CursorScope = CursorScope::new("test.rows");
    const OTHER_SCOPE: CursorScope = CursorScope::new("test.other");

    /// Largest page any admin list will return.
    ///
    /// The clamp itself lives in `PageQuery::limit` (`src/domain/admin.rs`) and there is
    /// only one of it: since `From<&PageQuery>` is the sole way to build a [`PageRequest`],
    /// no limit can reach the repository without passing through that clamp. This constant
    /// is the *test's* expectation of that value, not a second implementation of it, and
    /// `page_request_limit_matches_page_query_clamp` fails if the domain clamp moves.
    const MAX_PAGE_LIMIT: i64 = 200;

    /// `count` rows in the descending order a list query would return them.
    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|index| Row {
                created_at: chrono::DateTime::from_timestamp_micros(
                    1_753_401_600_000_000 - (index as i64 * 1_000_000),
                )
                .expect("in-range timestamp"),
                id: Uuid::from_u128(1_000 - index as u128),
            })
            .collect()
    }

    /// A first page of `limit` rows, built the only way a `PageRequest` can be built:
    /// through a `PageQuery`, exactly as an HTTP handler does.
    fn page_of(rows: Vec<Row>, limit: i64) -> ListResponse<Row> {
        let page = PageRequest::from(&PageQuery {
            limit: Some(limit),
            ..PageQuery::default()
        });
        paginate(rows, &page, TEST_SCOPE, |row| {
            ListCursor::new(row.created_at, row.id)
        })
    }

    #[test]
    fn has_more_is_false_when_exactly_limit_rows_are_available() {
        // The repository over-fetches by one, so "exactly a full page" means it returned
        // `limit` rows, not `limit + 1`. That is the end of the data.
        let response = page_of(rows(10), 10);

        assert_eq!(response.data.len(), 10);
        assert!(!response.pagination.has_more);
        assert_eq!(response.pagination.next_cursor, None);
    }

    #[test]
    fn has_more_is_true_and_page_is_trimmed_when_limit_plus_one_rows_are_fetched() {
        let fetched = rows(11);
        let response = page_of(fetched.clone(), 10);

        assert!(response.pagination.has_more);
        assert_eq!(
            response.data.len(),
            10,
            "the over-fetched row must not be served"
        );
        assert_eq!(response.data, fetched[..10]);
        assert!(response.pagination.next_cursor.is_some());
    }

    #[test]
    fn next_cursor_encodes_the_last_returned_row_not_the_over_fetched_row() {
        // The classic off-by-one: resuming from the over-fetched row would skip exactly one
        // record at every page boundary, and the loss would be invisible in any single
        // response.
        let fetched = rows(11);
        let response = page_of(fetched.clone(), 10);

        let encoded = response
            .pagination
            .next_cursor
            .as_deref()
            .expect("has_more is true");
        let decoded = ListCursor::decode(encoded, TEST_SCOPE).expect("our own cursor");

        let last_returned = &fetched[9];
        assert_eq!(decoded.id, last_returned.id);
        assert_eq!(decoded.ts, last_returned.created_at);
        assert_ne!(
            decoded.id, fetched[10].id,
            "cursor points past the served page"
        );
    }

    #[test]
    fn next_cursor_is_none_when_has_more_is_false() {
        for available in 0..=10 {
            let response = page_of(rows(available), 10);
            assert!(!response.pagination.has_more, "{available} rows");
            assert_eq!(response.pagination.next_cursor, None, "{available} rows");
        }
    }

    #[test]
    fn an_empty_final_page_is_not_an_error() {
        // What a caller sees when the rows their cursor pointed at were deleted between
        // pages: a clean empty page, not a 500 and not a phantom cursor.
        let response = page_of(Vec::new(), 10);

        assert!(response.data.is_empty());
        assert!(!response.pagination.has_more);
        assert_eq!(response.pagination.next_cursor, None);
    }

    #[test]
    fn next_cursor_is_scoped_to_its_own_endpoint() {
        let response = page_of(rows(11), 10);
        let encoded = response.pagination.next_cursor.expect("has_more is true");

        assert!(ListCursor::decode(&encoded, TEST_SCOPE).is_ok());
        assert!(
            ListCursor::decode(&encoded, OTHER_SCOPE).is_err(),
            "a cursor must not page through a different list"
        );
    }

    #[test]
    fn page_request_carries_the_cursor_from_the_query_string() {
        let cursor = ListCursor::new(
            chrono::DateTime::from_timestamp_micros(1_753_401_600_000_000).expect("in range"),
            Uuid::from_u128(7),
        );
        let encoded = cursor.encode(TEST_SCOPE);

        let query = PageQuery {
            limit: Some(25),
            cursor: Some(encoded.clone()),
            ..PageQuery::default()
        };
        let page = PageRequest::from(&query);

        assert_eq!(page.limit(), 25);
        assert_eq!(page.cursor(), Some(encoded.as_str()));
        assert_eq!(page.decode(TEST_SCOPE).unwrap(), Some(cursor));
        // Wrong endpoint, and garbage, both fail closed rather than paging wrongly.
        assert!(page.decode(OTHER_SCOPE).is_err());

        let absent = PageRequest::from(&PageQuery::default());
        assert_eq!(absent.cursor(), None);
        assert_eq!(absent.decode(TEST_SCOPE).unwrap(), None);
    }

    #[test]
    fn a_malformed_cursor_is_rejected_with_the_invalid_cursor_code() {
        let query = PageQuery {
            cursor: Some("not-a-cursor".to_string()),
            ..PageQuery::default()
        };

        let error = PageRequest::from(&query)
            .decode(TEST_SCOPE)
            .expect_err("must reject");
        let response = error.error_response(Some("req-test".to_string()));

        assert_eq!(response.error.code, "invalid_cursor");
        assert_eq!(response.error.message_key, "moira.error.invalid_cursor");
        assert!(!response.error.message.is_empty());
    }

    #[test]
    fn page_request_limit_matches_page_query_clamp() {
        // `MAX_PAGE_LIMIT` is restated from `PageQuery::limit`; this is what stops the two
        // drifting apart. If the domain clamp changes, this fails rather than silently
        // letting one path over-fetch.
        let huge = PageQuery {
            limit: Some(i64::MAX),
            ..PageQuery::default()
        };
        assert_eq!(huge.limit(), MAX_PAGE_LIMIT);
        assert_eq!(PageRequest::from(&huge).limit(), MAX_PAGE_LIMIT);

        // And the floor: a non-positive limit can never reach the repository, where it
        // would become a negative `LIMIT` and a database error.
        for absurd in [0, -1, i64::MIN] {
            let query = PageQuery {
                limit: Some(absurd),
                ..PageQuery::default()
            };
            assert_eq!(query.limit(), 1);
            assert_eq!(PageRequest::from(&query).limit(), 1);
        }
    }

    #[test]
    fn every_admin_list_uses_a_distinct_cursor_scope() {
        // Two lists sharing a label would let a cursor from one silently page the other,
        // which is precisely what the scope exists to prevent.
        let scopes = [
            APPLICATIONS_CURSOR,
            PROVIDERS_CURSOR,
            PROVIDER_MODELS_CURSOR,
            CREDENTIALS_CURSOR,
            USER_CREDENTIALS_CURSOR,
            SYSTEM_KEYS_CURSOR,
            CONSUMER_KEYS_CURSOR,
            TRUSTED_JWT_ISSUERS_CURSOR,
            AUDIT_LOGS_CURSOR,
        ];
        let mut labels: Vec<&str> = scopes.iter().map(|scope| scope.label()).collect();
        labels.sort_unstable();
        let total = labels.len();
        labels.dedup();

        assert_eq!(total, 9, "all nine admin lists must declare a scope");
        assert_eq!(labels.len(), total, "duplicate cursor scope: {labels:?}");
    }

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

    /// A trusted-JWT actor whose only distinguishing field is set by the caller.
    fn trusted_actor() -> Actor {
        use crate::security::ActorType;

        Actor {
            actor_type: ActorType::TrustedJwt,
            subject: Some("shared-subject".to_string()),
            trusted_jwt_issuer_id: Some(Uuid::nil()),
            api_key_id: None,
            ..Actor::default()
        }
    }

    // The three tests below are the unit-level statement of the property Module 16 exists
    // to restore. Each varies exactly one identity field and requires the fingerprint to
    // move. Run against the pre-unification 3-field runtime-admin formula
    // (`{actor_type, subject, api_key_id}`) all three fail, because none of the fields they
    // vary is an input to it; that is the bug, reproduced. The permanent, mechanical
    // version of that proof lives next to each surviving legacy formula —
    // `runtime_admin::tests::the_legacy_runtime_admin_fingerprint_collided_across_issuer_tenant_and_delegation`
    // and `public::tests::the_legacy_public_fingerprint_collided_across_tenant_and_delegation`
    // assert the old formulas collide *and* that the unified one does not, in one test.

    #[test]
    fn actor_fingerprint_distinguishes_actors_differing_only_by_trusted_jwt_issuer() {
        // Two IdPs can mint the same `sub`. If the issuer does not reach the fingerprint,
        // issuer A's Idempotency-Key replays issuer B's stored response.
        let first = trusted_actor();
        let second = Actor {
            trusted_jwt_issuer_id: Some(Uuid::now_v7()),
            ..first.clone()
        };

        assert_ne!(
            actor_fingerprint(&first),
            actor_fingerprint(&second),
            "trusted_jwt_issuer_id must partition the replay ledger"
        );
    }

    #[test]
    fn actor_fingerprint_distinguishes_actors_differing_only_by_tenant() {
        // Both tenant channels are checked: `tenant_id` (the claim as presented) and
        // `external_tenant_id` (the resolved one). They are populated independently, so a
        // formula covering only one of them still leaks across tenants on the other.
        let base = trusted_actor();

        let tenant_claim = Actor {
            tenant_id: Some("tenant-a".to_string()),
            ..base.clone()
        };
        let other_tenant_claim = Actor {
            tenant_id: Some("tenant-b".to_string()),
            ..base.clone()
        };
        assert_ne!(
            actor_fingerprint(&tenant_claim),
            actor_fingerprint(&other_tenant_claim),
            "tenant_id must partition the replay ledger"
        );

        let external_tenant = Actor {
            external_tenant_id: Some("tenant-a".to_string()),
            ..base.clone()
        };
        let other_external_tenant = Actor {
            external_tenant_id: Some("tenant-b".to_string()),
            ..base
        };
        assert_ne!(
            actor_fingerprint(&external_tenant),
            actor_fingerprint(&other_external_tenant),
            "external_tenant_id must partition the replay ledger"
        );
    }

    #[test]
    fn actor_fingerprint_distinguishes_actors_differing_only_by_delegated_subject() {
        // On-behalf-of: the same authenticated caller acting for two different end users
        // must not share one replay ledger entry.
        let first = Actor {
            delegated_subject: Some("user-a".to_string()),
            ..trusted_actor()
        };
        let second = Actor {
            delegated_subject: Some("user-b".to_string()),
            ..first.clone()
        };

        assert_ne!(
            actor_fingerprint(&first),
            actor_fingerprint(&second),
            "delegated_subject must partition the replay ledger"
        );
    }

    #[test]
    fn every_identity_field_on_the_actor_moves_the_fingerprint() {
        // The generalisation of the three tests above, and the guard against the next
        // identity field being added to `Actor` without being added here. Each mutation
        // varies exactly one field away from a fully-populated baseline; every one must
        // produce a distinct fingerprint, and all ten must be distinct from each other.
        use crate::security::ActorType;

        let base = Actor {
            actor_type: ActorType::TrustedJwt,
            subject: Some("subject".to_string()),
            tenant_id: Some("tenant".to_string()),
            application_id: Some("app-claim".to_string()),
            external_user_id: Some("external-user".to_string()),
            external_tenant_id: Some("external-tenant".to_string()),
            internal_application_id: Some(Uuid::nil()),
            delegated_subject: Some("delegate".to_string()),
            roles: vec!["role".to_string()],
            scopes: vec!["moira:admin".to_string()],
            trusted_jwt_issuer_id: Some(Uuid::nil()),
            api_key_id: Some(Uuid::nil()),
        };
        let other_uuid = Uuid::now_v7();

        let mutations: Vec<(&str, Actor)> = vec![
            (
                "actor_type",
                Actor {
                    actor_type: ActorType::ConsumerKey,
                    ..base.clone()
                },
            ),
            (
                "subject",
                Actor {
                    subject: Some("other".to_string()),
                    ..base.clone()
                },
            ),
            (
                "api_key_id",
                Actor {
                    api_key_id: Some(other_uuid),
                    ..base.clone()
                },
            ),
            (
                "trusted_jwt_issuer_id",
                Actor {
                    trusted_jwt_issuer_id: Some(other_uuid),
                    ..base.clone()
                },
            ),
            (
                "internal_application_id",
                Actor {
                    internal_application_id: Some(other_uuid),
                    ..base.clone()
                },
            ),
            (
                "application_id",
                Actor {
                    application_id: Some("other".to_string()),
                    ..base.clone()
                },
            ),
            (
                "tenant_id",
                Actor {
                    tenant_id: Some("other".to_string()),
                    ..base.clone()
                },
            ),
            (
                "external_user_id",
                Actor {
                    external_user_id: Some("other".to_string()),
                    ..base.clone()
                },
            ),
            (
                "external_tenant_id",
                Actor {
                    external_tenant_id: Some("other".to_string()),
                    ..base.clone()
                },
            ),
            (
                "delegated_subject",
                Actor {
                    delegated_subject: Some("other".to_string()),
                    ..base.clone()
                },
            ),
        ];

        let baseline = actor_fingerprint(&base);
        let mut seen = std::collections::HashSet::new();
        seen.insert(baseline.clone());
        for (field, mutated) in mutations {
            let fingerprint = actor_fingerprint(&mutated);
            assert_ne!(
                fingerprint, baseline,
                "changing `{field}` alone must change the fingerprint"
            );
            assert!(
                seen.insert(fingerprint),
                "`{field}` collided with another field's mutation — the tuple encoding is \
                 ambiguous"
            );
        }
    }

    #[test]
    fn roles_and_scopes_are_deliberately_outside_the_fingerprint() {
        // Authorization state is not identity. Granting a caller a new scope mid-window
        // must not orphan its in-flight Idempotency-Keys and silently re-execute them.
        let base = trusted_actor();
        let re_scoped = Actor {
            roles: vec!["new-role".to_string()],
            scopes: vec!["moira:admin".to_string()],
            ..base.clone()
        };

        assert_eq!(actor_fingerprint(&base), actor_fingerprint(&re_scoped));
    }
}
