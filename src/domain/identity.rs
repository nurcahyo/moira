//! Admin identity claiming DTOs (plan 07 modules 1-3).
//!
//! Moira grants a *human* admin authority by binding `moira:admin` to a stable
//! `(issuer, subject)` pair from an already-registered trusted JWT issuer. It never issues
//! a password, a session cookie, or a login page — after a grant exists, the human's
//! existing trusted-JWT bearer token simply carries more authority.
//!
//! These types live here rather than in [`crate::domain::admin`] on purpose: that module's
//! `SetupStatusResponse`/`SetupChecks` describe *deployment readiness checks*, which is a
//! different concept from *who holds admin*, and folding the two together would blur a
//! boundary the API deliberately keeps separate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::i18n::ResponseText;

/// The entire response body of `GET /api/v1/admin/setup/claim-status`.
///
/// One boolean is the whole contract, deliberately. This is the only unauthenticated
/// endpoint in the identity surface, and it is unauthenticated *because* its response is a
/// single bit that an attacker could infer anyway from the fact that the instance is
/// freshly deployed. No count, no timestamp, no issuer, no subject, no enumeration of who
/// holds a grant — anything more would be reconnaissance on the surface an attacker would
/// target during the setup window, when the deployment is least defended.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupClaimStatusResponse {
    pub claimed: bool,
}

/// Request body of `POST /api/v1/admin/setup/claim`.
///
/// The endpoint is gated by `X-Moira-System-Key` only. A bare trusted-JWT bearer token is
/// rejected even if it verifies, which is the structural refusal of "the first successful
/// admin JWT wins" (`plans/01` §4.4).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimAdminIdentityRequest {
    /// Must resolve to an **active, registered** `trusted_jwt_issuers` row. Moira never
    /// accepts a free-text issuer at claim time.
    pub issuer: String,
    /// The identity provider's stable subject. Together with `issuer` this is the grant's
    /// uniqueness key — never email, which is mutable and reassignable.
    pub subject: String,
    /// Required, not optional (decision D5). Carrying a human-identifiable attribute on
    /// every grant is what makes the deny-by-default allowed-domain policy enforceable.
    pub email: String,
    /// Required, and with **no `#[serde(default)]`** — that omission is load-bearing rather
    /// than cosmetic. With a default, a body that simply left the field out would
    /// deserialize to `false` and then fail the verified-email check with a *misleading*
    /// 403 telling the caller their email is unverified, when in fact they never sent the
    /// field. Without it, an omitted field is the schema violation it actually is.
    ///
    /// Must be `true`; a claim naming an unverified address is refused.
    pub email_verified: bool,
    #[serde(default = "default_admin_grant_scopes")]
    pub scopes: Vec<String>,
    /// **Reserved and rejected — never silently ignored.**
    ///
    /// The one-time setup-token credential path is deferred (plan 07 §0.2 decision D1):
    /// plan 08's console declares this field and never sends it, and the path was specified
    /// but never exercised. The field survives in the schema only so that plan 08's
    /// generated client keeps typechecking.
    ///
    /// A request that populates it is refused with a clear, keyed error. Accepting and
    /// discarding it would let a caller believe they had presented a credential Moira had
    /// honoured, when Moira had in fact authenticated them by some other means entirely —
    /// the one failure mode worse than not supporting the field at all.
    pub setup_token: Option<String>,
}

/// A granted admin identity, as returned by `POST /api/v1/admin/setup/claim`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminIdentityRecord {
    pub id: Uuid,
    pub issuer: String,
    pub subject: String,
    /// Always present — a grant cannot be created without a verified email (decision D5).
    /// The `admin_identities.email` column stays nullable so a future anonymisation path
    /// can clear it; the always-present invariant is an application invariant.
    pub email: String,
    pub email_verified: bool,
    pub granted_scopes: Vec<String>,
    /// Ownership, as **row state** rather than as a capability (plan 09 decision D1).
    ///
    /// A primary admin may transfer ownership and revoke other grants. This is not a
    /// scope, and could not be one: `AuthorizationService::has_scope` grants a
    /// `moira:admin`-holding trusted-JWT actor every scope by implication with no
    /// per-scope opt-out, so a `moira:admins:manage` scope would be satisfied by every
    /// admin Moira can currently grant — and the last-primary guard would have nothing
    /// to count.
    pub is_primary: bool,
    pub status: AdminIdentityStatus,
    pub created_at: DateTime<Utc>,
    pub version: i64,
    /// i18n envelope for the success message (CONVENTIONS §4.2).
    pub notice: ResponseText,
}

/// Request body of `PATCH /api/v1/admin/admin-identities/{id}` — ownership transfer.
///
/// One field, deliberately. Everything else about a grant is either immutable
/// (`issuer`/`subject`, the uniqueness key) or belongs to a different operation
/// (`status`, which `DELETE` owns). A general-purpose patch surface here would be a
/// second, weaker way to edit `granted_scopes`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminIdentityPatchRequest {
    /// Required, not `Option`: a `PATCH` that changes nothing is a request whose
    /// `If-Match` precondition and `Idempotency-Key` ledger entry describe a no-op, and
    /// the console has no reason to send one.
    pub is_primary: bool,
}

/// Which of the two mutually exclusive constraints an invite carries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminInviteConstraint {
    /// The invite may only be redeemed by exactly this verified email address.
    Email,
    /// The invite may be redeemed by any verified address in this domain.
    Domain,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminInviteStatus {
    Pending,
    Consumed,
    Revoked,
}

/// Request body of `POST /api/v1/admin/admin-invites`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminInviteCreateRequest {
    pub constraint: AdminInviteConstraint,
    /// The email address (for `constraint = "email"`) or bare domain (for
    /// `constraint = "domain"`) the invite is bound to. Never "anyone with the link":
    /// an unbound invite would make a leaked URL equivalent to handing out admin.
    pub value: String,
    /// Clamped server-side by [`MAX_INVITE_EXPIRY_SECONDS`], as a **hard cap** rather
    /// than a UI default — a client that asks for a year is refused, not quietly
    /// honoured.
    pub expires_in_seconds: u32,
}

/// The invite as it is safe to return **after** creation: no token, no hash, no prefix.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminInviteRecord {
    pub id: Uuid,
    pub constraint: AdminInviteConstraint,
    pub value: String,
    pub status: AdminInviteStatus,
    /// Derived, not stored: `status` has no `'expired'` value, because nothing sweeps
    /// for it. A `pending` invite past `expires_at` reads as `expired = true` here and
    /// is refused at redemption.
    pub expired: bool,
    pub expires_at: DateTime<Utc>,
    pub created_by_subject: Option<String>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub consumed_subject: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

/// The once-only envelope for a freshly minted invite token.
///
/// Field-for-field the shape of [`crate::domain::ApiKeySecretResponse`] — `resource`,
/// `secret`, `secret_retrievable` — so `once_only_key_responses_use_the_secret_envelope`
/// covers this operation with the same assertions it makes about API keys, rather than a
/// second envelope that would have to be remembered separately.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminInviteSecretResponse {
    pub resource: AdminInviteRecord,
    /// The raw token. Returned exactly once, at creation, and `None` on an idempotent
    /// replay — the stored replay body is the sanitized record.
    pub secret: Option<String>,
    pub secret_retrievable: bool,
    /// i18n envelope for the success message (CONVENTIONS §4.2).
    pub notice: ResponseText,
}

/// Request body of `POST /api/v1/admin/admin-invites/preview`.
///
/// `POST` with the token in the **body**, never a `GET` with it in the path or query, so
/// it cannot land in an access log, a proxy log, or a `Referer` chain.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminInvitePreviewRequest {
    pub token: String,
}

/// What an **unauthenticated** invitee is told about an invite they hold the token for.
///
/// Deliberately minimal. It carries no inviter identity, no invite id, no deployment
/// detail and no policy: everything here is either something the holder of the token
/// needs in order to act (which address to sign in with, how long they have) or
/// something they already know. Plan 09's body proposes a masked inviter email; that is
/// not built, because "who invited you" is information the invitee learns out of band
/// with the link itself, and adding it to an anonymous response buys nothing the flow
/// needs. Reversal condition: if product wants inviter attribution, it arrives with its
/// own masking function and its own leak test.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminInvitePreviewResponse {
    pub constraint: AdminInviteConstraint,
    pub value: String,
    pub expires_at: DateTime<Utc>,
}

/// Request body of `POST /api/v1/admin/admin-invites/redeem`.
///
/// Bound to plan 07's post-D5 [`ClaimAdminIdentityRequest`] shape: `email` and
/// `email_verified` are **required and non-optional** — no `Option`, no
/// `#[serde(default)]` — because redemption creates the same `admin_identities` grant,
/// and a grant with no human-identifiable attribute makes the domain policy
/// unenforceable on that path.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminInviteRedeemRequest {
    pub token: String,
    /// Asserted by the BFF from the just-verified session, on **every** redemption.
    /// There is no optional-email path and no conditional branch that omits it.
    pub email: String,
    /// Required, and with no `#[serde(default)]`, for the same reason
    /// [`ClaimAdminIdentityRequest::email_verified`] has none: a default would turn an
    /// omitted field into a misleading "your email is not verified" 403.
    pub email_verified: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminIdentityStatus {
    Active,
    Revoked,
}

fn default_admin_grant_scopes() -> Vec<String> {
    vec!["moira:admin".to_string()]
}

/// Hard server-side ceiling on an invite's lifetime: 72 hours.
///
/// A **cap**, not a default — plan 09's own risk analysis is that a leaked,
/// still-valid, constraint-matching link is equivalent to handing out full
/// `moira:admin`, so the window has to be bounded by the server rather than by whatever
/// the console's form happens to send. A request above it is refused with
/// `admin_invite_expiry_too_long` rather than silently clamped: an operator who thinks
/// they issued a 30-day invite and actually issued a 3-day one will discover it at the
/// worst possible moment.
///
/// **Needs product sign-off**: 72 h is plan 09's recommendation, not a measured value.
pub const MAX_INVITE_EXPIRY_SECONDS: u32 = 72 * 60 * 60;

/// Floor on an invite's lifetime, so a zero or one-second expiry cannot be created and
/// then reported as a mysterious `invite_expired` at redemption time.
pub const MIN_INVITE_EXPIRY_SECONDS: u32 = 60;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_request_defaults_scopes_to_admin_only() {
        let request: ClaimAdminIdentityRequest = serde_json::from_str(
            r#"{"issuer":"https://issuer.example","subject":"sub-1",
                "email":"admin@example.com","email_verified":true}"#,
        )
        .expect("claim request without scopes deserializes");

        assert_eq!(request.scopes, vec!["moira:admin".to_string()]);
        assert!(request.setup_token.is_none());
    }

    /// Dropping `#[serde(default)]` from `email_verified` is what turns an omitted field
    /// into the schema violation it is, instead of a silent `false` that later surfaces as
    /// a misleading "your email is not verified" 403.
    #[test]
    fn claim_request_rejects_an_omitted_email_verified() {
        let error = serde_json::from_str::<ClaimAdminIdentityRequest>(
            r#"{"issuer":"https://issuer.example","subject":"sub-1",
                "email":"admin@example.com"}"#,
        )
        .expect_err("email_verified is required");

        assert!(error.to_string().contains("email_verified"));
    }

    #[test]
    fn claim_request_rejects_an_omitted_email() {
        let error = serde_json::from_str::<ClaimAdminIdentityRequest>(
            r#"{"issuer":"https://issuer.example","subject":"sub-1","email_verified":true}"#,
        )
        .expect_err("email is required");

        assert!(error.to_string().contains("email"));
    }

    /// `deny_unknown_fields` is what makes a stale client fail loudly rather than believe
    /// Moira stored something it dropped.
    #[test]
    fn claim_request_rejects_unknown_fields() {
        let error = serde_json::from_str::<ClaimAdminIdentityRequest>(
            r#"{"issuer":"https://issuer.example","subject":"sub-1",
                "email":"admin@example.com","email_verified":true,"password":"hunter2"}"#,
        )
        .expect_err("unknown fields are refused");

        assert!(error.to_string().contains("password"));
    }

    /// The deferred setup-token field still parses, so plan 08's generated client
    /// typechecks. Refusing it is the handler's job, and it must refuse rather than ignore.
    #[test]
    fn claim_request_still_accepts_the_reserved_setup_token_field_at_the_schema_level() {
        let request: ClaimAdminIdentityRequest = serde_json::from_str(
            r#"{"issuer":"https://issuer.example","subject":"sub-1",
                "email":"admin@example.com","email_verified":true,"setup_token":"tok"}"#,
        )
        .expect("reserved field remains part of the schema");

        assert_eq!(request.setup_token.as_deref(), Some("tok"));
    }

    #[test]
    fn admin_identity_status_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&AdminIdentityStatus::Revoked).expect("status serializes"),
            "\"revoked\""
        );
    }

    // -----------------------------------------------------------------------
    // Plan 09 wave 2 — invitation DTOs.
    //
    // Decision D5 is inherited verbatim, so the redeem body's tests are the claim
    // body's tests: the point is that the two shapes cannot drift apart.
    // -----------------------------------------------------------------------

    #[test]
    fn redeem_request_requires_email_and_email_verified() {
        for body in [
            r#"{"token":"moira_inv_x","email_verified":true}"#,
            r#"{"token":"moira_inv_x","email":"colleague@example.com"}"#,
        ] {
            serde_json::from_str::<AdminInviteRedeemRequest>(body)
                .expect_err("email and email_verified are both required on every redemption");
        }

        let request: AdminInviteRedeemRequest = serde_json::from_str(
            r#"{"token":"moira_inv_x","email":"colleague@example.com","email_verified":true}"#,
        )
        .expect("a complete redeem body deserializes");
        assert!(request.email_verified);
    }

    /// The D5 regression this guards is a contributor "simplifying a call site" by
    /// making either field optional. Asserting on the *type* rather than on a parse
    /// failure is what makes `Option<String>` a compile error here rather than a
    /// behaviour change nobody notices.
    #[test]
    fn redeem_request_email_fields_are_not_optional_types() {
        fn assert_required(_: &String, _: &bool) {}
        let request = AdminInviteRedeemRequest {
            token: "moira_inv_x".to_string(),
            email: "colleague@example.com".to_string(),
            email_verified: true,
        };
        assert_required(&request.email, &request.email_verified);
    }

    #[test]
    fn redeem_request_rejects_unknown_fields() {
        let error = serde_json::from_str::<AdminInviteRedeemRequest>(
            r#"{"token":"t","email":"a@b.test","email_verified":true,"scope":"moira:admin"}"#,
        )
        .expect_err("a self-asserted scope must not be silently dropped");
        assert!(error.to_string().contains("scope"));
    }

    #[test]
    fn preview_request_rejects_unknown_fields_and_requires_the_token() {
        serde_json::from_str::<AdminInvitePreviewRequest>(r#"{}"#)
            .expect_err("the token is the credential; it is required");
        serde_json::from_str::<AdminInvitePreviewRequest>(r#"{"token":"t","email":"a@b.test"}"#)
            .expect_err("preview takes the token and nothing else");
    }

    #[test]
    fn invite_create_request_round_trips_both_constraint_kinds() {
        let email: AdminInviteCreateRequest = serde_json::from_str(
            r#"{"constraint":"email","value":"colleague@example.com","expires_in_seconds":3600}"#,
        )
        .expect("an email-constrained invite deserializes");
        assert_eq!(email.constraint, AdminInviteConstraint::Email);

        let domain: AdminInviteCreateRequest = serde_json::from_str(
            r#"{"constraint":"domain","value":"example.com","expires_in_seconds":3600}"#,
        )
        .expect("a domain-constrained invite deserializes");
        assert_eq!(domain.constraint, AdminInviteConstraint::Domain);

        serde_json::from_str::<AdminInviteCreateRequest>(
            r#"{"constraint":"anyone","value":"","expires_in_seconds":3600}"#,
        )
        .expect_err("there is no unbound invite kind");
    }

    /// The cap is a *contract* value the console renders and the service enforces, so it
    /// is pinned rather than left as a magic number two files can disagree about.
    #[test]
    fn the_invite_expiry_bounds_are_the_documented_ones() {
        assert_eq!(MAX_INVITE_EXPIRY_SECONDS, 259_200, "72 hours");
        assert_eq!(MIN_INVITE_EXPIRY_SECONDS, 60);
        assert!(MIN_INVITE_EXPIRY_SECONDS < MAX_INVITE_EXPIRY_SECONDS);
    }

    /// The preview response is the one body an **anonymous** caller can obtain from this
    /// surface. Its field set is therefore an assertion, not a description: anything
    /// added here is served to whoever holds the token and to nobody else, and this test
    /// is where a widening has to be argued for.
    #[test]
    fn the_anonymous_preview_response_carries_only_constraint_and_expiry() {
        let body = serde_json::to_value(AdminInvitePreviewResponse {
            constraint: AdminInviteConstraint::Domain,
            value: "example.com".to_string(),
            expires_at: DateTime::<Utc>::MIN_UTC,
        })
        .expect("preview response serializes");
        let mut fields: Vec<&str> = body
            .as_object()
            .expect("preview response is an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(fields, ["constraint", "expires_at", "value"]);
    }

    #[test]
    fn the_invite_secret_envelope_matches_the_api_key_envelope_field_names() {
        let body = serde_json::to_value(AdminInviteSecretResponse {
            resource: AdminInviteRecord {
                id: Uuid::nil(),
                constraint: AdminInviteConstraint::Email,
                value: "colleague@example.com".to_string(),
                status: AdminInviteStatus::Pending,
                expired: false,
                expires_at: DateTime::<Utc>::MIN_UTC,
                created_by_subject: None,
                consumed_at: None,
                consumed_subject: None,
                created_at: DateTime::<Utc>::MIN_UTC,
                version: 1,
            },
            secret: Some("moira_inv_xyz".to_string()),
            secret_retrievable: true,
            notice: ResponseText::new("moira.notice.admin_invite_created", "created"),
        })
        .expect("invite secret response serializes");
        for field in ["resource", "secret", "secret_retrievable"] {
            assert!(
                body.get(field).is_some(),
                "the once-only envelope must carry {field}, like ApiKeySecretResponse"
            );
        }
    }

    /// The stored invite record must never carry the token, its hash, or its prefix —
    /// the sanitized shape is what a list, a get, and an idempotent replay all return.
    #[test]
    fn the_invite_record_carries_no_secret_material() {
        let body = serde_json::to_value(AdminInviteRecord {
            id: Uuid::nil(),
            constraint: AdminInviteConstraint::Domain,
            value: "example.com".to_string(),
            status: AdminInviteStatus::Pending,
            expired: false,
            expires_at: DateTime::<Utc>::MIN_UTC,
            created_by_subject: Some("owner".to_string()),
            consumed_at: None,
            consumed_subject: None,
            created_at: DateTime::<Utc>::MIN_UTC,
            version: 1,
        })
        .expect("invite record serializes");
        let object = body.as_object().expect("invite record is an object");
        for forbidden in [
            "token",
            "secret",
            "token_hash",
            "token_prefix",
            "fingerprint",
            "pepper_version",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "the invite record must not expose {forbidden}"
            );
        }
    }

    #[test]
    fn the_ownership_patch_request_has_exactly_one_field() {
        serde_json::from_str::<AdminIdentityPatchRequest>(r#"{}"#)
            .expect_err("is_primary is required");
        serde_json::from_str::<AdminIdentityPatchRequest>(
            r#"{"is_primary":true,"granted_scopes":["moira:admin"]}"#,
        )
        .expect_err("ownership transfer must not become a second scope-editing surface");
        let request: AdminIdentityPatchRequest =
            serde_json::from_str(r#"{"is_primary":false}"#).expect("a minimal patch deserializes");
        assert!(!request.is_primary);
    }
}
