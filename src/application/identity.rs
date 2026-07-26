//! Admin identity claiming (plan 07, modules 8 and 10).
//!
//! Grants a *human* admin authority by binding `moira:admin` to a stable `(issuer,
//! subject)` pair from an already-registered trusted JWT issuer. Moira issues no password,
//! no session and no login page: after a grant exists, that human's existing trusted-JWT
//! bearer token simply carries more authority on the **admin plane**.
//!
//! # Decision D1 — one credential path
//!
//! The one-time setup-token path is deferred, so [`ClaimCredential`] has a single variant
//! and `ClaimAdminIdentityRequest::setup_token` is **refused, never ignored**. The enum is
//! kept rather than collapsed to a bare `Actor` because it is the shape the reversal of D1
//! would extend, and because it names what the parameter *means* at every call site.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandMutation, AdminCommandRunner, RequestContext,
        admin::shared::{admin_command_spec, command_hasher, require_non_empty, success_audit},
    },
    domain::{
        AdminIdentityRecord, ClaimAdminIdentityRequest, ResponseText, SetupClaimStatusResponse,
    },
    error::AppError,
    infra::repositories::{
        AdminIdentityGrant, AdminIdentityGrantInsert, AdminIdentityRepository,
        AuthProviderSettingsRepository, GoverningAuthPolicy, PgAdminIdentityRepository,
        PgAdminRepository, PgAuthProviderSettingsRepository,
    },
    security::{Actor, ActorType, AuthorizationService},
};

const ADMIN_IDENTITY_CLAIMED_NOTICE: &str = "moira.notice.admin_identity_claimed";

/// The credential a claim was submitted under.
///
/// The endpoint accepts exactly one shape and refuses everything else — **including a bare
/// trusted-JWT bearer token that verifies perfectly**. That refusal is the structural
/// rejection of "the first successful admin JWT wins" (`plans/01` §4.4): if a verified JWT
/// could claim, then whoever reached a fresh deployment first would own it.
#[derive(Debug, Clone)]
pub enum ClaimCredential {
    /// An `X-Moira-System-Key` holder. The actor is carried so the audit row and the
    /// command envelope's fingerprint name the real credential.
    SystemKey(Actor),
}

pub struct AdminIdentityService<'a> {
    state: &'a AppState,
    /// The command runner is built on the concrete admin repository, because the
    /// idempotency ledger, the advisory lock and the savepoint live there.
    repo: PgAdminRepository,
    identities: Arc<dyn AdminIdentityRepository>,
    auth_settings: Arc<dyn AuthProviderSettingsRepository>,
}

impl<'a> AdminIdentityService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            repo: PgAdminRepository::new(pool.clone()),
            identities: Arc::new(PgAdminIdentityRepository::new(pool.clone())),
            auth_settings: Arc::new(PgAuthProviderSettingsRepository::new(pool)),
        })
    }

    /// Whether an admin identity has ever been claimed.
    ///
    /// # This method takes no `Actor` and performs no authorization check, deliberately
    ///
    /// It is the one call in Moira's admin surface with no actor, and that is not an
    /// oversight for a later reviewer to "fix": an unauthenticated setup wizard must be
    /// able to ask "do I need to show the claim flow?" *before* any human has a credential
    /// to present. The entire response is one boolean — no count, no timestamp, no issuer,
    /// no subject — which an attacker could infer anyway from the fact that the instance is
    /// freshly deployed. Anything richer would be reconnaissance on the surface an attacker
    /// would target during the setup window.
    pub async fn claim_status(&self) -> Result<SetupClaimStatusResponse, AppError> {
        Ok(SetupClaimStatusResponse {
            claimed: self.identities.setup_claimed().await?,
        })
    }

    /// Grants admin scope to `(issuer, subject)`.
    ///
    /// Returns the record and whether it came from an `Idempotency-Key` replay, so the
    /// handler can map replay → 200 and fresh → 201. The `notice` carries the same key in
    /// both cases: the **status code**, not the notice, distinguishes them, and a second
    /// "replayed" notice key could never be emitted because a replay returns the stored
    /// body verbatim.
    ///
    /// Every validation below runs **before** the transactional envelope, so a
    /// policy-rejected request never takes the advisory lock and never writes an
    /// idempotency record for a request that was never going to succeed.
    pub async fn claim(
        &self,
        ctx: &RequestContext,
        credential: ClaimCredential,
        request: ClaimAdminIdentityRequest,
    ) -> Result<(AdminIdentityRecord, bool), AppError> {
        // Step 0 — the deferred credential path is refused, not ignored (D1). Accepting and
        // discarding the field would let a caller believe they had presented a credential
        // Moira honoured, when Moira had in fact authenticated them by some other means
        // entirely — the one failure mode worse than not supporting the field at all.
        if request.setup_token.is_some() {
            return Err(AppError::coded(
                StatusCode::BAD_REQUEST,
                "setup_token_not_supported",
                "the one-time setup token path is not available on this deployment",
            ));
        }

        let ClaimCredential::SystemKey(actor) = credential;
        // Belt and braces with the handler's `verify_system_key_only`: a dev-trust-header
        // or trusted-JWT actor that somehow reached this method is refused here too, so the
        // no-first-login-wins property does not depend on one call site staying correct.
        if actor.actor_type != ActorType::SystemKey {
            return Err(AppError::Unauthorized(
                "claiming an admin identity requires a system key".to_string(),
            ));
        }
        self.state.authz.require(&actor, "moira:admin")?;

        require_non_empty("issuer", &request.issuer)?;
        require_non_empty("subject", &request.subject)?;
        // Reuses the existing `scope_invalid` code rather than minting a key for a
        // condition the catalog already covers.
        let granted_scopes = AuthorizationService::normalize_scopes(&request.scopes)?;

        let trusted_jwt_issuer_id = self
            .identities
            .resolve_active_issuer(&request.issuer)
            .await?;

        // Module 10. Runs on every credential path, and can deny a system-key claim.
        let policy = self
            .auth_settings
            .governing_policy(&request.issuer, trusted_jwt_issuer_id)
            .await?;
        evaluate_claim_policy(&request.email, request.email_verified, policy.as_ref())?;

        let spec = admin_command_spec(
            ctx,
            &actor,
            "admin_identity.claim",
            json!({ "issuer": request.issuer, "subject": request.subject }),
            &request,
        )?;

        let insert = AdminIdentityGrantInsert {
            id: Uuid::now_v7(),
            trusted_jwt_issuer_id,
            issuer: request.issuer.clone(),
            subject: request.subject.clone(),
            email: request.email.trim().to_string(),
            email_verified: request.email_verified,
            granted_scopes,
            // Recorded honestly. Under D1 this is always `'system_key'`; a token-path claim
            // must never be able to describe itself as one.
            granted_by_actor_type: "system_key".to_string(),
            granted_by_subject: actor.subject.clone(),
        };

        let identities = Arc::clone(&self.identities);
        let audit_actor = actor.clone();
        let audit_ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let grant = identities
                        .insert_grant(transaction.connection(), &insert)
                        .await?;
                    identities
                        .mark_setup_claimed(transaction.connection(), grant.id)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "admin_identity.claim",
                            "admin_identity",
                            Some(grant.id.to_string()),
                            json!({
                                "issuer": grant.issuer,
                                "subject": grant.subject,
                                "email": grant.email,
                                "granted_scopes": grant.granted_scopes,
                                "granted_by_actor_type": insert.granted_by_actor_type,
                            }),
                        ))
                        .await?;
                    let id = grant.id.to_string();
                    AdminCommandMutation::new(record_from_grant(grant), 201, Some(id))
                })
            })
            .await?;

        Ok((outcome.response, outcome.replayed))
    }
}

/// The deny-by-default verified-email and allowed-domain policy (plan 07, module 10).
///
/// # It takes no credential, and that is the enforcement
///
/// There is **no first-claim exemption and no bootstrap bypass.** A
/// `matches!(credential, ClaimCredential::SystemKey(_))` short-circuit "so bootstrap works"
/// is exactly the bypass this module rules out, and the way to make it un-writable is to
/// keep the credential out of scope entirely: this function cannot branch on something it
/// cannot see.
///
/// A bypass would exist precisely during the setup window — when the deployment is least
/// defended and most attractive — and from the outside it would be indistinguishable from
/// the "first-login-wins" land-grab this whole plan exists to make structurally impossible.
/// A patch reintroducing one is a security regression to reject in review.
///
/// The designed setup order is: bootstrap the system key, register the trusted JWT issuer,
/// create **and enable** an `auth_provider_settings` row carrying `allowed_email_domains`,
/// and only then claim. A fresh deployment's first `403` is that order asserting itself,
/// not a defect.
pub(crate) fn evaluate_claim_policy(
    email: &str,
    email_verified: bool,
    policy: Option<&GoverningAuthPolicy>,
) -> Result<(), AppError> {
    // 1. No enabled configuration governs the target issuer. This is a *stricter* case of
    //    "no allowed domains" and resolves the same way — and it shares the code
    //    deliberately, because distinguishing them on the wire would tell an unprivileged
    //    caller whether a policy exists, and both have the same operator remedy.
    let Some(policy) = policy else {
        return Err(domain_not_allowed());
    };

    // 2. Verified email. Hard requirement, not configurable, every path.
    if !email_verified {
        return Err(AppError::coded(
            StatusCode::FORBIDDEN,
            "admin_claim_email_not_verified",
            "the email address for this identity is not verified",
        ));
    }

    // 3. What the type system cannot catch: the DTO makes `email` required, so an omitted
    //    field never reaches here, but a present-but-blank value or one with no extractable
    //    domain still can.
    let domain = email_domain(email).ok_or_else(|| {
        AppError::coded(
            StatusCode::BAD_REQUEST,
            "admin_claim_email_required",
            "a usable email address is required to claim an admin identity",
        )
    })?;

    // 4. Deny by default. An empty array means deny all: there is no "empty means
    //    unrestricted" reading, and every claim must match an explicit entry.
    let allowed = policy
        .allowed_email_domains
        .iter()
        .any(|candidate| candidate.trim().to_ascii_lowercase() == domain);
    if allowed {
        Ok(())
    } else {
        Err(domain_not_allowed())
    }
}

fn domain_not_allowed() -> AppError {
    AppError::coded(
        StatusCode::FORBIDDEN,
        "admin_claim_domain_not_allowed",
        "this email domain is not allowed to claim an admin identity",
    )
}

/// The lower-cased substring after the **last** `@`.
///
/// Matching is exact: `example.com` does **not** admit `sub.example.com`. Wildcard and
/// subdomain matching are a deferred follow-up, because supporting them silently would be a
/// policy hole — an operator who wrote one domain would be granting a subtree.
fn email_domain(email: &str) -> Option<String> {
    let email = email.trim();
    let (local, domain) = email.rsplit_once('@')?;
    if local.trim().is_empty() || domain.trim().is_empty() {
        return None;
    }
    Some(domain.trim().to_ascii_lowercase())
}

fn record_from_grant(grant: AdminIdentityGrant) -> AdminIdentityRecord {
    AdminIdentityRecord {
        id: grant.id,
        issuer: grant.issuer,
        subject: grant.subject,
        // The column is nullable so a future anonymisation path can clear it; no grant this
        // service writes can reach here without one, because the policy above refuses a
        // claim with no extractable domain.
        email: grant.email.unwrap_or_default(),
        email_verified: grant.email_verified,
        granted_scopes: grant.granted_scopes,
        status: grant.status,
        created_at: grant.created_at,
        version: grant.version,
        notice: claimed_notice(),
    }
}

fn claimed_notice() -> ResponseText {
    ResponseText::new(
        ADMIN_IDENTITY_CLAIMED_NOTICE,
        crate::i18n::default_message_for_key(ADMIN_IDENTITY_CLAIMED_NOTICE)
            .unwrap_or("Admin access has been granted to this identity."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(domains: &[&str]) -> GoverningAuthPolicy {
        GoverningAuthPolicy {
            id: Uuid::nil(),
            allowed_email_domains: domains.iter().map(|value| (*value).to_string()).collect(),
        }
    }

    fn assert_coded(error: &AppError, status: StatusCode, code: &str) {
        assert_eq!(error.status(), status, "unexpected status for {error}");
        assert!(
            error.to_string().starts_with(&format!("{code}:")),
            "expected code {code}, got: {error}"
        );
    }

    /// The named guard from the plan's Verification section. An unconfigured allow-list
    /// denies, and it denies for the caller who holds the most authority Moira recognises —
    /// there is no bootstrap carve-out.
    #[test]
    fn claim_is_denied_by_default_when_no_domain_allow_list_is_configured() {
        let error = evaluate_claim_policy("owner@example.com", true, Some(&policy(&[])))
            .expect_err("an empty allow-list denies every claim");
        assert_coded(
            &error,
            StatusCode::FORBIDDEN,
            "admin_claim_domain_not_allowed",
        );
    }

    #[test]
    fn claim_is_denied_when_no_enabled_configuration_governs_the_issuer() {
        let error = evaluate_claim_policy("owner@example.com", true, None)
            .expect_err("no governing configuration denies");
        assert_coded(
            &error,
            StatusCode::FORBIDDEN,
            "admin_claim_domain_not_allowed",
        );
    }

    /// The signature is the enforcement: the policy cannot branch on a credential it never
    /// receives, so the "system keys skip the domain check" bypass is not expressible here.
    /// The only way to reintroduce it is to change this function's parameters, which is a
    /// visible, reviewable diff rather than a one-line `matches!`.
    #[test]
    fn the_policy_reaches_the_same_verdict_for_every_caller() {
        for allow_list in [vec![], vec!["other.example"]] {
            let error =
                evaluate_claim_policy("owner@example.com", true, Some(&policy(&allow_list)))
                    .expect_err("a non-matching domain is denied");
            assert_coded(
                &error,
                StatusCode::FORBIDDEN,
                "admin_claim_domain_not_allowed",
            );
        }
    }

    #[test]
    fn an_unverified_email_is_refused_before_the_domain_is_examined() {
        let error =
            evaluate_claim_policy("owner@example.com", false, Some(&policy(&["example.com"])))
                .expect_err("an unverified email is refused");
        assert_coded(
            &error,
            StatusCode::FORBIDDEN,
            "admin_claim_email_not_verified",
        );
    }

    #[test]
    fn a_blank_or_domainless_email_is_a_required_error_not_a_domain_error() {
        for email in ["", "   ", "owner", "@example.com", "owner@", "owner@   "] {
            let error = evaluate_claim_policy(email, true, Some(&policy(&["example.com"])))
                .expect_err("a blank or domainless email must be refused");
            assert_coded(
                &error,
                StatusCode::BAD_REQUEST,
                "admin_claim_email_required",
            );
        }
    }

    #[test]
    fn an_allowed_domain_matches_case_insensitively_on_the_last_at_sign() {
        evaluate_claim_policy("Owner@EXAMPLE.com", true, Some(&policy(&["example.com"])))
            .expect("case is not part of a domain's identity");
        evaluate_claim_policy(
            "\"weird@local\"@example.com",
            true,
            Some(&policy(&["  Example.COM "])),
        )
        .expect("the domain is what follows the last @, and entries are trimmed");
    }

    /// Exact match only. Admitting `sub.example.com` because `example.com` is allowed would
    /// hand the operator a subtree they never wrote down.
    #[test]
    fn a_subdomain_is_not_admitted_by_its_parent() {
        let error = evaluate_claim_policy(
            "owner@sub.example.com",
            true,
            Some(&policy(&["example.com"])),
        )
        .expect_err("subdomain matching is not supported");
        assert_coded(
            &error,
            StatusCode::FORBIDDEN,
            "admin_claim_domain_not_allowed",
        );
    }

    // -----------------------------------------------------------------------
    // Database-backed: the whole `claim` path, not just the policy function.
    //
    // The pure tests above prove the policy denies; these prove nothing *around* it
    // exempts a caller from it. That distinction is the point — a bootstrap bypass would
    // live in `claim`, not in `evaluate_claim_policy`.
    // -----------------------------------------------------------------------

    fn system_key_actor() -> Actor {
        Actor {
            actor_type: ActorType::SystemKey,
            subject: Some("bootstrap-operator".to_string()),
            scopes: vec!["moira:admin".to_string()],
            ..Actor::default()
        }
    }

    fn claim_request(issuer: &str, subject: &str, email: &str) -> ClaimAdminIdentityRequest {
        ClaimAdminIdentityRequest {
            issuer: issuer.to_string(),
            subject: subject.to_string(),
            email: email.to_string(),
            email_verified: true,
            scopes: vec!["moira:admin".to_string()],
            setup_token: None,
        }
    }

    fn context() -> RequestContext {
        RequestContext {
            request_id: format!("identity-test-{}", Uuid::now_v7()),
            source_ip: None,
            user_agent: Some("moira-identity-test".to_string()),
            idempotency_key: None,
        }
    }

    /// Mutual exclusion for the `setup_state` singleton, keyed so that every process
    /// touching the shared test database agrees on it.
    ///
    /// PostgreSQL advisory locks are *database*-scoped, which is exactly the scope of the
    /// problem: `setup_state` is one row per database, and `MOIRA_TEST_DATABASE_URL` is a
    /// database several test threads — and, under `cargo test --workspace`, several test
    /// *binaries* — share. A `Mutex` in this module would only serialise the threads of
    /// one binary, so the lock lives in the database instead.
    ///
    /// The integration suites take the other established route (`tests/support/mod.rs`
    /// clones a private database per fixture) and therefore never contend for this key.
    /// That harness is an integration-test module, not part of the library crate, so it is
    /// unreachable from a `#[cfg(test)]` module in `src/`; reproducing its
    /// `CREATE DATABASE … TEMPLATE`, leak-sweeping machinery inside the shipped crate to
    /// isolate six tests would be far more code than this lock, for the same guarantee.
    const SETUP_STATE_LOCK_KEY: i64 = i64::from_be_bytes(*b"moirastp");

    /// Exclusive access to the `setup_state` singleton for the lifetime of one test.
    ///
    /// # Why the connection is not drawn from the pool
    ///
    /// `pg_advisory_lock` takes a *session*-level lock, so it is released by the session
    /// that took it — and a pooled connection is returned to the pool still holding it,
    /// where a later checkout would silently inherit (and re-entrantly re-take) the lock.
    /// A dedicated connection instead ties the lock's lifetime to a socket: dropping it
    /// closes the socket, PostgreSQL reaps the backend, and the lock is released. That is
    /// what makes the guard sound under a **panicking** test, which is the case that
    /// matters — the assertion failure this guards against aborts the test before any
    /// explicit release could run.
    struct SetupStateLock {
        _session: sqlx::PgConnection,
    }

    impl SetupStateLock {
        async fn acquire(database_url: &str) -> Self {
            use sqlx::Connection as _;

            let mut session = sqlx::PgConnection::connect(database_url)
                .await
                .expect("open the setup-state lock session");
            sqlx::query("select pg_advisory_lock($1)")
                .bind(SETUP_STATE_LOCK_KEY)
                .execute(&mut session)
                .await
                .expect("take the setup-state advisory lock");
            Self { _session: session }
        }
    }

    /// Restores the singleton to its migrated state.
    ///
    /// Called at both ends of every database-backed test in this module: at the end so a
    /// claim does not leak a "setup is done" fact, and at the *start* so a test that
    /// panicked before its cleanup — or a run killed outright — cannot poison the next
    /// one. Neither of those is sufficient alone; see [`migrated_pool`].
    async fn reset_setup_state(pool: &sqlx::PgPool) {
        sqlx::query(
            "update setup_state set claimed = false, claimed_admin_identity_id = null, \
             claimed_at = null where id",
        )
        .execute(pool)
        .await
        .expect("reset the setup singleton");
    }

    /// A pool onto the shared test database, plus exclusive access to `setup_state`.
    ///
    /// # The guard is not optional, and must be bound for the whole test
    ///
    /// Bind it as `let Some((pool, _setup_lock)) = …`, never `_`: a bare `_` drops the
    /// guard immediately and reopens the window below.
    ///
    /// `setup_state` is a singleton row on a database every test thread in this binary
    /// shares, and `cleanup` resets it *globally* — it has no per-test predicate to scope
    /// it by, because a singleton has no key. So a sibling test finishing mid-test writes
    /// `claimed = false` underneath whoever is running, and
    /// `a_configured_domain_lets_the_claim_through_and_marks_setup_claimed` reads that
    /// zero back out of `claim_status()` between its own claim and its own assertion.
    ///
    /// Resetting at the start of the test — which this does — fixes only the inherited
    /// half of the problem. It cannot fix that half, because the offending write lands
    /// *during* the test, after any start-of-test reset has already run. Only holding the
    /// singleton exclusively for the test's duration closes it, which is why the lock is
    /// taken here rather than a reset being relied on alone.
    async fn migrated_pool() -> Option<(sqlx::PgPool, SetupStateLock)> {
        let database_url = std::env::var("MOIRA_TEST_DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect the test database");
        // Before the lock, not under it: `migrate` takes sqlx's own advisory lock, and
        // acquiring ours first would order the two locks differently in different tests.
        crate::infra::db::migrate(&pool).await.expect("migrate");
        let lock = SetupStateLock::acquire(&database_url).await;
        reset_setup_state(&pool).await;
        Some((pool, lock))
    }

    async fn register_issuer(pool: &sqlx::PgPool, issuer: &str) -> Uuid {
        sqlx::query_scalar::<_, Uuid>(
            "insert into trusted_jwt_issuers (issuer, jwks_url) values ($1, $2) returning id",
        )
        .bind(issuer)
        .bind("https://idp.invalid/.well-known/jwks.json")
        .fetch_one(pool)
        .await
        .expect("register a trusted JWT issuer")
    }

    async fn cleanup(pool: &sqlx::PgPool, issuer: &str) {
        // `setup_state.claimed_admin_identity_id` is an FK onto the row being deleted, and
        // a successful claim also flips `claimed`. Restoring the singleton to its migrated
        // state keeps this test from leaking a "setup is done" fact into the shared
        // database every other unit test also uses.
        //
        // This write is global — a singleton has no key to scope it by — so it is only
        // safe while the caller holds the `SetupStateLock` that `migrated_pool` returns.
        reset_setup_state(pool).await;
        sqlx::query("delete from admin_identities where issuer = $1")
            .bind(issuer)
            .execute(pool)
            .await
            .expect("remove test grants");
        sqlx::query("delete from auth_provider_settings where issuer = $1")
            .bind(issuer)
            .execute(pool)
            .await
            .expect("remove test auth settings");
        sqlx::query("delete from trusted_jwt_issuers where issuer = $1")
            .bind(issuer)
            .execute(pool)
            .await
            .expect("remove the test issuer");
    }

    /// **The bootstrap-bypass guard.** A fresh deployment has no `auth_provider_settings`
    /// row, so the holder of the system key — the most authority Moira recognises, and the
    /// only credential the claim endpoint accepts — is refused. If someone adds a
    /// "so bootstrap works" short-circuit, this is the test that turns red.
    #[tokio::test]
    async fn a_system_key_claim_is_denied_on_a_fresh_deployment() {
        let Some((pool, _setup_lock)) = migrated_pool().await else {
            eprintln!("skipping admin identity claim integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let issuer = format!("https://fresh-{}.invalid", Uuid::now_v7().simple());
        register_issuer(&pool, &issuer).await;
        let state = AppState::new(crate::config::Settings::default(), Some(pool.clone()))
            .expect("build app state");

        let outcome = AdminIdentityService::new(&state)
            .expect("service")
            .claim(
                &context(),
                ClaimCredential::SystemKey(system_key_actor()),
                claim_request(&issuer, "sub-fresh", "owner@example.com"),
            )
            .await;

        let grants =
            sqlx::query_scalar::<_, i64>("select count(*) from admin_identities where issuer = $1")
                .bind(&issuer)
                .fetch_one(&pool)
                .await
                .expect("count grants");
        cleanup(&pool, &issuer).await;

        let error = outcome.expect_err("a fresh deployment refuses the claim");
        assert_coded(
            &error,
            StatusCode::FORBIDDEN,
            "admin_claim_domain_not_allowed",
        );
        assert_eq!(grants, 0, "a denied claim must write no grant");
    }

    /// The same denial with a governing row present but its allow-list empty — the "empty
    /// means unrestricted" reading, refuted.
    #[tokio::test]
    async fn a_system_key_claim_is_denied_when_the_allow_list_is_empty() {
        let Some((pool, _setup_lock)) = migrated_pool().await else {
            eprintln!("skipping admin identity claim integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let issuer = format!("https://empty-{}.invalid", Uuid::now_v7().simple());
        let issuer_id = register_issuer(&pool, &issuer).await;
        sqlx::query(
            "insert into auth_provider_settings \
                 (method, display_name, enabled, issuer, client_id, allowed_email_domains, \
                  trusted_jwt_issuer_id) \
             values ('generic_oidc', 'Console', true, $1, 'cid', '{}', $2)",
        )
        .bind(&issuer)
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("configure an enabled provider with no allowed domains");
        let state = AppState::new(crate::config::Settings::default(), Some(pool.clone()))
            .expect("build app state");

        let outcome = AdminIdentityService::new(&state)
            .expect("service")
            .claim(
                &context(),
                ClaimCredential::SystemKey(system_key_actor()),
                claim_request(&issuer, "sub-empty", "owner@example.com"),
            )
            .await;
        cleanup(&pool, &issuer).await;

        assert_coded(
            &outcome.expect_err("an empty allow-list denies"),
            StatusCode::FORBIDDEN,
            "admin_claim_domain_not_allowed",
        );
    }

    /// The designed setup order, end to end: configure the allow-list first, then claim.
    /// This is what makes the two denials above a *policy* rather than a broken path.
    #[tokio::test]
    async fn a_configured_domain_lets_the_claim_through_and_marks_setup_claimed() {
        let Some((pool, _setup_lock)) = migrated_pool().await else {
            eprintln!("skipping admin identity claim integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let issuer = format!("https://allowed-{}.invalid", Uuid::now_v7().simple());
        let issuer_id = register_issuer(&pool, &issuer).await;
        sqlx::query(
            "insert into auth_provider_settings \
                 (method, display_name, enabled, issuer, client_id, allowed_email_domains, \
                  trusted_jwt_issuer_id) \
             values ('generic_oidc', 'Console', true, $1, 'cid', array['example.com'], $2)",
        )
        .bind(&issuer)
        .bind(issuer_id)
        .execute(&pool)
        .await
        .expect("configure an enabled provider with an allow-list");
        let state = AppState::new(crate::config::Settings::default(), Some(pool.clone()))
            .expect("build app state");
        let service = AdminIdentityService::new(&state).expect("service");

        let before = service.claim_status().await.map(|status| status.claimed);
        let outcome = service
            .claim(
                &context(),
                ClaimCredential::SystemKey(system_key_actor()),
                claim_request(&issuer, "sub-allowed", "Owner@Example.com"),
            )
            .await;
        // A second claim for the same identity must hit the unique index, not create a
        // second grant — the database-level backstop behind the advisory lock.
        let duplicate = service
            .claim(
                &context(),
                ClaimCredential::SystemKey(system_key_actor()),
                claim_request(&issuer, "sub-allowed", "Owner@Example.com"),
            )
            .await;
        let after = service.claim_status().await.map(|status| status.claimed);
        cleanup(&pool, &issuer).await;

        assert!(
            !before.expect("status before"),
            "the singleton starts unclaimed"
        );
        let (record, replayed) = outcome.expect("a configured domain is admitted");
        assert!(!replayed);
        assert_eq!(record.email, "Owner@Example.com");
        assert_eq!(record.granted_scopes, vec!["moira:admin".to_string()]);
        assert_eq!(record.notice.message_key, ADMIN_IDENTITY_CLAIMED_NOTICE);
        assert_coded(
            &duplicate.expect_err("the second claim conflicts"),
            StatusCode::CONFLICT,
            "admin_identity_already_claimed",
        );
        assert!(
            after.expect("status after"),
            "the singleton must be flipped"
        );
    }

    /// D1: the reserved field is refused before anything else happens, so a caller can
    /// never believe Moira read a credential it does not support.
    #[tokio::test]
    async fn a_populated_setup_token_is_refused_rather_than_ignored() {
        let Some((pool, _setup_lock)) = migrated_pool().await else {
            eprintln!("skipping admin identity claim integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let state =
            AppState::new(crate::config::Settings::default(), Some(pool)).expect("build app state");
        let mut request = claim_request("https://unused.invalid", "sub", "owner@example.com");
        request.setup_token = Some("moira_setup_whatever".to_string());

        let error = AdminIdentityService::new(&state)
            .expect("service")
            .claim(
                &context(),
                ClaimCredential::SystemKey(system_key_actor()),
                request,
            )
            .await
            .expect_err("the deferred credential path is refused");
        assert_coded(&error, StatusCode::BAD_REQUEST, "setup_token_not_supported");
    }

    /// The structural refusal of "the first successful admin JWT wins": a verified trusted
    /// JWT is not a claim credential, even carrying `moira:admin`.
    #[tokio::test]
    async fn a_trusted_jwt_actor_cannot_claim_even_with_admin_scope() {
        let Some((pool, _setup_lock)) = migrated_pool().await else {
            eprintln!("skipping admin identity claim integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let state =
            AppState::new(crate::config::Settings::default(), Some(pool)).expect("build app state");
        let jwt_actor = Actor {
            actor_type: ActorType::TrustedJwt,
            subject: Some("first-arrival".to_string()),
            scopes: vec!["moira:admin".to_string()],
            ..Actor::default()
        };

        let error = AdminIdentityService::new(&state)
            .expect("service")
            .claim(
                &context(),
                ClaimCredential::SystemKey(jwt_actor),
                claim_request("https://unused.invalid", "sub", "owner@example.com"),
            )
            .await
            .expect_err("a bearer JWT is not a claim credential");
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    }

    /// Scope validation runs on the system-key path before any DB write reaches the
    /// issuer lookup: an unknown scope string is refused with the **existing**
    /// `scope_invalid` code, per module 8 step 3 — no new key is minted for a condition
    /// the catalog already covers.
    #[tokio::test]
    async fn claim_rejects_a_scope_outside_admin_scopes() {
        let Some((pool, _setup_lock)) = migrated_pool().await else {
            eprintln!("skipping admin identity claim integration: set MOIRA_TEST_DATABASE_URL");
            return;
        };
        let state =
            AppState::new(crate::config::Settings::default(), Some(pool)).expect("build app state");
        let mut request = claim_request("https://unused.invalid", "sub", "owner@example.com");
        request.scopes = vec!["moira:not-a-real-scope".to_string()];

        let error = AdminIdentityService::new(&state)
            .expect("service")
            .claim(
                &context(),
                ClaimCredential::SystemKey(system_key_actor()),
                request,
            )
            .await
            .expect_err("an unknown scope must be rejected before any issuer lookup");
        assert_coded(&error, StatusCode::UNPROCESSABLE_ENTITY, "scope_invalid");
    }

    #[test]
    fn the_claim_notice_resolves_through_the_catalog() {
        let notice = claimed_notice();
        assert_eq!(notice.message_key, ADMIN_IDENTITY_CLAIMED_NOTICE);
        assert_eq!(
            crate::i18n::default_message_for_key(ADMIN_IDENTITY_CLAIMED_NOTICE),
            Some(notice.message.as_str()),
            "the notice must come from the catalog, not from the inline fallback"
        );
    }
}
