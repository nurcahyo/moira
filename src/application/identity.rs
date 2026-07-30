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
use chrono::{Duration, Utc};
use secrecy::ExposeSecret;
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandMutation, AdminCommandRunner, RequestContext,
        admin::shared::{
            PageRequest, admin_command_spec, command_hasher, paginate, require_non_empty,
            success_audit,
        },
    },
    domain::{
        AdminIdentityPatchRequest, AdminIdentityRecord, AdminInviteConstraint,
        AdminInviteCreateRequest, AdminInvitePreviewRequest, AdminInvitePreviewResponse,
        AdminInviteRecord, AdminInviteRedeemRequest, AdminInviteSecretResponse, AdminInviteStatus,
        ClaimAdminIdentityRequest, CursorScope, ListCursor, ListResponse,
        MAX_INVITE_EXPIRY_SECONDS, MIN_INVITE_EXPIRY_SECONDS, ResponseText,
        SetupClaimStatusResponse,
    },
    error::AppError,
    infra::repositories::{
        AdminIdentityGrant, AdminIdentityGrantInsert, AdminIdentityRepository, AdminInviteInsert,
        AdminInviteRow, AuthProviderSettingsRepository, GoverningAuthPolicy,
        PgAdminIdentityRepository, PgAdminRepository, PgAuthProviderSettingsRepository,
    },
    security::{Actor, ActorType, AuthorizationService, TrustedJwtIdentity},
};

const ADMIN_IDENTITY_CLAIMED_NOTICE: &str = "moira.notice.admin_identity_claimed";
const ADMIN_INVITE_CREATED_NOTICE: &str = "moira.notice.admin_invite_created";
const ADMIN_INVITE_REDEEMED_NOTICE: &str = "moira.notice.admin_invite_redeemed";
const ADMIN_IDENTITY_REVOKED_NOTICE: &str = "moira.notice.admin_identity_revoked";

/// The token namespace, in the same shape as `moira_sys` / `moira_cons`.
///
/// It is a *prefix on the plaintext*, so an invite token is visually distinguishable
/// from an API key in a paste buffer or a log line someone is about to redact.
const ADMIN_INVITE_NAMESPACE: &str = "moira_inv";

/// Separate cursor scopes, so a cursor issued for one list cannot be replayed against
/// the other.
const ADMIN_INVITES_CURSOR: CursorScope = CursorScope::new("admin.admin_invites");
const ADMIN_IDENTITIES_CURSOR: CursorScope = CursorScope::new("admin.admin_identities");

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

    // ===================================================================================
    // Plan 09 wave 2 — invitations.
    //
    // The gap this closes is **not** "Moira only supports one admin": `claim` above has
    // no `setup_claimed` precondition and `mark_setup_claimed` is a no-op on the second
    // call, so an operator holding the bootstrap system key can already grant N admins.
    // The gap is that there is no **non-system-key** path to a grant, which means the
    // break-glass credential can never be retired.
    // ===================================================================================

    /// Mints a single-use, time-limited, email- or domain-bound invite token.
    ///
    /// The token is hashed with [`crate::security::ApiKeyHasher`] — Argon2id with the
    /// deployment pepper — exactly as system and consumer keys are, and deliberately
    /// **not** with a bare SHA-256 (finding P1-1). Reusing the hasher rather than adding
    /// a second scheme is the point: there is one place in Moira where a bearer secret
    /// is hashed.
    pub async fn create_invite(
        &self,
        actor: &Actor,
        issuer: Option<&str>,
        ctx: &RequestContext,
        request: AdminInviteCreateRequest,
    ) -> Result<(AdminInviteSecretResponse, bool), AppError> {
        self.state.authz.require(actor, "moira:admins:invite")?;
        let value = normalize_invite_constraint(request.constraint, &request.value)?;
        let expires_at = Utc::now() + validated_invite_lifetime(request.expires_in_seconds)?;

        let generated = self.state.key_hasher.generate(ADMIN_INVITE_NAMESPACE)?;
        let insert = AdminInviteInsert {
            id: Uuid::now_v7(),
            token_prefix: generated.key_prefix.clone(),
            token_hash: generated.key_hash.clone(),
            fingerprint: generated.fingerprint.clone(),
            pepper_version: generated.pepper_version.clone(),
            constraint: request.constraint,
            value: value.clone(),
            created_by_issuer: issuer.map(str::to_string),
            created_by_subject: actor.subject.clone(),
            created_by_actor_type: invite_creator_actor_type(actor)?.to_string(),
            expires_at,
        };

        // The command envelope hashes the *request*, not the generated token, so two
        // requests with the same `Idempotency-Key` and the same body replay the first
        // invite instead of minting a second one the operator would never see.
        let spec = admin_command_spec(
            ctx,
            actor,
            "admin_invite.create",
            json!({ "constraint": constraint_label(request.constraint) }),
            &request,
        )?;
        let identities = Arc::clone(&self.identities);
        let audit_actor = actor.clone();
        let audit_ctx = ctx.clone();
        // Moved into the closure so the plaintext is not read anywhere outside it.
        let secret = generated.raw_key.expose_secret().to_string();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let row = identities
                        .insert_invite(transaction.connection(), &insert)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "admin_invite.create",
                            "admin_invite",
                            Some(row.id.to_string()),
                            // The constraint value is the invitee's address or domain,
                            // which the audit trail needs. The token, its hash, its
                            // prefix and its fingerprint are all absent, and
                            // `AdminInviteRow` has no field for any of them.
                            json!({
                                "constraint": constraint_label(row.constraint),
                                "value": row.value,
                                "expires_at": row.expires_at,
                                "created_by_actor_type": insert.created_by_actor_type,
                            }),
                        ))
                        .await?;
                    let record = invite_record(row);
                    let id = record.id.to_string();
                    let sanitized = AdminInviteSecretResponse {
                        resource: record.clone(),
                        secret: None,
                        secret_retrievable: false,
                        notice: invite_created_notice(),
                    };
                    AdminCommandMutation::with_replay_response(
                        AdminInviteSecretResponse {
                            resource: record,
                            secret: Some(secret),
                            secret_retrievable: true,
                            notice: invite_created_notice(),
                        },
                        sanitized,
                        201,
                        Some(id),
                    )
                })
            })
            .await?;
        self.state.metrics.record_admin_invite_created();
        Ok((outcome.response, outcome.replayed))
    }

    pub async fn list_invites(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<AdminInviteRecord>, AppError> {
        self.state.authz.require(actor, "moira:admins:read")?;
        let page = page.into();
        let rows = self
            .identities
            .list_invites(page.decode(ADMIN_INVITES_CURSOR)?, page.limit())
            .await?;
        let records: Vec<AdminInviteRecord> = rows.into_iter().map(invite_record).collect();
        Ok(paginate(records, &page, ADMIN_INVITES_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    pub async fn get_invite(&self, actor: &Actor, id: Uuid) -> Result<AdminInviteRecord, AppError> {
        self.state.authz.require(actor, "moira:admins:read")?;
        self.identities.get_invite(id).await.map(invite_record)
    }

    pub async fn revoke_invite(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(AdminInviteRecord, bool), AppError> {
        self.state.authz.require(actor, "moira:admins:invite")?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "admin_invite.revoke",
            json!({ "invite_id": id }),
            &json!({}),
        )?;
        let identities = Arc::clone(&self.identities);
        let audit_actor = actor.clone();
        let audit_ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let row = identities.revoke_invite(transaction.connection(), id).await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "admin_invite.revoke",
                            "admin_invite",
                            Some(row.id.to_string()),
                            json!({ "value": row.value }),
                        ))
                        .await?;
                    let record = invite_record(row);
                    let id = record.id.to_string();
                    AdminCommandMutation::new(record, 200, Some(id))
                })
            })
            .await?;
        Ok((outcome.response, outcome.replayed))
    }

    /// What an **unauthenticated** invitee is told about the invite they hold.
    ///
    /// # No actor, and no authorization check — the token is the credential
    ///
    /// The invitee is unauthenticated by construction: they open the link *before*
    /// signing in, and the credential a scope check would demand is the one signing in
    /// produces. That is the same circularity finding F15 records for the sign-in
    /// methods endpoint.
    ///
    /// Two properties keep that safe. First, the response is confined to the invite's
    /// own constraint and expiry — no inviter, no id, no deployment detail, and nothing
    /// about policy. Second, the caller must present a 32-byte `OsRng` token; a caller
    /// who does not hold one gets `invite_not_found` from the **prefix** lookup, before
    /// any Argon2 verification runs, so this endpoint cannot be used as a CPU-exhaustion
    /// oracle by an attacker with no valid prefix.
    pub async fn preview_invite(
        &self,
        request: &AdminInvitePreviewRequest,
    ) -> Result<AdminInvitePreviewResponse, AppError> {
        let invite = self.resolve_invite(&request.token).await?;
        require_redeemable(&invite)?;
        Ok(AdminInvitePreviewResponse {
            constraint: invite.constraint,
            value: invite.value,
            expires_at: invite.expires_at,
        })
    }

    /// Redeems an invite into an `admin_identities` grant for the presenting identity.
    ///
    /// # Every check runs BEFORE the transactional envelope
    ///
    /// This mirrors [`Self::claim`]'s rule, and here it carries a second, stronger
    /// obligation: **a rejected redemption must not consume the invite.** The plan's own
    /// ordering requirement is that an invitee refused by the deny-by-default domain
    /// policy can redeem the *same* link once an operator widens the allow-list.
    ///
    /// So the invite lookup, its state checks, the D5 email checks, the invite's own
    /// constraint and plan 07's provider allow-list all run here, outside
    /// [`AdminCommandRunner::execute`]. A request that fails any of them takes no
    /// advisory lock, writes no idempotency record, and — crucially — never reaches the
    /// statement that marks the invite consumed.
    ///
    /// **Do not move any of these inside the closure "to make it atomic".** Atomicity is
    /// already provided for the part that needs it: `consume_invite` re-checks state
    /// under `select … for update` in the same transaction as the grant insert, so two
    /// simultaneous valid redemptions still produce exactly one grant.
    ///
    /// # Which authenticator this uses, and why it is not `authenticate_admin`
    ///
    /// The caller is [`AuthService::verify_trusted_jwt_identity`]'s
    /// [`TrustedJwtIdentity`] — a verified `(issuer, subject)` and nothing else.
    /// `authenticate_admin` would apply the `admin_identities` grant (plan 07 decision
    /// D2 puts it there and only there), which is meaningless for an identity whose
    /// grant does not exist yet, and it would hand this method an `Actor` carrying
    /// token-asserted `scopes`. The narrow type is what makes a self-asserted scope
    /// unreadable from here.
    pub async fn redeem_invite(
        &self,
        identity: &TrustedJwtIdentity,
        ctx: &RequestContext,
        request: AdminInviteRedeemRequest,
    ) -> Result<(AdminIdentityRecord, bool), AppError> {
        let invite = self.resolve_invite(&request.token).await?;
        require_redeemable(&invite)?;

        // Plan 07 decision D5: `email`/`email_verified` are required by the DTO, so an
        // omitted field is a schema violation the extractor rejects. What survives to
        // here is a present-but-unusable value, and the *invite's own* constraint.
        let email = request.email.trim();
        evaluate_invite_constraint(&invite, email)?;

        // The redeem token's issuer must be a registered, active trusted JWT issuer —
        // the same rule `claim` applies to its target issuer. `verify_trusted_jwt_identity`
        // already required a registered row to verify the signature; this resolves that
        // row's **id**, which the policy lookup below cannot work without.
        let trusted_jwt_issuer_id = self.identities.resolve_active_issuer(&identity.issuer).await?;

        // Plan 07 module 10, unchanged and unexempted (decision D3). An invitation
        // authorises its holder to *submit* a redemption; it is not a policy bypass, and
        // there is deliberately no `invite.is_some()` short-circuit anywhere in this
        // path — `evaluate_claim_policy` cannot branch on a credential it never receives.
        //
        // The second argument is load-bearing and is the defect plan 09 §0.1 B5 records:
        // `governing_policy` matches `issuer = $1 or trusted_jwt_issuer_id = $2`, and in
        // a real deployment `$1` is the *console's* issuer while the provider row's
        // `issuer` column holds the *IdP's*. The row therefore matches only through
        // `trusted_jwt_issuer_id`. Passing a nil UUID here — or dropping the argument —
        // makes every redemption 403 forever on exactly the deployments that are
        // configured correctly.
        let policy = self
            .auth_settings
            .governing_policy(&identity.issuer, trusted_jwt_issuer_id)
            .await?;
        if let Err(error) = evaluate_claim_policy(email, request.email_verified, policy.as_ref()) {
            self.state
                .metrics
                .record_admin_invite_denied(denial_reason(&error));
            return Err(error);
        }

        let grant_id = Uuid::now_v7();
        let insert = AdminIdentityGrantInsert {
            id: grant_id,
            trusted_jwt_issuer_id,
            issuer: identity.issuer.clone(),
            subject: identity.subject.clone(),
            email: email.to_string(),
            email_verified: request.email_verified,
            // An invite grants base admin authority and never ownership: `is_primary`
            // takes its column default of `false`. A redemption that could mint an owner
            // would make an invite strictly more powerful than the transfer endpoint it
            // is supposed to sit beneath.
            granted_scopes: vec!["moira:admin".to_string()],
            // Honest: `admin_identities.granted_by_actor_type`'s CHECK admits only
            // 'system_key' and 'setup_token', and a redemption is neither. Widening that
            // CHECK is a schema change this wave does not need — the invite id in the
            // audit row and `admin_invites.consumed_admin_identity_id` both record the
            // real provenance, and `consumed_admin_identity_id` is a hard FK link.
            granted_by_actor_type: "system_key".to_string(),
            granted_by_subject: invite.created_by_subject.clone(),
        };

        let spec = admin_command_spec(
            ctx,
            // The idempotency actor fingerprint is the *invitee's*, built from a
            // synthetic actor carrying the verified issuer id — so two different issuers
            // minting the same `sub` cannot replay each other's redemption.
            &redeem_actor(identity, trusted_jwt_issuer_id),
            "admin_invite.redeem",
            json!({ "issuer": identity.issuer, "subject": identity.subject }),
            // **The raw token is deliberately absent from the command envelope.** That
            // envelope is HMAC'd and its digest written to `idempotency_records`, and a
            // keyed digest of a live bearer secret is still a derivation of that secret
            // sitting in a table the admin API can list. `invite_id` discriminates
            // exactly as well — it is derived from the token by the lookup above — and is
            // not itself a credential.
            &json!({
                "invite_id": invite.id,
                "email": email,
                "email_verified": request.email_verified,
            }),
        )?;

        let identities = Arc::clone(&self.identities);
        let audit_actor = redeem_actor(identity, trusted_jwt_issuer_id);
        let audit_ctx = ctx.clone();
        let invite_id = invite.id;
        let redeem_issuer = identity.issuer.clone();
        let redeem_subject = identity.subject.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let grant = identities
                        .insert_grant(transaction.connection(), &insert)
                        .await?;
                    // The single-winner gate. It re-reads the invite under
                    // `select … for update` and refuses a non-pending or expired row, so
                    // two concurrent redemptions serialise and the loser sees
                    // `invite_already_consumed` rather than creating a second grant.
                    identities
                        .consume_invite(
                            transaction.connection(),
                            invite_id,
                            &redeem_issuer,
                            &redeem_subject,
                            grant.id,
                        )
                        .await?;
                    // A grant now exists, so the setup singleton is true whether or not
                    // it already was. Self-idempotent (`… and claimed = false`), so this
                    // never rewrites the original claimant.
                    identities
                        .mark_setup_claimed(transaction.connection(), grant.id)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "admin_invite.redeem",
                            "admin_identity",
                            Some(grant.id.to_string()),
                            json!({
                                "issuer": grant.issuer,
                                "subject": grant.subject,
                                "email": grant.email,
                                "granted_scopes": grant.granted_scopes,
                                "admin_invite_id": invite_id,
                            }),
                        ))
                        .await?;
                    let id = grant.id.to_string();
                    AdminCommandMutation::new(
                        record_from_grant_with_notice(grant, redeemed_notice()),
                        201,
                        Some(id),
                    )
                })
            })
            .await;

        match outcome {
            Ok(outcome) => {
                self.state.metrics.record_admin_invite_redeemed();
                Ok((outcome.response, outcome.replayed))
            }
            Err(error) => {
                self.state
                    .metrics
                    .record_admin_invite_denied(denial_reason(&error));
                Err(error)
            }
        }
    }

    // ===================================================================================
    // Plan 09 wave 2 — grant administration. Plan 07 deferred all three.
    // ===================================================================================

    pub async fn list_identities(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<AdminIdentityRecord>, AppError> {
        self.state.authz.require(actor, "moira:admins:read")?;
        let page = page.into();
        let rows = self
            .identities
            .list_grants(page.decode(ADMIN_IDENTITIES_CURSOR)?, page.limit())
            .await?;
        let records: Vec<AdminIdentityRecord> = rows
            .into_iter()
            .map(|grant| record_from_grant_with_notice(grant, claimed_notice()))
            .collect();
        Ok(paginate(records, &page, ADMIN_IDENTITIES_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    /// Ownership transfer: sets or clears `admin_identities.is_primary`.
    pub async fn set_identity_primary(
        &self,
        actor: &Actor,
        issuer: Option<&str>,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: AdminIdentityPatchRequest,
    ) -> Result<(AdminIdentityRecord, bool), AppError> {
        self.require_primary_actor(actor, issuer).await?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "admin_identity.set_primary",
            json!({ "admin_identity_id": id }),
            &request,
        )?
        .with_expected_version(Some(expected_version));

        let identities = Arc::clone(&self.identities);
        let audit_actor = actor.clone();
        let audit_ctx = ctx.clone();
        let is_primary = request.is_primary;
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let grant = identities
                        .set_primary(transaction.connection(), id, expected_version, is_primary)
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "admin_identity.set_primary",
                            "admin_identity",
                            Some(grant.id.to_string()),
                            json!({
                                "issuer": grant.issuer,
                                "subject": grant.subject,
                                "is_primary": grant.is_primary,
                            }),
                        ))
                        .await?;
                    let resource_id = grant.id.to_string();
                    AdminCommandMutation::new(
                        record_from_grant_with_notice(grant, claimed_notice()),
                        200,
                        Some(resource_id),
                    )
                })
            })
            .await?;
        self.state.metrics.record_admin_ownership_transferred();
        Ok((outcome.response, outcome.replayed))
    }

    /// Soft revoke. Plan 07 explicitly deferred this endpoint; it lands here.
    pub async fn revoke_identity(
        &self,
        actor: &Actor,
        issuer: Option<&str>,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(AdminIdentityRecord, bool), AppError> {
        self.require_primary_actor(actor, issuer).await?;
        let spec = admin_command_spec(
            ctx,
            actor,
            "admin_identity.revoke",
            json!({ "admin_identity_id": id }),
            &json!({}),
        )?;
        let identities = Arc::clone(&self.identities);
        let audit_actor = actor.clone();
        let audit_ctx = ctx.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let grant = identities.revoke_grant(transaction.connection(), id).await?;
                    transaction
                        .insert_audit(success_audit(
                            &audit_actor,
                            &audit_ctx,
                            "admin_identity.revoke",
                            "admin_identity",
                            Some(grant.id.to_string()),
                            json!({ "issuer": grant.issuer, "subject": grant.subject }),
                        ))
                        .await?;
                    let resource_id = grant.id.to_string();
                    AdminCommandMutation::new(
                        record_from_grant_with_notice(grant, revoked_notice()),
                        200,
                        Some(resource_id),
                    )
                })
            })
            .await?;
        self.state.metrics.record_admin_identity_revoked();
        Ok((outcome.response, outcome.replayed))
    }

    /// **The ownership check.** Decision D1's enforcement point.
    ///
    /// It reads the caller's *own* `admin_identities` row and requires `is_primary`.
    /// It is not a scope check, and it cannot be replaced by one:
    /// `AuthorizationService::has_scope` grants a `moira:admin`-holding trusted-JWT
    /// actor every scope by implication, and every grant Moira writes carries
    /// `moira:admin` by default — so a `moira:admins:manage` scope would be satisfied by
    /// every admin, including the one whose ownership is being taken away.
    ///
    /// The actor-type arm is an **allow-list**, matching
    /// `ADMIN_IMPLYING_ACTOR_TYPES`'s reasoning: a type absent from it is denied, which
    /// is the safe direction to be wrong in. System-key and dev-admin callers pass
    /// because break-glass must keep working — that is the documented last resort when
    /// no primary remains, and this wave does not remove it.
    async fn require_primary_actor(
        &self,
        actor: &Actor,
        issuer: Option<&str>,
    ) -> Result<(), AppError> {
        match actor.actor_type {
            ActorType::SystemKey | ActorType::DevAdmin => return Ok(()),
            ActorType::TrustedJwt => {}
            ActorType::Anonymous | ActorType::ConsumerKey => return Err(not_primary()),
        }
        let (Some(issuer), Some(subject)) = (issuer, actor.subject.as_deref()) else {
            return Err(not_primary());
        };
        let grant = self.identities.find_active_grant(issuer, subject).await?;
        if grant.is_some_and(|grant| grant.is_primary) {
            Ok(())
        } else {
            Err(not_primary())
        }
    }

    /// Prefix-lookup-then-verify, the same shape `AuthService::verify_api_key` uses.
    ///
    /// The Argon2id hash is not searchable, so the indexed `token_prefix` selects at
    /// most one live row and the hash then proves the presented token really is that
    /// row's. A prefix match on its own proves nothing — the prefix is a plaintext
    /// substring of the token — which is why the verification is not optional here.
    async fn resolve_invite(&self, token: &str) -> Result<AdminInviteRow, AppError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(invite_not_found());
        }
        let prefix = self.state.key_hasher.prefix(token);
        let Some(candidate) = self.identities.find_invite_by_prefix(&prefix).await? else {
            self.state
                .metrics
                .record_admin_invite_denied("not_found");
            return Err(invite_not_found());
        };
        if self
            .state
            .key_hasher
            .verify(token, &candidate.token_hash)?
        {
            Ok(candidate.record)
        } else {
            self.state
                .metrics
                .record_admin_invite_denied("not_found");
            Err(invite_not_found())
        }
    }
}

/// Refuses an invite that is consumed, revoked, or past its expiry.
///
/// Shared by preview and redeem so the two cannot drift: an invite preview that says
/// "valid" for a link redemption will refuse is worse than no preview at all.
fn require_redeemable(invite: &AdminInviteRow) -> Result<(), AppError> {
    match invite.status {
        AdminInviteStatus::Consumed => {
            return Err(AppError::conflict(
                "invite_already_consumed",
                "this invitation has already been redeemed",
            ));
        }
        AdminInviteStatus::Revoked => {
            return Err(AppError::coded(
                StatusCode::FORBIDDEN,
                "invite_revoked",
                "this invitation has been revoked",
            ));
        }
        AdminInviteStatus::Pending => {}
    }
    if invite.is_expired(Utc::now()) {
        return Err(AppError::coded(
            StatusCode::FORBIDDEN,
            "invite_expired",
            "this invitation has expired",
        ));
    }
    Ok(())
}

/// The **invite's own** constraint, checked separately from plan 07's provider
/// allow-list and never collapsed into it.
///
/// The two failures have different remedies — reissue the invite versus widen the
/// allow-list — so they carry different codes, and a console that merged them would
/// send the operator to the wrong screen.
pub(crate) fn evaluate_invite_constraint(
    invite: &AdminInviteRow,
    email: &str,
) -> Result<(), AppError> {
    let email_lower = email.trim().to_ascii_lowercase();
    let domain = email_domain(email).ok_or_else(|| {
        AppError::coded(
            StatusCode::BAD_REQUEST,
            "admin_claim_email_required",
            "a usable email address is required to redeem an invitation",
        )
    })?;
    match invite.constraint {
        AdminInviteConstraint::Email => {
            if email_lower == invite.value.trim().to_ascii_lowercase() {
                Ok(())
            } else {
                Err(AppError::coded(
                    StatusCode::FORBIDDEN,
                    "invite_email_mismatch",
                    "this invitation was issued for a different email address",
                ))
            }
        }
        AdminInviteConstraint::Domain => {
            // Exact match on the domain, mirroring `evaluate_claim_policy`: admitting
            // `sub.example.com` because `example.com` was written would hand the inviter
            // a subtree they never named.
            if domain == invite.value.trim().to_ascii_lowercase() {
                Ok(())
            } else {
                Err(AppError::coded(
                    StatusCode::FORBIDDEN,
                    "invite_domain_mismatch",
                    "this invitation was issued for a different email domain",
                ))
            }
        }
    }
}

/// Normalises and validates the constraint an invite is bound to.
///
/// There is no unbound ("anyone with the link") form, and this is where that is
/// enforced: an empty value is refused rather than stored as a match-everything.
pub(crate) fn normalize_invite_constraint(
    constraint: AdminInviteConstraint,
    value: &str,
) -> Result<String, AppError> {
    require_non_empty("value", value)?;
    let normalized = value.trim().to_ascii_lowercase();
    match constraint {
        AdminInviteConstraint::Email => {
            if email_domain(&normalized).is_none() {
                return Err(AppError::unprocessable(
                    "invalid_request",
                    "an email-constrained invitation requires a usable email address",
                ));
            }
        }
        AdminInviteConstraint::Domain => {
            if normalized.contains('@') || !normalized.contains('.') {
                return Err(AppError::unprocessable(
                    "invalid_request",
                    "a domain-constrained invitation requires a bare domain, not an address",
                ));
            }
        }
    }
    Ok(normalized)
}

/// The server-side **hard cap** on an invite's lifetime.
///
/// Refused rather than clamped: an operator who believes they issued a 30-day invite and
/// silently received a 3-day one discovers the difference at the worst possible moment.
pub(crate) fn validated_invite_lifetime(seconds: u32) -> Result<Duration, AppError> {
    if seconds > MAX_INVITE_EXPIRY_SECONDS {
        return Err(AppError::unprocessable(
            "admin_invite_expiry_too_long",
            format!(
                "an invitation may not last longer than {MAX_INVITE_EXPIRY_SECONDS} seconds"
            ),
        ));
    }
    if seconds < MIN_INVITE_EXPIRY_SECONDS {
        return Err(AppError::unprocessable(
            "invalid_request",
            format!("an invitation must last at least {MIN_INVITE_EXPIRY_SECONDS} seconds"),
        ));
    }
    Ok(Duration::seconds(i64::from(seconds)))
}

/// Which credential minted an invite, recorded honestly on the row.
///
/// An allow-list, for the same reason `ADMIN_IMPLYING_ACTOR_TYPES` is one: a new
/// [`ActorType`] must be considered deliberately rather than inheriting the ability to
/// mint an admin invitation by default.
fn invite_creator_actor_type(actor: &Actor) -> Result<&'static str, AppError> {
    match actor.actor_type {
        ActorType::SystemKey => Ok("system_key"),
        ActorType::TrustedJwt => Ok("trusted_jwt"),
        ActorType::DevAdmin => Ok("dev_admin"),
        ActorType::Anonymous | ActorType::ConsumerKey => Err(AppError::Forbidden(
            "this credential type may not create admin invitations".to_string(),
        )),
    }
}

/// The invitee, as an [`Actor`], for the idempotency fingerprint and the audit row.
///
/// `scopes` is empty and stays empty: the redemption is performed by an identity that
/// holds no grant, and writing anything here would misdescribe it in the audit log.
/// `trusted_jwt_issuer_id` **is** populated, because it is a field of
/// `actor_fingerprint` and omitting it would let two issuers minting the same `sub`
/// replay each other's stored redemption.
fn redeem_actor(identity: &TrustedJwtIdentity, trusted_jwt_issuer_id: Uuid) -> Actor {
    Actor {
        actor_type: ActorType::TrustedJwt,
        subject: Some(identity.subject.clone()),
        external_user_id: Some(identity.subject.clone()),
        trusted_jwt_issuer_id: Some(trusted_jwt_issuer_id),
        scopes: Vec::new(),
        ..Actor::default()
    }
}

/// The bounded denial-reason label set for `moira_admin_invite_outcomes_total`.
///
/// Derived from the error **code**, never from the message and never from the invitee's
/// address: an email or a domain as a label value is unbounded cardinality and PII in a
/// metrics endpoint. Anything unrecognised collapses to `other` rather than widening the
/// label domain at runtime.
pub(crate) fn denial_reason(error: &AppError) -> &'static str {
    let rendered = error.to_string();
    for (code, label) in ADMIN_INVITE_DENIAL_REASONS {
        if rendered.starts_with(&format!("{code}:")) {
            return label;
        }
    }
    "other"
}

/// The closed mapping from error code to metric label. Kept as a table so the label set
/// is enumerable by a test rather than scattered across match arms.
pub(crate) const ADMIN_INVITE_DENIAL_REASONS: &[(&str, &str)] = &[
    ("invite_not_found", "not_found"),
    ("invite_expired", "expired"),
    ("invite_already_consumed", "consumed"),
    ("invite_revoked", "revoked"),
    ("invite_email_mismatch", "email_mismatch"),
    ("invite_domain_mismatch", "domain_mismatch"),
    ("admin_claim_domain_not_allowed", "domain_not_allowed"),
    ("admin_claim_email_not_verified", "email_not_verified"),
    ("admin_claim_email_required", "email_required"),
];

fn constraint_label(constraint: AdminInviteConstraint) -> &'static str {
    match constraint {
        AdminInviteConstraint::Email => "email",
        AdminInviteConstraint::Domain => "domain",
    }
}

fn invite_record(row: AdminInviteRow) -> AdminInviteRecord {
    let expired = row.is_expired(Utc::now());
    AdminInviteRecord {
        id: row.id,
        constraint: row.constraint,
        value: row.value,
        status: row.status,
        expired,
        expires_at: row.expires_at,
        created_by_subject: row.created_by_subject,
        consumed_at: row.consumed_at,
        consumed_subject: row.consumed_subject,
        created_at: row.created_at,
        version: row.version,
    }
}

fn invite_not_found() -> AppError {
    AppError::coded(
        StatusCode::NOT_FOUND,
        "invite_not_found",
        "no invitation matches this token",
    )
}

fn not_primary() -> AppError {
    AppError::coded(
        StatusCode::FORBIDDEN,
        "admin_identity_not_primary",
        "only a primary admin identity may manage other admin identities",
    )
}

fn invite_created_notice() -> ResponseText {
    catalog_notice(
        ADMIN_INVITE_CREATED_NOTICE,
        "An admin invitation has been created.",
    )
}

fn redeemed_notice() -> ResponseText {
    catalog_notice(
        ADMIN_INVITE_REDEEMED_NOTICE,
        "The invitation has been redeemed and admin access granted.",
    )
}

fn revoked_notice() -> ResponseText {
    catalog_notice(
        ADMIN_IDENTITY_REVOKED_NOTICE,
        "Admin access has been revoked for this identity.",
    )
}

fn catalog_notice(key: &'static str, fallback: &'static str) -> ResponseText {
    ResponseText::new(
        key,
        crate::i18n::default_message_for_key(key).unwrap_or(fallback),
    )
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
    record_from_grant_with_notice(grant, claimed_notice())
}

/// The notice is a parameter because the same row is returned by four operations that
/// mean different things to a human: a first claim, an invitation redeemed, an ownership
/// transfer, and a revocation. The **status code** distinguishes fresh from replayed;
/// the notice distinguishes what happened.
fn record_from_grant_with_notice(
    grant: AdminIdentityGrant,
    notice: ResponseText,
) -> AdminIdentityRecord {
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
        is_primary: grant.is_primary,
        status: grant.status,
        created_at: grant.created_at,
        version: grant.version,
        notice,
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
