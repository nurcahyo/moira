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

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::LifecycleFixture;
use tokio::{net::TcpListener, task::JoinHandle};
use tower::ServiceExt;
use uuid::Uuid;

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
    /// `auth_provider_settings.issuer` equals the token's issuer. The `governing_policy`
    /// query matches on `$1` and the `trusted_jwt_issuer_id` argument is never needed.
    ByIssuerString(&'a str),
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
        PolicyBinding::ByIssuerString(issuer) => (issuer.to_string(), Value::Null),
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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;
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
        json!(false),
        "an invite grants base admin authority and never ownership"
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
    assert!(!grant.3, "a redeemed grant is not primary");
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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;

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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;

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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;

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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["already-onboarded.test"],
    )
    .await;
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;

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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["example.com"],
    )
    .await;
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;

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
        PolicyBinding::ByIssuerString(&issuer.issuer),
        &["example.com"],
    )
    .await;
    // Premise: the provider allow-list *does* admit the presented domain, so a refusal here
    // can only come from the invite's own constraint.
    assert_exactly_one_enabled_policy(&fixture.pool, provider_id, Some(&issuer.issuer), None).await;

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
