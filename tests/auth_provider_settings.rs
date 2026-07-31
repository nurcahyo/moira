//! End-to-end coverage for plan 07's runtime auth-provider settings surface.
//!
//! Two properties dominate this suite:
//!
//! * **Decision D7** — Moira stores no OAuth client secret. That is asserted structurally
//!   (the request DTOs reject one, no response carries one) and *contractually* (the removed
//!   `rotate-secret` operation is absent from the generated document and genuinely unrouted).
//!   Plans 08 and 09 bind to that absence, which is why it is a test rather than a comment.
//! * **CONVENTIONS §7.2** — an auth-settings write invalidates the runtime cache through the
//!   existing Postgres `LISTEN/NOTIFY` path, proven by driving a real listener rather than
//!   asserting that a call was made.

mod support;

use std::time::{Duration, Instant};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use moira::app::AppState;
use serde_json::{Value, json};
use support::LifecycleFixture;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

/// Every column, field or key name that would indicate secret material had crept back onto
/// this surface.
///
/// Deliberately **not** the bare word `token`: `token_url` is legitimate, non-secret OAuth
/// configuration, and a needle that flags it would have to be suppressed at every call site
/// — at which point it stops catching anything. `client_secret` is the concrete shape the
/// thing D7 excludes would take.
const SECRET_SHAPED: [&str; 8] = [
    "client_secret",
    "secret_fingerprint",
    "masked_secret",
    "encrypted_payload",
    "encrypted_data_key",
    "encryption_algorithm",
    "encryption_version",
    "nonce",
];

const WAIT: Duration = Duration::from_secs(10);

/// The channel `listen_once` subscribes to (`src/infra/db.rs`) and every
/// `notify_moira_runtime_config_change()` trigger publishes on (`migrations/0002`,
/// `migrations/0013`). Repeated here rather than imported because it is not exported;
/// a rename on either side makes [`wait_for_listener_attached`] fail loudly on its
/// deadline rather than quietly stop gating.
const RUNTIME_CONFIG_CHANNEL: &str = "moira_runtime_config";

/// Paces [`wait_for_listener_attached`]. The first tick of a `tokio` interval completes
/// immediately, so an already-attached listener costs nothing and only a genuine failure
/// pays the deadline.
const ATTACH_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Blocks until this database holds a session that has *committed*
/// `LISTEN "moira_runtime_config"`.
///
/// # Why a write needs this before it can be observed
///
/// `spawn_runtime_config_listener` returns its `JoinHandle` the instant the task is
/// created; only then does the task acquire a connection and execute its `LISTEN`.
/// Postgres delivers a notification solely to sessions that were already listening when
/// the notifying transaction committed, so a write that races the attach is not delivered
/// late — it is **lost**, permanently, and no amount of polling afterwards recovers it.
/// Waiting for the attach is therefore a missing precondition, not a tolerance: the
/// assertion that follows it is unchanged, and a fixed sleep here would only be a guess
/// at how long the attach takes (CONVENTIONS §3).
///
/// # Why this observation is sound
///
/// [`LifecycleFixture`] clones a database per test, so `datname = current_database()`
/// cannot be satisfied by a concurrent suite's listener — this can only ever see the one
/// this test spawned. `state = 'idle'` is what makes it an acknowledgement rather than a
/// sighting: a backend reports idle only once the statement it was running has committed,
/// which is exactly the point from which delivery is guaranteed. And sqlx's `PgListener`
/// issues `LISTEN "moira_runtime_config"` (`PgListener::listen`, quoting the channel) and
/// then executes nothing further while it blocks in `recv()`, so that text remains the
/// session's `query` for as long as the listener lives — verified against
/// `pg_stat_activity` on a live listener, both before and after a delivered notification.
async fn wait_for_listener_attached(pool: &sqlx::PgPool) {
    let mut ticker = tokio::time::interval(ATTACH_POLL_INTERVAL);
    let deadline = Instant::now() + WAIT;
    loop {
        let attached: bool = sqlx::query_scalar(
            "select exists( \
                 select 1 from pg_stat_activity \
                 where datname = current_database() \
                   and state = 'idle' \
                   and query ilike 'listen%' \
                   and strpos(query, $1) > 0 \
             )",
        )
        .bind(RUNTIME_CONFIG_CHANNEL)
        .fetch_one(pool)
        .await
        .expect("read pg_stat_activity");
        if attached {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the runtime config listener never issued LISTEN \"{RUNTIME_CONFIG_CHANNEL}\" \
             within {WAIT:?}; without it every NOTIFY this test emits is dropped on the floor"
        );
        ticker.tick().await;
    }
}

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
    // Lenient rather than `expect`: a `deny_unknown_fields` violation on a bare `Json<T>`
    // extractor is still axum's plain-text rejection on the pre-existing admin handlers
    // (the §4 gap plan 07 records as a deferred follow-up rather than fixing repo-wide), and
    // this suite asserts on such a response.
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    HttpResult { status, body, etag }
}

fn base(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", format!("auth-settings-{}", Uuid::now_v7()))
}

async fn request(
    router: Router,
    method: &str,
    path: &str,
    headers: HeaderMap,
    if_match: Option<i64>,
    body: Option<Value>,
) -> HttpResult {
    let mut builder = base(method, path);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    if let Some(version) = if_match {
        builder = builder.header("if-match", version.to_string());
    }
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    send(router, builder.body(body).unwrap()).await
}

fn system_key_headers(secret: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-moira-system-key",
        secret.parse().expect("system key header"),
    );
    headers
}

async fn mint_system_key(router: &Router, scopes: &[&str]) -> String {
    let created = request(
        router.clone(),
        "POST",
        "/api/v1/admin/system-keys",
        HeaderMap::new(),
        None,
        Some(json!({
            "display_name": format!("auth-settings-{}", Uuid::now_v7()),
            "scopes": scopes
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    created.body["secret"]
        .as_str()
        .expect("the secret is returned exactly once, at creation")
        .to_string()
}

async fn create_provider(router: &Router, overrides: Value) -> HttpResult {
    let mut body = json!({
        "method": "google_oauth",
        "display_name": "Google",
        "issuer": format!("https://accounts.test/{}", Uuid::now_v7()),
        "client_id": "console-client",
        "allowed_email_domains": ["example.com"]
    });
    for (key, value) in overrides.as_object().cloned().unwrap_or_default() {
        body[key] = value;
    }
    request(
        router.clone(),
        "POST",
        "/api/v1/admin/auth/providers",
        HeaderMap::new(),
        None,
        Some(body),
    )
    .await
}

fn assert_no_secret_material(label: &str, value: &Value) {
    let rendered = value.to_string();
    for needle in SECRET_SHAPED {
        assert!(
            !rendered.contains(needle),
            "{label} response leaked {needle}: {rendered}"
        );
    }
}

#[tokio::test]
async fn create_google_oauth_provider_stores_only_non_secret_configuration() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let created = create_provider(&router, json!({})).await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    assert_eq!(created.body["enabled"], json!(false), "deny by default");
    assert_no_secret_material("create", &created.body);

    // Inspected at the table, not only at the API: D7's claim is that no envelope column
    // exists on `auth_provider_settings` at all, which a response-shape assertion alone
    // could not distinguish from a column that is merely hidden from serialization.
    let columns: Vec<String> = sqlx::query_scalar(
        "select column_name from information_schema.columns \
         where table_name = 'auth_provider_settings'",
    )
    .fetch_all(&fixture.pool)
    .await
    .expect("read the table's columns");
    for column in &columns {
        for needle in SECRET_SHAPED {
            assert!(
                !column.contains(needle),
                "auth_provider_settings grew a secret-shaped column: {column}"
            );
        }
    }
}

#[tokio::test]
async fn auth_provider_requests_reject_a_client_secret_field() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let created = create_provider(&router, json!({})).await;
    let id = created.body["id"].as_str().unwrap();

    // A stale console still sending a secret must fail loudly rather than have the field
    // silently dropped — believing Moira stored a secret it never stored is the worse
    // failure of the two.
    let on_create = create_provider(&router, json!({ "client_secret": "shhh" })).await;
    let on_patch = request(
        router,
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{id}"),
        HeaderMap::new(),
        Some(created.version()),
        Some(json!({ "client_secret": "shhh" })),
    )
    .await;

    for rejected in [&on_create, &on_patch] {
        assert!(
            rejected.status.is_client_error(),
            "a client_secret must be refused: {:?}",
            rejected.body
        );
    }
}

#[tokio::test]
async fn no_auth_provider_response_contains_secret_material() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &["moira:admin"]).await;
    let created = create_provider(&router, json!({})).await;
    let id = created.body["id"].as_str().unwrap().to_string();

    let enabled = request(
        router.clone(),
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        Some(created.version()),
        None,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);

    let responses = [
        ("create", created.body.clone()),
        ("enable", enabled.body.clone()),
        (
            "get",
            request(
                router.clone(),
                "GET",
                &format!("/api/v1/admin/auth/providers/{id}"),
                HeaderMap::new(),
                None,
                None,
            )
            .await
            .body,
        ),
        (
            "list",
            request(
                router.clone(),
                "GET",
                "/api/v1/admin/auth/providers",
                HeaderMap::new(),
                None,
                None,
            )
            .await
            .body,
        ),
        (
            "patch",
            request(
                router.clone(),
                "PATCH",
                &format!("/api/v1/admin/auth/providers/{id}"),
                HeaderMap::new(),
                Some(enabled.version()),
                Some(json!({ "display_name": "Google Workspace" })),
            )
            .await
            .body,
        ),
        (
            "auth-methods",
            request(
                router,
                "GET",
                "/api/v1/admin/setup/auth-methods",
                system_key_headers(&secret),
                None,
                None,
            )
            .await
            .body,
        ),
    ];

    for (label, body) in responses {
        assert_no_secret_material(label, &body);
    }
}

/// D7 at the contract level. Plans 08 and 09 bind to this absence, so it is asserted rather
/// than left implicit — a `rotate-secret` operation reappearing would be a silent break of a
/// frozen contract.
#[test]
fn the_committed_openapi_document_has_no_rotate_secret_operation_or_secret_schema_field() {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/openapi.json"))
            .expect("read the committed OpenAPI document"),
    )
    .expect("parse the committed OpenAPI document");

    let paths = document["paths"].as_object().expect("paths");
    assert!(
        !paths.contains_key("/api/v1/admin/auth/providers/{id}/rotate-secret"),
        "decision D7 removed rotate-secret; the frozen contract is 10 operations, not 11"
    );

    for schema in [
        "AuthProviderSettingsRecord",
        "AuthProviderSettingsCreateRequest",
        "AuthProviderSettingsPatchRequest",
        "PublicAuthMethod",
        // The anonymous one (F15). It matters most here: a secret-shaped property on this
        // schema is one published to unauthenticated callers. The committed document is a
        // snapshot, so what makes this bite on a *new* field is the drift gate
        // (`committed_openapi_matches_the_generated_document`) forcing the snapshot forward.
        "PublicSignInMethod",
    ] {
        let properties = document["components"]["schemas"][schema]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{schema} must be part of the published contract"));
        for property in properties.keys() {
            for needle in SECRET_SHAPED {
                assert!(
                    !property.contains(needle),
                    "{schema} gained a secret-shaped property: {property}"
                );
            }
        }
        assert!(
            properties.contains_key("client_id") || schema.ends_with("PatchRequest"),
            "{schema} must expose client_id: it is the non-secret anchor the console \
             fingerprints for D7 drift protection"
        );
    }
}

#[tokio::test]
async fn the_rotate_secret_path_is_genuinely_unrouted_not_merely_undocumented() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let created = create_provider(&router, json!({})).await;
    let id = created.body["id"].as_str().unwrap();

    let response = request(
        router,
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/rotate-secret"),
        HeaderMap::new(),
        None,
        Some(json!({})),
    )
    .await;

    assert_eq!(
        response.status,
        StatusCode::NOT_FOUND,
        "{:?}",
        response.body
    );
}

#[tokio::test]
async fn patch_requires_if_match_and_conflicts_on_a_stale_version() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let created = create_provider(&router, json!({})).await;
    let id = created.body["id"].as_str().unwrap().to_string();
    let stale = created.version();

    let without = request(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{id}"),
        HeaderMap::new(),
        None,
        Some(json!({ "display_name": "Renamed" })),
    )
    .await;
    assert_eq!(
        without.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        without.body
    );
    assert_eq!(without.code(), "if_match_required");

    // D7: `issuer` and `client_id` are ordinary mutable configuration now — no secret is
    // bound to them, so the deleted `auth_provider_secret_rebind_required` 409 must not
    // come back.
    let updated = request(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{id}"),
        HeaderMap::new(),
        Some(stale),
        Some(json!({ "client_id": "rotated-client", "issuer": "https://accounts.test/moved" })),
    )
    .await;
    assert_eq!(updated.status, StatusCode::OK, "{:?}", updated.body);
    assert_eq!(updated.body["client_id"], json!("rotated-client"));

    let conflicting = request(
        router,
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{id}"),
        HeaderMap::new(),
        Some(stale),
        Some(json!({ "display_name": "Renamed again" })),
    )
    .await;
    assert_eq!(
        conflicting.status,
        StatusCode::CONFLICT,
        "{:?}",
        conflicting.body
    );
}

#[tokio::test]
async fn enabling_a_provider_with_incomplete_non_secret_config_is_refused() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let incomplete = create_provider(
        &router,
        json!({ "method": "jwks", "client_id": null, "issuer": null }),
    )
    .await;
    assert_eq!(
        incomplete.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        incomplete.body
    );
    assert_eq!(incomplete.code(), "auth_provider_method_config_incomplete");
}

/// Decision D4, and the asymmetry it rests on, pinned in one place: the same anonymous
/// client gets one bit from `claim-status` and nothing at all from `auth-methods`. If a
/// future change makes these two agree, this test fails and forces the reasoning to be
/// revisited rather than quietly dropped.
/// Builds a router whose admin JWT auth is **enabled**, unlike the default fixture.
///
/// With it disabled an uncredentialed admin request falls back to a dev-admin actor, which
/// would mask both halves of the anonymity boundary: the `401` a gated endpoint must return,
/// and — worse — the `200` an anonymous one returns, which would prove nothing because the
/// fallback actor would have satisfied a gate had one existed.
fn router_with_admin_auth_enabled(fixture: &LifecycleFixture) -> Router {
    let mut settings = moira::config::Settings::default();
    settings.auth.jwks.allow_insecure_dev_urls = true;
    settings.auth.admin.enabled = true;
    let state =
        AppState::new(settings, Some(fixture.pool.clone())).expect("state with admin auth on");
    moira::build_router(state).expect("router")
}

/// The anonymity boundary across the whole `/setup` surface, pinned as one invariant.
///
/// **This replaces `claim_status_is_anonymous_while_auth_methods_is_not`, whose two-way
/// asymmetry is no longer the shape of the contract.** Finding F15: every read of the auth
/// configuration required a credential, so a console could not render a sign-in button
/// without one it can only obtain by signing in — circular, and blocking for plan 09's public
/// `/invite/[token]` page. The fix was a *third* operation, not a relaxation of the second:
///
/// * `claim-status` — anonymous, one bit.
/// * `sign-in-methods` — anonymous, and only what the browser already sees while signing in.
/// * `auth-methods` — **still authenticated**, because it alone carries
///   `allowed_email_domains`, the deny-by-default admin-claim policy.
///
/// The load-bearing assertion is the third one: relaxing `auth-methods` was the obvious fix
/// and it is the wrong one, so its `401` is asserted here rather than left to erode.
#[tokio::test]
async fn the_anonymous_setup_surface_is_claim_status_and_sign_in_methods_but_never_auth_methods() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = router_with_admin_auth_enabled(&fixture);

    let claim_status = request(
        router.clone(),
        "GET",
        "/api/v1/admin/setup/claim-status",
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    let sign_in_methods = request(
        router.clone(),
        "GET",
        "/api/v1/admin/setup/sign-in-methods",
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    let auth_methods = request(
        router,
        "GET",
        "/api/v1/admin/setup/auth-methods",
        HeaderMap::new(),
        None,
        None,
    )
    .await;

    assert_eq!(claim_status.status, StatusCode::OK);
    assert_eq!(claim_status.body, json!({ "claimed": false }));

    assert_eq!(
        sign_in_methods.status,
        StatusCode::OK,
        "a login screen holds no credential by construction: {:?}",
        sign_in_methods.body
    );
    assert!(
        sign_in_methods.body["methods"].is_array(),
        "{:?}",
        sign_in_methods.body
    );

    assert_eq!(
        auth_methods.status,
        StatusCode::UNAUTHORIZED,
        "the policy-bearing read stays gated: {:?}",
        auth_methods.body
    );
    assert!(!auth_methods.code().is_empty());
    assert!(
        !auth_methods.body["error"]["message_key"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "even the anonymous refusal carries a catalogued key (CONVENTIONS §4.5)"
    );
    // Anonymous reconnaissance against the *gated* endpoint must still yield nothing: no
    // method list, no issuer, no client id, no domain policy.
    assert!(auth_methods.body.get("methods").is_none());
    let rendered = auth_methods.body.to_string();
    for leaked in ["client_id", "issuer", "allowed_email_domains"] {
        assert!(!rendered.contains(leaked), "anonymous call leaked {leaked}");
    }
}

/// The proof of the F15 fix, against a **real, enabled, populated** provider row.
///
/// Asserting "no secret leaked" against an empty `methods` array would pass vacuously and
/// prove nothing, so this test first creates and enables a provider carrying a `client_id` and
/// a non-empty `allowed_email_domains`, then asserts the anonymous response contains the
/// former and not the latter. Both halves matter: dropping `client_id` would leave the console
/// unable to start a flow, and including `allowed_email_domains` would publish the admin-claim
/// policy to the internet.
#[tokio::test]
async fn sign_in_methods_serves_a_populated_list_with_no_credential_and_no_secret_or_policy() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // Setup runs on the default fixture router (admin auth off, dev-admin fallback), because
    // creating and enabling a provider is an authenticated operation. The anonymous read then
    // runs on a *separate* router whose admin auth is on, over the same pool — so the 200
    // below cannot be an artefact of the dev-admin fallback.
    let admin_router = moira::build_router(fixture.state.clone()).expect("router");
    let created = create_provider(&admin_router, json!({})).await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();
    let enabled = request(
        admin_router,
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        Some(created.version()),
        None,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);

    let anonymous = request(
        router_with_admin_auth_enabled(&fixture),
        "GET",
        "/api/v1/admin/setup/sign-in-methods",
        // No bearer token, no system key, no consumer key. Not a weak credential — none.
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::OK, "{:?}", anonymous.body);

    let methods = anonymous.body["methods"].as_array().expect("methods array");
    assert_eq!(
        methods.len(),
        1,
        "the anonymous list must actually carry the enabled provider, or every assertion \
         below is vacuous: {:?}",
        anonymous.body
    );
    let method = methods[0].as_object().expect("a method object");

    // The projection is defined by what it lists — an exact key set, so a field added to an
    // internet-facing response fails here rather than shipping.
    let mut keys: Vec<&str> = method.keys().map(String::as_str).collect();
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
        "unexpected anonymous projection shape: {:?}",
        anonymous.body
    );

    // The console can actually render and start a flow from this.
    assert_eq!(method["client_id"], json!("console-client"));
    assert_eq!(method["display_name"], json!("Google"));
    assert_eq!(method["method"], json!("google_oauth"));

    // No secret material of any kind, asserted over the whole rendered body.
    assert_no_secret_material("anonymous sign-in-methods", &anonymous.body);
    // And no policy: `create_provider` wrote `allowed_email_domains: ["example.com"]`, so the
    // needle would match if the field or its value had survived the projection.
    let rendered = anonymous.body.to_string();
    for leaked in ["allowed_email_domains", "example.com"] {
        assert!(
            !rendered.contains(leaked),
            "the anonymous sign-in list published the admin-claim policy ({leaked}): {rendered}"
        );
    }
}

/// A `jwks` row is machine token verification, not a button a human can press. It has no
/// `authorization_url` and no `client_id`, so a console that rendered it would produce a
/// broken control — and the row's `jwks_url` would be published for nothing.
#[tokio::test]
async fn sign_in_methods_omits_enabled_jwks_rows() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let admin_router = moira::build_router(fixture.state.clone()).expect("router");
    let created = create_provider(
        &admin_router,
        json!({
            "method": "jwks",
            "display_name": "Machine issuer",
            "client_id": Value::Null,
            "jwks_url": "https://issuer.test/jwks"
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();
    let enabled = request(
        admin_router.clone(),
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        Some(created.version()),
        None,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);

    let anonymous = request(
        router_with_admin_auth_enabled(&fixture),
        "GET",
        "/api/v1/admin/setup/sign-in-methods",
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::OK, "{:?}", anonymous.body);
    assert!(
        anonymous.body["methods"]
            .as_array()
            .expect("methods array")
            .is_empty(),
        "a jwks row is not a sign-in method: {:?}",
        anonymous.body
    );
    // The authenticated read still sees it — this is a projection filter, not a data filter.
    let admin_key = mint_system_key(&admin_router, &["moira:admin"]).await;
    let gated = request(
        admin_router,
        "GET",
        "/api/v1/admin/setup/auth-methods",
        system_key_headers(&admin_key),
        None,
        None,
    )
    .await;
    assert_eq!(gated.status, StatusCode::OK, "{:?}", gated.body);
    assert_eq!(
        gated.body["methods"]
            .as_array()
            .expect("methods array")
            .len(),
        1
    );
}

#[tokio::test]
async fn setup_auth_methods_is_setup_actor_gated_and_projects_only_public_fields() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let admin_key = mint_system_key(&router, &["moira:admin"]).await;
    // A system key with no `moira:setup:read` and no `moira:admin`: the scope half of the
    // gate, not just the actor-type half.
    let unscoped_key = mint_system_key(&router, &["moira:models:read"]).await;

    let created = create_provider(&router, json!({})).await;
    let id = created.body["id"].as_str().unwrap().to_string();
    let enabled = request(
        router.clone(),
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        Some(created.version()),
        None,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);

    let unscoped = request(
        router.clone(),
        "GET",
        "/api/v1/admin/setup/auth-methods",
        system_key_headers(&unscoped_key),
        None,
        None,
    )
    .await;
    assert_eq!(
        unscoped.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        unscoped.body
    );

    let allowed = request(
        router,
        "GET",
        "/api/v1/admin/setup/auth-methods",
        system_key_headers(&admin_key),
        None,
        None,
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{:?}", allowed.body);

    let methods = allowed.body["methods"].as_array().expect("methods array");
    assert_eq!(methods.len(), 1);
    let method = methods[0].as_object().expect("a method object");
    // The projection is defined by what it lists. `status`, `metadata`, `version` and the
    // timestamps belong to the admin record and must not appear here.
    let mut keys: Vec<&str> = method.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "allowed_email_domains",
            "authorization_url",
            "client_id",
            "discovery_url",
            "display_name",
            "id",
            "issuer",
            "jwks_url",
            "method",
            "requested_scopes",
        ]
    );
    // D7 drift protection: `client_id` is the whole of Moira's obligation, so it must be
    // present and must reflect what was written.
    assert_eq!(method["client_id"], json!("console-client"));
}

/// CONVENTIONS §7.2, proven rather than asserted: a real `LISTEN`er, a real write, a real
/// trigger, and a cache that is empty afterwards.
#[tokio::test]
async fn an_auth_settings_write_invalidates_the_cache_via_listen_notify() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &["moira:admin"]).await;

    let created = create_provider(&router, json!({})).await;
    let enabled = request(
        router.clone(),
        "POST",
        &format!(
            "/api/v1/admin/auth/providers/{}/enable",
            created.body["id"].as_str().unwrap()
        ),
        HeaderMap::new(),
        Some(created.version()),
        None,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);

    // Populate the cache through the read path the console uses.
    //
    // Done *before* the listener exists, deliberately. The read used to sit between the
    // spawn and the write, where its latency happened to give the listener time to attach —
    // an accident, not a guarantee, and one that stopped holding under CI load (run
    // 30617393166). With it moved here, the only thing standing between the spawn and the
    // write is `wait_for_listener_attached`, so the gate is load-bearing and a regression in
    // it cannot be papered over by incidental delay. The create and enable above commit
    // before the listener attaches too, which means their notifications are lost rather than
    // arriving later and clearing the cache for a reason this test is not asserting.
    let read = request(
        router,
        "GET",
        "/api/v1/admin/setup/auth-methods",
        system_key_headers(&secret),
        None,
        None,
    )
    .await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
    assert!(
        fixture
            .state
            .auth_settings_cache
            .enabled_methods()
            .await
            .is_some(),
        "the read path must populate the cache, or this test proves nothing"
    );

    let listener = moira::infra::db::spawn_runtime_config_listener(
        fixture.pool.clone(),
        moira::infra::db::RuntimeInvalidationTargets::from_state(&fixture.state),
    );
    // The precondition the write below depends on: Postgres routes a notification only to
    // sessions already listening at commit time, so a write issued before the attach is
    // silently discarded rather than delayed.
    wait_for_listener_attached(&fixture.pool).await;

    // Write straight to the table rather than through the service, so the *only* thing that
    // can clear the cache is the NOTIFY trigger — the service's own local invalidation is
    // deliberately bypassed here, because it is the cross-instance path that §7.2 is about.
    sqlx::query("update auth_provider_settings set display_name = $1")
        .bind(format!("renamed-{}", Uuid::now_v7()))
        .execute(&fixture.pool)
        .await
        .expect("write auth provider settings");

    // Polled rather than slept on: the listener is asynchronous and an acknowledgement gate
    // is what CONVENTIONS §3 asks for in place of a fixed sleep.
    let invalidated = timeout(WAIT, async {
        loop {
            if fixture
                .state
                .auth_settings_cache
                .enabled_methods()
                .await
                .is_none()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    listener.abort();

    invalidated.expect(
        "an auth-settings write must invalidate the cache on every instance through the \
         existing LISTEN/NOTIFY path (CONVENTIONS §7.2)",
    );
}

/// The D7 drift-protection contract, at the HTTP level rather than the domain-type level:
/// both read endpoints the console binds to (`GET …/{id}` and `GET …/setup/auth-methods`)
/// must return `client_id`, and it must reflect the value most recently written — not a
/// stale snapshot from creation.
#[tokio::test]
async fn client_id_is_returned_by_the_read_endpoints_for_drift_comparison() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let secret = mint_system_key(&router, &["moira:admin"]).await;
    let created = create_provider(&router, json!({ "client_id": "initial-client" })).await;
    let id = created.body["id"].as_str().unwrap().to_string();
    let enabled = request(
        router.clone(),
        "POST",
        &format!("/api/v1/admin/auth/providers/{id}/enable"),
        HeaderMap::new(),
        Some(created.version()),
        None,
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);

    let patched = request(
        router.clone(),
        "PATCH",
        &format!("/api/v1/admin/auth/providers/{id}"),
        HeaderMap::new(),
        Some(enabled.version()),
        Some(json!({ "client_id": "rotated-client" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{:?}", patched.body);

    let get = request(
        router.clone(),
        "GET",
        &format!("/api/v1/admin/auth/providers/{id}"),
        HeaderMap::new(),
        None,
        None,
    )
    .await;
    assert_eq!(get.body["client_id"], json!("rotated-client"));

    let auth_methods = request(
        router,
        "GET",
        "/api/v1/admin/setup/auth-methods",
        system_key_headers(&secret),
        None,
        None,
    )
    .await;
    assert_eq!(
        auth_methods.status,
        StatusCode::OK,
        "{:?}",
        auth_methods.body
    );
    let methods = auth_methods.body["methods"].as_array().expect("methods");
    let method = methods
        .iter()
        .find(|method| method["id"] == json!(id))
        .expect("the enabled provider must appear in the bootstrap projection");
    assert_eq!(
        method["client_id"],
        json!("rotated-client"),
        "the bootstrap read must reflect the most recently written client_id, not a stale copy"
    );
}

#[tokio::test]
async fn jwks_method_without_a_jwks_url_returns_400() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let incomplete = create_provider(
        &router,
        json!({
            "method": "jwks",
            "client_id": null,
            "issuer": null,
            "jwks_url": null
        }),
    )
    .await;

    assert_eq!(
        incomplete.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        incomplete.body
    );
    assert_eq!(incomplete.code(), "auth_provider_method_config_incomplete");
}

#[tokio::test]
async fn a_second_provider_for_the_same_method_and_issuer_returns_409() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let issuer = format!("https://accounts.test/{}", Uuid::now_v7());

    let first = create_provider(&router, json!({ "issuer": issuer })).await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);

    let second = create_provider(&router, json!({ "issuer": issuer })).await;

    assert_eq!(second.status, StatusCode::CONFLICT, "{:?}", second.body);
    assert_eq!(second.code(), "duplicate_auth_provider");
}

/// **Finding F13.** Every uniqueness conflict in this tree is a `409` — except this one,
/// which fell through `AppError::Sqlx` to `500 database_error` because
/// `trusted_jwt_issuers` had no mapping to match `auth_provider_settings`'s
/// `duplicate_auth_provider` above.
///
/// It lives beside that test on purpose: the two are the same condition on two tables, and
/// the reason the gap survived is that nothing ever compared them. The consequence was
/// found while building the console — a client recovering from a half-finished registration
/// cannot adopt the existing issuer by catching a `409` when the `409` never arrives, and a
/// `500` is indistinguishable from an outage worth paging someone for.
///
/// Asserted on the status, the code **and** the `message_key`: the code alone would pass
/// against an uncatalogued literal, which is exactly the class of defect
/// `every_coded_error_literal_in_src_has_a_catalog_entry` exists to prevent.
#[tokio::test]
async fn a_second_trusted_jwt_issuer_for_the_same_issuer_returns_409_not_500() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let issuer = format!("https://duplicate-issuer-{}.example", Uuid::now_v7());
    let body = json!({
        "issuer": issuer,
        "jwks_url": "https://issuer.example/jwks",
        "allowed_algorithms": ["RS256"],
        "subject_claim": "sub"
    });

    let first = request(
        router.clone(),
        "POST",
        "/api/v1/admin/jwt-issuers",
        HeaderMap::new(),
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);

    let second = request(
        router.clone(),
        "POST",
        "/api/v1/admin/jwt-issuers",
        HeaderMap::new(),
        None,
        Some(body),
    )
    .await;

    assert_eq!(
        second.status,
        StatusCode::CONFLICT,
        "a duplicate issuer must be a conflict the caller can act on, not a 500: {:?}",
        second.body
    );
    assert_eq!(second.code(), "duplicate_trusted_jwt_issuer");
    assert_eq!(
        second.body["error"]["message_key"],
        json!("moira.error.duplicate_trusted_jwt_issuer")
    );

    // The refusal must not have written a second row, and the recovery the `409` enables —
    // find the existing issuer and adopt it — must actually be available.
    let rows: i64 =
        sqlx::query_scalar("select count(*) from trusted_jwt_issuers where issuer = $1")
            .bind(&issuer)
            .fetch_one(&fixture.pool)
            .await
            .expect("count issuers");
    assert_eq!(rows, 1, "a refused create must leave exactly the first row");
}

/// **Only a uniqueness conflict is a duplicate.**
///
/// Found by `cargo mutants`: replacing the `database.is_unique_violation()` match guard with
/// `true` survived the suite, because the test above is the only thing that reaches the mapper
/// and it only ever presents a duplicate. Under that mutation *every* database failure on this
/// insert — a check violation, a truncation, a constraint the schema grows next year — comes
/// back as `409 duplicate_trusted_jwt_issuer`, telling a client to go adopt an existing row
/// that does not exist. `already_claimed_on_unique_violation` has carried the equivalent unit
/// test since plan 07; this is the one that was missing.
///
/// `clock_skew_seconds` is `i32` in the DTO and `check (clock_skew_seconds >= 0)` in the
/// schema, so a negative value is the reachable non-unique database failure on exactly this
/// statement.
///
/// The assertion is deliberately **negative**. What this deployment currently does with a
/// negative skew is return `500 database_error` — the value should be refused as a `422` by
/// the service before it ever reaches SQL, which is a separate gap and not this test's
/// subject. Pinning the 500 would cement it; pinning "this is not a duplicate" states only the
/// property the mapper is responsible for.
#[tokio::test]
async fn a_non_unique_database_failure_is_not_reported_as_a_duplicate_issuer() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let issuer = format!("https://skew-issuer-{}.example", Uuid::now_v7());

    let refused = request(
        router.clone(),
        "POST",
        "/api/v1/admin/jwt-issuers",
        HeaderMap::new(),
        None,
        Some(json!({
            "issuer": issuer,
            "jwks_url": "https://issuer.example/jwks",
            "allowed_algorithms": ["RS256"],
            "subject_claim": "sub",
            "clock_skew_seconds": -1
        })),
    )
    .await;

    assert_ne!(
        refused.status,
        StatusCode::CREATED,
        "the schema's check constraint must refuse a negative clock skew: {:?}",
        refused.body
    );
    assert_ne!(
        refused.code(),
        "duplicate_trusted_jwt_issuer",
        "a check violation is not a uniqueness conflict, and reporting it as one sends the \
         client to adopt a row that was never created: {:?}",
        refused.body
    );
    assert_ne!(
        refused.status,
        StatusCode::CONFLICT,
        "nor is it any other kind of conflict: {:?}",
        refused.body
    );

    let rows: i64 =
        sqlx::query_scalar("select count(*) from trusted_jwt_issuers where issuer = $1")
            .bind(&issuer)
            .fetch_one(&fixture.pool)
            .await
            .expect("count issuers");
    assert_eq!(rows, 0, "the premise is that no row was written");
}

#[tokio::test]
async fn create_is_idempotent_under_an_idempotency_key() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");
    let mut headers = HeaderMap::new();
    headers.insert(
        "idempotency-key",
        format!("auth-provider-{}", Uuid::now_v7())
            .parse()
            .expect("idempotency-key header"),
    );
    let body = json!({
        "method": "google_oauth",
        "display_name": "Google",
        "issuer": format!("https://accounts.test/{}", Uuid::now_v7()),
        "client_id": "console-client",
        "allowed_email_domains": ["example.com"]
    });

    let fresh = request(
        router.clone(),
        "POST",
        "/api/v1/admin/auth/providers",
        headers.clone(),
        None,
        Some(body.clone()),
    )
    .await;
    let replay = request(
        router,
        "POST",
        "/api/v1/admin/auth/providers",
        headers,
        None,
        Some(body),
    )
    .await;

    assert_eq!(fresh.status, StatusCode::CREATED, "{:?}", fresh.body);
    assert_eq!(replay.status, StatusCode::CREATED, "{:?}", replay.body);
    assert_eq!(fresh.body["id"], replay.body["id"]);
}

/// The scope matrix, across all seven `/api/v1/admin/auth/providers…` operations: a system
/// key presenting its credential explicitly (never the anonymous dev-admin fallback, which
/// only applies when *no* credential is supplied at all — `authenticate_admin`,
/// `src/security/auth.rs`) must be refused every operation it does not hold the scope for,
/// and admitted to every operation it does. `If-Match` is supplied on every mutating
/// negative check too: the header is extracted before authorization runs
/// (`src/http/auth_settings.rs`'s handlers call `require_if_match` ahead of the service
/// call), so an absent header would produce `400 if_match_required` and mask the `403` this
/// test exists to prove.
#[tokio::test]
async fn auth_settings_endpoints_require_their_scopes() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let read_only = mint_system_key(&router, &["moira:auth-settings:read"]).await;
    let write_only = mint_system_key(&router, &["moira:auth-settings:write"]).await;
    let delete_only = mint_system_key(&router, &["moira:auth-settings:delete"]).await;
    let unrelated = mint_system_key(&router, &["moira:models:read"]).await;

    // A fixed target for every negative check below. None of them can succeed — the
    // scope gate is the very first thing every service method does — so its version
    // never moves and it is safe to reuse across the whole matrix.
    let target = create_provider(&router, json!({})).await;
    let target_id = target.body["id"].as_str().unwrap().to_string();
    let target_version = target.version();

    struct Case<'a> {
        label: &'a str,
        method: &'a str,
        path: String,
        if_match: Option<i64>,
        body: Option<Value>,
    }
    let cases = [
        Case {
            label: "list",
            method: "GET",
            path: "/api/v1/admin/auth/providers".to_string(),
            if_match: None,
            body: None,
        },
        Case {
            label: "get",
            method: "GET",
            path: format!("/api/v1/admin/auth/providers/{target_id}"),
            if_match: None,
            body: None,
        },
        Case {
            label: "create",
            method: "POST",
            path: "/api/v1/admin/auth/providers".to_string(),
            if_match: None,
            body: Some(json!({
                "method": "jwks",
                "display_name": "Scope probe",
                "jwks_url": "https://idp.test/jwks"
            })),
        },
        Case {
            label: "patch",
            method: "PATCH",
            path: format!("/api/v1/admin/auth/providers/{target_id}"),
            if_match: Some(target_version),
            body: Some(json!({ "display_name": "Renamed by scope probe" })),
        },
        Case {
            label: "delete",
            method: "DELETE",
            path: format!("/api/v1/admin/auth/providers/{target_id}"),
            if_match: Some(target_version),
            body: None,
        },
        Case {
            label: "enable",
            method: "POST",
            path: format!("/api/v1/admin/auth/providers/{target_id}/enable"),
            if_match: Some(target_version),
            body: None,
        },
        Case {
            label: "disable",
            method: "POST",
            path: format!("/api/v1/admin/auth/providers/{target_id}/disable"),
            if_match: Some(target_version),
            body: None,
        },
    ];

    // Every unscoped or wrongly-scoped key must be refused every operation.
    for key in [&read_only, &write_only, &delete_only, &unrelated] {
        for case in &cases {
            let required_scope = match case.label {
                "list" | "get" => "moira:auth-settings:read",
                "create" | "patch" | "enable" | "disable" => "moira:auth-settings:write",
                "delete" => "moira:auth-settings:delete",
                other => unreachable!("unhandled case {other}"),
            };
            let held = match key {
                _ if key == &read_only => "moira:auth-settings:read",
                _ if key == &write_only => "moira:auth-settings:write",
                _ if key == &delete_only => "moira:auth-settings:delete",
                _ => "moira:models:read",
            };
            if held == required_scope {
                continue;
            }
            let result = request(
                router.clone(),
                case.method,
                &case.path,
                system_key_headers(key),
                case.if_match,
                case.body.clone(),
            )
            .await;
            assert_eq!(
                result.status,
                StatusCode::FORBIDDEN,
                "{} with scope {held} (needs {required_scope}) should be refused: {:?}",
                case.label,
                result.body
            );
        }
    }

    // Verify the target was never actually mutated by the negative sweep above.
    let unchanged = request(
        router.clone(),
        "GET",
        &format!("/api/v1/admin/auth/providers/{target_id}"),
        system_key_headers(&read_only),
        None,
        None,
    )
    .await;
    assert_eq!(
        unchanged.version(),
        target_version,
        "no negative check may have mutated the target"
    );

    // Now prove the correctly-scoped key succeeds at each operation, each against its own
    // fixture so a successful mutation cannot invalidate a later positive check.
    let list_ok = request(
        router.clone(),
        "GET",
        "/api/v1/admin/auth/providers",
        system_key_headers(&read_only),
        None,
        None,
    )
    .await;
    assert_eq!(list_ok.status, StatusCode::OK, "{:?}", list_ok.body);

    let get_ok = request(
        router.clone(),
        "GET",
        &format!("/api/v1/admin/auth/providers/{target_id}"),
        system_key_headers(&read_only),
        None,
        None,
    )
    .await;
    assert_eq!(get_ok.status, StatusCode::OK, "{:?}", get_ok.body);

    let create_ok = request(
        router.clone(),
        "POST",
        "/api/v1/admin/auth/providers",
        system_key_headers(&write_only),
        None,
        Some(json!({
            "method": "jwks",
            "display_name": "Scope probe create",
            "jwks_url": "https://idp.test/jwks"
        })),
    )
    .await;
    assert_eq!(
        create_ok.status,
        StatusCode::CREATED,
        "{:?}",
        create_ok.body
    );

    let patch_target = create_provider(&router, json!({})).await;
    let patch_ok = request(
        router.clone(),
        "PATCH",
        &format!(
            "/api/v1/admin/auth/providers/{}",
            patch_target.body["id"].as_str().unwrap()
        ),
        system_key_headers(&write_only),
        Some(patch_target.version()),
        Some(json!({ "display_name": "Renamed" })),
    )
    .await;
    assert_eq!(patch_ok.status, StatusCode::OK, "{:?}", patch_ok.body);

    let enable_target = create_provider(&router, json!({})).await;
    let enable_ok = request(
        router.clone(),
        "POST",
        &format!(
            "/api/v1/admin/auth/providers/{}/enable",
            enable_target.body["id"].as_str().unwrap()
        ),
        system_key_headers(&write_only),
        Some(enable_target.version()),
        None,
    )
    .await;
    assert_eq!(enable_ok.status, StatusCode::OK, "{:?}", enable_ok.body);

    let disable_ok = request(
        router.clone(),
        "POST",
        &format!(
            "/api/v1/admin/auth/providers/{}/disable",
            enable_target.body["id"].as_str().unwrap()
        ),
        system_key_headers(&write_only),
        Some(enable_ok.version()),
        None,
    )
    .await;
    assert_eq!(disable_ok.status, StatusCode::OK, "{:?}", disable_ok.body);

    let delete_target = create_provider(&router, json!({})).await;
    let delete_ok = request(
        router,
        "DELETE",
        &format!(
            "/api/v1/admin/auth/providers/{}",
            delete_target.body["id"].as_str().unwrap()
        ),
        system_key_headers(&delete_only),
        Some(delete_target.version()),
        None,
    )
    .await;
    assert_eq!(
        delete_ok.status,
        StatusCode::NO_CONTENT,
        "{:?}",
        delete_ok.body
    );
}

/// Pagination correctness for `GET /api/v1/admin/auth/providers`, which plan 07 added
/// without an accompanying walk in `tests/list_pagination.rs`. Seeds more rows than fit in
/// one page and walks the whole list through `pagination.next_cursor`, asserting the
/// concatenation of every page contains each seeded row exactly once — the failure mode a
/// single-page test cannot see (a `cursor` that is accepted and then ignored still passes a
/// one-page assertion).
#[tokio::test]
async fn auth_provider_settings_list_pages_without_duplicates_or_gaps() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("router");

    let mut seeded_ids = std::collections::HashSet::new();
    for _ in 0..5 {
        let created = create_provider(&router, json!({})).await;
        assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
        seeded_ids.insert(created.body["id"].as_str().unwrap().to_string());
    }

    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(cursor) => format!("/api/v1/admin/auth/providers?limit=2&cursor={cursor}"),
            None => "/api/v1/admin/auth/providers?limit=2".to_string(),
        };
        let page = request(router.clone(), "GET", &path, HeaderMap::new(), None, None).await;
        assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
        pages += 1;
        assert!(
            pages < 1000,
            "pagination did not terminate — a cursor is likely being ignored"
        );

        for row in page.body["data"].as_array().expect("data array") {
            let id = row["id"].as_str().unwrap().to_string();
            assert!(
                seen.insert(id.clone()),
                "row {id} was returned on more than one page — the cursor skipped nothing \
                 but the walk still duplicated it"
            );
        }

        let has_more = page.body["pagination"]["has_more"]
            .as_bool()
            .unwrap_or(false);
        let next = page.body["pagination"]["next_cursor"]
            .as_str()
            .map(str::to_string);
        if !has_more {
            assert!(next.is_none(), "has_more=false must carry no next_cursor");
            break;
        }
        cursor = Some(next.expect("has_more=true must carry a next_cursor"));
    }

    // The vacuity guard: the walk must actually have observed every row this test seeded,
    // not merely "found nothing and terminated immediately".
    for id in &seeded_ids {
        assert!(
            seen.contains(id),
            "seeded row {id} was never observed across the full walk"
        );
    }
}
