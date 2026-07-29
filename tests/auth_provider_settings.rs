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

use std::time::Duration;

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
#[tokio::test]
async fn claim_status_is_anonymous_while_auth_methods_is_not() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // Admin JWT auth *enabled*, unlike the default fixture: with it disabled an
    // uncredentialed admin request falls back to a dev-admin actor, which would mask the
    // 401 this test exists to assert.
    let mut settings = moira::config::Settings::default();
    settings.auth.jwks.allow_insecure_dev_urls = true;
    settings.auth.admin.enabled = true;
    let state =
        AppState::new(settings, Some(fixture.pool.clone())).expect("state with admin auth on");
    let router = moira::build_router(state).expect("router");

    let claim_status = request(
        router.clone(),
        "GET",
        "/api/v1/admin/setup/claim-status",
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
        auth_methods.status,
        StatusCode::UNAUTHORIZED,
        "{:?}",
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
    // Anonymous reconnaissance must yield nothing: no method list, no issuer, no client id,
    // no domain policy.
    assert!(auth_methods.body.get("methods").is_none());
    let rendered = auth_methods.body.to_string();
    for leaked in ["client_id", "issuer", "allowed_email_domains"] {
        assert!(!rendered.contains(leaked), "anonymous call leaked {leaked}");
    }
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

    let listener = moira::infra::db::spawn_runtime_config_listener(
        fixture.pool.clone(),
        moira::infra::db::RuntimeInvalidationTargets::from_state(&fixture.state),
    );

    // Populate the cache through the read path the console uses.
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
