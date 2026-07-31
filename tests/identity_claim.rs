//! End-to-end coverage for plan 07's admin-identity claiming surface.
//!
//! The headline property is `plans/01` §4.4's: **"the first successful admin JWT wins" must
//! be structurally impossible**. Two tests hold it from opposite sides —
//! `bare_trusted_jwt_cannot_claim_regardless_of_its_scopes` proves a verified JWT cannot
//! *create* a grant, and `a_granted_identity_gains_admin_scope_only_on_the_admin_plane`
//! proves a grant that does exist does not silently widen the public execution API.

mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use moira::app::AppState;
use serde_json::{Value, json};
use support::LifecycleFixture;
use tokio::{net::TcpListener, sync::Barrier, task::JoinHandle};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_KEY_ID: &str = "identity-claim-test-key";
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

    fn message_key(&self) -> &str {
        self.body["error"]["message_key"]
            .as_str()
            .unwrap_or_default()
    }

    fn version(&self) -> i64 {
        self.etag
            .as_deref()
            .map(|value| value.trim_matches('"'))
            .and_then(|value| value.parse().ok())
            .expect("an ETag carrying the resource version")
    }
}

/// A console-style trusted JWT issuer: **`scopes_claim` is NULL**, which CONVENTIONS §7.5
/// requires of any issuer a console links, so its tokens cannot self-assert authority. The
/// tokens minted here therefore carry no scopes at all, and every scope the resulting actor
/// holds must have come from an `admin_identities` grant.
struct ConsoleIssuer {
    /// The `trusted_jwt_issuers.id`. Every provider row this suite configures is BOUND to
    /// it: since plan 09 wave 4A a row whose own `issuer` column names a registered trusted
    /// issuer it is not bound to is refused `409 auth_provider_issuer_shadows_trusted_issuer`
    /// (finding F23 shape (b)), and binding is what a real console deployment does anyway.
    id: Uuid,
    issuer: String,
    task: JoinHandle<()>,
}

impl ConsoleIssuer {
    async fn start(pool: &sqlx::PgPool) -> Self {
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

    fn token(&self, subject: &str, scopes: &[&str]) -> String {
        let mut claims = json!({
            "iss": self.issuer,
            "sub": subject,
            "exp": chrono::Utc::now().timestamp() + 3600
        });
        if !scopes.is_empty() {
            claims["scope"] = Value::String(scopes.join(" "));
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KEY_ID.to_string());
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY.as_bytes())
                .expect("parse test RSA private key"),
        )
        .expect("sign test JWT")
    }

    fn bearer(&self, subject: &str, scopes: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", self.token(subject, scopes))
                .parse()
                .expect("authorization header"),
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
        .header("x-request-id", format!("identity-{}", Uuid::now_v7()))
}

async fn get(router: Router, path: &str) -> HttpResult {
    send(router, base("GET", path).body(Body::empty()).unwrap()).await
}

async fn get_with(router: Router, path: &str, headers: HeaderMap) -> HttpResult {
    let mut builder = base("GET", path);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    send(router, builder.body(Body::empty()).unwrap()).await
}

async fn post_json(
    router: Router,
    path: &str,
    headers: HeaderMap,
    idempotency_key: Option<&str>,
    if_match: Option<i64>,
    body: Value,
) -> HttpResult {
    let mut builder = base("POST", path).header("content-type", "application/json");
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    if let Some(key) = idempotency_key {
        builder = builder.header("idempotency-key", key);
    }
    if let Some(version) = if_match {
        builder = builder.header("if-match", version.to_string());
    }
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

/// Releases every task on the same `barrier.wait()` before it issues the claim, so the
/// requests race rather than run sequentially — the acknowledgement-gate pattern
/// `tests/admin_idempotency.rs` uses (`spawn_post` there), in place of a fixed `sleep()`
/// (CONVENTIONS §3).
#[allow(clippy::too_many_arguments)]
fn spawn_claim(
    router: Router,
    barrier: std::sync::Arc<Barrier>,
    headers: HeaderMap,
    idempotency_key: Option<String>,
    body: Value,
) -> JoinHandle<HttpResult> {
    tokio::spawn(async move {
        barrier.wait().await;
        post_json(
            router,
            "/api/v1/admin/setup/claim",
            headers,
            idempotency_key.as_deref(),
            None,
            body,
        )
        .await
    })
}

fn system_key_headers(secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-moira-system-key",
        secret.parse().expect("system key header"),
    );
    headers
}

/// Mints a real system key through the admin API, so the claim tests present the credential
/// an operator would actually hold rather than a synthetic `Actor`.
async fn mint_system_key(router: &Router, state: &AppState) -> String {
    let _ = state;
    let created = post_json(
        router.clone(),
        "/api/v1/admin/system-keys",
        HeaderMap::new(),
        None,
        None,
        json!({
            "display_name": format!("identity-claim-{}", Uuid::now_v7()),
            "scopes": ["moira:admin"]
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    created.body["secret"]
        .as_str()
        .expect("the secret is returned exactly once, at creation")
        .to_string()
}

/// The designed setup order, steps 3 and 4: create an auth-provider configuration carrying
/// the intended `allowed_email_domains`, then enable it. Until this runs, every claim is
/// refused — that is the deny-by-default policy, not a defect.
async fn configure_and_enable_policy(
    router: &Router,
    issuer: &ConsoleIssuer,
    domains: &[&str],
) -> Uuid {
    let created = post_json(
        router.clone(),
        "/api/v1/admin/auth/providers",
        HeaderMap::new(),
        None,
        None,
        json!({
            "method": "generic_oidc",
            "display_name": "Console",
            // The IdP's issuer, which is what a real deployment stores in this column,
            // plus the binding that is what actually resolves the admission policy. Before
            // wave 4A this row carried the CONSOLE's issuer and no binding; that shape is
            // now refused at create time (F23 shape (b)) and the binding is what
            // `admission_policy`'s first stage matches on.
            "issuer": format!("https://idp.test/{}", Uuid::now_v7().simple()),
            "trusted_jwt_issuer_id": issuer.id,
            "client_id": "console-client",
            "allowed_email_domains": domains
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id: Uuid = created.body["id"].as_str().unwrap().parse().unwrap();

    let enabled = post_json(
        router.clone(),
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        None,
        Some(created.version()),
        Value::Null,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);
    id
}

fn claim_body(issuer: &str, subject: &str, email: &str) -> Value {
    json!({
        "issuer": issuer,
        "subject": subject,
        "email": email,
        "email_verified": true
    })
}

#[tokio::test]
async fn claim_status_is_unauthenticated_and_returns_only_a_boolean() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let status = get(router, "/api/v1/admin/setup/claim-status").await;

    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.body, json!({ "claimed": false }));
    // The shape is frozen for plans 08/09: one key, and that key is `claimed`. A count, a
    // timestamp, or an issuer would each be a reconnaissance gift on the one endpoint an
    // anonymous caller can reach.
    assert_eq!(
        status.body.as_object().expect("an object").len(),
        1,
        "the claim-status contract is a single boolean and nothing else"
    );
}

#[tokio::test]
async fn bare_trusted_jwt_cannot_claim_regardless_of_its_scopes() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    // A JWT that verifies perfectly *and* asserts `moira:admin` — the strongest bearer
    // credential the trust model can produce.
    let refused = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        issuer.bearer("land-grabber", &["moira:admin"]),
        None,
        None,
        claim_body(&issuer.issuer, "land-grabber", "owner@example.com"),
    )
    .await;

    assert_eq!(
        refused.status,
        StatusCode::UNAUTHORIZED,
        "{:?}",
        refused.body
    );
    assert_eq!(refused.code(), "setup_claim_credential_required");
    assert_eq!(
        refused.message_key(),
        "moira.error.setup_claim_credential_required"
    );

    let grants: i64 = sqlx::query_scalar("select count(*) from admin_identities")
        .fetch_one(&fixture.pool)
        .await
        .expect("count grants");
    assert_eq!(grants, 0, "a refused claim must write no grant");
    let status = get(router, "/api/v1/admin/setup/claim-status").await;
    assert_eq!(status.body["claimed"], json!(false));
}

#[tokio::test]
async fn claim_is_denied_when_no_auth_provider_configuration_exists_at_all() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;

    // Fresh deployment: zero `auth_provider_settings` rows, valid system key, nothing
    // claimed. This is the no-bootstrap-bypass test — it cannot be made to pass by adding
    // a first-claim exemption.
    let denied = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "owner", "owner@example.com"),
    )
    .await;

    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    assert_eq!(denied.code(), "admin_claim_domain_not_allowed");
    assert_eq!(
        denied.message_key(),
        "moira.error.admin_claim_domain_not_allowed"
    );
    assert!(
        !denied.body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "every error response carries a non-empty message (CONVENTIONS §4.5)"
    );
}

#[tokio::test]
async fn claim_succeeds_once_the_operator_configures_and_enables_the_allowed_domain() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let granted = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "owner", "Owner@Example.com"),
    )
    .await;

    assert_eq!(granted.status, StatusCode::CREATED, "{:?}", granted.body);
    assert_eq!(granted.body["granted_scopes"], json!(["moira:admin"]));
    assert_eq!(
        granted.body["notice"]["message_key"],
        "moira.notice.admin_identity_claimed"
    );
    let email: Option<String> =
        sqlx::query_scalar("select email from admin_identities where issuer = $1")
            .bind(&issuer.issuer)
            .fetch_one(&fixture.pool)
            .await
            .expect("read the grant");
    assert!(
        email.is_some_and(|email| !email.is_empty()),
        "every grant carries the human-identifiable audit attribute"
    );

    let status = get(router, "/api/v1/admin/setup/claim-status").await;
    assert_eq!(status.body["claimed"], json!(true));
}

/// **Decision D2, asserted in both directions.**
///
/// A grant that exists must widen the admin plane and must **not** widen the public
/// execution API. One direction alone proves nothing: a test that only checks the 403 would
/// pass against an implementation that never applies the grant anywhere, and a test that
/// only checks the admin plane would pass against the privilege-escalating placement this
/// plan's body originally implied (inside `authenticate_trusted_jwt`, which
/// `authenticate_caller` also calls and whose actor it returns verbatim).
#[tokio::test]
async fn a_granted_identity_gains_admin_scope_only_on_the_admin_plane() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let bearer = issuer.bearer("granted-human", &[]);
    let pool = &fixture.pool;

    // Before the grant: the console issuer maps no scopes claim and the token asserts
    // none, so both planes see an actor with no scopes at all.
    let before_admin = fixture
        .state
        .auth
        .authenticate_admin(pool, &bearer)
        .await
        .expect("the JWT verifies before any grant exists");
    assert!(before_admin.scopes.is_empty());

    let claimed = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "granted-human", "owner@example.com"),
    )
    .await;
    assert_eq!(claimed.status, StatusCode::CREATED, "{:?}", claimed.body);

    // Direction 1 — the grant DOES apply on the admin plane.
    let admin_actor = fixture
        .state
        .auth
        .authenticate_admin(pool, &bearer)
        .await
        .expect("admin authentication");
    assert_eq!(
        admin_actor.scopes,
        vec!["moira:admin".to_string()],
        "the grant must be unioned onto the admin-plane actor"
    );
    fixture
        .state
        .authz
        .require(&admin_actor, "moira:auth-settings:read")
        .expect("a granted admin holds the admin surface's scopes by implication");

    let admin_read = get_with(
        router.clone(),
        "/api/v1/admin/auth/providers",
        bearer.clone(),
    )
    .await;
    assert_eq!(
        admin_read.status,
        StatusCode::OK,
        "the grant must work end to end through the router: {:?}",
        admin_read.body
    );

    // Direction 2 — the grant does NOT apply on the public plane.
    let caller_actor = fixture
        .state
        .auth
        .authenticate_caller(pool, &bearer)
        .await
        .expect("caller authentication");
    assert!(
        caller_actor.scopes.is_empty(),
        "the public-plane actor must be byte-identical to its pre-grant self, got {:?}",
        caller_actor.scopes
    );
    for escalated in [
        "moira:execution:override-credential",
        "moira:execution:override-model",
        "moira:identity:delegate",
        "moira:models:read",
    ] {
        let refused = fixture
            .state
            .authz
            .require(&caller_actor, escalated)
            .expect_err("the admin grant must not reach the public execution API");
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    // And the same, over real HTTP. The message is asserted, not just the status, so this
    // cannot be satisfied by a 403 raised for some unrelated reason (e.g. the missing
    // application binding `public_access` also enforces).
    let public_read = get_with(router, "/api/v1/models", bearer).await;
    assert_eq!(public_read.status, StatusCode::FORBIDDEN);
    assert!(
        public_read.body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("missing required scope moira:models:read"),
        "the public plane must refuse on the scope gate, proving the grant never arrived: \
         {:?}",
        public_read.body
    );
}

#[tokio::test]
async fn an_ungranted_subject_on_the_same_issuer_gains_no_scopes() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let claimed = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "granted-human", "owner@example.com"),
    )
    .await;
    assert_eq!(claimed.status, StatusCode::CREATED, "{:?}", claimed.body);

    // The grant key is `(issuer, subject)`. A colleague at the same IdP must gain nothing.
    let neighbour = fixture
        .state
        .auth
        .authenticate_admin(&fixture.pool, &issuer.bearer("someone-else", &[]))
        .await
        .expect("the neighbour's JWT still verifies");
    assert!(
        neighbour.scopes.is_empty(),
        "a grant must not leak across subjects on the same issuer"
    );
}

#[tokio::test]
async fn claim_is_idempotent_under_an_idempotency_key_and_conflicts_without_one() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;
    let key = format!("claim-{}", Uuid::now_v7());
    let body = claim_body(&issuer.issuer, "owner", "owner@example.com");

    let fresh = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        Some(&key),
        None,
        body.clone(),
    )
    .await;
    let replay = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        Some(&key),
        None,
        body.clone(),
    )
    .await;
    // Without a key there is no replay to serve, so the DB-level unique index answers.
    let retry = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        body,
    )
    .await;

    assert_eq!(fresh.status, StatusCode::CREATED, "{:?}", fresh.body);
    assert_eq!(replay.status, StatusCode::OK, "{:?}", replay.body);
    assert_eq!(fresh.body["id"], replay.body["id"]);
    // The notice is the same on both, deliberately: a replay returns the stored body
    // verbatim, so the status code — not a second notice key — distinguishes them.
    assert_eq!(fresh.body["notice"], replay.body["notice"]);
    assert_eq!(retry.status, StatusCode::CONFLICT, "{:?}", retry.body);
    assert_eq!(retry.code(), "admin_identity_already_claimed");

    let grants: i64 = sqlx::query_scalar("select count(*) from admin_identities")
        .fetch_one(&fixture.pool)
        .await
        .expect("count grants");
    assert_eq!(grants, 1, "exactly one grant, whatever the retry pattern");
}

#[tokio::test]
async fn a_claim_body_missing_a_required_field_is_rejected_with_a_catalogued_error() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;

    for body in [
        // `email` omitted (decision D5 made it required).
        json!({ "issuer": issuer.issuer, "subject": "owner", "email_verified": true }),
        // `email_verified` omitted — it deliberately has no `#[serde(default)]`, so this is
        // the schema violation it actually is rather than a silent `false` that would later
        // surface as a misleading "your email is not verified" 403.
        json!({ "issuer": issuer.issuer, "subject": "owner", "email": "owner@example.com" }),
    ] {
        let rejected = post_json(
            router.clone(),
            "/api/v1/admin/setup/claim",
            system_key_headers(&secret),
            None,
            None,
            body.clone(),
        )
        .await;

        assert_eq!(
            rejected.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{:?}",
            rejected.body
        );
        assert_eq!(rejected.code(), "invalid_request", "for body {body}");
        assert_eq!(rejected.message_key(), "moira.error.invalid_request");
        assert!(
            !rejected.body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "the envelope must be Moira's, not axum's bare plain-text rejection"
        );
    }
}

/// The reserved field is refused, never ignored: a caller must not be able to believe Moira
/// honoured a credential it never read.
#[tokio::test]
async fn a_populated_setup_token_is_refused_with_its_own_code() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let mut body = claim_body(&issuer.issuer, "owner", "owner@example.com");
    body["setup_token"] = json!("moira_setup_whatever");

    let refused = post_json(
        router,
        "/api/v1/admin/setup/claim",
        HeaderMap::new(),
        None,
        None,
        body,
    )
    .await;

    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        refused.body
    );
    assert_eq!(refused.code(), "setup_token_not_supported");
}

/// **Barrier-gated.** Two concurrent claims for the identical `(issuer, subject)`, with no
/// `Idempotency-Key` on either — so the only thing that can prevent a duplicate grant is
/// `admin_identities_issuer_subject_active_unique`, raced honestly rather than serialized
/// by an artificial delay. Exactly one request must observe `201`; the other must observe
/// `409 admin_identity_already_claimed`, the database-level backstop `insert_grant`'s
/// `already_claimed_on_unique_violation` maps a unique violation to. Never both `201`,
/// never both `409`, never a second row.
#[tokio::test]
async fn concurrent_claims_for_the_same_identity_yield_one_201_and_one_409() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;
    let body = claim_body(&issuer.issuer, "contested", "owner@example.com");

    let barrier = std::sync::Arc::new(Barrier::new(3));
    let first = spawn_claim(
        router.clone(),
        barrier.clone(),
        system_key_headers(&secret),
        None,
        body.clone(),
    );
    let second = spawn_claim(
        router.clone(),
        barrier.clone(),
        system_key_headers(&secret),
        None,
        body,
    );
    barrier.wait().await;
    let first = first.await.expect("first claim task");
    let second = second.await.expect("second claim task");

    let statuses = [first.status, second.status];
    assert!(
        statuses.contains(&StatusCode::CREATED),
        "one of the two concurrent claims must succeed: {statuses:?}"
    );
    assert!(
        statuses.contains(&StatusCode::CONFLICT),
        "the other must be refused as already claimed, not silently succeed too: {statuses:?}"
    );
    let loser = if first.status == StatusCode::CONFLICT {
        &first
    } else {
        &second
    };
    assert_eq!(loser.code(), "admin_identity_already_claimed");

    let grants: i64 = sqlx::query_scalar("select count(*) from admin_identities where issuer = $1")
        .bind(&issuer.issuer)
        .fetch_one(&fixture.pool)
        .await
        .expect("count grants");
    assert_eq!(grants, 1, "a race must never produce a second grant row");
}

/// **Barrier-gated**, the sibling assertion: the advisory-lock/unique-index machinery that
/// serializes a race for *one* identity must not also serialize unrelated identities. Two
/// different subjects on the same governing issuer, released together, must both succeed.
///
/// Since finding F20, `insert_grant` also takes the deployment-wide `moiraown` ownership
/// lock, which *does* serialise these two — so this test now asserts the property that
/// actually matters and was always the point: serialising them must not make either of them
/// **fail**, and exactly one of them must come out owning the deployment. A claim refused
/// because someone else was claiming a different identity would be the regression; a claim
/// that merely waited is not.
#[tokio::test]
async fn concurrent_claims_for_different_identities_both_succeed() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let barrier = std::sync::Arc::new(Barrier::new(3));
    let first = spawn_claim(
        router.clone(),
        barrier.clone(),
        system_key_headers(&secret),
        None,
        claim_body(&issuer.issuer, "distinct-one", "owner-one@example.com"),
    );
    let second = spawn_claim(
        router.clone(),
        barrier.clone(),
        system_key_headers(&secret),
        None,
        claim_body(&issuer.issuer, "distinct-two", "owner-two@example.com"),
    );
    barrier.wait().await;
    let first = first.await.expect("first claim task");
    let second = second.await.expect("second claim task");

    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    assert_eq!(second.status, StatusCode::CREATED, "{:?}", second.body);
    assert_ne!(
        first.body["id"], second.body["id"],
        "two different identities must produce two different grants"
    );

    let grants: i64 = sqlx::query_scalar("select count(*) from admin_identities where issuer = $1")
        .bind(&issuer.issuer)
        .fetch_one(&fixture.pool)
        .await
        .expect("count grants");
    assert_eq!(grants, 2, "independent identities proceed independently");

    // Decision D-F20: two grants raced for one empty ownership slot. The advisory lock is
    // what makes the loser *not primary* rather than *refused* — both bodies above are
    // already asserted to be `201`, so a design that let
    // `admin_identities_single_active_primary` decide the race would have failed there.
    let primaries: i64 = sqlx::query_scalar(
        "select count(*) from admin_identities \
         where issuer = $1 and deleted_at is null and status = 'active' and is_primary",
    )
    .bind(&issuer.issuer)
    .fetch_one(&fixture.pool)
    .await
    .expect("count primaries");
    assert_eq!(
        primaries, 1,
        "exactly one of two racing first claims may own the deployment"
    );
    let claimed = [&first, &second]
        .iter()
        .filter(|result| result.body["is_primary"] == serde_json::json!(true))
        .count();
    assert_eq!(
        claimed, 1,
        "exactly one response body may report ownership, and it must agree with the row"
    );
}

/// **Only a uniqueness conflict is `admin_identity_already_claimed`.**
///
/// Found by `cargo mutants`, which turned `already_claimed_on_unique_violation`'s
/// `is_unique_violation() && constraint() != …` into `||` and watched the suite stay green.
/// Under that mutation *any* database failure on the grant insert — a truncation, a check
/// violation, a constraint the schema grows later — comes back as `409` telling the operator
/// the identity is already an admin, when no grant exists and none was created.
///
/// `only_a_unique_violation_becomes_already_claimed` in `src/infra/repositories/identity.rs`
/// covers the non-`Database` arm with `sqlx::Error::RowNotFound`; it cannot reach the guard
/// itself, because the guard only runs for `Database` errors. This does: `admin_identities.email`
/// is `varchar(320)`, and the domain policy passes on the *domain*, so an over-long local part
/// on an allowed domain reaches PostgreSQL and is truncated.
///
/// The assertion is **negative** on purpose. What this deployment does with a 400-character
/// address today is `500 database_error` — it should be a `422` from the service, which is a
/// separate gap; pinning the 500 would cement it. The property under test is only that a
/// truncation is not a claim conflict.
#[tokio::test]
async fn a_non_unique_database_failure_is_not_reported_as_already_claimed() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    // `varchar(320)` in `0012`; the local part alone is longer than the whole column.
    let oversized = format!("{}@example.com", "a".repeat(400));
    let refused = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "oversized-email", &oversized),
    )
    .await;

    assert_ne!(
        refused.status,
        StatusCode::CREATED,
        "an address wider than the column cannot be stored: {:?}",
        refused.body
    );
    assert_ne!(
        refused.code(),
        "admin_identity_already_claimed",
        "a truncation is not a uniqueness conflict, and reporting it as one tells the operator \
         a grant exists that does not: {:?}",
        refused.body
    );

    let grants: i64 = sqlx::query_scalar("select count(*) from admin_identities where issuer = $1")
        .bind(&issuer.issuer)
        .fetch_one(&fixture.pool)
        .await
        .expect("count grants");
    assert_eq!(grants, 0, "the premise is that no grant was written");
}

#[tokio::test]
async fn claim_with_an_unregistered_issuer_returns_400_unregistered_trusted_issuer() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;

    // No `ConsoleIssuer` is started, and no `trusted_jwt_issuers` row is registered for
    // this string: Moira never accepts a free-text issuer at claim time (module 5).
    let refused = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(
            "https://never-registered.invalid",
            "owner",
            "owner@example.com",
        ),
    )
    .await;

    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        refused.body
    );
    assert_eq!(refused.code(), "unregistered_trusted_issuer");
    assert_eq!(
        refused.message_key(),
        "moira.error.unregistered_trusted_issuer"
    );
}

/// Policy step 2 at the HTTP level: a registered issuer, an enabled and matching domain
/// policy, and a body that simply asserts `email_verified: false`. The hard requirement
/// holds regardless of how permissive the domain configuration is.
#[tokio::test]
async fn claim_with_an_unverified_email_returns_403() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let mut body = claim_body(&issuer.issuer, "owner", "owner@example.com");
    body["email_verified"] = json!(false);
    let refused = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        body,
    )
    .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    assert_eq!(refused.code(), "admin_claim_email_not_verified");
    assert_eq!(
        refused.message_key(),
        "moira.error.admin_claim_email_not_verified"
    );
}

/// Policy step 4 at the HTTP level, distinct from the "no configuration at all" sibling:
/// an enabled `auth_provider_settings` row genuinely governs this issuer, but its
/// `allowed_email_domains` names a different domain than the one being claimed.
#[tokio::test]
async fn claim_with_a_disallowed_domain_returns_403() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["allowed.example"]).await;

    let refused = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "owner", "owner@not-allowed.example"),
    )
    .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    assert_eq!(refused.code(), "admin_claim_domain_not_allowed");
    assert_eq!(
        refused.message_key(),
        "moira.error.admin_claim_domain_not_allowed"
    );
}

/// Deny-by-default at the HTTP level, distinct from
/// `claim_is_denied_when_no_auth_provider_configuration_exists_at_all`: here a row
/// genuinely exists and is enabled, but its `allowed_email_domains` is the empty array —
/// refuting the "empty means unrestricted" reading specifically, rather than the stricter
/// "no configuration governs this issuer at all" case.
#[tokio::test]
async fn claim_is_denied_by_default_when_no_domain_allow_list_is_configured() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    // Empty allow-list, deliberately: `configure_and_enable_policy` with no domains.
    configure_and_enable_policy(&router, &issuer, &[]).await;

    let refused = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "owner", "owner@example.com"),
    )
    .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{:?}", refused.body);
    assert_eq!(refused.code(), "admin_claim_domain_not_allowed");
    assert_eq!(
        refused.message_key(),
        "moira.error.admin_claim_domain_not_allowed"
    );
}

/// A vacuity-guarded walk: performs several successful claims across the suite's own
/// fixtures and asserts every resulting `admin_identities` row carries a non-null,
/// non-empty email. `record.email` is `String`, not `Option<String>` (decision D5) — this
/// is the corresponding database-level guarantee, checked against real rows rather than
/// trusted from the type alone.
#[tokio::test]
async fn every_granted_admin_identity_row_carries_a_non_null_email() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;

    let mut issuers = Vec::new();
    for index in 0..3 {
        let issuer = ConsoleIssuer::start(&fixture.pool).await;
        configure_and_enable_policy(&router, &issuer, &["example.com"]).await;
        let granted = post_json(
            router.clone(),
            "/api/v1/admin/setup/claim",
            system_key_headers(&secret),
            None,
            None,
            claim_body(
                &issuer.issuer,
                &format!("owner-{index}"),
                &format!("owner-{index}@example.com"),
            ),
        )
        .await;
        assert_eq!(granted.status, StatusCode::CREATED, "{:?}", granted.body);
        issuers.push(issuer);
    }

    let mut walked = 0;
    for issuer in &issuers {
        let email: Option<String> =
            sqlx::query_scalar("select email from admin_identities where issuer = $1")
                .bind(&issuer.issuer)
                .fetch_one(&fixture.pool)
                .await
                .expect("read the grant written by this test");
        assert!(
            email.is_some_and(|email| !email.trim().is_empty()),
            "every grant this plan writes must carry a non-null, non-empty email"
        );
        walked += 1;
    }
    // The vacuity guard: this walk finds nothing and proves nothing if the claims above
    // did not actually land.
    assert_eq!(
        walked,
        issuers.len(),
        "expected to have walked exactly one grant row per claim made in this test"
    );
}

/// A vacuity-guarded walk across every documented non-2xx status this endpoint can
/// return, each triggered for real: CONVENTIONS §4.5 requires a non-empty `message_key`
/// and `message` on every error response, not merely on the ones a hand-picked assertion
/// happens to check.
#[tokio::test]
async fn every_claim_error_response_carries_a_nonempty_message_key_and_message() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["allowed.example"]).await;

    let mut unverified_body = claim_body(&issuer.issuer, "unverified", "owner@allowed.example");
    unverified_body["email_verified"] = json!(false);

    let scenarios: Vec<(&str, HeaderMap, Value)> = vec![
        (
            "no credential",
            HeaderMap::new(),
            claim_body(&issuer.issuer, "no-credential", "owner@allowed.example"),
        ),
        (
            "unregistered issuer",
            system_key_headers(&secret),
            claim_body(
                "https://never-registered.invalid",
                "owner",
                "owner@allowed.example",
            ),
        ),
        (
            "unverified email",
            system_key_headers(&secret),
            unverified_body,
        ),
        (
            "disallowed domain",
            system_key_headers(&secret),
            claim_body(&issuer.issuer, "owner", "owner@disallowed.example"),
        ),
        (
            "missing email field",
            system_key_headers(&secret),
            json!({ "issuer": issuer.issuer, "subject": "owner", "email_verified": true }),
        ),
    ];

    let mut checked = 0;
    for (label, headers, body) in scenarios {
        let result = post_json(
            router.clone(),
            "/api/v1/admin/setup/claim",
            headers,
            None,
            None,
            body,
        )
        .await;
        assert!(
            result.status.is_client_error(),
            "{label} was expected to fail with a 4xx: {:?}",
            result.body
        );
        assert!(
            !result.code().is_empty(),
            "{label}: error.code must be non-empty: {:?}",
            result.body
        );
        assert!(
            !result.message_key().is_empty(),
            "{label}: error.message_key must be non-empty: {:?}",
            result.body
        );
        assert!(
            !result.body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{label}: error.message must be non-empty: {:?}",
            result.body
        );
        checked += 1;
    }
    // The vacuity guard: fail loudly if the scenario list above is ever emptied out
    // rather than silently "passing" a walk that checked nothing.
    assert!(
        checked >= 5,
        "expected to have exercised at least 5 distinct error scenarios"
    );
}

/// Audit-row fidelity: a successful claim writes **exactly one** `audit_logs` row, naming
/// the real actor type — `success_audit` records `format!("{:?}", actor.actor_type)`
/// lower-cased, so a `SystemKey` actor is stored as `"systemkey"` (Debug's own spelling,
/// not the JSON/API `snake_case` rendering) — and its metadata carries the identity being
/// granted but never the system key that authorized the write.
#[tokio::test]
async fn every_claim_attempt_writes_exactly_one_audit_row_with_the_correct_actor_type() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let granted = post_json(
        router,
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "audited-owner", "owner@example.com"),
    )
    .await;
    assert_eq!(granted.status, StatusCode::CREATED, "{:?}", granted.body);
    let resource_id = granted.body["id"].as_str().expect("grant id").to_string();

    let rows: Vec<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
        "select actor_type, actor_subject, metadata from audit_logs \
         where resource_type = 'admin_identity' and action = 'admin_identity.claim' \
         and resource_id = $1",
    )
    .bind(&resource_id)
    .fetch_all(&fixture.pool)
    .await
    .expect("read the audit rows for this claim");

    assert_eq!(
        rows.len(),
        1,
        "exactly one audit row per successful claim, got {rows:?}"
    );
    let (actor_type, actor_subject, metadata) = &rows[0];
    assert_eq!(
        actor_type, "systemkey",
        "the audit row must name the real credential type"
    );
    assert!(
        actor_subject.is_some(),
        "the audit row must name an actor subject"
    );
    let rendered = metadata.to_string();
    assert!(
        !rendered.contains(&secret),
        "the audit row metadata must never contain the system key secret"
    );
}

/// The two setup concepts must stay distinct: this plan's identity-claiming status lives
/// beside the pre-existing structural readiness endpoint and neither renames nor absorbs it.
#[tokio::test]
async fn the_existing_structural_setup_status_endpoint_is_unaffected() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;

    // Still setup-actor gated exactly as before: a dev-admin actor is refused, a system key
    // is admitted. This plan changes neither.
    let structural = get_with(
        router.clone(),
        "/api/v1/admin/setup/status",
        system_key_headers(&secret),
    )
    .await;
    let claim_status = get(router, "/api/v1/admin/setup/claim-status").await;

    assert_eq!(structural.status, StatusCode::OK, "{:?}", structural.body);
    assert!(
        structural.body["checks"].is_object(),
        "the structural endpoint keeps its granular per-check detail"
    );
    assert!(
        structural.body.get("claimed").is_none(),
        "the structural endpoint must not grow this plan's boolean"
    );
    assert!(
        claim_status.body.get("checks").is_none(),
        "the claim-status endpoint must not grow the structural endpoint's detail"
    );
}

/* -------------------------------------------------------------------------- */
/* Plan 09 wave 4A — G6: retiring a trusted issuer with live grants            */
/* -------------------------------------------------------------------------- */

/// **G6 — a trusted issuer with live grants cannot be deleted or disabled.**
///
/// # The defect
///
/// `admin_identities.trusted_jwt_issuer_id` is a real foreign key, so a *hard* delete would
/// be refused by Postgres. Both retirement paths are **soft** — `soft_delete_trusted_jwt_issuer`
/// sets `status = 'deleted'` and `deleted_at`, `set_trusted_jwt_issuer_status` sets
/// `'disabled'` — so the FK never fires. Meanwhile `load_issuer` filters
/// `status = 'active' and deleted_at is null`. One button therefore stopped every token
/// minted under that issuer from verifying, and every grant made through it from
/// authorising anybody: a silent, deployment-wide admin revocation, on a deployment whose
/// only other way in is the bootstrap system key the invitation flow exists to retire.
///
/// # The mutation, and why the assertion goes through `authenticate_admin`
///
/// Remove the guard and a repository-level test still passes: the soft delete succeeds
/// either way, and asserting "the row is gone" is true in both arrangements. What changes
/// is whether the **grant still authorises**, and only the authentication path can observe
/// that. So this test deletes, expects a coded 409, and then re-authenticates the *same
/// bearer* — the assertion that would have gone red before the guard existed, and the one a
/// row-level assertion cannot make.
#[tokio::test]
async fn a_trusted_issuer_with_live_grants_cannot_be_retired() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = ConsoleIssuer::start(&fixture.pool).await;
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &fixture.state).await;
    configure_and_enable_policy(&router, &issuer, &["example.com"]).await;

    let bearer = issuer.bearer("retire-me", &[]);

    // No grant yet: retiring an issuer nobody depends on stays allowed, so the refusal
    // below is about the grant and not about the issuer being in use at all.
    let unused = ConsoleIssuer::start(&fixture.pool).await;
    let unused_row = get(
        router.clone(),
        &format!("/api/v1/admin/jwt-issuers/{}", unused.id),
    )
    .await;
    assert_eq!(unused_row.status, StatusCode::OK, "{:?}", unused_row.body);
    let removed = send(
        router.clone(),
        base(
            "DELETE",
            &format!("/api/v1/admin/jwt-issuers/{}", unused.id),
        )
        .header("if-match", unused_row.version().to_string())
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(
        removed.status,
        StatusCode::NO_CONTENT,
        "an issuer with no grants must still be removable: {:?}",
        removed.body
    );

    // Now make a grant through the issuer under test.
    let claimed = post_json(
        router.clone(),
        "/api/v1/admin/setup/claim",
        system_key_headers(&secret),
        None,
        None,
        claim_body(&issuer.issuer, "retire-me", "owner@example.com"),
    )
    .await;
    assert_eq!(claimed.status, StatusCode::CREATED, "{:?}", claimed.body);

    // The premise, asserted: the grant authorises on the admin plane right now.
    let before = fixture
        .state
        .auth
        .authenticate_admin(&fixture.pool, &bearer)
        .await
        .expect("the granted identity authenticates before any retirement attempt");
    assert_eq!(before.scopes, vec!["moira:admin".to_string()]);

    let row = get(
        router.clone(),
        &format!("/api/v1/admin/jwt-issuers/{}", issuer.id),
    )
    .await;
    assert_eq!(row.status, StatusCode::OK, "{:?}", row.body);
    let version = row.version();

    let deleted = send(
        router.clone(),
        base(
            "DELETE",
            &format!("/api/v1/admin/jwt-issuers/{}", issuer.id),
        )
        .header("if-match", version.to_string())
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::CONFLICT, "{:?}", deleted.body);
    assert_eq!(deleted.code(), "trusted_issuer_has_active_grants");

    // The disable path is the same silent revocation by another name: `load_issuer` filters
    // on `status = 'active'`, so `'disabled'` is exactly as fatal to a live grant as
    // `'deleted'`. Guarding only `DELETE` would leave the whole defect one button over.
    let disabled = post_json(
        router.clone(),
        &format!("/api/v1/admin/jwt-issuers/{}/disable", issuer.id),
        HeaderMap::new(),
        None,
        Some(version),
        Value::Null,
    )
    .await;
    assert_eq!(disabled.status, StatusCode::CONFLICT, "{:?}", disabled.body);
    assert_eq!(disabled.code(), "trusted_issuer_has_active_grants");

    // ---- the assertion the guard exists for ----------------------------------
    // Not "the row is still there" — that is true whether or not the guard ran, because
    // both refusals happen before the write. This re-authenticates the same bearer and
    // asserts the grant STILL AUTHORISES, which is the property the retirement would have
    // destroyed.
    let after = fixture
        .state
        .auth
        .authenticate_admin(&fixture.pool, &bearer)
        .await
        .expect("the grant must still authenticate after a refused retirement");
    assert_eq!(
        after.scopes,
        vec!["moira:admin".to_string()],
        "the trusted issuer was retired anyway and the grant stopped authorising"
    );

    // And once the grant is gone, retirement is allowed again — so the guard is a
    // precondition, not a permanent lock on the row.
    sqlx::query("update admin_identities set status = 'revoked', is_primary = false where trusted_jwt_issuer_id = $1")
        .bind(issuer.id)
        .execute(&fixture.pool)
        .await
        .expect("revoke the grant");
    let row = get(
        router.clone(),
        &format!("/api/v1/admin/jwt-issuers/{}", issuer.id),
    )
    .await;
    let finally = send(
        router,
        base(
            "DELETE",
            &format!("/api/v1/admin/jwt-issuers/{}", issuer.id),
        )
        .header("if-match", row.version().to_string())
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(
        finally.status,
        StatusCode::NO_CONTENT,
        "a revoked grant must not keep the issuer locked forever: {:?}",
        finally.body
    );
}
