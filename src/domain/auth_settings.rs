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

/// How a deployment authenticates the humans who may hold Moira admin.
///
/// # `github_oauth` is a fourth variant, not a `provider_id`-keyed generic row (plan 09 W4-D1)
///
/// `AuthMethod` is already the discriminator in `0013`'s SQL CHECK, in the shape validator,
/// in the DB encoder and in the committed spec, so adding a variant lights up three
/// compile-time stops — [`PublicSignInMethod::from_enabled_method`],
/// `validate_method_shape` and `auth_method_to_db` — each of which forces an explicit
/// GitHub answer at exactly the right place. A second discriminator alongside `method`
/// would have left all three matches untouched, which is the same as having no forcing
/// function at all.
///
/// GitHub OAuth is **not** OIDC: no discovery document, no `id_token`, no issuer. Its shape
/// branch is `client_id` + `authorization_url` + `token_url`, with `issuer` and
/// `discovery_url` required to be absent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    GoogleOauth,
    GenericOidc,
    Jwks,
    GithubOauth,
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
///
/// D4 stands. [`SetupSignInMethodsResponse`] is the anonymous surface a login screen calls
/// instead, and it is *narrower* than this one rather than the same thing with the gate
/// removed — most importantly it drops `allowed_email_domains`, the deny-by-default
/// admin-claim policy, which is the single field that makes this response unsafe to publish.
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

/// Response body of the **anonymous** `GET /api/v1/admin/setup/sign-in-methods`.
///
/// # Why this exists instead of relaxing `/setup/auth-methods` (finding F15)
///
/// A console cannot render a sign-in button without knowing which methods a deployment
/// offers, and it cannot hold a bearer JWT before someone has signed in. Plan 09's public
/// `/invite/[token]` page makes that blocking: its visitor is unauthenticated by
/// construction. An operator who removes `MOIRA_SYSTEM_KEY` after setup — the normal fate of
/// a bootstrap credential — would otherwise be locked out entirely.
///
/// The obvious fix, serving [`SetupAuthMethodsResponse`] anonymously, was **rejected**:
/// [`PublicAuthMethod`] carries `allowed_email_domains`, which is not configuration a login
/// screen needs but *the* deny-by-default admin-claim policy (plan 07 decision D3). Publishing
/// it anonymously would hand any unauthenticated caller the exact list of email domains that
/// can obtain Moira admin — a ready-made phishing target list — for no rendering benefit.
/// Decision **D4** made `/setup/auth-methods` authenticated on information-content grounds and
/// that judgement stands unchanged for that endpoint.
///
/// # The rule that defines this projection
///
/// **Every field here is something the browser itself already transmits or receives while
/// signing in.** `client_id`, `issuer`, `authorization_url` and `requested_scopes` all appear
/// in the OAuth authorization URL the browser is about to be redirected to; `discovery_url` is
/// the world-readable `.well-known` document derivable from `issuer`. Nothing here tells an
/// anonymous caller anything it could not learn by clicking the button.
///
/// Anything that fails that rule does not belong here. `allowed_email_domains` fails it
/// (policy, never on the wire to a browser) and `jwks_url` fails it (machine token-verification
/// configuration, irrelevant to a sign-in button).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupSignInMethodsResponse {
    pub methods: Vec<PublicSignInMethod>,
}

/// One renderable sign-in button, and nothing else.
///
/// Strictly narrower than [`PublicAuthMethod`] — see [`SetupSignInMethodsResponse`] for the
/// rule every field must satisfy. It is served anonymously, so a field added here is a field
/// published to the internet: add one only if the browser would already have seen it during a
/// sign-in.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicSignInMethod {
    pub id: Uuid,
    pub method: AuthMethod,
    pub display_name: String,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    /// Non-secret, and specifically not confidential: a `client_id` appears in every OAuth
    /// redirect URL a browser sends. Moira stores no `client_secret` at all (D7).
    pub client_id: Option<String>,
    pub requested_scopes: Vec<String>,
}

impl PublicSignInMethod {
    /// Narrows an enabled method to its login-screen projection, or `None` if it is not a
    /// method a human signs in with.
    ///
    /// [`AuthMethod::Jwks`] is machine-to-machine token verification: it has no
    /// `authorization_url` and no `client_id`, so a console that rendered it as a button would
    /// produce a control that cannot work. Filtering here rather than in the caller keeps the
    /// "is this a sign-in method" judgement in one place.
    ///
    /// [`AuthMethod::GithubOauth`] **is** a browser sign-in method and joins the `Some` arm.
    /// It carries no `issuer` and no `discovery_url` — both are null by
    /// `auth_provider_settings_method_shape` — and needs neither: everything a GitHub button
    /// requires is `authorization_url`, `client_id` and `requested_scopes`, all of which this
    /// projection already carries. **No field is added here for GitHub.** The projection is
    /// served anonymously, and F15's admitting rule (see [`SetupSignInMethodsResponse`]) is
    /// what decides what may appear, not what a caller would find convenient.
    pub fn from_enabled_method(method: &PublicAuthMethod) -> Option<Self> {
        match method.method {
            AuthMethod::GoogleOauth | AuthMethod::GenericOidc | AuthMethod::GithubOauth => {
                Some(Self {
                    id: method.id,
                    method: method.method,
                    display_name: method.display_name.clone(),
                    issuer: method.issuer.clone(),
                    discovery_url: method.discovery_url.clone(),
                    authorization_url: method.authorization_url.clone(),
                    client_id: method.client_id.clone(),
                    requested_scopes: method.requested_scopes.clone(),
                })
            }
            AuthMethod::Jwks => None,
        }
    }
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
        assert_eq!(
            serde_json::to_string(&AuthMethod::GithubOauth).expect("method serializes"),
            "\"github_oauth\""
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

    /// The positive counterpart to the guard above: `client_id` is not merely absent of
    /// a secret shape, it is **present** and carries the value it was given. This is the
    /// whole of Moira's D7 drift-protection obligation — the console fingerprints this
    /// field, so a projection that dropped it would silently break plan 08 with no
    /// signal on Moira's side.
    #[test]
    fn public_auth_method_exposes_client_id() {
        let method = PublicAuthMethod {
            id: Uuid::nil(),
            method: AuthMethod::GoogleOauth,
            display_name: "Google".to_string(),
            issuer: Some("https://accounts.google.com".to_string()),
            discovery_url: None,
            authorization_url: None,
            jwks_url: None,
            client_id: Some("console-client".to_string()),
            requested_scopes: default_requested_scopes(),
            allowed_email_domains: vec!["example.com".to_string()],
        };

        let json = serde_json::to_value(&method).expect("projection serializes");
        assert_eq!(
            json["client_id"],
            serde_json::json!("console-client"),
            "client_id must survive the narrow bootstrap projection"
        );
    }

    fn enabled_method(method: AuthMethod) -> PublicAuthMethod {
        PublicAuthMethod {
            id: Uuid::nil(),
            method,
            display_name: "Google".to_string(),
            issuer: Some("https://accounts.google.com".to_string()),
            discovery_url: None,
            authorization_url: Some("https://accounts.google.com/o/oauth2/v2/auth".to_string()),
            jwks_url: Some("https://accounts.google.com/jwks".to_string()),
            client_id: Some("console-client".to_string()),
            requested_scopes: default_requested_scopes(),
            allowed_email_domains: vec!["example.com".to_string()],
        }
    }

    /// The whole reason `PublicSignInMethod` exists rather than `PublicAuthMethod` being
    /// served anonymously (F15): the deny-by-default domain policy must not reach an
    /// unauthenticated caller. Asserted as an exact key set, not a `contains` check, so a
    /// field added to the anonymous projection fails here before it reaches the internet.
    #[test]
    fn the_anonymous_projection_drops_the_domain_policy_and_the_jwks_url() {
        let method =
            PublicSignInMethod::from_enabled_method(&enabled_method(AuthMethod::GoogleOauth))
                .expect("google_oauth is a sign-in method");

        let json = serde_json::to_value(&method).expect("projection serializes");
        let object = json.as_object().expect("projection is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "authorization_url",
                "client_id",
                "discovery_url",
                "display_name",
                "id",
                "issuer",
                "method",
                "requested_scopes",
            ],
            "the anonymous projection gained or lost a field"
        );
        assert!(
            !object.contains_key("allowed_email_domains"),
            "allowed_email_domains is the deny-by-default admin-claim policy; publishing it \
             anonymously hands out a phishing target list"
        );
        // Every surviving field is one the browser already sees in the authorization URL, so
        // each must actually carry its value — a projection that silently dropped `client_id`
        // would render a button that cannot start a flow.
        assert_eq!(object["client_id"], serde_json::json!("console-client"));
    }

    /// A `jwks` row is machine token-verification configuration with no `authorization_url`
    /// and no `client_id`. Rendering it as a button would produce a control that cannot work,
    /// so it is not a sign-in method at all.
    ///
    /// Spelled as an exhaustive walk over [`AuthMethod`] rather than as three hand-picked
    /// cases: a fifth variant added without an answer here fails to compile, which is the
    /// forcing function W4-D1 chose the enum for. An array literal would have compiled
    /// happily and left the new variant untested.
    #[test]
    fn the_anonymous_projection_excludes_jwks_rows() {
        for method in [
            AuthMethod::GoogleOauth,
            AuthMethod::GenericOidc,
            AuthMethod::Jwks,
            AuthMethod::GithubOauth,
        ] {
            let projected = PublicSignInMethod::from_enabled_method(&enabled_method(method));
            let expected_interactive = match method {
                AuthMethod::GoogleOauth | AuthMethod::GenericOidc | AuthMethod::GithubOauth => true,
                AuthMethod::Jwks => false,
            };
            assert_eq!(
                projected.is_some(),
                expected_interactive,
                "{method:?} is on the wrong side of the sign-in filter"
            );
        }
    }

    /// **`PublicSignInMethod` gains no field for GitHub.**
    ///
    /// A GitHub row has `issuer: null` and `discovery_url: null` by
    /// `auth_provider_settings_method_shape`, and everything a button needs is already
    /// projected. Asserted as an exact key set on a GitHub-shaped row, so a field added
    /// "for GitHub" fails here before it is published to anonymous callers — the same gate
    /// [`the_anonymous_projection_drops_the_domain_policy_and_the_jwks_url`] applies to the
    /// OIDC shape, applied to the one wave 4 adds.
    #[test]
    fn the_github_projection_adds_no_field_and_carries_no_issuer() {
        let mut source = enabled_method(AuthMethod::GithubOauth);
        source.issuer = None;
        source.discovery_url = None;
        source.authorization_url = Some("https://github.com/login/oauth/authorize".to_string());

        let method = PublicSignInMethod::from_enabled_method(&source)
            .expect("github_oauth is a sign-in method");
        let json = serde_json::to_value(&method).expect("projection serializes");
        let object = json.as_object().expect("projection is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "authorization_url",
                "client_id",
                "discovery_url",
                "display_name",
                "id",
                "issuer",
                "method",
                "requested_scopes",
            ],
            "the anonymous projection gained a field for GitHub"
        );
        assert_eq!(object["issuer"], serde_json::Value::Null);
        assert_eq!(object["discovery_url"], serde_json::Value::Null);
        assert_eq!(
            object["authorization_url"],
            serde_json::json!("https://github.com/login/oauth/authorize"),
            "a GitHub button is unrenderable without its authorization URL"
        );
    }

    /// The same forward guard [`public_auth_method_never_exposes_secret_fields`] applies to
    /// the bootstrap projection, applied here to the projection that is served with **no**
    /// credential at all — where a secret-shaped field would be published to the internet.
    #[test]
    fn public_sign_in_method_never_exposes_secret_fields() {
        let method =
            PublicSignInMethod::from_enabled_method(&enabled_method(AuthMethod::GoogleOauth))
                .expect("google_oauth is a sign-in method");

        let json = serde_json::to_value(&method).expect("projection serializes");
        let object = json.as_object().expect("projection is an object");

        for key in object.keys() {
            assert!(
                !key.contains("secret")
                    && !key.contains("encrypted")
                    && !key.contains("nonce")
                    && !key.contains("token"),
                "PublicSignInMethod gained a secret-shaped field: {key}"
            );
        }
    }
}
