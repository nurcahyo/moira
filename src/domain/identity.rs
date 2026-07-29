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
    pub status: AdminIdentityStatus,
    pub created_at: DateTime<Utc>,
    pub version: i64,
    /// i18n envelope for the success message (CONVENTIONS §4.2).
    pub notice: ResponseText,
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
}
