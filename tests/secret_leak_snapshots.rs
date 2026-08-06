//! Secret-leak snapshot suite (plan 05, P1-10b).
//!
//! Every assertion here is the **absence of a specific value this test itself
//! created**, never a shape check. The fixture mints real key material through
//! the real API — a system key, a consumer key, a provider credential, and an
//! RSA signing key backing a trusted JWT issuer — captures the plaintext at the
//! single point the API is *supposed* to reveal it, reads the AES-256-GCM
//! ciphertext and GCM nonce straight out of `provider_credentials`, and then
//! drives a representative slice of the admin and public surface asserting none
//! of it ever comes back.
//!
//! Two properties make the suite hard to leave silently green:
//!
//! 1. **Every needle is proven live before it is used.** The system-key creation
//!    response is asserted to *contain* the plaintext, the credential row is
//!    asserted to hold non-empty ciphertext and nonce, and the log buffer is
//!    asserted non-empty before any "the canary is not in the logs" claim. A
//!    needle that was never actually created would make every absence assertion
//!    vacuous; each is checked positively first.
//! 2. **Every identifier is suffixed with a per-run `Uuid`.** No assertion can
//!    accidentally be satisfied by a row an earlier run left behind, and no
//!    earlier run's leftovers can satisfy this run's positive checks either.
//!
//! Binary encodings are checked in every form a leak could plausibly take:
//! lowercase and uppercase hex, standard and URL-safe base64, and the
//! `serde_json` byte-array rendering that a `Vec<u8>` field would serialise to.

mod support;

use std::collections::BTreeSet;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use moira::{app::AppState, config::Settings};
use serde_json::{Value, json};
use sqlx::Row;
use support::{CapturedLogs, LifecycleFixture, install_log_capture};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

/// Public modulus of [`SIGNING_PRIVATE_KEY`], published by the stub JWKS server.
const SIGNING_KEY_ID: &str = "secret-leak-suite-key";
const SIGNING_MODULUS: &str = "r686LSRV-46Cn3oh00Zo43hZNDiHY-Oei0JLSjApgCAD1btVtD2ju5zlGxA97OPjzWAGC0Z8ZqYwmfNwFWLyaC8Sr5-R2ejUuBpH32t8aFf4Z6p1MLUlmXWHviBNVutUzeicKMPWzVQ0xnoktJ6jOxDOkox8JMiNPGbTRAuQ-7poobvKH34738OP8fdaCpPIabtTfvz5gI11PYTLDlwrDWje3smeonXuxwj1lChvv5m08J7BsK4Jvb_YaUq0kCuQbpjFApOaTc_cY-xYrWVRcv9aprKEsJQvBm8xdDiAukfybT-GE3vFOMjmrWqVcPd46mYL0cr_VdxScWum5S1rcQ";

/// Raw JWT signing material. This is a throwaway test key (the same one
/// `tests/public_authorization.rs` uses), and it is a needle: no part of it, and
/// no signature produced with it, may appear in any driven surface.
const SIGNING_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
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

/// Strings that must never appear in an `example`/`default` of the generated
/// OpenAPI document. Verified against the current document at authoring time:
/// none of them occurs anywhere in it today, so a hit is a real regression and
/// not a pre-existing false positive.
const SECRET_SHAPED_MARKERS: &[&str] = &[
    "moira_sys_",
    "moira_cons_",
    "sk-",
    "-----BEGIN",
    "eyJhbGciOi",
];

// ---------------------------------------------------------------------------
// Needles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Needle {
    label: String,
    value: String,
}

impl Needle {
    fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(
            value.len() >= 8,
            "a needle shorter than 8 characters is not a meaningful canary"
        );
        Self {
            label: label.into(),
            value,
        }
    }
}

fn hex_encode(bytes: &[u8], upper: bool) -> String {
    bytes
        .iter()
        .map(|byte| {
            if upper {
                format!("{byte:02X}")
            } else {
                format!("{byte:02x}")
            }
        })
        .collect()
}

/// Every textual form a raw byte string could plausibly leak in.
fn binary_needles(label: &str, bytes: &[u8]) -> Vec<Needle> {
    assert!(
        !bytes.is_empty(),
        "{label}: refusing to build needles from empty bytes — the fixture did not produce real material"
    );
    vec![
        Needle::new(format!("{label} (hex)"), hex_encode(bytes, false)),
        Needle::new(format!("{label} (HEX)"), hex_encode(bytes, true)),
        Needle::new(format!("{label} (base64)"), BASE64_STANDARD.encode(bytes)),
        Needle::new(
            format!("{label} (base64url)"),
            URL_SAFE_NO_PAD.encode(bytes),
        ),
        Needle::new(
            format!("{label} (json byte array)"),
            serde_json::to_string(bytes).expect("byte array serialises"),
        ),
    ]
}

// ---------------------------------------------------------------------------
// HTTP driving
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Driven {
    label: String,
    method: String,
    uri: String,
    status: StatusCode,
    text: String,
    body: Value,
}

impl Driven {
    fn is_error_envelope(&self) -> bool {
        self.status.is_client_error() || self.status.is_server_error()
    }
}

async fn drive(
    router: &Router,
    label: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Driven {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", format!("leak-{}", Uuid::now_v7()));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(match &body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .expect("build request");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("HTTP response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);

    Driven {
        label: label.to_string(),
        method: method.to_string(),
        uri: uri.to_string(),
        status,
        text,
        body: parsed,
    }
}

// ---------------------------------------------------------------------------
// Scenario
// ---------------------------------------------------------------------------

struct SigningIssuer {
    id: Uuid,
    token: String,
    signature_segment: String,
    _task: tokio::task::JoinHandle<()>,
}

impl SigningIssuer {
    async fn start(fixture: &LifecycleFixture, run_id: &str) -> Self {
        let jwks = json!({
            "keys": [{
                "kty": "RSA",
                "kid": SIGNING_KEY_ID,
                "use": "sig",
                "alg": "RS256",
                "n": SIGNING_MODULUS,
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
            .expect("bind stub JWKS server");
        let address = listener.local_addr().expect("stub JWKS address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve stub JWKS");
        });

        let issuer = format!("https://leak-suite.test/{run_id}");
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            insert into trusted_jwt_issuers (
                id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                subject_claim, application_id_claim, scopes_claim
            )
            values ($1, $2, $3, '{}', array['RS256'], 'sub', 'application_id', 'scope')
            "#,
        )
        .bind(id)
        .bind(&issuer)
        .bind(format!("http://{address}/jwks"))
        .execute(&fixture.pool)
        .await
        .expect("register the stub trusted JWT issuer");

        let claims = json!({
            "iss": issuer,
            "sub": format!("leak-suite-{run_id}"),
            "scope": "moira:applications:read",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(SIGNING_KEY_ID.to_string());
        let token = encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(SIGNING_PRIVATE_KEY.as_bytes()).expect("RSA signing key"),
        )
        .expect("mint the suite's JWT");
        let signature_segment = token
            .rsplit('.')
            .next()
            .expect("a JWT always has a signature segment")
            .to_string();

        Self {
            id,
            token,
            signature_segment,
            _task: task,
        }
    }
}

struct SecretScenario {
    fixture: LifecycleFixture,
    router: Router,
    logs: CapturedLogs,
    needles: Vec<Needle>,
    responses: Vec<Driven>,
    run_id: String,
    issuer_id: Uuid,
    provider_id: Uuid,
    reflected_probes: Vec<(String, String, Driven)>,
}

impl SecretScenario {
    /// Builds the whole fixture and drives every flow, recording each response.
    ///
    /// Returns `None` when `MOIRA_TEST_DATABASE_URL` is unset outside CI — the
    /// same fail-closed pattern every other DB-backed suite uses.
    async fn build() -> Option<Self> {
        let logs = install_log_capture();
        let fixture = LifecycleFixture::new().await?;
        let router = moira::build_router(fixture.state.clone()).expect("build the Moira router");
        let run_id = Uuid::now_v7().simple().to_string();

        let mut needles = Vec::new();
        let mut responses = Vec::new();

        // ---- system key: the API reveals the plaintext exactly once ---------
        let create_system_key = drive(
            &router,
            "create system key",
            "POST",
            "/api/v1/admin/system-keys",
            &[],
            Some(json!({
                "display_name": format!("leak-sys-{run_id}"),
                "scopes": ["moira:admin"],
                "expires_at": null,
            })),
        )
        .await;
        assert_eq!(
            create_system_key.status,
            StatusCode::CREATED,
            "system key creation failed: {}",
            create_system_key.text
        );
        let system_key_secret = create_system_key.body["secret"]
            .as_str()
            .expect("the create response reveals the system key once")
            .to_string();
        // Positive check: the needle is real and really was revealed here. If this
        // ever fails, every absence assertion below would have been vacuous.
        assert!(
            create_system_key.text.contains(&system_key_secret),
            "the system key plaintext must appear in its own creation response"
        );
        assert!(system_key_secret.starts_with("moira_sys_"));
        let system_key_id = create_system_key.body["resource"]["id"]
            .as_str()
            .expect("system key id")
            .to_string();
        needles.push(Needle::new("system key plaintext", &system_key_secret));

        // ---- consumer key ---------------------------------------------------
        let create_consumer_key = drive(
            &router,
            "create consumer key",
            "POST",
            "/api/v1/admin/consumer-keys",
            &[],
            Some(json!({
                "application_id": fixture.application_id,
                "display_name": format!("leak-cons-{run_id}"),
                "scopes": [
                    "moira:models:read",
                    "moira:routes:read",
                    "moira:executions:read",
                    "moira:usage:read",
                    "moira:capabilities:read",
                ],
                "expires_at": null,
            })),
        )
        .await;
        assert_eq!(
            create_consumer_key.status,
            StatusCode::CREATED,
            "consumer key creation failed: {}",
            create_consumer_key.text
        );
        let consumer_key_secret = create_consumer_key.body["secret"]
            .as_str()
            .expect("the create response reveals the consumer key once")
            .to_string();
        assert!(
            create_consumer_key.text.contains(&consumer_key_secret),
            "the consumer key plaintext must appear in its own creation response"
        );
        let consumer_key_id = create_consumer_key.body["resource"]["id"]
            .as_str()
            .expect("consumer key id")
            .to_string();
        needles.push(Needle::new("consumer key plaintext", &consumer_key_secret));

        // ---- provider credential: plaintext in, ciphertext + nonce at rest ---
        let create_provider = drive(
            &router,
            "create provider",
            "POST",
            "/api/v1/admin/providers",
            &[],
            Some(json!({
                "provider_type": "open_ai_compatible",
                "display_name": format!("leak-provider-{run_id}"),
                "base_url": "http://127.0.0.1:9/v1",
                "metadata": { "suite": "secret_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            create_provider.status,
            StatusCode::CREATED,
            "provider creation failed: {}",
            create_provider.text
        );
        let provider_id: Uuid = create_provider.body["id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .expect("provider id");

        let credential_plaintext = format!("sk-leakcanary-{run_id}");
        let create_credential = drive(
            &router,
            "create provider credential",
            "POST",
            "/api/v1/admin/provider-credentials",
            &[],
            Some(json!({
                "provider_id": provider_id,
                "credential_type": "api_key",
                "scope": { "type": "global" },
                "secret": { "api_key": credential_plaintext },
                "display_name": format!("leak-credential-{run_id}"),
                "priority": 100,
                "expires_at": null,
                "metadata": { "suite": "secret_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            create_credential.status,
            StatusCode::CREATED,
            "credential creation failed: {}",
            create_credential.text
        );
        let credential_id: Uuid = create_credential.body["id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .expect("credential id");
        let credential_version = create_credential.body["version"]
            .as_i64()
            .expect("credential version for If-Match");
        needles.push(Needle::new(
            "provider credential plaintext",
            &credential_plaintext,
        ));

        let row = sqlx::query(
            "select encrypted_payload, nonce, encryption_algorithm from provider_credentials where id = $1",
        )
        .bind(credential_id)
        .fetch_one(&fixture.pool)
        .await
        .expect("read the encrypted credential envelope");
        let ciphertext: Vec<u8> = row.get("encrypted_payload");
        let nonce: Vec<u8> = row.get("nonce");
        let algorithm: String = row.get("encryption_algorithm");
        assert_eq!(
            algorithm, "AES-256-GCM",
            "the suite's ciphertext/nonce needles assume the AES-256-GCM envelope"
        );
        assert!(
            !ciphertext.is_empty() && !nonce.is_empty(),
            "the credential row must hold real ciphertext and a real GCM nonce"
        );
        assert_eq!(nonce.len(), 12, "AES-GCM nonces are 96 bits");
        assert_ne!(
            ciphertext.as_slice(),
            credential_plaintext.as_bytes(),
            "the stored payload must actually be encrypted"
        );
        needles.extend(binary_needles("credential ciphertext", &ciphertext));
        needles.extend(binary_needles("credential GCM nonce", &nonce));

        // ---- raw JWT signing material ---------------------------------------
        let issuer = SigningIssuer::start(&fixture, &run_id).await;
        needles.push(Needle::new(
            "JWT signing key (PEM body)",
            SIGNING_PRIVATE_KEY
                .lines()
                .nth(1)
                .expect("PEM body line")
                .to_string(),
        ));
        needles.push(Needle::new(
            "JWT signature segment",
            &issuer.signature_segment,
        ));

        let system_key_header: &str = &system_key_secret;
        let consumer_key_header: &str = &consumer_key_secret;
        let bearer = format!("Bearer {}", issuer.token);

        // ---- representative admin flows -------------------------------------
        responses.push(create_system_key);
        responses.push(create_consumer_key);
        responses.push(create_provider);
        responses.push(create_credential);

        let admin_flows: Vec<(&str, &str, String, Option<Value>)> = vec![
            (
                "list system keys",
                "GET",
                "/api/v1/admin/system-keys?limit=50".to_string(),
                None,
            ),
            (
                "get system key",
                "GET",
                format!("/api/v1/admin/system-keys/{system_key_id}"),
                None,
            ),
            (
                "list consumer keys",
                "GET",
                "/api/v1/admin/consumer-keys?limit=50".to_string(),
                None,
            ),
            (
                "get consumer key",
                "GET",
                format!("/api/v1/admin/consumer-keys/{consumer_key_id}"),
                None,
            ),
            (
                "list provider credentials",
                "GET",
                "/api/v1/admin/provider-credentials?limit=50".to_string(),
                None,
            ),
            (
                "get provider credential",
                "GET",
                format!("/api/v1/admin/provider-credentials/{credential_id}"),
                None,
            ),
            (
                "get provider",
                "GET",
                format!("/api/v1/admin/providers/{provider_id}"),
                None,
            ),
            (
                "list applications",
                "GET",
                "/api/v1/admin/applications?limit=25".to_string(),
                None,
            ),
            (
                "list providers",
                "GET",
                "/api/v1/admin/providers?limit=25".to_string(),
                None,
            ),
            (
                "list jwt issuers",
                "GET",
                "/api/v1/admin/jwt-issuers?limit=25".to_string(),
                None,
            ),
            (
                "get jwt issuer",
                "GET",
                format!("/api/v1/admin/jwt-issuers/{}", issuer.id),
                None,
            ),
            (
                "admin setup status",
                "GET",
                "/api/v1/admin/setup/status".to_string(),
                None,
            ),
        ];
        for (label, method, uri, body) in admin_flows {
            responses.push(
                drive(
                    &router,
                    label,
                    method,
                    &uri,
                    &[("x-moira-system-key", system_key_header)],
                    body,
                )
                .await,
            );
        }

        // `PATCH` and `rotate` on a credential both require `If-Match` (plan 04),
        // so the current version is threaded explicitly rather than guessed.
        let patch_credential = drive(
            &router,
            "patch provider credential",
            "PATCH",
            &format!("/api/v1/admin/provider-credentials/{credential_id}"),
            &[
                ("x-moira-system-key", system_key_header),
                ("if-match", &credential_version.to_string()),
            ],
            Some(json!({ "display_name": format!("leak-credential-renamed-{run_id}") })),
        )
        .await;
        assert_eq!(
            patch_credential.status,
            StatusCode::OK,
            "credential patch failed: {}",
            patch_credential.text
        );
        let patched_version = patch_credential.body["version"]
            .as_i64()
            .expect("patched credential version");
        responses.push(patch_credential);

        // Credential rotation carries a *new* plaintext in the request body; it
        // must not be echoed anywhere, including in its own response.
        let rotated_plaintext = format!("sk-leakrotated-{run_id}");
        let rotate_credential = drive(
            &router,
            "rotate provider credential",
            "POST",
            &format!("/api/v1/admin/provider-credentials/{credential_id}/rotate"),
            &[
                ("x-moira-system-key", system_key_header),
                ("if-match", &patched_version.to_string()),
            ],
            Some(json!({ "secret": { "api_key": rotated_plaintext } })),
        )
        .await;
        assert_eq!(
            rotate_credential.status,
            StatusCode::OK,
            "credential rotation failed: {}",
            rotate_credential.text
        );
        needles.push(Needle::new(
            "rotated credential plaintext",
            &rotated_plaintext,
        ));
        responses.push(rotate_credential);

        // ---- authenticated JWT admin flow -----------------------------------
        responses.push(
            drive(
                &router,
                "admin list via trusted JWT",
                "GET",
                "/api/v1/admin/applications?limit=5",
                &[("authorization", bearer.as_str())],
                None,
            )
            .await,
        );

        // ---- audit surface --------------------------------------------------
        let audit_list = drive(
            &router,
            "list audit events",
            "GET",
            "/api/v1/admin/audit-events?limit=100",
            &[("x-moira-system-key", system_key_header)],
            None,
        )
        .await;
        let first_audit_id = audit_list.body["items"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item["id"].as_str())
            .map(str::to_string);
        responses.push(audit_list);
        if let Some(audit_id) = first_audit_id {
            responses.push(
                drive(
                    &router,
                    "get audit event",
                    "GET",
                    &format!("/api/v1/admin/audit-events/{audit_id}"),
                    &[("x-moira-system-key", system_key_header)],
                    None,
                )
                .await,
            );
        }

        // ---- public + infrastructure flows ----------------------------------
        for (label, uri) in [
            ("public models", "/api/v1/models?limit=25"),
            ("public routes", "/api/v1/routes?limit=25"),
            ("public capabilities", "/api/v1/capabilities"),
            ("public executions", "/api/v1/executions?limit=25"),
            ("public usage", "/api/v1/usage?limit=25"),
        ] {
            responses.push(
                drive(
                    &router,
                    label,
                    "GET",
                    uri,
                    &[("x-consumer-key", consumer_key_header)],
                    None,
                )
                .await,
            );
        }
        for (label, uri) in [
            ("openapi document", "/openapi.json"),
            ("metrics", "/metrics"),
            ("liveness", "/health/live"),
            ("readiness", "/health/ready"),
        ] {
            responses.push(drive(&router, label, "GET", uri, &[], None).await);
        }

        // ---- error flows, one per status class the plan names ----------------
        let mut reflected_probes = Vec::new();

        let bogus_system_key = format!("moira_sys_reflectcanary{run_id}");
        let unauthorized = drive(
            &router,
            "401 via a bogus system key",
            "GET",
            "/api/v1/admin/applications?limit=1",
            &[("x-moira-system-key", bogus_system_key.as_str())],
            None,
        )
        .await;
        assert_eq!(unauthorized.status, StatusCode::UNAUTHORIZED);
        reflected_probes.push((
            "rejected system key (header)".to_string(),
            bogus_system_key.clone(),
            unauthorized.clone(),
        ));
        responses.push(unauthorized);

        let bogus_bearer = format!("Bearer eyJhbGciOiJIUzI1NiJ9.e30.reflectcanary{run_id}");
        let bad_bearer = drive(
            &router,
            "401 via an unregistered bearer token",
            "GET",
            "/api/v1/admin/applications?limit=1",
            &[("authorization", bogus_bearer.as_str())],
            None,
        )
        .await;
        assert_eq!(bad_bearer.status, StatusCode::UNAUTHORIZED);
        reflected_probes.push((
            "rejected bearer token".to_string(),
            format!("reflectcanary{run_id}"),
            bad_bearer.clone(),
        ));
        responses.push(bad_bearer);

        let forbidden = drive(
            &router,
            "403 via a consumer key on an admin route",
            "GET",
            "/api/v1/admin/applications?limit=1",
            &[("x-consumer-key", consumer_key_header)],
            None,
        )
        .await;
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
        responses.push(forbidden);

        let not_found = drive(
            &router,
            "404 on an unknown application",
            "GET",
            &format!("/api/v1/admin/applications/{}", Uuid::now_v7()),
            &[("x-moira-system-key", system_key_header)],
            None,
        )
        .await;
        assert_eq!(not_found.status, StatusCode::NOT_FOUND);
        responses.push(not_found);

        let bogus_cursor = format!("reflectcanarycursor{run_id}");
        let bad_request = drive(
            &router,
            "400 on a tampered cursor",
            "GET",
            &format!("/api/v1/admin/applications?limit=1&cursor={bogus_cursor}"),
            &[("x-moira-system-key", system_key_header)],
            None,
        )
        .await;
        assert_eq!(bad_request.status, StatusCode::BAD_REQUEST);
        reflected_probes.push((
            "rejected pagination cursor (query)".to_string(),
            bogus_cursor,
            bad_request.clone(),
        ));
        responses.push(bad_request);

        // 409 via the idempotency ledger: the same `Idempotency-Key` replayed with
        // a different payload. (A duplicate `application_slug` is *not* usable
        // here — the unique index surfaces as `AppError::Sqlx`, i.e. a 500
        // `database_error`, not a 409. Reported as a finding, not fixed here.)
        let conflict_key = format!("leak-conflict-{run_id}");
        responses.push(
            drive(
                &router,
                "seed application for the conflict probe",
                "POST",
                "/api/v1/admin/applications",
                &[
                    ("x-moira-system-key", system_key_header),
                    ("idempotency-key", conflict_key.as_str()),
                ],
                Some(json!({
                    "application_slug": format!("leak-dup-a-{run_id}"),
                    "display_name": format!("Leak dup A {run_id}"),
                    "metadata": { "suite": "secret_leak_snapshots" },
                })),
            )
            .await,
        );
        let conflict = drive(
            &router,
            "409 on an idempotency-key replay with a different payload",
            "POST",
            "/api/v1/admin/applications",
            &[
                ("x-moira-system-key", system_key_header),
                ("idempotency-key", conflict_key.as_str()),
            ],
            Some(json!({
                "application_slug": format!("leak-dup-b-{run_id}"),
                "display_name": format!("Leak dup B {run_id}"),
                "metadata": { "suite": "secret_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            conflict.status,
            StatusCode::CONFLICT,
            "idempotency conflict probe: {}",
            conflict.text
        );
        responses.push(conflict);

        let unprocessable = drive(
            &router,
            "422 on an unknown scope",
            "POST",
            "/api/v1/admin/consumer-keys",
            &[("x-moira-system-key", system_key_header)],
            Some(json!({
                "application_id": fixture.application_id,
                "display_name": format!("leak-badscope-{run_id}"),
                "scopes": [format!("moira:reflectcanary:{run_id}")],
                "expires_at": null,
            })),
        )
        .await;
        assert_eq!(unprocessable.status, StatusCode::UNPROCESSABLE_ENTITY);
        responses.push(unprocessable);

        // 503 `database_unavailable` needs a state with no pool, so it gets its
        // own throwaway router rather than being faked on this one.
        let poolless = AppState::new(Settings::default(), None)
            .await
            .expect("pool-less app state");
        let poolless_router = moira::build_router(poolless).expect("pool-less router");
        let unavailable = drive(
            &poolless_router,
            "503 readiness without a database",
            "GET",
            "/health/ready",
            &[],
            None,
        )
        .await;
        assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
        responses.push(unavailable);

        // ---- system key rotation, deliberately last --------------------------
        //
        // `rotate_system_key` ignores the request body and always uses
        // `ApiKeyRotateRequest::default()`, whose `revoke_previous_immediately`
        // is `true` — so the key every flow above authenticated with dies here.
        // Rotating last keeps that from silently turning the suite into a run of
        // 401s. The response reveals a *new* plaintext once, by design; it is
        // registered as a needle and the only response allowed to contain it is
        // the reveal itself.
        let rotate_system_key = drive(
            &router,
            "rotate system key",
            "POST",
            &format!("/api/v1/admin/system-keys/{system_key_id}/rotate"),
            &[("x-moira-system-key", system_key_header)],
            None,
        )
        .await;
        assert_eq!(
            rotate_system_key.status,
            StatusCode::OK,
            "system key rotation failed: {}",
            rotate_system_key.text
        );
        let rotated_system_key_secret = rotate_system_key.body["secret"]
            .as_str()
            .expect("rotation reveals the new system key once")
            .to_string();
        assert!(
            rotate_system_key.text.contains(&rotated_system_key_secret),
            "the rotated plaintext must appear in its own rotation response"
        );
        assert_ne!(
            rotated_system_key_secret, system_key_secret,
            "rotation must mint a different key"
        );
        responses.push(rotate_system_key);
        needles.push(Needle::new(
            "rotated system key plaintext",
            &rotated_system_key_secret,
        ));

        // Prove the new key works and that reading keys back never reveals it.
        responses.push(
            drive(
                &router,
                "list system keys after rotation",
                "GET",
                "/api/v1/admin/system-keys?limit=50",
                &[("x-moira-system-key", rotated_system_key_secret.as_str())],
                None,
            )
            .await,
        );
        // And that the superseded key is now rejected.
        let superseded = drive(
            &router,
            "401 via the superseded system key",
            "GET",
            "/api/v1/admin/applications?limit=1",
            &[("x-moira-system-key", system_key_header)],
            None,
        )
        .await;
        assert_eq!(
            superseded.status,
            StatusCode::UNAUTHORIZED,
            "the previous system key must be revoked by rotation: {}",
            superseded.text
        );
        responses.push(superseded);

        logs.emit_probe(&format!("secret-leak-suite-{run_id}"));

        Some(Self {
            fixture,
            router,
            logs,
            needles,
            responses,
            run_id,
            issuer_id: issuer.id,
            provider_id,
            reflected_probes,
        })
    }

    /// Responses a needle may legitimately appear in: the one-time reveals.
    fn is_sanctioned_reveal(label: &str) -> bool {
        matches!(
            label,
            "create system key" | "create consumer key" | "rotate system key"
        )
    }

    fn served_openapi(&self) -> &Value {
        &self
            .responses
            .iter()
            .find(|driven| driven.label == "openapi document")
            .expect("the OpenAPI document was driven")
            .body
    }

    async fn audit_metadata_rows(&self) -> Vec<String> {
        sqlx::query_scalar::<_, String>("select metadata::text from audit_logs")
            .fetch_all(&self.fixture.pool)
            .await
            .expect("read audit_logs.metadata")
    }

    async fn cleanup(self) {
        let _ = sqlx::query("delete from trusted_jwt_issuers where id = $1")
            .bind(self.issuer_id)
            .execute(&self.fixture.pool)
            .await;
        let _ = sqlx::query("delete from providers where id = $1")
            .bind(self.provider_id)
            .execute(&self.fixture.pool)
            .await;
        drop(self.router);
    }
}

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

fn assert_absent(haystack: &str, where_: &str, needles: &[Needle]) {
    for needle in needles {
        assert!(
            !haystack.contains(&needle.value),
            "SECRET LEAK: {where_} contains the {} canary ({} chars). \
             This value was created by the test fixture and must never appear here.",
            needle.label,
            needle.value.len()
        );
    }
}

#[tokio::test]
async fn admin_and_public_response_bodies_never_contain_fixture_secret_material() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    assert!(
        scenario.responses.len() >= 30,
        "the suite must drive a representative surface, drove {}",
        scenario.responses.len()
    );
    assert!(
        scenario.needles.len() >= 14,
        "expected the full needle set (plaintexts + ciphertext/nonce encodings + JWT material), got {}",
        scenario.needles.len()
    );

    for driven in &scenario.responses {
        if SecretScenario::is_sanctioned_reveal(&driven.label) {
            continue;
        }
        assert_absent(
            &driven.text,
            &format!(
                "the response body of `{}` ({} {} -> {})",
                driven.label, driven.method, driven.uri, driven.status
            ),
            &scenario.needles,
        );
    }

    // The credential surface must still be *useful* — masked suffix and
    // fingerprint present — so this is not passing because the endpoint returns
    // nothing at all.
    let credential = scenario
        .responses
        .iter()
        .find(|driven| driven.label == "get provider credential")
        .expect("the credential read was driven");
    assert_eq!(credential.status, StatusCode::OK);
    assert!(
        credential.body["masked_secret"].is_string()
            && credential.body["secret_fingerprint"].is_string(),
        "the credential read must still return the masked/fingerprinted view: {}",
        credential.text
    );

    scenario.cleanup().await;
}

#[tokio::test]
async fn generated_openapi_document_contains_no_secret_like_examples() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    let document = scenario.served_openapi();
    assert!(
        document["openapi"]
            .as_str()
            .is_some_and(|v| v.starts_with("3.")),
        "the served document must be an OpenAPI document, got {}",
        &document.to_string()[..120.min(document.to_string().len())]
    );
    let serialized = serde_json::to_string(document).expect("serialise the served document");
    assert!(
        serialized.len() > 10_000,
        "the served OpenAPI document is implausibly small ({} bytes) — the assertion below would be vacuous",
        serialized.len()
    );

    assert_absent(
        &serialized,
        "the served OpenAPI document",
        &scenario.needles,
    );

    // Schemas, examples and defaults specifically: a hand-written example is the
    // classic place a real key gets pasted.
    let mut sampled = 0_usize;
    let mut offenders: Vec<String> = Vec::new();
    collect_examples(document, String::new(), &mut |pointer, value| {
        sampled += 1;
        let text = value.to_string();
        for marker in SECRET_SHAPED_MARKERS {
            if text.contains(marker) {
                offenders.push(format!("{pointer} contains {marker:?}: {text}"));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "SECRET LEAK: OpenAPI example/default values look like real credentials:\n{}",
        offenders.join("\n")
    );
    // Sanity: the walker actually visited the document.
    assert!(
        sampled > 0,
        "the example/default walker found nothing to inspect — it is not walking the document"
    );

    scenario.cleanup().await;
}

/// Collects every `example` / `examples` / `default` node with its JSON pointer.
fn collect_examples(node: &Value, pointer: String, sink: &mut impl FnMut(&str, &Value)) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                let child = format!("{pointer}/{key}");
                if matches!(key.as_str(), "example" | "examples" | "default") {
                    sink(&child, value);
                }
                collect_examples(value, child, sink);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                collect_examples(value, format!("{pointer}/{index}"), sink);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn audit_log_metadata_never_contains_secret_material() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    let rows = scenario.audit_metadata_rows().await;
    // The driven flows create keys, a credential, a provider and an application,
    // all of which audit. If nothing was written, the scan below proves nothing.
    assert!(
        rows.len() >= 5,
        "expected the driven flows to have written audit rows, found {}",
        rows.len()
    );

    let combined = rows.join("\n");
    assert_absent(&combined, "audit_logs.metadata", &scenario.needles);

    // Same check pushed into the database, so a row this process never read is
    // still covered.
    for needle in &scenario.needles {
        let hits: i64 = sqlx::query_scalar(
            "select count(*) from audit_logs where metadata::text like '%' || $1 || '%'",
        )
        .bind(&needle.value)
        .fetch_one(&scenario.fixture.pool)
        .await
        .expect("scan audit_logs.metadata");
        assert_eq!(
            hits, 0,
            "SECRET LEAK: {} audit_logs rows carry the {} canary in metadata",
            hits, needle.label
        );
    }

    scenario.cleanup().await;
}

#[tokio::test]
async fn captured_log_output_never_contains_secret_material() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    let captured = scenario.logs.contents();
    // Without this, an empty buffer would make the assertion below pass for the
    // wrong reason.
    assert!(
        captured.contains(&format!("secret-leak-suite-{}", scenario.run_id)),
        "the tracing capture is not live — the suite's own probe event is missing \
         from a {}-byte buffer",
        captured.len()
    );

    assert_absent(&captured, "captured tracing output", &scenario.needles);

    scenario.cleanup().await;
}

#[tokio::test]
async fn every_error_response_carries_a_non_empty_message_key_and_message() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    let errors: Vec<&Driven> = scenario
        .responses
        .iter()
        .filter(|driven| driven.is_error_envelope())
        .collect();
    let statuses: BTreeSet<u16> = errors.iter().map(|driven| driven.status.as_u16()).collect();
    assert!(
        statuses.is_superset(&BTreeSet::from([400, 401, 403, 404, 409, 422, 503])),
        "the suite must exercise every error class the plan names; saw {statuses:?}"
    );

    for driven in errors {
        let error = driven.body["error"].as_object().unwrap_or_else(|| {
            panic!(
                "`{}` ({} {} -> {}) returned no error envelope: {}",
                driven.label, driven.method, driven.uri, driven.status, driven.text
            )
        });

        let code = error["code"]
            .as_str()
            .unwrap_or_else(|| panic!("`{}`: error.code must be a string", driven.label));
        assert!(
            !code.trim().is_empty(),
            "`{}`: error.code must be non-empty",
            driven.label
        );

        let message_key = error["message_key"]
            .as_str()
            .unwrap_or_else(|| panic!("`{}`: error.message_key must be a string", driven.label));
        assert!(
            message_key.starts_with("moira.error."),
            "`{}`: message_key {message_key:?} must be namespaced",
            driven.label
        );
        assert_eq!(
            message_key,
            format!("moira.error.{code}"),
            "`{}`: message_key must be derived from the code (src/error.rs message_key())",
            driven.label
        );

        let message = error["message"]
            .as_str()
            .unwrap_or_else(|| panic!("`{}`: error.message must be a string", driven.label));
        assert!(
            !message.trim().is_empty(),
            "`{}`: error.message must be a non-empty default English string",
            driven.label
        );
        assert!(
            error["message_args"].is_object(),
            "`{}`: message_args must always be an object",
            driven.label
        );
        assert!(
            error["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "`{}`: request_id must be present",
            driven.label
        );
    }

    scenario.cleanup().await;
}

#[tokio::test]
async fn every_error_message_key_resolves_to_a_catalog_entry() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    let mut checked = BTreeSet::new();
    for driven in scenario
        .responses
        .iter()
        .filter(|driven| driven.is_error_envelope())
    {
        let message_key = driven.body["error"]["message_key"]
            .as_str()
            .unwrap_or_else(|| panic!("`{}`: message_key must be a string", driven.label));
        assert!(
            moira::i18n::is_known_key(message_key),
            "i18n CONTRACT VIOLATION: `{}` ({} {} -> {}) returned message_key {message_key:?}, \
             which has no entry in the response catalog and therefore renders as nothing",
            driven.label,
            driven.method,
            driven.uri,
            driven.status
        );
        checked.insert(message_key.to_string());
    }

    assert!(
        checked.len() >= 6,
        "expected at least six distinct error keys across the driven flows, saw {checked:?}"
    );
    assert!(
        checked.contains("moira.error.database_unavailable"),
        "the 503 readiness probe must contribute moira.error.database_unavailable, saw {checked:?}"
    );

    scenario.cleanup().await;
}

#[tokio::test]
async fn error_responses_never_echo_request_supplied_values_verbatim() {
    let Some(scenario) = SecretScenario::build().await else {
        return;
    };

    assert!(
        scenario.reflected_probes.len() >= 3,
        "expected header, bearer and query-parameter reflection probes"
    );

    for (what, supplied, driven) in &scenario.reflected_probes {
        assert!(
            driven.is_error_envelope(),
            "{what}: the probe must produce an error, got {}",
            driven.status
        );
        let error = &driven.body["error"];
        let message = error["message"].as_str().unwrap_or_default();
        let details = error["details"].to_string();
        let args = error["message_args"].to_string();

        assert!(
            !message.contains(supplied.as_str()),
            "REFLECTED INPUT: {what} was echoed into error.message: {message}"
        );
        assert!(
            !details.contains(supplied.as_str()),
            "REFLECTED INPUT: {what} was echoed into error.details: {details}"
        );
        assert!(
            !args.contains(supplied.as_str()),
            "REFLECTED INPUT: {what} was echoed into error.message_args: {args}"
        );
    }

    scenario.cleanup().await;
}
