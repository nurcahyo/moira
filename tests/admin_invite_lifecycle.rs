//! End-to-end coverage for plan 09 wave 2's invitation lifecycle.
//!
//! Wave 2 shipped nine operations with unit tests and code structure only; nothing
//! exercised `POST /api/v1/admin/admin-invites/redeem` against a real database. This suite
//! is that coverage, and it is written to fail against three specific defects rather than
//! to narrate the happy path:
//!
//! 1. **The `trusted_jwt_issuer_id` binding on the redeem path's `governing_policy` call.**
//!    Plan 08 shipped this exact defect once already. `governing_policy` matches
//!    `issuer = $1 or trusted_jwt_issuer_id = $2`, and on a correctly configured deployment
//!    the provider row's `issuer` column holds the *IdP's* issuer while `$1` is the
//!    console's — so the row matches only through `$2`.
//!    `redeem_resolves_the_governing_policy_through_the_trusted_jwt_issuer_link` builds
//!    exactly that arrangement and asserts the premise (`issuer` *differs*, the link
//!    *matches*, exactly one enabled row exists) before asserting the property.
//! 2. **A denied redemption must not consume the invite.**
//!    `a_policy_denied_redemption_leaves_the_invite_pending_and_the_same_link_still_works`
//!    asserts on `admin_invites.status` directly — the row, not a replayed response body.
//!    A 403 is not an `is_cacheable_admin_failure` (`src/error.rs:209`), so a denial writes
//!    no idempotency record and there is no stored response to replay; a test built on
//!    replay would pass in both arrangements and prove nothing.
//! 3. **Only a primary admin may transfer ownership.**
//!    `a_non_primary_admin_cannot_promote_itself_to_primary` proves the caller *is*
//!    authenticated on the admin plane before asserting the 403, so the assertion cannot
//!    be satisfied by an authentication failure wearing the same status code.
//!
//! Isolation: every test takes its own [`LifecycleFixture`], which clones a private
//! database per fixture. No test here reads a row another test could have written.

mod support;

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use moira::{app::AppState, config::Settings};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::LifecycleFixture;
use tokio::{net::TcpListener, sync::Barrier, task::JoinHandle};
use tower::ServiceExt;
use uuid::Uuid;

const INVITE_OUTCOMES: &str = "moira_admin_invite_outcomes_total";
const GRANT_EVENTS: &str = "moira_admin_identity_grant_events_total";

/// The same key material `tests/identity_claim.rs` uses.
///
/// Duplicated rather than hoisted into `tests/support/mod.rs`: `support` is compiled into
/// every one of the thirty-odd test binaries, and this suite is the second consumer, not
/// the third. Reversal condition — a third suite needing a console issuer moves
/// [`ConsoleIssuer`] into `support` and updates both call sites in the same change.
const TEST_KEY_ID: &str = "admin-invite-lifecycle-test-key";
const TEST_RSA_MODULUS: &str = "r686LSRV-46Cn3oh00Zo43hZNDiHY-Oei0JLSjApgCAD1btVtD2ju5zlGxA97OPjzWAGC0Z8ZqYwmfNwFWLyaC8Sr5-R2ejUuBpH32t8aFf4Z6p1MLUlmXWHviBNVutUzeicKMPWzVQ0xnoktJ6jOxDOkox8JMiNPGbTRAuQ-7poobvKH34738OP8fdaCpPIabtTfvz5gI11PYTLDlwrDWje3smeonXuxwj1lChvv5m08J7BsK4Jvb_YaUq0kCuQbpjFApOaTc_cY-xYrWVRcv9aprKEsJQvBm8xdDiAukfybT-GE3vFOMjmrWqVcPd46mYL0cr_VdxScWum5S1rcQ";
const TEST_RSA_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCvrzotJFX7joKf
eiHTRmjjeFk0OIdj456LQktKMCmAIAPVu1W0PaO7nOUbED3s4+PNYAYLRnxmpjCZ
83AVYvJoLxKvn5HZ6NS4Gkffa3xoV/hnqnUwtSWZdYe+IE1W61TN6Jwow9bNVDTG
eiS0nqM7EM6SjHwkyI08ZtNEC5D7umihu8offjvfw4/x91oKk8hpu1N+/PmAjXU9
hMsOXCsNaN7eyZ6ide7HCPWUKG+/mbTwnsGwrgm9v9hpSrSQK5BumMUCk5pNz9xj
7FitZVFy/1qmsoSwlC8GbzF0OIC6R/JtP4YTe8U4yOatapVw93jqZgvRyv9V3FJx
a6blLWtxAgMBAAECggEAFx+nNp3bu1qMktUOcrKHx7jldNwj5d/l1EqLgl5IeBa+
qnkX1LtwO5dxCFjg7bcpGrUS1pUWdqRVLU4/aHE3msLnYLpOBjKBHSJIZ33MSCec
CHkFJ74QDtzLWxkBVPlwlhGRzEPKmAgHUkBtaGCg93tE1UEsbeL/w/18vS4QjTFJ
bK+3O8vkDqdYQAJInbjURhcv7OQIF848CEwkmI/s5boSfOV3nTCRHd0cnCAuEjGv
/y0gikfzmDdBY+SK/tF41ctFuR+WU1xcR1PoLj87rKS9Nm5GkQeDuzO5JbgGqgIe
kFI41mqVcs1MK2sx63yHj1ngNF6B0PEgspKIpjQ6iQKBgQD3KZ16wzJkF4fakFcS
dFP3eoxXvAVgrODJx3IQpBrLcG+5pJTwFRKwPPdYvd6hqa+MUhEqYlhJNw6G89x2
bWy8Y7Cqjy92Oa95zTFCdJ/Fmd2Fhkx7Dhnnztn66Se8NbUl/LWnTLPspeVT2+XR
9DtSiB+Mugv0BCIBD2uAglQ3rwKBgQC191e1hpVUSzBnd4MtStMyIZegaUhKireJ
YBs/tNmEe7SVYWG3rGzZzOVwKkC3wcF+mYisRqncxjyySu8i6RjwrHgN+W+sF9h0
/4wIU/lOenKtzG0DER0gfwDuzI8fvwPpv4RvOop5+r0kRwB64BUm6+/4K6snlrrs
Em2BeY423wKBgDbKQt6z5rfJf5Qz6xlsMDDsObA5PffwWuRgEikeN9JhWmMM2Pdf
tITc/vftHy03MHMqviNnKasRSWchJ/4Yw8H/V2p3002h/AREOGdC8ygas8ClxM6C
kbuRX0D/7o8KWN3S53HuzvPm0q+ET637NitVgajwlTXCtMcHZA1Y1tKBAoGAbApw
CVffUi1SkBxlxn6m5x0K6jOYuKmkT+zAQRMgE4lfr1IisuuttaPylqZ/xptER+bh
P2i1cmBBqZrUYeYE6OF+Zs2zgHqoCs+wVUGGxRHvBUJbd3ax1JmT9DWAxVik+iS8
fU5E6if2JZQCtPJXnMR5tuA2v0q/sWs/maCS0AECgYEA9DjpdIMB+efwyqWiBGPe
KyAcIS0RU0PHejdULbZoW20yC4qRTwkRdUKVICbK7ubtzB8jy5HLBR2IsGPjQKyh
5JtiEST48mPRj2FLFCb5pW+S1Sxl0+kb2094nmbOZzZU0FmtqOlopBPD3RCv2twO
8PBnvQBPRWjhbQGhwavb5Lw=
-----END PRIVATE KEY-----"#;

struct HttpResult {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
}

impl HttpResult {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    fn version(&self) -> i64 {
        self.etag
            .as_deref()
            .map(|value| value.trim_matches('"'))
            .and_then(|value| value.parse().ok())
            .expect("an ETag carrying the resource version")
    }

    fn message_key(&self) -> &str {
        self.body["error"]["message_key"]
            .as_str()
            .unwrap_or_default()
    }
}

/// A console-style trusted JWT issuer with **`scopes_claim` NULL**, which CONVENTIONS §7.5
/// requires of any issuer a console links. Its tokens therefore assert no scopes at all,
/// so every scope a redeemed identity ends up holding came from an `admin_identities` row.
struct ConsoleIssuer {
    /// The `trusted_jwt_issuers.id`. The redeem path resolves this from the token's issuer
    /// and hands it to `governing_policy`; the mutation-1 test needs it to build a
    /// provider row that matches *only* through the link.
    id: Uuid,
    issuer: String,
    task: JoinHandle<()>,
}

impl ConsoleIssuer {
    async fn start(pool: &PgPool) -> Self {
        let jwks = json!({
            "keys": [{
                "kty": "RSA",
                "kid": TEST_KEY_ID,
                "use": "sig",
                "alg": "RS256",
                "n": TEST_RSA_MODULUS,
                "e": "AQAB"
            }]
        });
        let app = axum::Router::new().route(
            "/jwks",
            axum::routing::get(move || {
                let jwks = jwks.clone();
                async move { axum::Json(jwks) }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test JWKS server");
        let address = listener.local_addr().expect("test JWKS address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test JWKS");
        });
        let id = Uuid::now_v7();
        let issuer = format!("https://console-idp.test/{}", Uuid::now_v7());
        sqlx::query(
            r#"
            insert into trusted_jwt_issuers (
                id, issuer, jwks_url, expected_audiences, allowed_algorithms, subject_claim
            )
            values ($1, $2, $3, '{}', array['RS256'], 'sub')
            "#,
        )
        .bind(id)
        .bind(&issuer)
        .bind(format!("http://{address}/jwks"))
        .execute(pool)
        .await
        .expect("register the console trusted JWT issuer");
        Self { id, issuer, task }
    }

    fn bearer(&self, subject: &str) -> HeaderMap {
        let claims = json!({
            "iss": self.issuer,
            "sub": subject,
            "exp": chrono::Utc::now().timestamp() + 3600
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KEY_ID.to_string());
        let token = encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY.as_bytes())
                .expect("parse test RSA private key"),
        )
        .expect("sign test JWT");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("bearer header"),
        );
        headers
    }
}

impl Drop for ConsoleIssuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Scrapes the Prometheus exposition body and returns one sample's value.
///
/// `0.0` for an absent series rather than a panic, so an assertion reads as "this counter
/// did not move" instead of "the family is missing" — the two are the same defect here,
/// and every series in this family is seeded at zero anyway.
///
/// Matches the label *inside* the brace group rather than pinning the whole label set:
/// the exporter is free to order labels as it likes and a deployment may carry global
/// ones, and a needle that assumed `{outcome="…"}` was the entire set would read every
/// series as absent — which is indistinguishable from "the counter is stuck" and would
/// make the zero-valued premise below vacuous instead of load-bearing.
fn counter_value(exposition: &str, family: &str, label: &str, value: &str) -> f64 {
    let selector = format!("{label}=\"{value}\"");
    exposition
        .lines()
        .filter_map(|line| line.strip_prefix(family))
        .filter_map(|rest| rest.strip_prefix('{'))
        .find_map(|rest| {
            let (labels, amount) = rest.split_once('}')?;
            labels.contains(&selector).then_some(amount)
        })
        .and_then(|amount| amount.trim().parse().ok())
        .unwrap_or(0.0)
}

/// Every exposition line belonging to `family`, for an assertion message that shows what
/// was actually served instead of only what was expected.
fn family_lines(exposition: &str, family: &str) -> String {
    exposition
        .lines()
        .filter(|line| line.starts_with(family))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn send(router: Router, request: Request<Body>) -> HttpResult {
    let response = router.oneshot(request).await.expect("HTTP response");
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON response body")
    };
    HttpResult { status, body, etag }
}

fn base(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", format!("invite-{}", Uuid::now_v7()))
}

async fn get_with(router: Router, path: &str, headers: HeaderMap) -> HttpResult {
    let mut builder = base("GET", path);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    send(router, builder.body(Body::empty()).unwrap()).await
}

async fn send_json(
    router: Router,
    method: &str,
    path: &str,
    headers: HeaderMap,
    if_match: Option<i64>,
    body: Value,
) -> HttpResult {
    let mut builder = base(method, path).header("content-type", "application/json");
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    if let Some(version) = if_match {
        builder = builder.header("if-match", version.to_string());
    }
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

async fn post_json(router: Router, path: &str, headers: HeaderMap, body: Value) -> HttpResult {
    send_json(router, "POST", path, headers, None, body).await
}

/// How a governing `auth_provider_settings` row is bound to the token's issuer.
///
/// Named rather than passed as two loose `Option`s because the difference between the two
/// variants *is* the subject of `redeem_resolves_the_governing_policy_through_the_trusted_jwt_issuer_link`.
enum PolicyBinding<'a> {
    /// `auth_provider_settings.issuer` equals the token's issuer, **and** the row is bound
    /// to that same trusted issuer — CONVENTIONS §7.3 mode 3 configured explicitly.
    ///
    /// It used to be the unbound shape, on the theory that `governing_policy`'s
    /// `issuer = $1` branch was the thing under test. Finding F23 corrected that twice
    /// over: an unbound row carrying the CONSOLE's issuer is shape (b) — it outranked the
    /// correctly-bound provider at any age and applied its own `allowed_email_domains` —
    /// and since `migrations/0020` it is refused at create time with
    /// `409 auth_provider_issuer_shadows_trusted_issuer`. The variant is kept because the
    /// contrast with the next one is still the point; what it demonstrates is now that the
    /// `issuer` column's value does not decide anything, in either direction.
    ByIssuerString { issuer: &'a str, id: Uuid },
    /// `auth_provider_settings.issuer` holds a *different* string — the IdP's own issuer,
    /// which is what a real deployment stores — and the row reaches the console's issuer
    /// only through `trusted_jwt_issuer_id`. This is the arrangement that makes the second
    /// argument to `governing_policy` load-bearing.
    ByTrustedIssuerLink { other_issuer: &'a str, id: Uuid },
}

/// The designed setup order: create an auth-provider configuration carrying the intended
/// `allowed_email_domains`, then enable it. Until this runs every claim and every
/// redemption is refused — that is the deny-by-default policy, not a defect.
async fn configure_and_enable_policy(
    router: &Router,
    binding: PolicyBinding<'_>,
    domains: &[&str],
) -> (Uuid, i64) {
    let (issuer, trusted_jwt_issuer_id) = match binding {
        PolicyBinding::ByIssuerString { issuer, id } => (issuer.to_string(), json!(id)),
        PolicyBinding::ByTrustedIssuerLink { other_issuer, id } => {
            (other_issuer.to_string(), json!(id))
        }
    };
    let created = post_json(
        router.clone(),
        "/api/v1/admin/auth/providers",
        HeaderMap::new(),
        json!({
            "method": "generic_oidc",
            "display_name": "Console",
            "issuer": issuer,
            "client_id": "console-client",
            "trusted_jwt_issuer_id": trusted_jwt_issuer_id,
            "allowed_email_domains": domains
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id: Uuid = created.body["id"]
        .as_str()
        .expect("provider id")
        .parse()
        .expect("provider id is a uuid");

    let enabled = send_json(
        router.clone(),
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        Some(created.version()),
        Value::Null,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);
    (id, enabled.version())
}

/// **The premise, asserted before every property that depends on it.**
///
/// A redeem test whose deployment has no enabled, governing provider row proves nothing:
/// every redemption would 403 for a reason unrelated to what the test claims to measure,
/// and a 201-expecting test would fail loudly while a 403-expecting one would pass
/// vacuously. This reads the row back through SQL — not through the API that wrote it —
/// and pins that it is the *only* enabled row, so a match cannot come from somewhere else.
async fn assert_exactly_one_enabled_policy(
    pool: &PgPool,
    provider_id: Uuid,
    expected_issuer: Option<&str>,
    expected_link: Option<Uuid>,
) {
    let enabled_rows: i64 = sqlx::query_scalar(
        "select count(*) from auth_provider_settings \
         where deleted_at is null and status = 'active' and enabled",
    )
    .fetch_one(pool)
    .await
    .expect("count enabled auth provider settings");
    assert_eq!(
        enabled_rows, 1,
        "the premise is one governing row; more than one makes the binding under test ambiguous"
    );

    let row: (Option<String>, Option<Uuid>) = sqlx::query_as(
        "select issuer, trusted_jwt_issuer_id from auth_provider_settings where id = $1",
    )
    .bind(provider_id)
    .fetch_one(pool)
    .await
    .expect("read the governing auth provider row");
    assert_eq!(
        row.0.as_deref(),
        expected_issuer,
        "the governing row's issuer column is not what this test arranged"
    );
    assert_eq!(
        row.1, expected_link,
        "the governing row's trusted_jwt_issuer_id is not what this test arranged"
    );
}

async fn create_invite(router: &Router, constraint: &str, value: &str) -> (Uuid, String) {
    let created = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites",
        HeaderMap::new(),
        json!({
            "constraint": constraint,
            "value": value,
            "expires_in_seconds": 3600
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id: Uuid = created.body["resource"]["id"]
        .as_str()
        .expect("invite id")
        .parse()
        .expect("invite id is a uuid");
    let token = created.body["secret"]
        .as_str()
        .expect("the token is returned exactly once, at creation")
        .to_string();
    (id, token)
}

async fn invite_status(pool: &PgPool, id: Uuid) -> String {
    // Deliberately the raw column, not the API's `AdminInviteRecord.status`: the record is
    // built by the same layer under test, and this property is about the row.
    sqlx::query_scalar("select status from admin_invites where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the invite status")
}

async fn grant_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from admin_identities where deleted_at is null")
        .fetch_one(pool)
        .await
        .expect("count admin identity grants")
}

fn redeem_body(token: &str, email: &str) -> Value {
    json!({ "token": token, "email": email, "email_verified": true })
}

/// One redemption by `subject`, whose email is derived from the subject so two racers can
/// never collide on the invite's own domain constraint.
async fn redeem(router: &Router, issuer: &ConsoleIssuer, subject: &str, token: &str) -> HttpResult {
    post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer(subject),
        redeem_body(token, &format!("{subject}@example.com")),
    )
    .await
}

/// How many active grants hold ownership.
///
/// Ownership under decision D-F20 is a property of the *set* — at most one holder — so
/// every assertion about it counts rather than sampling one row. Read through SQL rather
/// than through the list endpoint, because the endpoint is built by the layer under test.
async fn active_primary_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "select count(*) from admin_identities \
         where deleted_at is null and status = 'active' and is_primary",
    )
    .fetch_one(pool)
    .await
    .expect("count active primaries")
}

async fn is_primary(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("select is_primary from admin_identities where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read a grant's ownership flag")
}

fn grant_id(result: &HttpResult) -> Uuid {
    result.body["id"]
        .as_str()
        .expect("grant id")
        .parse()
        .expect("grant id is a uuid")
}

fn with_idempotency_key(mut headers: HeaderMap, key: &str) -> HeaderMap {
    headers.insert(
        "idempotency-key",
        key.parse().expect("an Idempotency-Key header value"),
    );
    headers
}

/// A second [`AppState`] on the fixture's own database, with Prometheus turned on.
///
/// [`LifecycleFixture`] leaves `telemetry.prometheus_enabled` at its hardened default of
/// `false`, and flipping it there would change the state every other suite in the
/// repository builds. This state is used by one test, drives every request that test
/// makes, and therefore owns every counter that test reads — the recorder is per-`AppState`,
/// so the counts cannot be contaminated by a neighbouring fixture.
async fn observable_state(pool: &PgPool) -> AppState {
    let mut settings = Settings::default();
    // The stub IdP lives on `http://127.0.0.1:0`, which the JWKS SSRF policy denies by
    // design; the same dev-only escape hatch `LifecycleFixture` uses.
    settings.auth.jwks.allow_insecure_dev_urls = true;
    settings.telemetry.prometheus_enabled = true;
    AppState::new(settings, Some(pool.clone()))
        .await
        .expect("observable app state")
}

async fn scrape_metrics(router: Router) -> String {
    let response = router
        .oneshot(base("GET", "/metrics").body(Body::empty()).unwrap())
        .await
        .expect("HTTP response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/metrics is not enabled on this state"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("metrics body");
    String::from_utf8(bytes.to_vec()).expect("metrics body is UTF-8")
}

fn invite_outcome(exposition: &str, label: &str) -> f64 {
    counter_value(exposition, INVITE_OUTCOMES, "outcome", label)
}

fn ownership_transfers(exposition: &str) -> f64 {
    counter_value(exposition, GRANT_EVENTS, "event", "ownership_transferred")
}

// ===========================================================================================
// The lifecycle.
// ===========================================================================================

#[tokio::test]
async fn create_preview_redeem_grants_admin_and_consumes_the_invite() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;
    assert_eq!(
        grant_count(&fixture.pool).await,
        0,
        "the premise is a deployment with no grant yet; redemption is what creates the first one"
    );

    let (invite_id, token) = create_invite(&router, "email", "Colleague@Example.com").await;
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "pending",
        "a freshly minted invite is pending"
    );

    // Preview is anonymous — no bearer, no system key. The token in the body is the whole
    // credential, which is the point: the invitee has not signed in yet.
    let preview = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/preview",
        HeaderMap::new(),
        json!({ "token": token }),
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK, "{:?}", preview.body);
    assert_eq!(preview.body["constraint"], json!("email"));
    assert_eq!(
        preview.body["value"], "colleague@example.com",
        "the constraint value is normalised at creation"
    );
    assert!(
        preview.body.get("id").is_none() && preview.body.get("created_by_subject").is_none(),
        "the anonymous preview carries no invite id and no inviter: {:?}",
        preview.body
    );

    let redeemed = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("colleague-subject"),
        redeem_body(&token, "Colleague@Example.com"),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::CREATED, "{:?}", redeemed.body);
    assert_eq!(redeemed.body["granted_scopes"], json!(["moira:admin"]));
    assert_eq!(
        redeemed.body["is_primary"],
        json!(true),
        "this is the deployment's first grant, so it takes ownership (finding F20). That an \
         invite redeemed on a deployment which *already* has an owner does not mint a second \
         one is the other half of the property, and is asserted by \
         the_first_grant_on_a_deployment_owns_it_and_the_second_does_not"
    );
    assert_eq!(
        redeemed.body["notice"]["message_key"],
        "moira.notice.admin_invite_redeemed"
    );
    let grant_id: Uuid = redeemed.body["id"]
        .as_str()
        .expect("grant id")
        .parse()
        .expect("grant id is a uuid");

    // The grant exists as a row, with the (issuer, subject) the *token* proved rather than
    // anything the request body asserted.
    let grant: (String, String, Option<String>, bool, String) = sqlx::query_as(
        "select issuer, subject, email, is_primary, granted_by_actor_type \
         from admin_identities where id = $1",
    )
    .bind(grant_id)
    .fetch_one(&fixture.pool)
    .await
    .expect("read the redeemed grant");
    assert_eq!(grant.0, issuer.issuer);
    assert_eq!(grant.1, "colleague-subject");
    assert_eq!(grant.2.as_deref(), Some("Colleague@Example.com"));
    assert!(
        grant.3,
        "the deployment's first grant owns it, and the row must agree with the response body \
         asserted above — a service that reported ownership it had not stored would pass the \
         body assertion alone"
    );
    // The audit column names the credential that actually produced the grant. Writing
    // `'system_key'` here would be false in an audit column: no system key was presented,
    // and `admin_invites.consumed_admin_identity_id` points the other way.
    assert_eq!(
        grant.4, "admin_invite",
        "an invite-created grant records itself as one"
    );

    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "consumed",
        "a successful redemption consumes the invite"
    );
    let consumed: (Option<Uuid>, Option<String>) = sqlx::query_as(
        "select consumed_admin_identity_id, consumed_subject from admin_invites where id = $1",
    )
    .bind(invite_id)
    .fetch_one(&fixture.pool)
    .await
    .expect("read the consumed invite");
    assert_eq!(
        consumed.0,
        Some(grant_id),
        "the consumed invite links to the grant it produced"
    );
    assert_eq!(consumed.1.as_deref(), Some("colleague-subject"));
}

#[tokio::test]
async fn a_second_redemption_of_a_consumed_invite_is_refused_and_creates_no_second_grant() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "domain", "example.com").await;
    let first = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("first-colleague"),
        redeem_body(&token, "first@example.com"),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    assert_eq!(invite_status(&fixture.pool, invite_id).await, "consumed");
    assert_eq!(
        grant_count(&fixture.pool).await,
        1,
        "the premise for 'no second grant' is that there is exactly one"
    );

    // A *different* identity, so a pass here cannot be explained by idempotent replay of
    // the first redemption.
    let second = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("second-colleague"),
        redeem_body(&token, "second@example.com"),
    )
    .await;
    assert_eq!(second.status, StatusCode::CONFLICT, "{:?}", second.body);
    assert_eq!(second.code(), "invite_already_consumed");
    assert_eq!(second.message_key(), "moira.error.invite_already_consumed");
    assert_eq!(
        grant_count(&fixture.pool).await,
        1,
        "a single-use invite produced a second grant"
    );

    // Preview agrees with redeem. The two share `require_redeemable` precisely so an
    // invite page cannot render "valid" for a link redemption will refuse.
    let preview = post_json(
        router,
        "/api/v1/admin/admin-invites/preview",
        HeaderMap::new(),
        json!({ "token": token }),
    )
    .await;
    assert_eq!(preview.status, StatusCode::CONFLICT, "{:?}", preview.body);
    assert_eq!(preview.code(), "invite_already_consumed");
}

#[tokio::test]
async fn an_expired_invite_is_refused_by_both_preview_and_redeem() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "domain", "example.com").await;
    // Backdated rather than slept through: `MIN_INVITE_EXPIRY_SECONDS` is 60, so the API
    // cannot mint an already-expired invite and a test that waited would take a minute.
    // Expiry is *derived* from this column on every read — `status` has no 'expired'
    // value — so moving the timestamp is the whole of the state change.
    sqlx::query("update admin_invites set expires_at = now() - interval '1 hour' where id = $1")
        .bind(invite_id)
        .execute(&fixture.pool)
        .await
        .expect("backdate the invite expiry");
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "pending",
        "the premise is an invite that is still *pending* and expired only by its timestamp"
    );

    let preview = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/preview",
        HeaderMap::new(),
        json!({ "token": token }),
    )
    .await;
    assert_eq!(preview.status, StatusCode::FORBIDDEN, "{:?}", preview.body);
    assert_eq!(preview.code(), "invite_expired");

    let redeemed = post_json(
        router,
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("late-colleague"),
        redeem_body(&token, "late@example.com"),
    )
    .await;
    assert_eq!(
        redeemed.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        redeemed.body
    );
    assert_eq!(redeemed.code(), "invite_expired");
    assert_eq!(redeemed.message_key(), "moira.error.invite_expired");
    assert_eq!(
        grant_count(&fixture.pool).await,
        0,
        "an expired invite granted admin"
    );
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "pending",
        "a refused redemption must not consume the invite"
    );
}

#[tokio::test]
async fn a_revoked_invite_is_refused_by_both_preview_and_redeem() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "domain", "example.com").await;
    let revoked = post_json(
        router.clone(),
        &format!("/api/v1/admin/admin-invites/{invite_id}/revoke"),
        HeaderMap::new(),
        Value::Null,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{:?}", revoked.body);
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "revoked",
        "the premise is a row the revoke endpoint actually moved"
    );

    let preview = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/preview",
        HeaderMap::new(),
        json!({ "token": token }),
    )
    .await;
    assert_eq!(preview.status, StatusCode::FORBIDDEN, "{:?}", preview.body);
    assert_eq!(preview.code(), "invite_revoked");

    let redeemed = post_json(
        router,
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("withdrawn-colleague"),
        redeem_body(&token, "withdrawn@example.com"),
    )
    .await;
    assert_eq!(
        redeemed.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        redeemed.body
    );
    assert_eq!(redeemed.code(), "invite_revoked");
    assert_eq!(redeemed.message_key(), "moira.error.invite_revoked");
    assert_eq!(
        grant_count(&fixture.pool).await,
        0,
        "a revoked invite granted admin"
    );
}

/// **The invite is not a policy bypass, and a denial does not burn it.**
///
/// Both halves matter and both are asserted on the row. The plan's own ordering
/// requirement is that an invitee refused by the deny-by-default domain policy can redeem
/// the *same* link once an operator widens the allow-list — which is only true if the
/// refused attempt left `admin_invites.status = 'pending'`.
///
/// This asserts the status rather than replaying the request under an `Idempotency-Key`:
/// `AppError::is_cacheable_admin_failure` (`src/error.rs:209`) excludes 403, so a denied
/// redemption writes no ledger row and there is no stored failure to replay. A replay-based
/// test would pass whether the check ran before or inside the transactional envelope.
#[tokio::test]
async fn a_policy_denied_redemption_leaves_the_invite_pending_and_the_same_link_still_works() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    // The allow-list deliberately does NOT contain the invite's own domain: the invite's
    // constraint will pass and only plan 07's provider policy will refuse, which is the
    // separation the two error codes exist to express.
    let (provider_id, version) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["already-onboarded.test"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "domain", "example.com").await;
    let denied = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("early-colleague"),
        redeem_body(&token, "early@example.com"),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    assert_eq!(
        denied.code(),
        "admin_claim_domain_not_allowed",
        "the invite's own constraint matched; only the provider allow-list refused"
    );
    assert_eq!(
        grant_count(&fixture.pool).await,
        0,
        "a denied redemption created a grant"
    );
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "pending",
        "a denied redemption consumed the invite — the same link can never be retried"
    );

    // The operator widens the allow-list. Nothing about the invite changes.
    let widened = send_json(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{provider_id}"),
        HeaderMap::new(),
        Some(version),
        json!({ "allowed_email_domains": ["example.com"] }),
    )
    .await;
    assert_eq!(widened.status, StatusCode::OK, "{:?}", widened.body);

    let redeemed = post_json(
        router,
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("early-colleague"),
        redeem_body(&token, "early@example.com"),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::CREATED, "{:?}", redeemed.body);
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "consumed",
        "the retried redemption should consume the invite"
    );
    assert_eq!(grant_count(&fixture.pool).await, 1);
}

/// **The pre-envelope ordering, as an *observable* property.**
///
/// `redeem_invite`'s contract is that every validation runs before
/// `AdminCommandRunner::execute`, and its doc comment forbids moving them inside "to make
/// it atomic". Asserting that through `admin_invites.status` alone does not work, and this
/// was measured rather than assumed: moving all three checks inside the envelope, *after*
/// the statement that consumes the invite, changed **not one** of 869 tests. The reason is
/// `AdminCommandRunner::execute`'s last arm — a 403 is not an
/// `is_cacheable_admin_failure`, so the runner calls `transaction.rollback()` and the
/// consume is undone with everything else. Ordering and rollback defend the same property,
/// so the property cannot tell them apart.
///
/// What *is* observable is **which failure the caller is told about**. Inside the envelope,
/// `insert_grant` runs first, so a caller whose identity already holds a grant gets
/// `409 admin_identity_already_claimed` — before the invite has refused them and before
/// policy has. That is an enumeration oracle: a stranger holding a leaked token learns
/// whether an `(issuer, subject)` already has admin from a request the invite's own
/// constraint should have refused outright.
///
/// So the assertion is: a refusal that belongs to the invite or to policy must be reported
/// as such, and must not be pre-empted by anything that touches a table.
#[tokio::test]
async fn a_refusal_that_belongs_to_the_invite_is_not_pre_empted_by_the_grant_table() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, version) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    // Premise — this identity already holds a grant, which is what makes `insert_grant`
    // fail if it is ever reached. Without it both probes below would pass against the
    // in-envelope arrangement too.
    let (_, first_token) = create_invite(&router, "domain", "example.com").await;
    let first = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("returning-admin"),
        redeem_body(&first_token, "returning@example.com"),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    assert_eq!(
        grant_count(&fixture.pool).await,
        1,
        "the premise is an identity that already holds a grant"
    );

    // Probe 1 — the invite's own constraint refuses. The caller must be told that, not
    // that their identity is already an admin.
    let (mismatched_id, mismatched_token) =
        create_invite(&router, "email", "someone-else@example.com").await;
    let mismatch = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("returning-admin"),
        redeem_body(&mismatched_token, "returning@example.com"),
    )
    .await;
    assert_eq!(
        mismatch.code(),
        "invite_email_mismatch",
        "the invite's constraint must be evaluated before anything touches admin_identities"
    );
    assert_eq!(
        mismatch.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        mismatch.body
    );
    assert_eq!(invite_status(&fixture.pool, mismatched_id).await, "pending");

    // Probe 2 — the provider allow-list refuses. Same rule, one checkpoint later.
    let narrowed = send_json(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{provider_id}"),
        HeaderMap::new(),
        Some(version),
        json!({ "allowed_email_domains": ["elsewhere.test"] }),
    )
    .await;
    assert_eq!(narrowed.status, StatusCode::OK, "{:?}", narrowed.body);

    let (denied_id, denied_token) = create_invite(&router, "domain", "example.com").await;
    let denied = post_json(
        router,
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("returning-admin"),
        redeem_body(&denied_token, "returning@example.com"),
    )
    .await;
    assert_eq!(
        denied.code(),
        "admin_claim_domain_not_allowed",
        "the domain policy must be evaluated before anything touches admin_identities"
    );
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    assert_eq!(invite_status(&fixture.pool, denied_id).await, "pending");
    assert_eq!(
        grant_count(&fixture.pool).await,
        1,
        "no refused redemption created a second grant"
    );
}

/// **Plan 08's defect, as a test.**
///
/// `governing_policy` matches `issuer = $1 or trusted_jwt_issuer_id = $2`. On a deployment
/// configured the way an operator actually configures one, `auth_provider_settings.issuer`
/// holds the *IdP's* issuer and the row reaches the console's issuer only through
/// `trusted_jwt_issuer_id`. Dropping that second argument — or passing a nil UUID — makes
/// every redemption 403 forever on exactly the deployments that are set up correctly.
///
/// The premise assertions are the point: without them, a suite whose provider row happened
/// to carry the matching `issuer` string would pass against the defect.
#[tokio::test]
async fn redeem_resolves_the_governing_policy_through_the_trusted_jwt_issuer_link() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let idp_issuer = "https://idp.example.test/realms/moira";
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByTrustedIssuerLink {
            other_issuer: idp_issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;

    // Premise 1 — the row's `issuer` column is NOT the token's issuer, so `$1` cannot
    // match it.
    assert_ne!(
        idp_issuer, issuer.issuer,
        "the arrangement under test requires the two issuers to differ"
    );
    // Premise 2 — exactly one enabled row, holding the IdP issuer and the console link.
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(idp_issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "email", "linked@example.com").await;
    let redeemed = post_json(
        router,
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("linked-colleague"),
        redeem_body(&token, "linked@example.com"),
    )
    .await;
    assert_eq!(
        redeemed.status,
        StatusCode::CREATED,
        "the governing policy was not found through trusted_jwt_issuer_id: {:?}",
        redeemed.body
    );
    assert_eq!(invite_status(&fixture.pool, invite_id).await, "consumed");
}

/// **Ownership is not something an admin can hand itself.**
///
/// `require_primary_actor` reads the caller's *own* `admin_identities` row and requires
/// `is_primary`. It cannot be a scope check: `moira:admin` implies every scope for a
/// trusted-JWT actor, so a `moira:admins:manage` scope would be held by every admin,
/// including the one whose ownership is being taken away.
///
/// The 403 alone would be a weak assertion — a request that failed to authenticate at all
/// also yields a 4xx — so the test first proves the same bearer *is* accepted on the admin
/// plane, then proves the ownership mutation is still refused.
#[tokio::test]
async fn a_non_primary_admin_cannot_promote_itself_to_primary() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    // The deployment needs an owner *other than* the caller under test: since finding F20
    // the first grant takes ownership, so without this the "non-primary admin" would be the
    // owner and the 403 below would never be due. This grant exists only to occupy the slot.
    let (_, owner_token) = create_invite(&router, "email", "owner@example.com").await;
    let owner = redeem(&router, &issuer, "owner", &owner_token).await;
    assert_eq!(owner.status, StatusCode::CREATED, "{:?}", owner.body);

    let (_, token) = create_invite(&router, "email", "member@example.com").await;
    let redeemed = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("member-subject"),
        redeem_body(&token, "member@example.com"),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::CREATED, "{:?}", redeemed.body);
    let grant_id: Uuid = redeemed.body["id"]
        .as_str()
        .expect("grant id")
        .parse()
        .expect("grant id is a uuid");
    let version = redeemed.body["version"].as_i64().expect("grant version");

    // Premise 1 — the caller holds a grant and that grant is not primary.
    let is_primary: bool =
        sqlx::query_scalar("select is_primary from admin_identities where id = $1")
            .bind(grant_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("read the redeemed grant");
    assert!(
        !is_primary,
        "the premise is a non-primary admin; a primary one would be allowed to transfer"
    );

    // Premise 2 — this bearer really is admin on the admin plane. Without this the 403
    // below could be an authentication failure wearing the same status code, and the test
    // would pass against an implementation with no ownership check at all.
    let listed = get_with(
        router.clone(),
        "/api/v1/admin/admin-identities",
        issuer.bearer("member-subject"),
    )
    .await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "the premise is a caller the admin plane accepts: {:?}",
        listed.body
    );

    let refused = send_json(
        router,
        "PATCH",
        &format!("/api/v1/admin/admin-identities/{grant_id}"),
        issuer.bearer("member-subject"),
        Some(version),
        json!({ "is_primary": true }),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    assert_eq!(refused.code(), "admin_identity_not_primary");
    assert_eq!(
        refused.message_key(),
        "moira.error.admin_identity_not_primary"
    );

    let still_not_primary: bool =
        sqlx::query_scalar("select is_primary from admin_identities where id = $1")
            .bind(grant_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("re-read the grant");
    assert!(
        !still_not_primary,
        "a non-primary admin promoted itself to owner"
    );
}

/// The invite's own constraint and the provider allow-list are checked separately and
/// reported separately, because the remedies differ: reissue the invite versus widen the
/// allow-list. A console that merged them would send the operator to the wrong screen.
#[tokio::test]
async fn an_invite_bound_to_another_address_is_refused_with_its_own_code_and_stays_pending() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    // Premise: the provider allow-list *does* admit the presented domain, so a refusal here
    // can only come from the invite's own constraint.
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "email", "intended@example.com").await;
    let refused = post_json(
        router,
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("interloper"),
        redeem_body(&token, "someone-else@example.com"),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    assert_eq!(refused.code(), "invite_email_mismatch");
    assert_eq!(refused.message_key(), "moira.error.invite_email_mismatch");
    assert_eq!(
        grant_count(&fixture.pool).await,
        0,
        "an address the invite was not issued for was granted admin"
    );
    assert_eq!(
        invite_status(&fixture.pool, invite_id).await,
        "pending",
        "a constraint mismatch consumed the invite"
    );
}

/// **Every refused redemption lands in the counter, and nothing else does.**
///
/// Five of the twelve `ADMIN_INVITE_OUTCOMES` label values — `expired`, `consumed`,
/// `revoked`, `email_mismatch`, `domain_mismatch` — were seeded at zero and emitted by
/// nothing: `require_redeemable` and `evaluate_invite_constraint` both returned with a
/// bare `?`, before any recorder call. An operator alerting on "invitations are being
/// refused" would have watched a flat line through every one of those refusals.
///
/// The zero-valued premise below is what makes the assertion mean something: a counter
/// that is absent and a counter that is stuck both read as `0.0` through
/// [`counter_value`], so the test first proves the family is registered and each series
/// starts at zero, then proves each one moves by exactly one.
///
/// The preview half is the other direction. `resolve_invite` used to record `not_found`
/// itself, and it serves the *anonymous* preview too — so a visitor opening a stale link,
/// or a scanner probing the endpoint, was indistinguishable from a failed redemption on
/// the dashboard.
#[tokio::test]
async fn every_refused_redemption_increments_a_bounded_counter_and_no_preview_does() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(observable_state(&fixture.pool).await).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    const REASONS: &[&str] = &[
        "not_found",
        "expired",
        "revoked",
        "email_mismatch",
        "domain_mismatch",
    ];
    let before = scrape_metrics(router.clone()).await;
    assert!(
        before.contains(INVITE_OUTCOMES),
        "the family must be registered before a zero reading means anything"
    );
    for reason in REASONS {
        assert_eq!(
            invite_outcome(&before, reason),
            0.0,
            "{reason} must start at zero"
        );
    }

    // A preview refusal is a lookup, not a redemption outcome.
    let previewed = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/preview",
        HeaderMap::new(),
        json!({ "token": "moira_inv_this-token-matches-nothing" }),
    )
    .await;
    assert_eq!(
        previewed.status,
        StatusCode::NOT_FOUND,
        "{:?}",
        previewed.body
    );
    assert_eq!(
        invite_outcome(&scrape_metrics(router.clone()).await, "not_found"),
        0.0,
        "an anonymous preview must not be counted as a failed redemption"
    );

    // 1 — not_found.
    let unknown = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("stranger"),
        redeem_body(
            "moira_inv_this-token-matches-nothing",
            "stranger@example.com",
        ),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{:?}", unknown.body);
    assert_eq!(unknown.code(), "invite_not_found");

    // 2 — expired.
    let (expired_id, expired_token) = create_invite(&router, "domain", "example.com").await;
    sqlx::query("update admin_invites set expires_at = now() - interval '1 hour' where id = $1")
        .bind(expired_id)
        .execute(&fixture.pool)
        .await
        .expect("backdate the invite expiry");
    let expired = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("late"),
        redeem_body(&expired_token, "late@example.com"),
    )
    .await;
    assert_eq!(expired.code(), "invite_expired", "{:?}", expired.body);

    // 3 — revoked.
    let (revoked_id, revoked_token) = create_invite(&router, "domain", "example.com").await;
    let revoked = post_json(
        router.clone(),
        &format!("/api/v1/admin/admin-invites/{revoked_id}/revoke"),
        HeaderMap::new(),
        Value::Null,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{:?}", revoked.body);
    let refused = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("withdrawn"),
        redeem_body(&revoked_token, "withdrawn@example.com"),
    )
    .await;
    assert_eq!(refused.code(), "invite_revoked", "{:?}", refused.body);

    // 4 — email_mismatch.
    let (_, bound_token) = create_invite(&router, "email", "intended@example.com").await;
    let mismatch = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("interloper"),
        redeem_body(&bound_token, "other@example.com"),
    )
    .await;
    assert_eq!(
        mismatch.code(),
        "invite_email_mismatch",
        "{:?}",
        mismatch.body
    );

    // 5 — domain_mismatch. The provider allow-list admits `example.com`, so the only
    // thing that can refuse this is the invite's own domain constraint.
    let (_, domain_token) = create_invite(&router, "domain", "invited.test").await;
    let wrong_domain = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        issuer.bearer("wrong-domain"),
        redeem_body(&domain_token, "someone@example.com"),
    )
    .await;
    assert_eq!(
        wrong_domain.code(),
        "invite_domain_mismatch",
        "{:?}",
        wrong_domain.body
    );
    assert_eq!(
        wrong_domain.message_key(),
        "moira.error.invite_domain_mismatch"
    );

    let after = scrape_metrics(router).await;
    for reason in REASONS {
        assert_eq!(
            invite_outcome(&after, reason),
            1.0,
            "moira_admin_invite_outcomes_total{{outcome=\"{reason}\"}} has no emitter\n{}",
            family_lines(&after, INVITE_OUTCOMES)
        );
    }
    assert_eq!(
        grant_count(&fixture.pool).await,
        0,
        "not one of these refusals may have created a grant"
    );
}

/// The lifetime cap is a **refusal**, not a silent clamp: an operator who believes they
/// issued a 30-day invitation and actually issued a 3-day one finds out at the worst
/// possible moment.
#[tokio::test]
async fn an_invitation_longer_than_the_cap_is_refused_and_no_row_is_written() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let refused = post_json(
        router,
        "/api/v1/admin/admin-invites",
        HeaderMap::new(),
        json!({
            "constraint": "email",
            "value": "colleague@example.com",
            // 30 days, well past the documented 72-hour ceiling.
            "expires_in_seconds": 30 * 24 * 60 * 60
        }),
    )
    .await;

    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{:?}",
        refused.body
    );
    assert_eq!(refused.code(), "admin_invite_expiry_too_long");
    assert_eq!(
        refused.message_key(),
        "moira.error.admin_invite_expiry_too_long"
    );
    let invites: i64 = sqlx::query_scalar("select count(*) from admin_invites")
        .fetch_one(&fixture.pool)
        .await
        .expect("count invites");
    assert_eq!(
        invites, 0,
        "a refused request must not have been clamped and stored"
    );
}

/// The two ownership codes plan 09 names with no specified emitter, pinned to a path and a
/// status: `admin_identity_not_found` on `PATCH .../{id}` and `admin_identity_already_revoked`
/// on a repeated `DELETE .../{id}`.
///
/// Both are exercised through the system-key-equivalent break-glass caller.
///
/// **The revocation target is deliberately the *second* grant.** Since finding F20 the first
/// grant on a deployment is its owner, and revoking the owner is refused by the last-primary
/// guard — so a test that revoked the first grant would now be asserting
/// `admin_identity_already_revoked` against a path that never reaches a revocation at all.
/// The premise that the target is not primary is asserted rather than assumed, because it is
/// exactly the thing that changed underneath this test.
#[tokio::test]
async fn the_grant_administration_conflicts_are_pinned_to_their_paths() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    // admin_identity_not_found — premise: this id really is absent.
    let absent = Uuid::now_v7();
    let present: i64 = sqlx::query_scalar("select count(*) from admin_identities where id = $1")
        .bind(absent)
        .fetch_one(&fixture.pool)
        .await
        .expect("count grants for the absent id");
    assert_eq!(present, 0, "the premise is an id with no row");
    let missing = send_json(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/admin-identities/{absent}"),
        HeaderMap::new(),
        Some(1),
        json!({ "is_primary": true }),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{:?}", missing.body);
    assert_eq!(missing.code(), "admin_identity_not_found");
    assert_eq!(
        missing.message_key(),
        "moira.error.admin_identity_not_found"
    );

    // The first redemption takes ownership (finding F20), so it exists here only to give
    // the second one something to be *not* the owner of.
    let (_, owner_token) = create_invite(&router, "domain", "example.com").await;
    let owner = redeem(&router, &issuer, "staying", &owner_token).await;
    assert_eq!(owner.status, StatusCode::CREATED, "{:?}", owner.body);

    let (_, token) = create_invite(&router, "domain", "example.com").await;
    let redeemed = redeem(&router, &issuer, "departing", &token).await;
    assert_eq!(redeemed.status, StatusCode::CREATED, "{:?}", redeemed.body);
    let grant_id: Uuid = redeemed.body["id"]
        .as_str()
        .expect("grant id")
        .parse()
        .expect("grant id is a uuid");
    assert_eq!(
        redeemed.body["is_primary"],
        json!(false),
        "the premise is a revocable, non-owning grant; the owner cannot be revoked"
    );

    // admin_identity_already_revoked — the soft revoke leaves the row, so the second call
    // has something to conflict with.
    let first = send_json(
        router.clone(),
        "DELETE",
        &format!("/api/v1/admin/admin-identities/{grant_id}"),
        HeaderMap::new(),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{:?}", first.body);
    let status: String = sqlx::query_scalar("select status from admin_identities where id = $1")
        .bind(grant_id)
        .fetch_one(&fixture.pool)
        .await
        .expect("read the revoked grant");
    assert_eq!(
        status, "revoked",
        "the premise is a soft revoke that left the row in place"
    );

    let again = send_json(
        router,
        "DELETE",
        &format!("/api/v1/admin/admin-identities/{grant_id}"),
        HeaderMap::new(),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{:?}", again.body);
    assert_eq!(again.code(), "admin_identity_already_revoked");
    assert_eq!(
        again.message_key(),
        "moira.error.admin_identity_already_revoked"
    );
}

/// The lockout guard. Clearing the last active primary would leave every remaining admin
/// unable to manage admins, with system-key break-glass as the only re-entry path.
///
/// This is the concrete payoff of decision D1: the guard is expressible **as a query** only
/// because ownership is row state. Under the scope design plan 09's body proposed, "who
/// else can manage admins" answered *everyone, by implication*, so there was nothing to
/// count.
#[tokio::test]
async fn clearing_the_only_primary_is_refused_with_the_last_primary_conflict() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (_, token) = create_invite(&router, "domain", "example.com").await;
    let redeemed = redeem(&router, &issuer, "owner-to-be", &token).await;
    assert_eq!(redeemed.status, StatusCode::CREATED, "{:?}", redeemed.body);
    let grant_id: Uuid = redeemed.body["id"]
        .as_str()
        .expect("grant id")
        .parse()
        .expect("grant id is a uuid");

    // Premise — exactly one active primary, so clearing it is clearing the last.
    //
    // This premise used to read the other way: before finding F20 a redeemed grant was
    // *not* primary, the deployment had none, and this test had to promote through
    // break-glass first to have anything to clear. The guard it exercises is unchanged;
    // what changed is that the state it guards is now reachable without a system key,
    // which was the whole point of the finding.
    assert_eq!(
        redeemed.body["is_primary"],
        json!(true),
        "the first grant on a deployment is its owner"
    );
    assert_eq!(
        active_primary_count(&fixture.pool).await,
        1,
        "the premise is exactly one active primary"
    );

    let refused = send_json(
        router,
        "PATCH",
        &format!("/api/v1/admin/admin-identities/{grant_id}"),
        HeaderMap::new(),
        redeemed.body["version"].as_i64(),
        json!({ "is_primary": false }),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{:?}", refused.body);
    assert_eq!(refused.code(), "admin_identity_last_primary");
    assert_eq!(
        refused.message_key(),
        "moira.error.admin_identity_last_primary"
    );

    let still_primary: bool =
        sqlx::query_scalar("select is_primary from admin_identities where id = $1")
            .bind(grant_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("re-read the grant");
    assert!(
        still_primary,
        "the refused clear must not have taken effect"
    );
}

/// **The single-winner gate, under a real race.**
///
/// `consume_invite` re-reads the row under `select … for update` inside the grant's own
/// transaction, so two simultaneous valid redemptions serialise and exactly one wins. The
/// pre-envelope validation cannot provide this — both requests pass it, because at the
/// moment they run the invite really is pending.
///
/// Released by a barrier rather than a `sleep()` (CONVENTIONS §3): the two requests must
/// overlap for the assertion to be about locking rather than about ordering. Both use
/// distinct `(issuer, subject)` pairs, so a second grant is *possible* — which is what
/// makes "exactly one" an assertion rather than a restatement of the uniqueness index.
#[tokio::test]
async fn two_simultaneous_redemptions_of_one_token_produce_exactly_one_grant() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (invite_id, token) = create_invite(&router, "domain", "example.com").await;
    assert_eq!(invite_status(&fixture.pool, invite_id).await, "pending");

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for subject in ["racer-one", "racer-two"] {
        let router = router.clone();
        let headers = issuer.bearer(subject);
        let body = redeem_body(&token, &format!("{subject}@example.com"));
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            post_json(router, "/api/v1/admin/admin-invites/redeem", headers, body).await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.expect("redeem task"));
    }

    let created = results
        .iter()
        .filter(|result| result.status == StatusCode::CREATED)
        .count();
    assert_eq!(
        created,
        1,
        "exactly one redemption may win: {:?}",
        results
            .iter()
            .map(|r| (r.status, r.code()))
            .collect::<Vec<_>>()
    );
    let loser = results
        .iter()
        .find(|result| result.status != StatusCode::CREATED)
        .expect("one loser");
    assert_eq!(loser.status, StatusCode::CONFLICT, "{:?}", loser.body);
    assert_eq!(loser.code(), "invite_already_consumed");

    assert_eq!(
        grant_count(&fixture.pool).await,
        1,
        "a single-use invitation produced two grants"
    );
    assert_eq!(invite_status(&fixture.pool, invite_id).await, "consumed");
}

// ===========================================================================================
// Ownership — finding F20 and decision D-F20.
//
// `0017` made ownership row state and bootstrapped it with a migration-time `UPDATE` that,
// on a greenfield deployment, ran against an empty `admin_identities`. Nothing else ever
// wrote `is_primary` except the transfer endpoint, which requires a primary caller — so on
// every deployment created after `0017` no admin was ever primary, the last-primary guard
// protected an empty set, and ownership transfer was unreachable without the break-glass
// system key the invitation flow exists to let an operator retire.
//
// `0019` plus `insert_grant` close it: the first grant on a deployment with no owner becomes
// the owner, decided under the `moiraown` advisory lock and backstopped by
// `admin_identities_single_active_primary`.
// ===========================================================================================

/// The pair, and only the pair, is the property.
///
/// "The first grant owns the deployment" on its own would pass equally well against an
/// `insert_grant` that set the flag unconditionally — which is the bug on the other side, and
/// a worse one: an invitation that mints an owner every time is strictly more powerful than
/// the transfer endpoint it sits beneath. So the second half is asserted in the same test.
#[tokio::test]
async fn the_first_grant_on_a_deployment_owns_it_and_the_second_does_not() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    // Premise: a deployment with no grant and therefore no owner. This is the state F20
    // says every post-`0017` deployment was permanently stuck in.
    assert_eq!(grant_count(&fixture.pool).await, 0);
    assert_eq!(active_primary_count(&fixture.pool).await, 0);

    let (_, first_token) = create_invite(&router, "domain", "example.com").await;
    let first = redeem(&router, &issuer, "founder", &first_token).await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    assert_eq!(
        first.body["is_primary"],
        json!(true),
        "the first grant on an ownerless deployment must take ownership"
    );
    // The response is not taken on trust: `record_from_grant_with_notice` builds it from the
    // row `insert_grant` returned, so asserting the body alone would still pass against a
    // service that reported a flag the database never stored.
    assert!(is_primary(&fixture.pool, grant_id(&first)).await);

    let (_, second_token) = create_invite(&router, "domain", "example.com").await;
    let second = redeem(&router, &issuer, "colleague", &second_token).await;
    assert_eq!(second.status, StatusCode::CREATED, "{:?}", second.body);
    assert_eq!(
        second.body["is_primary"],
        json!(false),
        "an invitation redeemed on a deployment that already has an owner must not mint a \
         second one — that would make an invite more powerful than the transfer endpoint"
    );
    assert!(!is_primary(&fixture.pool, grant_id(&second)).await);
    assert_eq!(active_primary_count(&fixture.pool).await, 1);
}

/// **Exactly one owner, under a real race.**
///
/// Two *different* invitations redeemed by two *different* identities, so both requests
/// genuinely succeed and two grants really are created — which is what makes "exactly one
/// primary" an assertion about ownership rather than a restatement of the single-winner
/// invite gate above. Released by a barrier rather than a `sleep()` (CONVENTIONS §3): the
/// two must overlap for this to be about locking rather than about ordering.
///
/// The mechanism under test is the `moiraown` transaction advisory lock `insert_grant` takes.
/// Without it, both inserts evaluate "does an owner exist?" against a snapshot taken before
/// either committed, both answer no, and the loser is refused by
/// `admin_identities_single_active_primary` — turning a legitimate second grant into a `500`.
/// So the assertion is deliberately **not** "one of them failed": both must be `201`, and
/// exactly one must own. A test that accepted a failing racer would pass against the
/// index-only design this is specifically not.
#[tokio::test]
async fn two_simultaneous_first_grants_produce_exactly_one_owner_and_no_failure() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;
    assert_eq!(
        active_primary_count(&fixture.pool).await,
        0,
        "the premise is an ownerless deployment: both racers must be able to win"
    );

    // One invitation each, so the invite's own single-winner gate cannot decide this.
    let mut tokens = Vec::new();
    for _ in 0..2 {
        tokens.push(create_invite(&router, "domain", "example.com").await.1);
    }

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (subject, token) in ["racer-one", "racer-two"].into_iter().zip(tokens) {
        let router = router.clone();
        let headers = issuer.bearer(subject);
        let body = redeem_body(&token, &format!("{subject}@example.com"));
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            post_json(router, "/api/v1/admin/admin-invites/redeem", headers, body).await
        }));
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.expect("redeem task"));
    }

    for result in &results {
        assert_eq!(
            result.status,
            StatusCode::CREATED,
            "both redemptions are valid and must both succeed; a race for *ownership* must \
             not fail a request: {:?}",
            result.body
        );
    }
    assert_eq!(grant_count(&fixture.pool).await, 2);
    assert_eq!(
        active_primary_count(&fixture.pool).await,
        1,
        "two grants raced for an empty ownership slot and both took it"
    );
    // The bodies must agree with the row: exactly one of the two responses claims ownership.
    let claimed = results
        .iter()
        .filter(|result| result.body["is_primary"] == json!(true))
        .count();
    assert_eq!(
        claimed,
        1,
        "exactly one response may report ownership: {:?}",
        results
            .iter()
            .map(|result| result.body["is_primary"].clone())
            .collect::<Vec<_>>()
    );
}

/// Transfer **moves** the flag; it does not add a second holder — and it is the *only* thing
/// `moira_admin_identity_grant_events_total{event="ownership_transferred"}` counts.
///
/// Asserted on the incumbent as well as on the target, because "the target is now primary" is
/// true of both designs and only the incumbent's demotion distinguishes them. The incumbent's
/// `version` is checked too: a demotion that did not bump it would leave a console holding a
/// stale `ETag` that still validates, and the next `If-Match` write would act on a record
/// whose ownership had moved underneath it.
///
/// The counter half is a wave-2 leftover: `record_admin_ownership_transferred` fired after
/// **every** successful `PATCH`, including one that *cleared* the flag — a different event —
/// so a `{"is_primary": false}` on a grant that was never the owner, which changes nothing at
/// all, still reported an ownership transfer. Both directions are exercised here, because
/// asserting only the increment would pass against the version that counts everything.
#[tokio::test]
async fn transferring_ownership_moves_the_flag_rather_than_adding_a_second_owner() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(observable_state(&fixture.pool).await).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (_, outgoing_token) = create_invite(&router, "domain", "example.com").await;
    let outgoing = redeem(&router, &issuer, "outgoing", &outgoing_token).await;
    assert_eq!(outgoing.status, StatusCode::CREATED, "{:?}", outgoing.body);
    let (_, incoming_token) = create_invite(&router, "domain", "example.com").await;
    let incoming = redeem(&router, &issuer, "incoming", &incoming_token).await;
    assert_eq!(incoming.status, StatusCode::CREATED, "{:?}", incoming.body);

    // Premise: `outgoing` owns it, `incoming` does not, and nothing has been transferred yet.
    assert!(is_primary(&fixture.pool, grant_id(&outgoing)).await);
    assert!(!is_primary(&fixture.pool, grant_id(&incoming)).await);
    let outgoing_version_before = outgoing.body["version"].as_i64().expect("a version");
    let before = scrape_metrics(router.clone()).await;
    assert!(
        before.contains(GRANT_EVENTS),
        "the family must be registered before a zero reading means anything"
    );
    assert_eq!(ownership_transfers(&before), 0.0);

    // A `PATCH` that clears the flag on a grant which never held it: a successful 200 that
    // changes nothing. It is the case that used to be counted as an ownership transfer.
    let noop = send_json(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/admin-identities/{}", grant_id(&incoming)),
        HeaderMap::new(),
        incoming.body["version"].as_i64(),
        json!({ "is_primary": false }),
    )
    .await;
    assert_eq!(noop.status, StatusCode::OK, "{:?}", noop.body);
    assert_eq!(noop.body["is_primary"], json!(false));
    assert_eq!(
        ownership_transfers(&scrape_metrics(router.clone()).await),
        0.0,
        "clearing a flag nobody held is not an ownership transfer"
    );
    // **And it must not have touched the real owner.** Found by `cargo mutants`: turning
    // `set_primary`'s `is_primary && !current.is_primary` into `||` makes the demotion run on
    // a *clear* as well, so this request — which changes nothing on its own target — would
    // strip ownership from `outgoing` and leave the deployment with none. That is precisely
    // the ownerless state finding F20 describes, reached through a `200 OK` and around the
    // last-primary guard, which only ever looks at the row being written.
    assert!(
        is_primary(&fixture.pool, grant_id(&outgoing)).await,
        "a no-op clear on one grant must not demote a different grant"
    );
    assert_eq!(
        active_primary_count(&fixture.pool).await,
        1,
        "the deployment must still have exactly one owner"
    );

    // `If-Match` carries the version the no-op left behind: the bump trigger fires on any
    // UPDATE, so reusing the pre-no-op version would fail with `resource_version_conflict`
    // and this test would be asserting the wrong refusal.
    let transferred = send_json(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/admin-identities/{}", grant_id(&incoming)),
        HeaderMap::new(),
        Some(noop.version()),
        json!({ "is_primary": true }),
    )
    .await;
    assert_eq!(transferred.status, StatusCode::OK, "{:?}", transferred.body);
    assert_eq!(transferred.body["is_primary"], json!(true));
    assert_eq!(
        ownership_transfers(&scrape_metrics(router.clone()).await),
        1.0,
        "the flag moved onto someone, which is the one thing this counter means"
    );

    assert!(
        !is_primary(&fixture.pool, grant_id(&outgoing)).await,
        "the incumbent must have been demoted, not left in place"
    );
    assert_eq!(
        active_primary_count(&fixture.pool).await,
        1,
        "a transfer must leave exactly one owner"
    );
    let outgoing_version_after: i64 =
        sqlx::query_scalar("select version from admin_identities where id = $1")
            .bind(grant_id(&outgoing))
            .fetch_one(&fixture.pool)
            .await
            .expect("re-read the demoted grant");
    assert!(
        outgoing_version_after > outgoing_version_before,
        "the demoted grant's version must move, or a stale ETag keeps validating"
    );
}

/// A replayed revocation is not a second revocation.
///
/// **Found by `cargo mutants`**, which deleted the `!` from `revoke_identity`'s
/// `if !outcome.replayed` and watched the whole suite stay green: the guard was written in the
/// same change as the ones for `create_invite` and `redeem_invite`, and unlike those two it
/// had nothing asserting it. A retry under the same `Idempotency-Key` replays the stored
/// success — the row is already `revoked`, so no second revocation happened — and counting it
/// again would report two admins losing access when one did.
#[tokio::test]
async fn a_replayed_revocation_does_not_count_a_second_one() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(observable_state(&fixture.pool).await).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    // The owner exists only so the target is revocable: revoking the owner is refused by the
    // last-primary guard, which would make this test assert a conflict instead of a replay.
    let (_, owner_token) = create_invite(&router, "domain", "example.com").await;
    let owner = redeem(&router, &issuer, "owner", &owner_token).await;
    assert_eq!(owner.status, StatusCode::CREATED, "{:?}", owner.body);
    let (_, token) = create_invite(&router, "domain", "example.com").await;
    let target = redeem(&router, &issuer, "departing", &token).await;
    assert_eq!(target.status, StatusCode::CREATED, "{:?}", target.body);
    assert!(!is_primary(&fixture.pool, grant_id(&target)).await);

    let before = scrape_metrics(router.clone()).await;
    assert!(before.contains(GRANT_EVENTS));
    assert_eq!(
        counter_value(&before, GRANT_EVENTS, "event", "revoked"),
        0.0
    );

    let key = format!("revoke-{}", Uuid::now_v7());
    let path = format!("/api/v1/admin/admin-identities/{}", grant_id(&target));
    let revoked = send_json(
        router.clone(),
        "DELETE",
        &path,
        with_idempotency_key(HeaderMap::new(), &key),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{:?}", revoked.body);
    assert_eq!(
        counter_value(
            &scrape_metrics(router.clone()).await,
            GRANT_EVENTS,
            "event",
            "revoked"
        ),
        1.0
    );

    let replayed = send_json(
        router.clone(),
        "DELETE",
        &path,
        with_idempotency_key(HeaderMap::new(), &key),
        None,
        Value::Null,
    )
    .await;
    // The premise: this is a *replay* of the stored success, not a second revocation reaching
    // the repository — which would have produced `409 admin_identity_already_revoked`.
    assert_eq!(replayed.status, StatusCode::OK, "{:?}", replayed.body);
    assert_eq!(replayed.body["id"], revoked.body["id"]);

    let after = scrape_metrics(router).await;
    assert_eq!(
        counter_value(&after, GRANT_EVENTS, "event", "revoked"),
        1.0,
        "one revocation counted twice\n{}",
        family_lines(&after, GRANT_EVENTS)
    );
}

/// The other side of the last-primary guard: a revocation clears `is_primary` too, so the
/// guard refuses it for the owner just as it refuses an explicit clear.
///
/// This is a *consequence* of decision D-F20 worth pinning rather than discovering: since the
/// first grant on a deployment is now its owner, a deployment's sole admin cannot be revoked
/// through the API at all. Transfer first, then revoke. An admin plane with no owner is
/// precisely the state finding F20 describes, so refusing to create one is the point.
#[tokio::test]
async fn revoking_the_owner_is_refused_by_the_last_primary_guard() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let (_, token) = create_invite(&router, "domain", "example.com").await;
    let owner = redeem(&router, &issuer, "sole-admin", &token).await;
    assert_eq!(owner.status, StatusCode::CREATED, "{:?}", owner.body);
    assert!(is_primary(&fixture.pool, grant_id(&owner)).await);

    let refused = send_json(
        router,
        "DELETE",
        &format!("/api/v1/admin/admin-identities/{}", grant_id(&owner)),
        HeaderMap::new(),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{:?}", refused.body);
    assert_eq!(refused.code(), "admin_identity_last_primary");

    let status: String = sqlx::query_scalar("select status from admin_identities where id = $1")
        .bind(grant_id(&owner))
        .fetch_one(&fixture.pool)
        .await
        .expect("re-read the grant");
    assert_eq!(
        status, "active",
        "the refused revocation must not have taken effect"
    );
    assert_eq!(active_primary_count(&fixture.pool).await, 1);
}

/// **An idempotent replay is not a second event.**
///
/// `create_invite` and `redeem_invite` incremented their counters immediately after
/// `AdminCommandRunner::execute` returned — which a replay also does. So one invitation
/// retried under the same `Idempotency-Key` was counted as two, in the direction that
/// overstates activity, and `redeemed` drags
/// `moira_admin_identity_grant_events_total{event="granted"}` along with it, putting the
/// miscount on the grant ledger as well.
///
/// The premise that each second request really *is* a replay is asserted before the counter
/// is read: same resource id, and — for the invitation — `secret` withheld, which is the
/// observable that distinguishes a stored body from a fresh mint, since the token is returned
/// exactly once.
#[tokio::test]
async fn an_idempotent_replay_does_not_count_a_second_invitation_or_redemption() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(observable_state(&fixture.pool).await).expect("router");
    let (provider_id, _) = configure_and_enable_policy(
        &router,
        PolicyBinding::ByIssuerString {
            issuer: &issuer.issuer,
            id: issuer.id,
        },
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(
        &fixture.pool,
        provider_id,
        Some(&issuer.issuer),
        Some(issuer.id),
    )
    .await;

    let before = scrape_metrics(router.clone()).await;
    assert!(
        before.contains(INVITE_OUTCOMES),
        "the family must be registered before a zero reading means anything"
    );
    assert_eq!(invite_outcome(&before, "created"), 0.0);
    assert_eq!(invite_outcome(&before, "redeemed"), 0.0);

    let create_key = format!("invite-create-{}", Uuid::now_v7());
    let create_body = json!({
        "constraint": "domain",
        "value": "example.com",
        "expires_in_seconds": 3600
    });
    let created = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites",
        with_idempotency_key(HeaderMap::new(), &create_key),
        create_body.clone(),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let token = created.body["secret"]
        .as_str()
        .expect("the token is returned exactly once, at creation")
        .to_string();
    assert_eq!(
        invite_outcome(&scrape_metrics(router.clone()).await, "created"),
        1.0,
        "one invitation, one increment"
    );

    let replayed = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites",
        with_idempotency_key(HeaderMap::new(), &create_key),
        create_body,
    )
    .await;
    assert_eq!(replayed.status, StatusCode::CREATED, "{:?}", replayed.body);
    assert_eq!(
        replayed.body["resource"]["id"], created.body["resource"]["id"],
        "the premise is a replay of the same invitation, not a second one"
    );
    assert!(
        replayed.body["secret"].is_null(),
        "a replayed creation withholds the token — that absence is what proves this body \
         came from the ledger rather than from a second mint"
    );
    let invites: i64 = sqlx::query_scalar("select count(*) from admin_invites")
        .fetch_one(&fixture.pool)
        .await
        .expect("count invites");
    assert_eq!(invites, 1, "a replay must not have written a second row");
    assert_eq!(
        invite_outcome(&scrape_metrics(router.clone()).await, "created"),
        1.0,
        "one invitation counted twice: the replay incremented moira_admin_invite_outcomes_total"
    );

    let redeem_key = format!("invite-redeem-{}", Uuid::now_v7());
    let body = redeem_body(&token, "invitee@example.com");
    let redeemed = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        with_idempotency_key(issuer.bearer("invitee"), &redeem_key),
        body.clone(),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::CREATED, "{:?}", redeemed.body);
    let after_success = scrape_metrics(router.clone()).await;
    assert_eq!(invite_outcome(&after_success, "redeemed"), 1.0);
    assert_eq!(
        counter_value(&after_success, GRANT_EVENTS, "event", "granted"),
        1.0,
        "`redeemed` also drives the grant ledger"
    );

    // **A successful redemption cannot be replayed, and that is not what the endpoint
    // documents.** `redeem_invite` validates the invite *before* the transactional envelope
    // — deliberately, so a policy refusal never consumes it — and by the time the retry
    // arrives the invite is `consumed`, so `require_redeemable` refuses it with
    // `invite_already_consumed` and `AdminCommandRunner` is never entered. The
    // `Idempotency-Key` is read but can never matter on the success path.
    //
    // Asserted rather than worked around, because the alternative reading — "the counter
    // guard on the success path is what stops the double count here" — is false, and a test
    // written to that reading would pass while proving nothing.
    let retried = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        with_idempotency_key(issuer.bearer("invitee"), &redeem_key),
        body,
    )
    .await;
    assert_eq!(
        retried.status,
        StatusCode::CONFLICT,
        "a retried successful redemption is refused by the pre-envelope check, not replayed: \
         {:?}",
        retried.body
    );
    assert_eq!(retried.code(), "invite_already_consumed");
    assert_eq!(
        grant_count(&fixture.pool).await,
        1,
        "and no second grant was written"
    );

    // The refusal that *can* replay: one raised **inside** the envelope. A second identity
    // that already holds a grant gets `admin_identity_already_claimed` from `insert_grant` —
    // a cacheable `409`, so the ledger stores it, `consume_invite` never runs, and the invite
    // stays pending. The retry therefore passes pre-envelope validation, reaches the ledger,
    // and comes back as `AppError::Replayed`. That is the path where a refusal really was
    // counted once per client retry.
    let (second_invite_id, second_token) = create_invite(&router, "domain", "example.com").await;
    let denied_key = format!("invite-denied-{}", Uuid::now_v7());
    let denied_body = redeem_body(&second_token, "invitee@example.com");
    let denied = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        with_idempotency_key(issuer.bearer("invitee"), &denied_key),
        denied_body.clone(),
    )
    .await;
    assert_eq!(denied.status, StatusCode::CONFLICT, "{:?}", denied.body);
    assert_eq!(
        denied.code(),
        "admin_identity_already_claimed",
        "the premise is a refusal raised inside the envelope, not before it"
    );
    assert_eq!(
        invite_status(&fixture.pool, second_invite_id).await,
        "pending",
        "the premise is that the invite survived, so the retry gets past require_redeemable \
         and reaches the ledger where the stored failure lives"
    );
    let after_denial = scrape_metrics(router.clone()).await;
    assert_eq!(
        invite_outcome(&after_denial, "other"),
        1.0,
        "admin_identity_already_claimed is outside the bounded reason table, so it lands in \
         `other`\n{}",
        family_lines(&after_denial, INVITE_OUTCOMES)
    );

    let denied_replay = post_json(
        router.clone(),
        "/api/v1/admin/admin-invites/redeem",
        with_idempotency_key(issuer.bearer("invitee"), &denied_key),
        denied_body,
    )
    .await;
    assert_eq!(
        denied_replay.status,
        StatusCode::CONFLICT,
        "{:?}",
        denied_replay.body
    );
    assert_eq!(denied_replay.code(), "admin_identity_already_claimed");

    let after = scrape_metrics(router).await;
    assert_eq!(
        invite_outcome(&after, "other"),
        1.0,
        "one refused redemption counted twice: a replayed failure is the stored response of \
         the refusal that already happened, so counting it again measures the client's retry \
         policy rather than the denial rate\n{}",
        family_lines(&after, INVITE_OUTCOMES)
    );
    assert_eq!(
        invite_outcome(&after, "redeemed"),
        1.0,
        "and the success counter has not moved either"
    );
}
