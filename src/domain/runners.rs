//! Containerised Claude runner DTOs (issue #275, workstream R2 of #272).
//!
//! # The token has no field here, on purpose
//!
//! A runner's whole purpose is to mint a Claude subscription token inside a container that has
//! a real pty. Moira fetches that token from `moira-runner`'s one-shot
//! `GET /v1/runners/{id}/token` and hands it straight to the existing credential chain
//! (`AdminService::create_credential`: AAD build, `cipher.encrypt`, fingerprint, mask, audit
//! row). It is never returned to a caller.
//!
//! There is therefore **no token field on any type in this file**, and none may be added.
//! [`ClaudeRunnerRecord`] carries a `credential_id` — a *reference* to the row the token became
//! — and nothing more. Unlike [`crate::domain::admin::CredentialRecord`] there is no
//! `#[serde(skip_serializing)]` hiding here either, for the reason `auth_settings.rs` records
//! about D7: there is nothing to hide, and adding the pattern would imply a secret exists.
//!
//! `authorization_url` looks secret-adjacent and is not: it is the public
//! `https://claude.com/cai/oauth/authorize?…` URL the operator opens in their own browser. It
//! carries a PKCE challenge and a state parameter, neither of which is a credential, and the
//! operator cannot complete the flow without it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::admin::{CredentialScope, CredentialType};

/// Where a runner is in its lifecycle.
///
/// The first six are `moira-runner`'s own contract states, mirrored verbatim so an operator
/// reading Moira's console and the runner service's logs sees one vocabulary. [`Self::Linked`]
/// is Moira-only: it means the token was fetched and stored as a provider credential, which the
/// runner service has no way to know.
///
/// The `ready -> linked` edge is the one that matters for correctness. `GET /v1/runners/{id}/token`
/// is **one-shot** — a second call answers `410 token_already_retrieved` — so a row still sitting
/// at `Ready` after a finalize attempt is a row whose token was fetched and *not* persisted, and
/// whose runner can never yield it again. That is why finalize writes the row only after the
/// credential exists, and why a failed finalize is reported rather than retried silently.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeRunnerState {
    Provisioning,
    AwaitingAuthorization,
    Exchanging,
    Ready,
    Linked,
    Failed,
    Expired,
}

impl ClaudeRunnerState {
    /// Whether a live refresh against the runner service could still change this state.
    ///
    /// `Linked`, `Failed` and `Expired` are terminal, so a console poll on one of them is
    /// answered from Moira's mirror with no outbound call at all — which is also what keeps a
    /// dashboard left open overnight from holding a connection to the runner service per row.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Linked | Self::Failed | Self::Expired)
    }
}

/// One runner, as the `/api/v1/admin/runners` surface serves it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClaudeRunnerRecord {
    pub id: Uuid,
    /// Operator-facing name, forwarded verbatim to the runner service.
    pub label: String,
    /// The identifier `moira-runner` minted for this runner. Opaque to Moira, and deliberately
    /// **not** a Docker container id: the runner service is the only component that knows those,
    /// and publishing one here would leak the shape of a surface Moira must not be able to
    /// address.
    pub runner_reference: String,
    pub state: ClaudeRunnerState,
    /// The public authorization URL the operator opens. `None` until the container's tty stream
    /// has rendered it.
    pub authorization_url: Option<String>,
    /// The runner service's own error code, verbatim, so it correlates with that service's logs.
    pub error_code: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    /// The provider credential this runner's token became. `None` until finalize succeeds.
    pub credential_id: Option<Uuid>,
    pub provider_id: Option<Uuid>,
    /// Whose Claude account this runner is for.
    ///
    /// `global` is the platform-wide account and is the default. A tenant that has subscribed its
    /// own account provisions at `{"type": "tenant", "external_tenant_id": "…"}`, and
    /// `PgRuntimeRepository::resolve_runtime_credential` then prefers that credential over the
    /// platform one for that tenant automatically — tenant ranks 7, global ranks 8, and nothing in
    /// resolution had to change for this.
    ///
    /// Published on every read because an operator has to be able to tell a tenant's runner from
    /// the platform's at a glance, and because the value is **not** editable afterwards: it is
    /// part of the credential AAD, so re-scoping would mean re-encrypting.
    pub scope: CredentialScope,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

/// `POST /api/v1/admin/runners`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaudeRunnerProvisionRequest {
    /// `[a-z0-9-]`, 1..=64 — the charset the runner service accepts, because the value ends up
    /// in a container name. Validated on this side too rather than relying on the remote
    /// refusal, so a bad label is a `422` naming the rule instead of a relayed `400`.
    pub label: String,
    /// How long the runner may live before the runner service's reaper force-removes it. Bounded
    /// on this side as well: a runner is an interactive login window, and one that outlives the
    /// operator's attention is a container holding a half-finished OAuth flow.
    #[serde(default = "default_ttl_seconds")]
    pub ttl_seconds: u32,
    /// Whose Claude account this runner is for. Absent means [`CredentialScope::Global`] — the
    /// platform-wide account, and the behaviour of every deployment that never sets this.
    ///
    /// A tenant connecting its own subscription sends
    /// `{"type": "tenant", "external_tenant_id": "…"}`, which is the existing
    /// [`CredentialScope`] wire shape verbatim rather than a second spelling of it.
    ///
    /// **It is fixed here and nowhere else.** The finalize request has no scope field: the
    /// credential is sealed under this scope through `credential_aad`, so a scope chosen at
    /// finalize time could disagree with the one the console has been displaying since
    /// provisioning, and re-scoping a written credential means re-encrypting it.
    #[serde(default)]
    pub scope: Option<CredentialScope>,
    #[serde(default)]
    pub metadata: Value,
}

fn default_ttl_seconds() -> u32 {
    // The contract's own example. Long enough for an operator to open a browser, sign in and
    // paste a code; short enough that an abandoned runner is gone within the quarter hour.
    900
}

/// `POST /api/v1/admin/runners/{id}/authorization-code`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaudeRunnerAuthorizationCodeRequest {
    /// The code the operator pasted out of `platform.claude.com`'s hosted redirect.
    ///
    /// This is a single-use OAuth authorization code, not a token, and it is forwarded to the
    /// runner service and dropped. It is never stored, never logged and never echoed back.
    pub code: String,
}

/// `POST /api/v1/admin/runners/{id}/finalize`.
///
/// The provider this runner's token becomes a credential for. Required, and deliberately not
/// defaulted: a token minted for a subscription has to be bound to the provider row that will
/// execute against it, and guessing that binding is how a credential ends up on the wrong
/// provider with a correct-looking audit trail.
///
/// **There is deliberately no `scope` field here**, and `deny_unknown_fields` means a client that
/// sends one is refused loudly rather than having it silently ignored. The scope is fixed at
/// provisioning time and stored on the runner row: it is part of the credential AAD, so a scope
/// supplied here could contradict the one the console has been displaying, and a written
/// credential cannot be re-scoped without re-encrypting it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaudeRunnerFinalizeRequest {
    pub provider_id: Uuid,
    pub display_name: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

impl ClaudeRunnerFinalizeRequest {
    /// The credential type a runner token is stored as.
    ///
    /// `Oauth2`, not `ApiKey`: `claude setup-token` completes an OAuth authorization-code
    /// exchange and what comes back is an OAuth access token. Storing it as an API key would
    /// make it indistinguishable from a metered key in every list, and the expiry semantics of
    /// the two differ.
    pub const CREDENTIAL_TYPE: CredentialType = CredentialType::Oauth2;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_linked_failed_and_expired_are_terminal() {
        for state in [
            ClaudeRunnerState::Linked,
            ClaudeRunnerState::Failed,
            ClaudeRunnerState::Expired,
        ] {
            assert!(state.is_terminal(), "{state:?} must be terminal");
        }
        for state in [
            ClaudeRunnerState::Provisioning,
            ClaudeRunnerState::AwaitingAuthorization,
            ClaudeRunnerState::Exchanging,
            // `Ready` is emphatically NOT terminal: it is the state finalize acts on, and
            // treating it as settled would stop the console ever refreshing into it.
            ClaudeRunnerState::Ready,
        ] {
            assert!(!state.is_terminal(), "{state:?} must not be terminal");
        }
    }

    #[test]
    fn the_record_schema_publishes_no_token_shaped_field() {
        // The invariant this module exists to hold, asserted against the generated schema rather
        // than described in prose: a future field called `token`, `secret` or `access_token`
        // fails here before it can reach a response body.
        let schema = serde_json::to_value(<ClaudeRunnerRecord as utoipa::PartialSchema>::schema())
            .expect("serialize record schema");
        let properties = schema["properties"]
            .as_object()
            .expect("record schema properties");
        for forbidden in [
            "token",
            "secret",
            "access_token",
            "refresh_token",
            "credential_secret",
        ] {
            assert!(
                !properties.contains_key(forbidden),
                "ClaudeRunnerRecord must not publish {forbidden}"
            );
        }
        assert!(
            properties.contains_key("scope"),
            "the record must publish its scope: an operator has to be able to tell a tenant's \
             runner from the platform's without opening the database"
        );
    }

    /// The finalize request must not be able to name a scope.
    ///
    /// The scope is sealed into the credential's AAD, so one supplied at finalize time could
    /// contradict the one stored on the runner and displayed since provisioning. `deny_unknown_fields`
    /// turns a client that still sends one into a loud rejection rather than a silent drop.
    #[test]
    fn the_finalize_request_rejects_a_scope_instead_of_ignoring_one() {
        let with_scope = serde_json::json!({
            "provider_id": Uuid::now_v7(),
            "scope": { "type": "tenant", "external_tenant_id": "acme" }
        });
        assert!(
            serde_json::from_value::<ClaudeRunnerFinalizeRequest>(with_scope).is_err(),
            "a scope on the finalize request must be refused; the scope is fixed at provisioning \
             time because it is part of the credential AAD"
        );
    }

    /// An absent scope is the platform-wide account, which is what every existing deployment has.
    #[test]
    fn an_absent_provisioning_scope_means_the_platform_wide_account() {
        let request: ClaudeRunnerProvisionRequest =
            serde_json::from_value(serde_json::json!({ "label": "claude-1" }))
                .expect("a provisioning request needs only a label");
        assert!(request.scope.is_none());
        assert_eq!(request.ttl_seconds, 900);

        let tenant: ClaudeRunnerProvisionRequest = serde_json::from_value(serde_json::json!({
            "label": "claude-acme",
            "scope": { "type": "tenant", "external_tenant_id": "acme" }
        }))
        .expect("the existing CredentialScope wire shape, verbatim");
        assert_eq!(
            tenant.scope,
            Some(CredentialScope::Tenant {
                external_tenant_id: "acme".to_string()
            })
        );
    }
}
