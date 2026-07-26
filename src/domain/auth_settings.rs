//! Runtime auth-provider settings DTOs (plan 07 module 3).
//!
//! Which auth methods a deployment offers, and with what policy, is runtime configuration
//! owned by Moira's database (CONVENTIONS §7.2) — the same place providers, models, routing
//! and credentials already live — not build-time environment.
//!
//! # Decision D7: these DTOs carry no secret material of any kind
//!
//! There is no `client_secret` field on any request, no envelope fields on the record, no
//! `secret_fingerprint`, no `masked_secret`, and no `RotateAuthProviderSecretRequest`. The
//! OAuth client secret is owned by the console and stored in the console's own
//! `console_auth` database: Better Auth needs the plaintext in-process to run the
//! authorization-code exchange, and Moira's secret envelope is deliberately write-only, so
//! exposing the secret over HTTP would break the invariant that a decrypted secret never
//! crosses a network boundary.
//!
//! Consequently, unlike [`crate::domain::admin::CredentialRecord`], **no
//! `#[serde(skip_serializing)]` / `#[schema(ignore)]` hiding appears below** — there is
//! nothing to hide, and adding the pattern here would only imply a secret exists. That
//! pattern remains correct and required for provider credentials (the AI-provider API
//! keys), which D7 does not touch.
//!
//! Both request DTOs are `deny_unknown_fields`, so a console still sending `client_secret`
//! is rejected loudly with a schema error rather than silently accepted and dropped. That
//! is deliberate: it makes a stale client fail fast instead of believing Moira stored a
//! secret it never stored.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::admin::ResourceStatus;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    GoogleOauth,
    GenericOidc,
    Jwks,
}

/// One configured auth method, as returned by the `/api/v1/admin/auth/providers` surface.
///
/// Every field is non-secret configuration and is serialized normally.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthProviderSettingsRecord {
    pub id: Uuid,
    pub method: AuthMethod,
    pub display_name: String,
    pub enabled: bool,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub jwks_url: Option<String>,
    /// Non-secret. Always returned. This is the value the console fingerprints and compares
    /// against its own stored fingerprint for D7 drift protection — a `client_id` changed
    /// in Moira while the console still holds the old client's secret would otherwise fail
    /// the code exchange with an opaque provider error. Exposing it on the read path is the
    /// whole of Moira's obligation there; no extra endpoint, header, or
    /// fingerprint-computation is required, and Moira deliberately does not store the
    /// console's fingerprint.
    pub client_id: Option<String>,
    pub requested_scopes: Vec<String>,
    pub allowed_email_domains: Vec<String>,
    pub allowed_algorithms: Vec<String>,
    pub expected_audiences: Vec<String>,
    pub redirect_uris: Vec<String>,
    pub trusted_jwt_issuer_id: Option<Uuid>,
    pub metadata: Value,
    pub status: ResourceStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

/// Request body of `POST /api/v1/admin/auth/providers`.
///
/// Defaults mirror the column defaults in `migrations/0013_auth_provider_settings.sql`, so
/// a row created through this DTO and a row created by raw insert agree.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthProviderSettingsCreateRequest {
    pub method: AuthMethod,
    pub display_name: String,
    #[serde(default)]
    pub enabled: bool,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub jwks_url: Option<String>,
    /// Non-secret. There is no companion `client_secret` field, and there never will be
    /// (D7) — the secret belongs to the console's own store.
    pub client_id: Option<String>,
    #[serde(default = "default_requested_scopes")]
    pub requested_scopes: Vec<String>,
    /// Deny-by-default: an empty list refuses every claim. Operators configure this
    /// **before** the first claim, or every claim is refused.
    #[serde(default)]
    pub allowed_email_domains: Vec<String>,
    #[serde(default = "default_allowed_algorithms")]
    pub allowed_algorithms: Vec<String>,
    #[serde(default)]
    pub expected_audiences: Vec<String>,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    pub trusted_jwt_issuer_id: Option<Uuid>,
    #[serde(default)]
    pub metadata: Value,
}

/// Request body of `PATCH /api/v1/admin/auth/providers/{id}`.
///
/// Every field is optional; an absent field leaves the stored value alone. `issuer` and
/// `client_id` are ordinary mutable configuration here — under D7 no secret is bound to
/// them, so there is no rebind-or-rotate rule and no `409` on changing either.
#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthProviderSettingsPatchRequest {
    pub display_name: Option<String>,
    pub enabled: Option<bool>,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub jwks_url: Option<String>,
    pub client_id: Option<String>,
    pub requested_scopes: Option<Vec<String>>,
    pub allowed_email_domains: Option<Vec<String>>,
    pub allowed_algorithms: Option<Vec<String>>,
    pub expected_audiences: Option<Vec<String>>,
    pub redirect_uris: Option<Vec<String>>,
    pub trusted_jwt_issuer_id: Option<Uuid>,
    pub metadata: Option<Value>,
}

/// Response body of `GET /api/v1/admin/setup/auth-methods`.
///
/// Authenticated, not anonymous (decision D4): the response is identity *configuration* —
/// which IdP, which issuer, which client id, which allowed domains — and that is
/// reconnaissance-worthy even though D7 removed the secret. Plan 08's console calls it
/// server-side with the system key it already holds; the browser never sees it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupAuthMethodsResponse {
    pub methods: Vec<PublicAuthMethod>,
}

/// A deliberately narrower projection of [`AuthProviderSettingsRecord`] for the bootstrap
/// read.
///
/// It must never gain a field carrying secret material. Under D7 no such field exists on
/// the source record either, so the guard is a *forward* one against reintroduction rather
/// than a redaction check.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicAuthMethod {
    pub id: Uuid,
    pub method: AuthMethod,
    pub display_name: String,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    pub jwks_url: Option<String>,
    /// Non-secret. The D7 drift-protection anchor: the console reads this on boot and
    /// compares its fingerprint against the one stored beside its own client secret.
    /// Sufficient on its own — plan 08 needs nothing more.
    pub client_id: Option<String>,
    pub requested_scopes: Vec<String>,
    pub allowed_email_domains: Vec<String>,
}

fn default_requested_scopes() -> Vec<String> {
    vec![
        "openid".to_string(),
        "email".to_string(),
        "profile".to_string(),
    ]
}

fn default_allowed_algorithms() -> Vec<String> {
    vec!["RS256".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_method_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&AuthMethod::GoogleOauth).expect("method serializes"),
            "\"google_oauth\""
        );
        assert_eq!(
            serde_json::to_string(&AuthMethod::GenericOidc).expect("method serializes"),
            "\"generic_oidc\""
        );
        assert_eq!(
            serde_json::to_string(&AuthMethod::Jwks).expect("method serializes"),
            "\"jwks\""
        );
    }

    /// The DTO defaults must agree with the column defaults in
    /// `migrations/0013_auth_provider_settings.sql`, or a row created through the API and a
    /// row created by raw insert would disagree on policy.
    #[test]
    fn create_request_defaults_match_the_migration_column_defaults() {
        let request: AuthProviderSettingsCreateRequest = serde_json::from_str(
            r#"{"method":"google_oauth","display_name":"Google","client_id":"cid",
                "issuer":"https://accounts.google.com"}"#,
        )
        .expect("create request with only required fields deserializes");

        assert!(!request.enabled);
        assert_eq!(
            request.requested_scopes,
            vec![
                "openid".to_string(),
                "email".to_string(),
                "profile".to_string()
            ]
        );
        assert_eq!(request.allowed_algorithms, vec!["RS256".to_string()]);
        assert!(request.allowed_email_domains.is_empty());
        assert!(request.expected_audiences.is_empty());
        assert!(request.redirect_uris.is_empty());
    }

    /// D7's fail-loud contract: a stale console still sending `client_secret` must be
    /// refused, never silently accepted with the field dropped.
    #[test]
    fn create_request_rejects_a_client_secret() {
        let error = serde_json::from_str::<AuthProviderSettingsCreateRequest>(
            r#"{"method":"google_oauth","display_name":"Google","client_id":"cid",
                "client_secret":"shhh"}"#,
        )
        .expect_err("client_secret is not a field Moira accepts");

        assert!(error.to_string().contains("client_secret"));
    }

    #[test]
    fn patch_request_rejects_a_client_secret() {
        let error =
            serde_json::from_str::<AuthProviderSettingsPatchRequest>(r#"{"client_secret":"shhh"}"#)
                .expect_err("client_secret is not a field Moira accepts");

        assert!(error.to_string().contains("client_secret"));
    }

    /// A forward guard against reintroduction: the bootstrap projection is defined by what
    /// it lists, and nothing secret-shaped may ever appear in its serialized form.
    #[test]
    fn public_auth_method_never_exposes_secret_fields() {
        let method = PublicAuthMethod {
            id: Uuid::nil(),
            method: AuthMethod::GoogleOauth,
            display_name: "Google".to_string(),
            issuer: Some("https://accounts.google.com".to_string()),
            discovery_url: None,
            authorization_url: None,
            jwks_url: None,
            client_id: Some("cid".to_string()),
            requested_scopes: default_requested_scopes(),
            allowed_email_domains: vec!["example.com".to_string()],
        };

        let json = serde_json::to_value(&method).expect("projection serializes");
        let object = json.as_object().expect("projection is an object");

        for key in object.keys() {
            assert!(
                !key.contains("secret")
                    && !key.contains("encrypted")
                    && !key.contains("nonce")
                    && !key.contains("token"),
                "PublicAuthMethod gained a secret-shaped field: {key}"
            );
        }
    }
}
