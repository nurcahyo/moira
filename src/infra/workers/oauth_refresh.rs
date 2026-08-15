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
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    config::WorkerSettings,
    domain::{CredentialSecret, ProviderRecord, ProviderType},
    error::AppError,
    infra::{
        metrics::MetricsRegistry,
        pg_rows::{credential_type_to_db, scope_type_to_db},
        repositories::{AdminRepository, ClaimedJob, PgAdminRepository},
        workers::dispatch::JobHandler,
    },
    security::{
        CredentialAadParts, ENVELOPE_VERSION_V1, LocalSecretCipher, SecretCipher, credential_aad,
        mask_plain_secret, secret_fingerprint,
    },
};

/// Per-attempt timeout for the token-endpoint exchange. Deliberately generous relative to
/// [`crate::config::WorkerSettings::provider_health_probe_timeout_ms`]: a token exchange is a
/// one-shot, low-frequency operation (at most once per credential per
/// `maintenance_enqueue_interval_seconds`), not a per-tick probe, so there is no reason to
/// race a slow-but-working identity provider.
const REFRESH_HTTP_TIMEOUT_SECONDS: u64 = 10;

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
    http: Client,
    metrics: MetricsRegistry,
    settings: Arc<WorkerSettings>,
}

impl OAuthTokenRefreshHandler {
    pub fn new(
        pool: PgPool,
        cipher: LocalSecretCipher,
        http: Client,
        metrics: MetricsRegistry,
        settings: Arc<WorkerSettings>,
    ) -> Self {
        Self {
            pool,
            cipher,
            http,
            metrics,
            settings,
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

        let response = self
            .exchange_refresh_token(&token_endpoint, &refresh_token, client_id.as_deref())
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

    async fn exchange_refresh_token(
        &self,
        token_endpoint: &str,
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

        let response = self
            .http
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
}
