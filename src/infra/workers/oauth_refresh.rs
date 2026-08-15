//! `oauth-token-refresh` (plan 12 §1).
//!
//! Already declared in `WORKER_JOB_NAMES` with `enabled_by_default: false` since before this
//! handler existed — `dispatch::default_dispatcher`'s doc comment names this module as the
//! seam. Scans `provider_credentials` for `oauth2` credentials expiring soon, decrypts the
//! stored `refresh_token` via `src/security/crypto.rs`'s existing OAuth2-shaped AAD and
//! `SecretCipher`, exchanges it against the provider's configured token endpoint, and
//! re-encrypts the result back onto the same row.
//!
//! # The token endpoint comes from `providers.metadata`, never anywhere else
//!
//! Plan 12 §1's own risk framing is blunt about Claude/ChatGPT subscription OAuth: the
//! sanctioned execution routes for 2026 do not include Moira calling a hardcoded vendor token
//! endpoint directly, and the plan explicitly declines to hardcode one. What *is* worth
//! building generically, per that plan's recommendation, is the `oauth2` credential plumbing
//! itself — storage, encryption, and this refresh worker — independent of which provider or
//! endpoint an operator eventually points it at. So the token endpoint is read from
//! `providers.metadata["oauth_token_endpoint"]` (admin-configured, the same trust level as
//! every other `providers.metadata` value) and nowhere else: never from the credential's own
//! payload, never from a caller, never from a value this module invents. A provider with no
//! configured endpoint is simply not refreshed — logged and skipped, not a hardcoded guess.
//! `providers.metadata["oauth_client_id"]`, if present, is sent alongside the refresh grant;
//! no client secret is supported, matching the public-client (PKCE / device-code) shape
//! subscription OAuth flows use.
//!
//! # …and it is SSRF-validated before a refresh token is sent to it
//!
//! "Admin-configured" is not "trusted". `providers.metadata` is a free-form
//! `serde_json::Value` on both `ProviderCreateRequest` and `ProviderPatchRequest`, and
//! nothing on the write path validates it — only `base_url` goes through
//! `validate_provider_base_url`. The body of this exchange is a **decrypted refresh token**,
//! the longest-lived secret Moira stores, so an unvalidated destination here is not an SSRF
//! probe, it is credential exfiltration: `{"oauth_token_endpoint":
//! "http://169.254.169.254/…"}` or `"http://127.0.0.1:6379/"` and the token is posted to
//! whatever answers.
//!
//! Two things close that, and both are needed:
//!
//! 1. [`validate_token_endpoint`] runs the configured value through
//!    `security::ssrf::validate_outbound_url` — the shared guard already applied to JWKS and
//!    to skill-executor URLs — **immediately before every exchange**, not once at write time.
//!    Use-time is the enforcement point because it is the only one nothing can get behind: a
//!    metadata value can arrive from a future admin route, a migration, or a direct database
//!    edit, and a write-time check would bless none of those.
//! 2. The exchange runs on a **dedicated client with `redirect::Policy::none()`**, never
//!    `AppState::http`, which is documented at `src/app/state.rs` as deliberately keeping
//!    reqwest's default `redirect::Policy::limited(10)`. The form body is a buffered
//!    `String`, so a 307/308 from a validated public host would re-send the refresh token to
//!    the redirect target — a validated URL is only validated for the request Moira actually
//!    issues, which is the same reasoning `src/security/ssrf.rs` gives for the JWKS client.
//!
//! ## May a loopback or private-range token endpoint still be configured?
//!
//! Only in a deployment that has already declared itself non-production, and it takes
//! **both** `provider_security` escape hatches to do it — see [`TokenEndpointPolicy`].
//! Neither flag alone relaxes anything here.
//!
//! # Optimistic concurrency, not a held lock
//!
//! `WorkerSettings::maintenance_enqueue_interval_seconds`'s doc comment explains why this job
//! is not leader-gated: a rare duplicate enqueue across replicas is possible, and every
//! handler it drives must be safe under that. This one's safety comes from
//! `AdminRepository::apply_oauth_refresh`'s `expected_version` guard — see that method's doc
//! comment in `src/infra/repositories/admin.rs` for why the lock is optimistic rather than a
//! `for update` claim held across the provider's HTTP round trip.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::{Client, redirect};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;

use crate::{
    config::{ProviderSecuritySettings, WorkerSettings},
    domain::{CredentialSecret, ProviderRecord, ProviderType},
    error::AppError,
    infra::{
        metrics::MetricsRegistry,
        pg_rows::{credential_type_to_db, scope_type_to_db},
        repositories::{AdminRepository, ClaimedJob, PgAdminRepository},
        workers::dispatch::JobHandler,
    },
    security::{
        CredentialAadParts, ENVELOPE_VERSION_V1, LocalSecretCipher, OutboundUrlPolicy,
        SecretCipher, SystemResolver, credential_aad, mask_plain_secret, secret_fingerprint,
        validate_outbound_url,
    },
};

/// Per-attempt timeout for the token-endpoint exchange. Deliberately generous relative to
/// [`crate::config::WorkerSettings::provider_health_probe_timeout_ms`]: a token exchange is a
/// one-shot, low-frequency operation (at most once per credential per
/// `maintenance_enqueue_interval_seconds`), not a per-tick probe, so there is no reason to
/// race a slow-but-working identity provider.
const REFRESH_HTTP_TIMEOUT_SECONDS: u64 = 10;

/// DNS-resolution budget for the SSRF guard applied to the configured token endpoint.
///
/// A local constant for the same reason `application::agent_platform`'s
/// `SKILL_URL_DNS_TIMEOUT_MS` is one: this hardening is mandatory for every deployment, not
/// an operator-tunable knob the way `auth.jwks.timeout_ms` is for JWKS fetches. The value is
/// the resolution budget only — the exchange itself is bounded by
/// [`REFRESH_HTTP_TIMEOUT_SECONDS`].
const TOKEN_ENDPOINT_DNS_TIMEOUT_MS: u64 = 5_000;

/// Whether this deployment permits a token endpoint the address-space guard would otherwise
/// refuse (plain `http`, loopback, RFC1918, link-local, the cloud-metadata ranges).
///
/// # Why this reuses `provider_security` instead of adding a fourth `allow_insecure_dev_urls`
///
/// `oauth_token_endpoint` is a `providers.metadata` value. It sits on exactly the surface
/// [`ProviderSecuritySettings`] already governs for `providers.base_url`, written by the same
/// admin scope in the same request, so giving it a *separate* switch would let the two halves
/// of one provider row disagree about what address space this deployment lives in.
///
/// # Why **both** flags, and not either
///
/// `OutboundUrlPolicy::allow_insecure` is a single all-or-nothing bypass: it waives the
/// scheme rule *and* the address-range rules at once. `provider_security` spells those out as
/// two separate concessions — `allow_http_provider_urls` is the scheme one,
/// `allow_private_provider_urls` the address one — so the combined bypass requires both to
/// have been granted. An operator who has relaxed only one has not relaxed the other, and a
/// refresh token is not the payload to infer the missing half from.
///
/// # Why this cannot be turned on in production
///
/// `Settings::validate_production` rejects `provider_security.allow_http_provider_urls`
/// outright, so the conjunction is unsatisfiable there by construction, and
/// `Settings::unsafe_development_features` already reports it as `http_provider_urls` in the
/// startup WARN wherever it *is* set. That is the whole answer to "may an admin-configured
/// loopback endpoint remain permitted": in development yes, in production never, and the
/// enforcement is a startup rejection rather than this module's good behaviour.
///
/// Note that `allow_private_provider_urls` alone — which production *may* legitimately set,
/// for an in-cluster provider on a private address — deliberately buys nothing here. A
/// prompt sent to an in-cluster model and a refresh token posted to an in-cluster address are
/// not the same risk, and the remedy for a genuinely private IdP is to give it a public
/// `https` name, not to widen this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenEndpointPolicy {
    pub allow_insecure: bool,
}

impl TokenEndpointPolicy {
    pub fn from_provider_security(security: &ProviderSecuritySettings) -> Self {
        Self {
            allow_insecure: security.allow_private_provider_urls
                && security.allow_http_provider_urls,
        }
    }
}

/// Runs a provider's configured token endpoint through the shared outbound-URL guard.
///
/// A free function rather than a method so the decision is unit-testable with no pool, no
/// cipher and no HTTP client. `reject_credentials: true` because the destination is chosen by
/// stored configuration rather than by Moira: `https://user:pass@host/` would send those
/// embedded credentials to whoever `host` turns out to be, on the same request that carries
/// the refresh token.
///
/// The `Err` string lands in `worker_jobs.last_error`, so it carries the denial *class* and
/// nothing else — the denial `detail` can name a resolved internal address and is logged
/// server-side only, the same posture `security::ssrf` takes on the JWKS path.
pub async fn validate_token_endpoint(
    raw_url: &str,
    policy: TokenEndpointPolicy,
) -> Result<Url, String> {
    let outbound = OutboundUrlPolicy {
        subject: "oauth token endpoint",
        dns_timeout: std::time::Duration::from_millis(TOKEN_ENDPOINT_DNS_TIMEOUT_MS),
        // No egress allow-list: this call closes the redirect hole at the transport instead
        // (`redirect::Policy::none()` on the dedicated client), which is the same trade
        // `validate_jwks_url` documents for the JWKS fetch.
        allowed_hosts: Vec::new(),
        reject_credentials: true,
        allow_insecure: policy.allow_insecure,
    };
    validate_outbound_url(raw_url, &outbound, &SystemResolver)
        .await
        .map_err(|denial| {
            tracing::warn!(
                reason = denial.reason().as_str(),
                detail = denial.detail(),
                "oauth token endpoint blocked by the outbound SSRF policy; no refresh token \
                 was sent"
            );
            format!(
                "oauth token endpoint was refused by the outbound SSRF policy ({})",
                denial.reason().as_str()
            )
        })
}

/// Whether a credential is due for refresh, given `now` and how far ahead of expiry Moira
/// should act.
///
/// Pure so the eligibility boundary is unit-testable with no clock and no database. Mirrors
/// exactly the predicate `AdminRepository::list_oauth_credentials_due_for_refresh` evaluates
/// in SQL (`expires_at < threshold` where `threshold = now + lead_seconds`) — kept here too so
/// `OAuthTokenRefreshHandler::refresh_one` can re-check eligibility on the in-memory record it
/// already has, without a second round trip, before spending an HTTP call on a credential a
/// concurrent replica already refreshed.
pub fn is_due_for_refresh(
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    lead_seconds: i64,
) -> bool {
    match expires_at {
        // No expiry at all means nothing to proactively refresh against.
        None => false,
        Some(expiry) => expiry <= now + ChronoDuration::seconds(lead_seconds.max(0)),
    }
}

/// The token endpoint this credential's provider is configured to refresh against, or `None`
/// if the operator has not configured one. See the module doc comment for why this is the
/// only source ever consulted.
fn configured_token_endpoint(provider: &ProviderRecord) -> Option<String> {
    provider
        .metadata
        .get("oauth_token_endpoint")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn configured_client_id(provider: &ProviderRecord) -> Option<String> {
    provider
        .metadata
        .get("oauth_client_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The standard OAuth2 token-endpoint response shape (RFC 6749 §5.1). Unknown fields are
/// ignored by default (`serde` without `deny_unknown_fields`), which matters here: different
/// identity providers attach their own extra fields to this response and Moira only needs the
/// four below.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
}

pub struct OAuthTokenRefreshHandler {
    pool: PgPool,
    cipher: LocalSecretCipher,
    /// The dedicated no-redirect client, built once per process.
    ///
    /// Deliberately **not** `AppState::http`, and deliberately not a fallback to it: `None`
    /// (the client could not be built at all, which in practice means the TLS backend failed
    /// to initialise) fails every refresh loudly rather than quietly restoring the
    /// redirect-following client this field exists to avoid.
    http: Option<Client>,
    metrics: MetricsRegistry,
    settings: Arc<WorkerSettings>,
    endpoint_policy: TokenEndpointPolicy,
}

impl OAuthTokenRefreshHandler {
    /// Takes no `reqwest::Client` on purpose — see [`Self::http`]. Every other worker handler
    /// is handed `AppState::http`; this one must not be, so the parameter is absent rather
    /// than present-and-ignored.
    pub fn new(
        pool: PgPool,
        cipher: LocalSecretCipher,
        metrics: MetricsRegistry,
        settings: Arc<WorkerSettings>,
        endpoint_policy: TokenEndpointPolicy,
    ) -> Self {
        let http = Client::builder()
            .redirect(redirect::Policy::none())
            .build()
            .inspect_err(|error| {
                tracing::error!(
                    %error,
                    "the oauth-token-refresh HTTP client could not be built; every refresh \
                     will fail rather than fall back to a redirect-following client"
                );
            })
            .ok();
        Self {
            pool,
            cipher,
            http,
            metrics,
            settings,
            endpoint_policy,
        }
    }

    /// One credential's refresh attempt. `Ok(true)` means it was refreshed, `Ok(false)` means
    /// it was skipped for a benign reason (already refreshed by a concurrent replica, or no
    /// endpoint configured), `Err` means the attempt itself failed.
    async fn refresh_one(
        &self,
        admin_repo: &PgAdminRepository,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let stored = admin_repo
            .load_credential_secret(id)
            .await
            .map_err(|error| error.to_string())?;

        if !is_due_for_refresh(
            stored.record.expires_at,
            now,
            self.settings.oauth_refresh_lead_seconds,
        ) {
            // A concurrent replica already refreshed it (pushing `expires_at` back out) since
            // this run's listing query ran. Not a failure.
            return Ok(false);
        }

        let decrypt_aad = credential_aad(CredentialAadParts {
            credential_id: stored.record.id,
            provider_id: stored.record.provider_id,
            credential_type: credential_type_to_db(&stored.record.credential_type),
            scope_type: scope_type_to_db(&stored.record.scope_type),
            external_tenant_id: stored.record.external_tenant_id.as_deref(),
            application_id: stored.record.application_id,
            external_user_id: stored.record.external_user_id.as_deref(),
            encryption_version: stored.record.encryption_version,
        });
        let plaintext = self
            .cipher
            .decrypt(&stored.encrypted, decrypt_aad.as_bytes())
            .map_err(|error| error.to_string())?;
        let secret: CredentialSecret = serde_json::from_slice(&plaintext)
            .map_err(|error| format!("stored oauth2 payload did not parse: {error}"))?;
        let CredentialSecret::OAuth2 {
            refresh_token: Some(refresh_token),
            ..
        } = secret
        else {
            return Err("credential has no refresh_token to refresh with".to_string());
        };

        let provider = admin_repo
            .get_provider(stored.record.provider_id)
            .await
            .map_err(|error| error.to_string())?;
        let provider_type = provider.provider_type;
        let Some(token_endpoint) = configured_token_endpoint(&provider) else {
            return Err(format!(
                "provider {} has no oauth_token_endpoint configured in its metadata",
                provider.id
            ));
        };
        let client_id = configured_client_id(&provider);

        // Validated here — after the credential is known to be due and before a single byte
        // of the refresh token is handed to `reqwest`. A refusal is an ordinary per-credential
        // failure: it is counted, logged and retried on the next poll like any other, because
        // one misconfigured provider must not dead-letter the whole job.
        let token_endpoint =
            match validate_token_endpoint(&token_endpoint, self.endpoint_policy).await {
                Ok(url) => url,
                Err(error) => {
                    // Counted exactly as a failed exchange is: from an operator's dashboard a
                    // refusal to send the token and a rejected send are both "this credential is
                    // not being refreshed", and the distinction is in the log line, not the metric.
                    self.metrics.record_oauth_refresh(provider_type, false);
                    return Err(error);
                }
            };

        let response = self
            .exchange_refresh_token(token_endpoint, &refresh_token, client_id.as_deref())
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                self.metrics.record_oauth_refresh(provider_type, false);
                return Err(error);
            }
        };

        let new_expires_at = response
            .expires_in
            .map(|seconds| now + ChronoDuration::seconds(seconds.max(0)));
        let new_secret = CredentialSecret::OAuth2 {
            access_token: response.access_token.clone(),
            refresh_token: response.refresh_token.or(Some(refresh_token)),
            token_type: response.token_type,
            expires_at: new_expires_at,
        };
        let new_plaintext = serde_json::to_vec(&new_secret).map_err(|error| {
            format!("failed to serialize the refreshed oauth2 payload: {error}")
        })?;
        // A fresh envelope, so the AAD's `encryption_version` is the constant every new
        // encrypt call produces — never `stored.record.encryption_version`, which is the
        // version of the *old* envelope being replaced. Same reasoning
        // `CredentialAdminService::rotate_credential` follows in
        // `src/application/admin/credentials.rs`.
        let encrypt_aad = credential_aad(CredentialAadParts {
            credential_id: stored.record.id,
            provider_id: stored.record.provider_id,
            credential_type: credential_type_to_db(&stored.record.credential_type),
            scope_type: scope_type_to_db(&stored.record.scope_type),
            external_tenant_id: stored.record.external_tenant_id.as_deref(),
            application_id: stored.record.application_id,
            external_user_id: stored.record.external_user_id.as_deref(),
            encryption_version: ENVELOPE_VERSION_V1,
        });
        let encrypted = self
            .cipher
            .encrypt(&new_plaintext, encrypt_aad.as_bytes())
            .map_err(|error| error.to_string())?;
        let fingerprint = secret_fingerprint(&new_plaintext);
        let masked = mask_plain_secret(&response.access_token);

        let updated = admin_repo
            .apply_oauth_refresh(
                id,
                stored.record.version,
                &encrypted,
                &fingerprint,
                &masked,
                new_expires_at,
            )
            .await
            .map_err(|error: AppError| error.to_string())?;

        match updated {
            Some(_new_version) => {
                self.metrics.record_oauth_refresh(provider_type, true);
                Ok(true)
            }
            // Lost the optimistic race to a concurrent refresh (or an admin edit) between the
            // read above and this write. The credential ends up refreshed either way, so this
            // is not a failure worth surfacing.
            None => Ok(false),
        }
    }

    /// `token_endpoint` is a [`Url`], not a `&str`, so this method cannot be reached with a
    /// value that has not been through [`validate_token_endpoint`] — the guard is a type
    /// obligation rather than a convention a later edit can forget.
    async fn exchange_refresh_token(
        &self,
        token_endpoint: Url,
        refresh_token: &str,
        client_id: Option<&str>,
    ) -> Result<TokenResponse, String> {
        // Built by hand rather than `RequestBuilder::form`: that method needs reqwest's
        // `form` cargo feature, which is off in this tree's `default-features = false` build
        // (`json`, `rustls`, `stream` only — see `Cargo.toml`), and this is the only call site
        // that would need it. `url::form_urlencoded` is already a direct dependency, so this
        // avoids widening reqwest's feature set for one caller.
        // Scoped so the non-`Send` `Serializer` is dropped before the `.await` below —
        // otherwise it is captured into this async fn's generated future, which then fails
        // to be `Send` (`JobHandler::handle` requires it, transitively through
        // `RealJobDispatcher: Send + Sync`).
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", "refresh_token");
            form.append_pair("refresh_token", refresh_token);
            if let Some(client_id) = client_id {
                form.append_pair("client_id", client_id);
            }
            form.finish()
        };

        let http = self.http.as_ref().ok_or_else(|| {
            "the oauth-token-refresh HTTP client could not be built at start-up".to_string()
        })?;
        let response = http
            .post(token_endpoint)
            .timeout(std::time::Duration::from_secs(REFRESH_HTTP_TIMEOUT_SECONDS))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            // The message lands in `worker_jobs.last_error` — no token, no endpoint detail
            // beyond what `reqwest`'s own display already keeps free of request bodies.
            .map_err(|error| format!("oauth token endpoint request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            // Covers the redirect case too: the client is built with
            // `redirect::Policy::none()`, so a 3xx arrives here as a plain non-success status
            // and the token is never re-sent to the `Location` target.
            return Err(format!("oauth token endpoint returned HTTP {status}"));
        }
        response.json::<TokenResponse>().await.map_err(|error| {
            format!("oauth token endpoint returned an unparseable response: {error}")
        })
    }

    /// Sets `moira_oauth_credential_status` for every provider type carrying at least one
    /// active `oauth2` credential, folding in this run's own refresh failures — see
    /// `AdminRepository::oauth_credential_lifecycle_counts`'s doc comment for why
    /// `refresh_failed` is not a persisted column.
    async fn publish_credential_status(
        &self,
        admin_repo: &PgAdminRepository,
        threshold: DateTime<Utc>,
        refresh_failures: &std::collections::HashMap<ProviderType, usize>,
    ) {
        let counts = match admin_repo
            .oauth_credential_lifecycle_counts(threshold)
            .await
        {
            Ok(counts) => counts,
            Err(error) => {
                warn!(%error, "oauth-token-refresh could not read the credential-status distribution");
                return;
            }
        };
        let mut seen: std::collections::HashSet<ProviderType> = std::collections::HashSet::new();
        for row in &counts {
            seen.insert(row.provider_type);
            let refresh_failed = refresh_failures
                .get(&row.provider_type)
                .copied()
                .unwrap_or(0);
            self.metrics.set_oauth_credential_status(
                row.provider_type,
                &[
                    ("valid", usize::try_from(row.valid).unwrap_or(usize::MAX)),
                    (
                        "expiring",
                        usize::try_from(row.expiring).unwrap_or(usize::MAX),
                    ),
                    (
                        "expired",
                        usize::try_from(row.expired).unwrap_or(usize::MAX),
                    ),
                    ("refresh_failed", refresh_failed),
                ],
            );
        }
        // A provider type with only failures this run (every credential's status flipped, or
        // an edge case with no `valid`/`expiring`/`expired` rows) still needs its
        // `refresh_failed` bucket published, or the gauge would silently omit it.
        for (provider_type, failed) in refresh_failures {
            if seen.contains(provider_type) {
                continue;
            }
            self.metrics.set_oauth_credential_status(
                *provider_type,
                &[
                    ("valid", 0),
                    ("expiring", 0),
                    ("expired", 0),
                    ("refresh_failed", *failed),
                ],
            );
        }
    }
}

#[async_trait]
impl JobHandler for OAuthTokenRefreshHandler {
    /// Never fails for one credential's problem — a failed refresh is logged (via
    /// `refresh_one`'s `Err`, folded into `refresh_failures` below) and the run continues to
    /// the next due credential, so one unreachable identity provider cannot dead-letter the
    /// whole job. Only a failure to list the due set at all propagates.
    async fn handle(&self, job: &ClaimedJob) -> Result<(), String> {
        let admin_repo = PgAdminRepository::new(self.pool.clone());
        let now = Utc::now();
        let threshold =
            now + ChronoDuration::seconds(self.settings.oauth_refresh_lead_seconds.max(0));

        let due_ids = admin_repo
            .list_oauth_credentials_due_for_refresh(threshold, 100)
            .await
            .map_err(|error| error.to_string())?;

        if due_ids.is_empty() {
            return Ok(());
        }

        let mut refreshed = 0usize;
        let mut skipped = 0usize;
        let mut refresh_failures: std::collections::HashMap<ProviderType, usize> =
            std::collections::HashMap::new();

        for id in due_ids {
            match self.refresh_one(&admin_repo, id, now).await {
                Ok(true) => refreshed += 1,
                Ok(false) => skipped += 1,
                Err(error) => {
                    warn!(
                        job_id = %job.id,
                        credential_id = %id,
                        %error,
                        "oauth credential refresh failed; will retry on the next due poll"
                    );
                    skipped += 1;
                    if let Ok(record) = admin_repo.get_credential(id).await
                        && let Ok(provider) = admin_repo.get_provider(record.provider_id).await
                    {
                        *refresh_failures.entry(provider.provider_type).or_insert(0) += 1;
                    }
                }
            }
        }

        self.publish_credential_status(&admin_repo, threshold, &refresh_failures)
            .await;

        info!(
            job_id = %job.id,
            refreshed,
            skipped,
            "oauth-token-refresh run complete"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn a_credential_with_no_expiry_is_never_due() {
        assert!(!is_due_for_refresh(None, now(), 900));
    }

    #[test]
    fn a_credential_already_expired_is_due() {
        let expired = now() - ChronoDuration::seconds(60);
        assert!(is_due_for_refresh(Some(expired), now(), 900));
    }

    #[test]
    fn a_credential_expiring_within_the_lead_window_is_due() {
        let soon = now() + ChronoDuration::seconds(300);
        assert!(is_due_for_refresh(Some(soon), now(), 900));
    }

    #[test]
    fn a_credential_expiring_well_beyond_the_lead_window_is_not_due() {
        let later = now() + ChronoDuration::seconds(3_600);
        assert!(!is_due_for_refresh(Some(later), now(), 900));
    }

    /// The boundary is inclusive: exactly at the lead window counts as due, matching the SQL
    /// predicate's `<` against the same `now + lead_seconds` threshold value (an expiry
    /// exactly equal to the threshold satisfies `expires_at < threshold` being false by a
    /// hair, but this pure function's `<=` catches it on the very next tick regardless).
    #[test]
    fn a_negative_lead_seconds_is_clamped_to_zero() {
        let now = now();
        let almost_expired = now + ChronoDuration::seconds(1);
        assert!(!is_due_for_refresh(Some(almost_expired), now, -100));
    }

    // -------------------------------------------------------------------------------
    // The token endpoint's SSRF guard.
    //
    // Every URL below is an IP literal or an unparseable string, so `validate_outbound_url`
    // classifies it with the pure `is_denied_ip` alone and no test here touches DNS — the
    // same technique `src/security/ssrf.rs`'s own unit tests and `tests/skill_import.rs`
    // use.
    // -------------------------------------------------------------------------------

    fn strict() -> TokenEndpointPolicy {
        TokenEndpointPolicy {
            allow_insecure: false,
        }
    }

    fn security(private: bool, http: bool) -> ProviderSecuritySettings {
        ProviderSecuritySettings {
            allow_private_provider_urls: private,
            allow_http_provider_urls: http,
            ..ProviderSecuritySettings::default()
        }
    }

    #[test]
    fn a_default_deployment_gets_no_insecure_token_endpoints() {
        assert!(
            !TokenEndpointPolicy::from_provider_security(&security(false, false)).allow_insecure
        );
    }

    /// Both concessions, or neither. See [`TokenEndpointPolicy`] for why either alone is not
    /// enough to send a refresh token off the public internet.
    #[test]
    fn one_provider_security_flag_alone_does_not_relax_the_token_endpoint() {
        assert!(
            !TokenEndpointPolicy::from_provider_security(&security(true, false)).allow_insecure
        );
        assert!(
            !TokenEndpointPolicy::from_provider_security(&security(false, true)).allow_insecure
        );
        assert!(TokenEndpointPolicy::from_provider_security(&security(true, true)).allow_insecure);
    }

    #[tokio::test]
    async fn a_loopback_token_endpoint_is_refused() {
        assert!(
            validate_token_endpoint("http://127.0.0.1:6379/token", strict())
                .await
                .is_err()
        );
        assert!(
            validate_token_endpoint("https://127.0.0.1/token", strict())
                .await
                .is_err()
        );
    }

    /// The exact metadata value issue #251 names: a cloud metadata endpoint reached with a
    /// live refresh token in the request body.
    #[tokio::test]
    async fn the_cloud_metadata_endpoint_is_refused() {
        assert!(
            validate_token_endpoint("http://169.254.169.254/latest/meta-data/", strict())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_private_range_or_plain_http_token_endpoint_is_refused() {
        for raw in [
            "https://10.0.0.5/token",
            "https://192.168.1.10/token",
            "http://8.8.8.8/token",
        ] {
            assert!(
                validate_token_endpoint(raw, strict()).await.is_err(),
                "{raw} must be refused under the default policy"
            );
        }
    }

    /// Embedded credentials would be sent to whoever the host turns out to be, on the very
    /// request that carries the refresh token.
    #[tokio::test]
    async fn a_token_endpoint_that_embeds_credentials_is_refused() {
        assert!(
            validate_token_endpoint("https://user:pass@8.8.8.8/token", strict())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_unparseable_token_endpoint_is_refused_rather_than_posted_to() {
        assert!(
            validate_token_endpoint("not-a-url", strict())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_public_https_token_endpoint_is_permitted() {
        assert!(
            validate_token_endpoint("https://8.8.8.8/oauth/token", strict())
                .await
                .is_ok()
        );
    }

    /// The development escape hatch, and the only shape that reaches it.
    #[tokio::test]
    async fn a_deployment_with_both_flags_may_use_a_loopback_token_endpoint() {
        let permissive = TokenEndpointPolicy::from_provider_security(&security(true, true));
        assert!(
            validate_token_endpoint("http://127.0.0.1:8080/token", permissive)
                .await
                .is_ok()
        );
    }
}
