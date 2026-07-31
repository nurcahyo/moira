//! Runtime auth-provider settings administration (plan 07, module 9).
//!
//! CONVENTIONS §7.2 is binding: auth-provider configuration is *runtime* configuration
//! owned by Moira's database, consistent with how providers, models, routing and
//! credentials already work. Plan 08's setup wizard **writes** this configuration and the
//! console **reads** it at boot, so the storage and the admin API have to exist in Moira
//! before 08 starts.
//!
//! # Decision D7 — Moira stores no OAuth client secret
//!
//! There is no secret to encrypt, rotate, rebind or read back anywhere in this file, and
//! the DTOs have no field for one. `issuer` and `client_id` are therefore ordinary mutable
//! configuration, freely patchable under the normal `If-Match` rules: the AAD that once
//! bound a stored secret to them is gone, and with it the `409` rebind-or-rotate rule.
//! The console's stale-secret concern is handled console-side, by comparing a fingerprint
//! of Moira's `client_id` against the one stored beside its own secret.
//!
//! Moira cannot and must not check whether a client secret exists before enabling a
//! method — it does not have one. Whether the console holds a usable secret is the
//! console's precondition, enforced by its own wizard.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandMutation, AdminCommandRunner, RequestContext,
        admin::shared::{
            PageRequest, admin_command_spec, audit_success, command_hasher, paginate,
            require_non_empty, success_audit,
        },
        setup::require_setup_actor,
    },
    domain::{
        AuthMethod, AuthProviderSettingsCreateRequest, AuthProviderSettingsPatchRequest,
        AuthProviderSettingsRecord, CursorScope, ListCursor, ListResponse, PublicAuthMethod,
        PublicSignInMethod, SetupAuthMethodsResponse, SetupSignInMethodsResponse,
    },
    error::AppError,
    infra::repositories::{
        AuthProviderSettingsRepository, PgAdminRepository, PgAuthProviderSettingsRepository,
    },
    security::Actor,
};

/// Its own scope, so a cursor issued for another list cannot be replayed against this one.
const AUTH_PROVIDERS_CURSOR: CursorScope = CursorScope::new("admin.auth_providers");

pub struct AuthProviderSettingsService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
    settings: Arc<dyn AuthProviderSettingsRepository>,
}

impl<'a> AuthProviderSettingsService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            repo: PgAdminRepository::new(pool.clone()),
            settings: Arc::new(PgAuthProviderSettingsRepository::new(pool)),
        })
    }

    pub async fn create(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: AuthProviderSettingsCreateRequest,
    ) -> Result<(AuthProviderSettingsRecord, bool), AppError> {
        self.state
            .authz
            .require(actor, "moira:auth-settings:write")?;
        require_non_empty("display_name", &request.display_name)?;
        validate_method_shape(
            request.method,
            request.issuer.as_deref(),
            request.discovery_url.as_deref(),
            request.authorization_url.as_deref(),
            request.token_url.as_deref(),
            request.jwks_url.as_deref(),
            request.client_id.as_deref(),
        )?;
        validate_urls(
            request.discovery_url.as_deref(),
            request.authorization_url.as_deref(),
            request.token_url.as_deref(),
            request.userinfo_url.as_deref(),
            request.jwks_url.as_deref(),
            request.redirect_uris.as_slice(),
        )?;
        validate_email_domains(&request.allowed_email_domains)?;
        // CONVENTIONS §7.5 (module 7d): a console issuer that maps a scopes claim lets its
        // own tokens self-assert scopes, which would defeat `admin_identities` as the sole
        // source of human authorization. Checked before the envelope, because it is a
        // property of a *different* row and a rejected request must not take the lock.
        self.reject_self_asserting_console_issuer(request.trusted_jwt_issuer_id)
            .await?;
        // Finding F23's two configure-time refusals. Same placement, same reason: both are
        // properties of *other* rows, and a request that cannot succeed must not enter the
        // envelope.
        self.reject_issuer_shadowing_a_trusted_issuer(
            request.issuer.as_deref(),
            request.trusted_jwt_issuer_id,
        )
        .await?;
        self.reject_duplicate_enabled_provider(
            request.trusted_jwt_issuer_id,
            request.enabled,
            None,
        )
        .await?;

        let spec = admin_command_spec(ctx, actor, "auth_provider.create", json!({}), &request)?;
        let settings = Arc::clone(&self.settings);
        let audit_actor = actor.clone();
        let audit_ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let record = settings
                        .create(transaction.connection(), Uuid::now_v7(), &request)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "auth_provider.create",
                            "auth_provider_settings",
                            Some(record.id.to_string()),
                            json!({
                                "method": record.method,
                                "issuer": record.issuer,
                                "enabled": record.enabled,
                            }),
                        ))
                        .await?;
                    let id = record.id.to_string();
                    AdminCommandMutation::new(record, 201, Some(id))
                })
            })
            .await?;
        self.invalidate_cache().await;
        Ok((outcome.response, outcome.replayed))
    }

    pub async fn list(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<AuthProviderSettingsRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:auth-settings:read")?;
        let page = page.into();
        let rows = self
            .settings
            .list(page.decode(AUTH_PROVIDERS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, AUTH_PROVIDERS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub async fn get(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<AuthProviderSettingsRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:auth-settings:read")?;
        self.settings.get(id).await
    }

    pub async fn patch(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: AuthProviderSettingsPatchRequest,
    ) -> Result<AuthProviderSettingsRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:auth-settings:write")?;
        if let Some(display_name) = &request.display_name {
            require_non_empty("display_name", display_name)?;
        }
        validate_urls(
            request.discovery_url.as_deref(),
            request.authorization_url.as_deref(),
            request.token_url.as_deref(),
            request.userinfo_url.as_deref(),
            request.jwks_url.as_deref(),
            request.redirect_uris.as_deref().unwrap_or_default(),
        )?;
        if let Some(domains) = &request.allowed_email_domains {
            validate_email_domains(domains)?;
        }
        self.reject_self_asserting_console_issuer(request.trusted_jwt_issuer_id)
            .await?;
        // `patch` `coalesce`s every absent field, so the two F23 guards have to run against
        // the row as it will BE, not against the fragment the caller sent. Enabling a row
        // that is already bound, or binding a row that is already enabled, are both the
        // duplicate condition and neither carries both values in its request body.
        //
        // The read is skipped entirely when the patch touches none of the three inputs, so
        // an ordinary `display_name` change still costs one statement and still surfaces a
        // stale `If-Match` as `resource_version_conflict` rather than as anything from here.
        if request.issuer.is_some()
            || request.trusted_jwt_issuer_id.is_some()
            || request.enabled == Some(true)
        {
            let current = self.settings.get(id).await?;
            let effective_issuer = request.issuer.as_deref().or(current.issuer.as_deref());
            let effective_binding = request
                .trusted_jwt_issuer_id
                .or(current.trusted_jwt_issuer_id);
            let effective_enabled = request.enabled.unwrap_or(current.enabled);
            self.reject_issuer_shadowing_a_trusted_issuer(effective_issuer, effective_binding)
                .await?;
            // `Some(id)`: the row is allowed to remain the enabled provider on its own
            // binding. Without the exclusion a no-op patch on an enabled provider would
            // refuse itself.
            self.reject_duplicate_enabled_provider(effective_binding, effective_enabled, Some(id))
                .await?;
        }
        // The version check happens inside the repository's transaction, on a row locked by
        // `select … for update` — never against a version read on a separate connection,
        // which is the lost update the `expected_version` parameter exists to close.
        let record = self.settings.patch(id, expected_version, &request).await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "auth_provider.update",
            "auth_provider_settings",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        self.invalidate_cache().await;
        Ok(record)
    }

    /// Enabling a method whose **non-secret** configuration is incomplete is refused.
    ///
    /// Deny-by-default holds on the way in too: `enabled` defaults to `false` on create, so
    /// a half-configured method can never be live by accident.
    ///
    /// **Note for a future reader:** as `0013` is written today, this re-check cannot
    /// actually fire. `auth_provider_settings_method_shape` is an unconditional CHECK, not
    /// an `enabled`-conditional one, so a stored row already satisfies the same rule and an
    /// incomplete row cannot exist to be enabled. The check is kept anyway, because it is
    /// what makes the *API's* behaviour independent of that constraint's exact wording — if
    /// a later migration ever relaxes the CHECK to allow drafts, this is already the gate
    /// that keeps a draft from going live, rather than a hole discovered afterwards.
    pub async fn set_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<AuthProviderSettingsRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:auth-settings:write")?;
        if enabled {
            let current = self.settings.get(id).await?;
            validate_method_shape(
                current.method,
                current.issuer.as_deref(),
                current.discovery_url.as_deref(),
                current.authorization_url.as_deref(),
                current.token_url.as_deref(),
                current.jwks_url.as_deref(),
                current.client_id.as_deref(),
            )?;
            // The path G4 exercises: A is already enabled on trusted issuer X, and this is
            // the request that would enable B on X too. Refused with a code here; refused
            // by `0020`'s index if two of these race.
            self.reject_duplicate_enabled_provider(current.trusted_jwt_issuer_id, true, Some(id))
                .await?;
        }
        let record = self
            .settings
            .set_enabled(id, expected_version, enabled)
            .await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            if enabled {
                "auth_provider.enable"
            } else {
                "auth_provider.disable"
            },
            "auth_provider_settings",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        self.invalidate_cache().await;
        Ok(record)
    }

    pub async fn delete(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:auth-settings:delete")?;
        self.settings.soft_delete(id, expected_version).await?;
        audit_success(
            &self.repo,
            actor,
            ctx,
            "auth_provider.delete",
            "auth_provider_settings",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        self.invalidate_cache().await;
        Ok(())
    }

    /// The bootstrap read behind `GET /api/v1/admin/setup/auth-methods`.
    ///
    /// # Authenticated, decided — the non-optional `Actor` is the enforcement
    ///
    /// Do not add an anonymous overload of *this* method or an `Option<Actor>` parameter.
    /// The response is identity *configuration* — which IdP, which issuer, which client id,
    /// which allowed domains — and that is reconnaissance-worthy even though D7 removed the
    /// secret. Plan 08's console calls it server-side with the system key it already holds;
    /// the browser never sees it.
    ///
    /// [`Self::public_sign_in_methods`] is a **strictly narrower** anonymous sibling, not a
    /// relaxation of this one: it returns a different, smaller type that carries no
    /// `allowed_email_domains`. Widening it back towards this response would reintroduce
    /// exactly the leak D4 refused.
    pub async fn setup_auth_methods(
        &self,
        actor: &Actor,
    ) -> Result<SetupAuthMethodsResponse, AppError> {
        require_setup_actor(actor)?;
        self.state.authz.require(actor, "moira:setup:read")?;
        // The gate above runs *first* and unconditionally: a cache hit must never be a way
        // to skip authorization.
        Ok(SetupAuthMethodsResponse {
            methods: self.enabled_methods().await?,
        })
    }

    /// The **anonymous** read behind `GET /api/v1/admin/setup/sign-in-methods` (finding F15).
    ///
    /// # Why this takes no `Actor`, and why that is not a hole
    ///
    /// A login screen cannot present a credential — the credential is what signing in
    /// produces. Plan 09's public `/invite/[token]` page has to render sign-in buttons for a
    /// visitor who is unauthenticated by construction, and an operator who removes
    /// `MOIRA_SYSTEM_KEY` after setup has no way back in otherwise. The circularity is the
    /// bug; requiring an actor here is what causes it.
    ///
    /// What makes it safe is the *response*, not the caller: [`PublicSignInMethod`] carries
    /// only fields the browser already transmits or receives during the sign-in it is about
    /// to start. It deliberately omits `allowed_email_domains` — the deny-by-default
    /// admin-claim policy — which is the field that made the wider projection unsafe to
    /// publish. Read that type's documentation before adding a field here.
    pub async fn public_sign_in_methods(&self) -> Result<SetupSignInMethodsResponse, AppError> {
        Ok(SetupSignInMethodsResponse {
            methods: self
                .enabled_methods()
                .await?
                .iter()
                .filter_map(PublicSignInMethod::from_enabled_method)
                .collect(),
        })
    }

    /// Read-through the process-local cache (module 13a), shared by both setup reads so the
    /// two cannot drift onto different snapshots of the same rows.
    ///
    /// It performs **no** authorization: every caller is responsible for its own gate, which
    /// must run before this is called.
    async fn enabled_methods(&self) -> Result<Vec<PublicAuthMethod>, AppError> {
        if let Some(methods) = self.state.auth_settings_cache.enabled_methods().await {
            return Ok(methods);
        }
        let methods = self.settings.list_enabled_public().await?;
        self.state
            .auth_settings_cache
            .put_enabled_methods(methods.clone())
            .await;
        Ok(methods)
    }

    /// Drops this instance's cached copy after a write.
    ///
    /// Cross-instance invalidation is the NOTIFY trigger's job — `auth_provider_settings`
    /// fires `notify_moira_runtime_config_change()`, and `listen_once` clears this cache on
    /// every instance that hears it. This local drop closes the one gap that leaves: the
    /// writing process's own listener is asynchronous, so without it a read issued on the
    /// same instance immediately after the write could still be served the pre-write list.
    /// It mirrors `schedule_runtime_cache_invalidation`'s intent, but is awaited rather
    /// than spawned, because a write handler is already in an async context and the
    /// invalidation must be visible before it returns.
    async fn invalidate_cache(&self) {
        self.state.auth_settings_cache.invalidate_all().await;
    }

    async fn reject_self_asserting_console_issuer(
        &self,
        trusted_jwt_issuer_id: Option<Uuid>,
    ) -> Result<(), AppError> {
        let Some(issuer_id) = trusted_jwt_issuer_id else {
            return Ok(());
        };
        if self
            .settings
            .trusted_issuer_scopes_claim(issuer_id)
            .await?
            .is_some()
        {
            return Err(AppError::coded(
                StatusCode::BAD_REQUEST,
                "console_issuer_must_not_assert_scopes",
                "a console issuer must not map a scopes claim",
            ));
        }
        Ok(())
    }

    /// Refuse a second enabled provider on a trusted issuer that already has one.
    ///
    /// # Why this exists when `migrations/0020` already has an index
    ///
    /// The index is the invariant and it is what makes
    /// [`AuthProviderSettingsRepository::admission_policy`]'s first stage single-valued.
    /// This is the *coded* refusal on the common path: without it the operator's request
    /// travels through a unique violation, and a unique violation that nothing maps by name
    /// renders as `500 database_error`. That is finding F13 verbatim, one table over, and
    /// it is why the guard test asserts on the code string rather than on the status.
    ///
    /// The two are not redundant. This check and the write are not atomic, so two
    /// concurrent enables can both pass here and race into the index; that request gets the
    /// same code through `map_constraint_violation`. Removing either one leaves a reachable
    /// path to the wrong answer.
    ///
    /// Runs **before** the transactional envelope, following the rule stated in
    /// `src/application/identity.rs`: a request that was never going to succeed must not
    /// take the advisory lock or write an idempotency record.
    async fn reject_duplicate_enabled_provider(
        &self,
        trusted_jwt_issuer_id: Option<Uuid>,
        enabled: bool,
        exclude_row: Option<Uuid>,
    ) -> Result<(), AppError> {
        if !enabled {
            return Ok(());
        }
        let Some(issuer_id) = trusted_jwt_issuer_id else {
            // Unbound rows do not compete: `0020`'s index treats NULLs as distinct, on
            // purpose — several `jwks` rows and several not-yet-bound providers are all
            // legitimate simultaneously.
            return Ok(());
        };
        let existing = self
            .settings
            .enabled_providers_on_trusted_issuer(issuer_id, exclude_row)
            .await?;
        if existing.is_empty() {
            return Ok(());
        }
        Err(AppError::conflict(
            "duplicate_enabled_provider_for_issuer",
            "more than one enabled auth provider is bound to this trusted JWT issuer",
        ))
    }

    /// Refuse a provider row whose own `issuer` string belongs to a registered trusted
    /// issuer it is not bound to.
    ///
    /// # The shape this closes (F23 shape (b))
    ///
    /// `admission_policy`'s second stage matches `issuer = $1 and trusted_jwt_issuer_id is
    /// null` — CONVENTIONS §7.3 mode 3, where the caller genuinely is the IdP. A row that
    /// carries a *console-owned* issuer string in that column is claiming to be the issuer
    /// the console mints under, which it is not. Before the two-stage lookup such a row
    /// outranked the correctly-bound provider at any age and applied its own
    /// `allowed_email_domains` — with an empty list, a 403 on every claim and every
    /// redemption for that provider, and no error naming the cause.
    ///
    /// The two-stage lookup already makes that unreachable (stage 2 runs only when stage 1
    /// matched nothing). This guard refuses to *store* the collision as well, because the
    /// shape is confusing to read and because stage 2 is genuinely reachable on a
    /// deployment with no bound provider — where the rogue row would then govern.
    ///
    /// A row bound to the very issuer whose string it carries is exempt: that is mode 3
    /// configured explicitly, and it is the one arrangement in which the two values
    /// *should* agree.
    async fn reject_issuer_shadowing_a_trusted_issuer(
        &self,
        issuer: Option<&str>,
        trusted_jwt_issuer_id: Option<Uuid>,
    ) -> Result<(), AppError> {
        let Some(issuer) = issuer else {
            return Ok(());
        };
        let colliding = self
            .settings
            .trusted_issuer_ids_for_issuer(issuer, trusted_jwt_issuer_id)
            .await?;
        if colliding.is_empty() {
            return Ok(());
        }
        Err(AppError::conflict(
            "auth_provider_issuer_shadows_trusted_issuer",
            "this issuer is already registered as a trusted JWT issuer that the provider is \
             not bound to",
        ))
    }
}

/// The method's required **non-secret** configuration.
///
/// Mirrors `auth_provider_settings_method_shape` in `0013`, so the API refuses a bad shape
/// with an actionable `400` instead of letting the CHECK constraint surface as a database
/// error. The constraint remains the backstop.
///
/// It deliberately does not — and cannot — check for an OAuth client secret: under D7 Moira
/// does not store one.
#[allow(clippy::too_many_arguments)]
fn validate_method_shape(
    method: AuthMethod,
    issuer: Option<&str>,
    discovery_url: Option<&str>,
    authorization_url: Option<&str>,
    token_url: Option<&str>,
    jwks_url: Option<&str>,
    client_id: Option<&str>,
) -> Result<(), AppError> {
    let complete = match method {
        AuthMethod::Jwks => jwks_url.is_some() && client_id.is_none(),
        AuthMethod::GoogleOauth | AuthMethod::GenericOidc => {
            client_id.is_some() && (issuer.is_some() || discovery_url.is_some())
        }
        // GitHub OAuth is not OIDC: no discovery document, no `id_token`, no issuer. Its
        // endpoints are fixed and must be supplied directly. `issuer` and `discovery_url`
        // are required to be **absent** rather than merely optional, mirroring
        // `auth_provider_settings_method_shape` in `migrations/0020` — a GitHub row
        // carrying an `issuer` would enter `admission_policy`'s mode-3 branch, which is for
        // deployments where the caller IS the IdP, under a value nothing verifies against.
        AuthMethod::GithubOauth => {
            client_id.is_some()
                && authorization_url.is_some()
                && token_url.is_some()
                && issuer.is_none()
                && discovery_url.is_none()
        }
    };
    if complete {
        Ok(())
    } else {
        Err(AppError::coded(
            StatusCode::BAD_REQUEST,
            "auth_provider_method_config_incomplete",
            "the auth provider configuration is incomplete for this method",
        ))
    }
}

fn validate_urls(
    discovery_url: Option<&str>,
    authorization_url: Option<&str>,
    token_url: Option<&str>,
    userinfo_url: Option<&str>,
    jwks_url: Option<&str>,
    redirect_uris: &[String],
) -> Result<(), AppError> {
    for url in [
        discovery_url,
        authorization_url,
        token_url,
        userinfo_url,
        jwks_url,
    ]
    .into_iter()
    .flatten()
    {
        validate_https_url(url)?;
    }
    for uri in redirect_uris {
        validate_https_url(uri)?;
    }
    Ok(())
}

/// A syntactic gate, and only a syntactic gate.
///
/// `discovery_url` and `jwks_url` are operator-supplied URLs Moira may one day fetch. This
/// plan does **not** fetch them: it stores them after checking scheme and host. When a fetch
/// is added it must go through the SSRF-hardened client (`crate::security::ssrf`), never a
/// second hand-rolled one — a scheme-and-host check is not a substitute, because
/// `https://169.254.169.254/` passes it.
fn validate_https_url(url: &str) -> Result<(), AppError> {
    let rejected = || {
        AppError::coded(
            StatusCode::BAD_REQUEST,
            "auth_provider_url_not_allowed",
            format!("{url} is not an absolute https URL"),
        )
    };
    let parsed = url::Url::parse(url).map_err(|_| rejected())?;
    if parsed.scheme() != "https" || parsed.host_str().is_none_or(str::is_empty) {
        return Err(rejected());
    }
    Ok(())
}

/// Entries must look like domains, because they are compared against the domain half of a
/// claimed email address. A stray `@user@example.com` or an empty string in the allow-list
/// would silently never match, which reads to an operator as "the allow-list is broken"
/// long after the write succeeded.
fn validate_email_domains(domains: &[String]) -> Result<(), AppError> {
    for domain in domains {
        let value = domain.trim();
        let plausible = !value.is_empty()
            && value.contains('.')
            && !value.starts_with('.')
            && !value.ends_with('.')
            && !value.contains("..")
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-')
            });
        if !plausible {
            return Err(AppError::coded(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("{domain:?} is not a valid email domain"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::PublicAuthMethod;

    fn assert_coded(error: &AppError, status: StatusCode, code: &str) {
        assert_eq!(error.status(), status, "unexpected status for {error}");
        assert!(
            error.to_string().starts_with(&format!("{code}:")),
            "expected code {code}, got: {error}"
        );
    }

    /// Named arguments by construction: `validate_method_shape` takes seven `Option<&str>`
    /// in a row, and a positional call in a test is one transposition away from asserting
    /// something else entirely while still compiling.
    #[derive(Default, Clone, Copy)]
    struct Shape {
        issuer: Option<&'static str>,
        discovery_url: Option<&'static str>,
        authorization_url: Option<&'static str>,
        token_url: Option<&'static str>,
        jwks_url: Option<&'static str>,
        client_id: Option<&'static str>,
    }

    fn check(method: AuthMethod, shape: Shape) -> Result<(), AppError> {
        validate_method_shape(
            method,
            shape.issuer,
            shape.discovery_url,
            shape.authorization_url,
            shape.token_url,
            shape.jwks_url,
            shape.client_id,
        )
    }

    #[test]
    fn jwks_needs_a_jwks_url_and_refuses_a_client_id() {
        check(
            AuthMethod::Jwks,
            Shape {
                jwks_url: Some("https://idp/jwks"),
                ..Shape::default()
            },
        )
        .expect("a jwks method with a jwks url is complete");
        assert_coded(
            &check(AuthMethod::Jwks, Shape::default()).unwrap_err(),
            StatusCode::BAD_REQUEST,
            "auth_provider_method_config_incomplete",
        );
        assert_coded(
            &check(
                AuthMethod::Jwks,
                Shape {
                    jwks_url: Some("https://idp/jwks"),
                    client_id: Some("cid"),
                    ..Shape::default()
                },
            )
            .unwrap_err(),
            StatusCode::BAD_REQUEST,
            "auth_provider_method_config_incomplete",
        );
    }

    #[test]
    fn oauth_methods_need_a_client_id_and_an_issuer_or_discovery_url() {
        for method in [AuthMethod::GoogleOauth, AuthMethod::GenericOidc] {
            check(
                method,
                Shape {
                    issuer: Some("https://idp"),
                    client_id: Some("cid"),
                    ..Shape::default()
                },
            )
            .expect("issuer plus client id is complete");
            check(
                method,
                Shape {
                    discovery_url: Some("https://idp/.well-known/openid-configuration"),
                    client_id: Some("cid"),
                    ..Shape::default()
                },
            )
            .expect("discovery url plus client id is complete");
            assert_coded(
                &check(
                    method,
                    Shape {
                        issuer: Some("https://idp"),
                        ..Shape::default()
                    },
                )
                .unwrap_err(),
                StatusCode::BAD_REQUEST,
                "auth_provider_method_config_incomplete",
            );
            assert_coded(
                &check(
                    method,
                    Shape {
                        client_id: Some("cid"),
                        ..Shape::default()
                    },
                )
                .unwrap_err(),
                StatusCode::BAD_REQUEST,
                "auth_provider_method_config_incomplete",
            );
        }
    }

    /// GitHub's shape, and the four ways it is *not* the OIDC shape.
    ///
    /// Mirrors `auth_provider_settings_method_shape` in `migrations/0020` field for field.
    /// If this validator and that CHECK ever disagree, the API returns `400` where the
    /// database returns `400` for a *different* reason — or worse, accepts a row the CHECK
    /// then refuses as an opaque constraint violation.
    #[test]
    fn github_needs_its_endpoints_and_refuses_an_issuer_or_a_discovery_url() {
        let complete = Shape {
            authorization_url: Some("https://github.com/login/oauth/authorize"),
            token_url: Some("https://github.com/login/oauth/access_token"),
            client_id: Some("cid"),
            ..Shape::default()
        };
        check(AuthMethod::GithubOauth, complete).expect("client id + both endpoints is complete");

        for (label, shape) in [
            (
                "no authorization_url",
                Shape {
                    authorization_url: None,
                    ..complete
                },
            ),
            (
                "no token_url",
                Shape {
                    token_url: None,
                    ..complete
                },
            ),
            (
                "no client_id",
                Shape {
                    client_id: None,
                    ..complete
                },
            ),
            (
                "an issuer GitHub does not have",
                Shape {
                    issuer: Some("https://github.com"),
                    ..complete
                },
            ),
            (
                "a discovery document GitHub does not publish",
                Shape {
                    discovery_url: Some("https://github.com/.well-known/openid-configuration"),
                    ..complete
                },
            ),
        ] {
            let error = check(AuthMethod::GithubOauth, shape)
                .expect_err(&format!("github with {label} must be refused"));
            assert_coded(
                &error,
                StatusCode::BAD_REQUEST,
                "auth_provider_method_config_incomplete",
            );
        }

        // The OIDC shape is not accepted under `github_oauth`, and GitHub's is not accepted
        // under the OIDC methods. Asserted in both directions because W4-D1's whole argument
        // for a fourth variant is that the shapes are genuinely different.
        assert!(
            check(
                AuthMethod::GenericOidc,
                Shape {
                    authorization_url: Some("https://github.com/login/oauth/authorize"),
                    token_url: Some("https://github.com/login/oauth/access_token"),
                    client_id: Some("cid"),
                    ..Shape::default()
                }
            )
            .is_err(),
            "generic_oidc still needs an issuer or a discovery url"
        );
    }

    /// The completeness check must never grow a secret condition: D7 means Moira has no
    /// client secret to look for, and adding one would make enabling depend on a value that
    /// lives in another system entirely.
    #[test]
    fn completeness_is_decided_without_any_secret_material() {
        check(
            AuthMethod::GoogleOauth,
            Shape {
                issuer: Some("https://accounts.google.com"),
                client_id: Some("cid"),
                ..Shape::default()
            },
        )
        .expect("no secret participates in this decision");
    }

    #[test]
    fn only_absolute_https_urls_are_stored() {
        validate_https_url("https://idp.example/.well-known/openid-configuration")
            .expect("https is accepted");
        for rejected in [
            "http://idp.example/jwks",
            "file:///etc/passwd",
            "idp.example/jwks",
            "",
            "https://",
        ] {
            assert_coded(
                &validate_https_url(rejected).unwrap_err(),
                StatusCode::BAD_REQUEST,
                "auth_provider_url_not_allowed",
            );
        }
    }

    #[test]
    fn allowed_email_domains_must_look_like_domains() {
        validate_email_domains(&["example.com".to_string(), "corp.example.co".to_string()])
            .expect("plain domains are accepted");
        for rejected in ["", "   ", "example", "@example.com", ".example.com", "a..b"] {
            assert_coded(
                &validate_email_domains(&[rejected.to_string()]).unwrap_err(),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            );
        }
    }

    /// The cursor scope must be this list's own, or a cursor issued for another admin list
    /// would decode here and page through the wrong table.
    #[test]
    fn the_cursor_scope_is_specific_to_this_list() {
        assert_eq!(AUTH_PROVIDERS_CURSOR.label(), "admin.auth_providers");
    }

    /// D7, asserted on the type rather than on prose: the bootstrap projection is defined
    /// by the fields it lists, and none of them is secret-shaped.
    #[test]
    fn the_bootstrap_projection_carries_no_secret_shaped_field() {
        let method = PublicAuthMethod {
            id: Uuid::nil(),
            method: AuthMethod::GoogleOauth,
            display_name: "Google".to_string(),
            issuer: Some("https://accounts.google.com".to_string()),
            discovery_url: None,
            authorization_url: None,
            jwks_url: None,
            client_id: Some("cid".to_string()),
            requested_scopes: vec!["openid".to_string()],
            allowed_email_domains: vec!["example.com".to_string()],
        };
        let json = serde_json::to_value(&method).expect("projection serializes");
        for key in json.as_object().expect("an object").keys() {
            assert!(
                !key.contains("secret") && !key.contains("encrypted") && !key.contains("nonce"),
                "the bootstrap projection gained a secret-shaped field: {key}"
            );
        }
    }
}
