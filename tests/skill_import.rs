//! End-to-end coverage for the OpenAPI import pipeline and `skill_http_executors` CRUD
//! (issue #237, plan 12 §5, workstream H).
//!
//! Drives `/api/v1/admin/skills/import` and `/api/v1/admin/skills/{id}/executor` against a
//! real Postgres database created by [`support::TestDatabase`], which already applies
//! `migrations/0031_agent_platform.sql` (workstream F's schema, which this workstream reuses
//! without a new migration). Skips (never fails) when no test database is configured,
//! following the CONVENTIONS §3 gating pattern already used by `tests/agent_platform.rs` —
//! CI's Postgres shard is the authoritative run.

mod support;

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{app::AppState, config::Settings};
use serde_json::{Map, Value, json};
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

use support::TestDatabase;

const WAIT: Duration = Duration::from_secs(10);

/// A public, always-reachable-without-DNS host for the tests below that expect a successful
/// import: an IP literal never needs a resolver call (`security::ssrf::validate_outbound_url`
/// classifies it via the pure `is_denied_ip` alone), so these tests never depend on real DNS
/// resolution or outbound network access being available in CI — the same technique
/// `src/security/ssrf.rs`'s own "public addresses are allowed" unit tests use.
const RESOLVABLE_TEST_HOST: &str = "8.8.8.8";

struct Fixture {
    router: Router,
    suffix: String,
    _database: TestDatabase,
}

struct HttpResult {
    status: StatusCode,
    body: Value,
    /// The raw `ETag` response header, quotes and all — echoed straight back as `If-Match`
    /// by the tests below, exactly as a real client would. Deliberately opaque here: this
    /// suite exercises both the integer-version resources (`skills`) and the
    /// timestamp-based one (`skill_http_executors`), and neither this fixture nor a caller
    /// needs to know which shape a given `ETag` carries.
    etag: Option<String>,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let database = TestDatabase::create().await?;
        let pool = database.pool.clone();
        let settings = Settings::default();
        let state = AppState::new(settings, Some(pool))
            .await
            .expect("test app state");
        let router = moira::build_router(state).expect("test router");
        Some(Self {
            router,
            suffix: Uuid::now_v7().simple().to_string(),
            _database: database,
        })
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        if_match: Option<&str>,
        body: Option<Value>,
    ) -> HttpResult {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("x-request-id", format!("skill-import-{}", Uuid::now_v7()));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        if let Some(value) = if_match {
            builder = builder.header("if-match", value);
        }
        let request = builder
            .body(match body {
                Some(value) => Body::from(value.to_string()),
                None => Body::empty(),
            })
            .expect("HTTP request");
        let response = timeout(WAIT, self.router.clone().oneshot(request))
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
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
            serde_json::from_slice(&bytes).expect("JSON response")
        };
        HttpResult { status, body, etag }
    }
}

/// A minimal, valid OpenAPI 3.0 document: one `GET` with a required path parameter and one
/// `POST` with a JSON request body, so a single import exercises both parameter-derived and
/// requestBody-derived `params_schema`.
fn sample_document(host: &str, suffix: &str) -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {"title": "Sample Orders API", "version": "1.0.0"},
        "servers": [{"url": format!("https://{host}/v1")}],
        "paths": {
            format!("/orders-{suffix}/{{order_id}}"): {
                "get": {
                    "operationId": format!("getOrder{suffix}"),
                    "summary": "Get an order",
                    "tags": ["orders"],
                    "parameters": [
                        {
                            "name": "order_id",
                            "in": "path",
                            "required": true,
                            "schema": {"type": "string"}
                        }
                    ]
                }
            },
            format!("/orders-{suffix}"): {
                "post": {
                    "operationId": format!("createOrder{suffix}"),
                    "summary": "Create an order",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {"sku": {"type": "string"}}
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// A second public IP literal, so a test can name a host that is legitimate as far as the
/// SSRF guard is concerned but is *not* [`RESOLVABLE_TEST_HOST`]. Standing in for issue #253
/// finding 1's `collector.attacker.example`: the whole point is that it passes
/// `validate_outbound_url`, because a public host always does.
const OTHER_PUBLIC_TEST_HOST: &str = "1.1.1.1";

fn skill_id_of(record: &Value) -> Uuid {
    Uuid::parse_str(record["id"].as_str().expect("skill id")).expect("UUID id")
}

impl Fixture {
    /// Creates a provider whose `base_url` is `https://{host}` and one `api_key` credential
    /// on it, returning the credential id. Both go through the real admin routes, so the
    /// credential is stored and encrypted exactly as a real one is.
    async fn credential_on_provider_at(&self, host: &str, label: &str) -> Uuid {
        let provider = self
            .request(
                "POST",
                "/api/v1/admin/providers",
                None,
                Some(json!({
                    "provider_type": "custom",
                    "display_name": format!("{label} provider {}", self.suffix),
                    "base_url": format!("https://{host}"),
                    "metadata": {}
                })),
            )
            .await;
        assert_eq!(
            provider.status,
            StatusCode::CREATED,
            "create provider: {}",
            provider.body
        );
        let provider_id = provider.body["id"]
            .as_str()
            .expect("provider id")
            .to_string();

        let credential = self
            .request(
                "POST",
                "/api/v1/admin/provider-credentials",
                None,
                Some(json!({
                    "provider_id": provider_id,
                    "credential_type": "api_key",
                    "scope": {"type": "global"},
                    "secret": {"api_key": format!("sk-{label}-{}", self.suffix)},
                    "display_name": format!("{label} credential"),
                    "priority": 100,
                    "metadata": {}
                })),
            )
            .await;
        assert_eq!(
            credential.status,
            StatusCode::CREATED,
            "create credential: {}",
            credential.body
        );
        Uuid::parse_str(credential.body["id"].as_str().expect("credential id"))
            .expect("UUID credential id")
    }

    /// Imports [`sample_document`] at `host` and returns `(skill_id, executor_etag)`.
    async fn imported_executor(&self, host: &str, label: &str) -> (Uuid, String) {
        let document = sample_document(host, &format!("{label}{}", self.suffix));
        let imported = self
            .request(
                "POST",
                "/api/v1/admin/skills/import",
                None,
                Some(json!({ "document": document })),
            )
            .await;
        assert_eq!(
            imported.status,
            StatusCode::CREATED,
            "import: {}",
            imported.body
        );
        let skill_id = skill_id_of(&imported.body["skills"][0]);
        let fetched = self
            .request(
                "GET",
                &format!("/api/v1/admin/skills/{skill_id}/executor"),
                None,
                None,
            )
            .await;
        assert_eq!(fetched.status, StatusCode::OK, "get: {}", fetched.body);
        assert_eq!(fetched.body["allowed_host"], json!(host));
        (skill_id, fetched.etag.expect("GET must return an ETag"))
    }
}

/// Issue #253 finding 1. `skill_http_executors.credential_id` is decrypted at call time and
/// sent as `Authorization: Bearer <plaintext>`, and the only check on it was that the row
/// existed — so `moira:skills:write` was silently equivalent to reading every provider secret
/// in the deployment, by binding one to an attacker-controlled *public* host.
///
/// Both directions of the attack are pinned here, because either half alone moves the secret:
/// binding a foreign credential to this executor's host, and moving a bound credential's host
/// after the fact.
#[tokio::test]
async fn a_skill_executor_may_only_carry_a_credential_its_own_provider_issued() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let (skill_id, etag) = fixture
        .imported_executor(RESOLVABLE_TEST_HOST, "bind")
        .await;
    let executor_path = format!("/api/v1/admin/skills/{skill_id}/executor");

    // The attack: a credential belonging to a provider that talks to a different host.
    // `1.1.1.1` is an ordinary public host, so the SSRF guard has no objection to it and
    // never sees this request at all — the refusal has to come from the binding rule.
    let foreign = fixture
        .credential_on_provider_at(OTHER_PUBLIC_TEST_HOST, "foreign")
        .await;
    let refused = fixture
        .request(
            "PATCH",
            &executor_path,
            Some(&etag),
            Some(json!({ "credential_id": foreign })),
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "body: {}",
        refused.body
    );
    assert_eq!(
        refused.body["error"]["code"], "skill_credential_host_mismatch",
        "body: {}",
        refused.body
    );

    // The refusal is a refusal, not a partial write.
    let after = fixture.request("GET", &executor_path, None, None).await;
    assert_eq!(after.status, StatusCode::OK);
    assert_eq!(
        after.body["credential_id"],
        Value::Null,
        "a refused bind must leave credential_id unset"
    );
    assert_eq!(
        after.etag.as_deref(),
        Some(etag.as_str()),
        "a refused bind must not bump updated_at"
    );

    // A credential whose provider does serve this executor's host binds normally.
    let own = fixture
        .credential_on_provider_at(RESOLVABLE_TEST_HOST, "own")
        .await;
    let bound = fixture
        .request(
            "PATCH",
            &executor_path,
            Some(&etag),
            Some(json!({ "credential_id": own })),
        )
        .await;
    assert_eq!(bound.status, StatusCode::OK, "body: {}", bound.body);
    assert_eq!(bound.body["credential_id"], json!(own.to_string()));
    let bound_etag = bound.etag.expect("PATCH must return an ETag");

    // The other direction: leave the credential alone and move the *host* under it. The new
    // URL is a perfectly good public https URL, so `validate_skill_url` allows it; only the
    // binding rule stands between a bound secret and a new destination.
    let moved = fixture
        .request(
            "PATCH",
            &executor_path,
            Some(&bound_etag),
            Some(json!({
                "url_template": format!("https://{OTHER_PUBLIC_TEST_HOST}/v1/orders")
            })),
        )
        .await;
    assert_eq!(
        moved.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "body: {}",
        moved.body
    );
    assert_eq!(
        moved.body["error"]["code"], "skill_credential_host_mismatch",
        "body: {}",
        moved.body
    );

    let final_state = fixture.request("GET", &executor_path, None, None).await;
    assert_eq!(
        final_state.body["allowed_host"],
        json!(RESOLVABLE_TEST_HOST),
        "a refused host move must leave allowed_host where it was"
    );
    assert_eq!(final_state.body["credential_id"], json!(own.to_string()));
}

/// A provider left on its vendor default has no `base_url`, so there is no host to compare
/// against and nothing it can entitle. Fail-closed — see
/// `domain::credential_binding_permits_host` for why inventing the vendor default would be
/// worse than refusing.
#[tokio::test]
async fn a_credential_whose_provider_has_no_base_url_cannot_be_bound_at_all() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let (skill_id, etag) = fixture
        .imported_executor(RESOLVABLE_TEST_HOST, "nobase")
        .await;

    let provider = fixture
        .request(
            "POST",
            "/api/v1/admin/providers",
            None,
            Some(json!({
                "provider_type": "custom",
                "display_name": format!("Vendor-default provider {}", fixture.suffix),
                "base_url": Value::Null,
                "metadata": {}
            })),
        )
        .await;
    assert_eq!(
        provider.status,
        StatusCode::CREATED,
        "body: {}",
        provider.body
    );
    let credential = fixture
        .request(
            "POST",
            "/api/v1/admin/provider-credentials",
            None,
            Some(json!({
                "provider_id": provider.body["id"],
                "credential_type": "api_key",
                "scope": {"type": "global"},
                "secret": {"api_key": format!("sk-default-{}", fixture.suffix)},
                "display_name": "Vendor-default credential",
                "priority": 100,
                "metadata": {}
            })),
        )
        .await;
    assert_eq!(
        credential.status,
        StatusCode::CREATED,
        "body: {}",
        credential.body
    );

    let refused = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            Some(&etag),
            Some(json!({ "credential_id": credential.body["id"] })),
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "body: {}",
        refused.body
    );
    assert_eq!(
        refused.body["error"]["code"],
        "skill_credential_host_mismatch"
    );
}

#[tokio::test]
async fn openapi_import_creates_draft_skills_and_http_executors() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let document = sample_document(RESOLVABLE_TEST_HOST, &fixture.suffix);

    let imported = fixture
        .request(
            "POST",
            "/api/v1/admin/skills/import",
            None,
            Some(json!({ "document": document })),
        )
        .await;
    assert_eq!(
        imported.status,
        StatusCode::CREATED,
        "body: {}",
        imported.body
    );
    assert_eq!(imported.body["imported_count"], 2);

    let skills = imported.body["skills"].as_array().expect("skills array");
    let executors = imported.body["executors"]
        .as_array()
        .expect("executors array");
    assert_eq!(skills.len(), 2);
    assert_eq!(executors.len(), 2);

    // Every imported skill lands draft/tool (fail-closed — §5 decision 22), and every
    // executor shares the one already-SSRF-validated host derived from the document's
    // server URL.
    for skill in skills {
        assert_eq!(skill["status"], "draft");
        assert_eq!(skill["kind"], "tool");
    }
    for executor in executors {
        assert_eq!(executor["allowed_host"], RESOLVABLE_TEST_HOST);
        assert!(
            executor["url_template"]
                .as_str()
                .expect("url_template")
                .starts_with(&format!("https://{RESOLVABLE_TEST_HOST}/v1")),
        );
    }

    // The GET-derived operation carries its path parameter in params_schema; the
    // POST-derived one carries its request body under the "body" property.
    let get_skill = skills
        .iter()
        .find(|skill| skill["skill_key"] == json!(format!("getorder{}", fixture.suffix)))
        .expect("the GET operation's skill");
    assert_eq!(
        get_skill["params_schema"]["properties"]["order_id"]["type"],
        "string"
    );
    assert_eq!(get_skill["params_schema"]["required"], json!(["order_id"]));

    let post_skill = skills
        .iter()
        .find(|skill| skill["skill_key"] == json!(format!("createorder{}", fixture.suffix)))
        .expect("the POST operation's skill");
    assert_eq!(
        post_skill["params_schema"]["properties"]["body"]["properties"]["sku"]["type"],
        "string"
    );

    // Verify against the standalone GET too, not just the import response.
    let get_skill_id = skill_id_of(get_skill);
    let fetched = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/skills/{get_skill_id}"),
            None,
            None,
        )
        .await;
    assert_eq!(fetched.status, StatusCode::OK);
    assert_eq!(fetched.body["status"], "draft");
}

#[tokio::test]
async fn openapi_import_is_idempotent_under_a_replay_key() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let document = sample_document(RESOLVABLE_TEST_HOST, &format!("idem{}", fixture.suffix));

    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/skills/import")
        .header("content-type", "application/json")
        .header("idempotency-key", format!("import-{}", fixture.suffix));
    builder = builder.header("x-request-id", format!("skill-import-{}", Uuid::now_v7()));
    let request = builder
        .body(Body::from(json!({ "document": document }).to_string()))
        .expect("request");
    let first = timeout(WAIT, fixture.router.clone().oneshot(request))
        .await
        .expect("timed out")
        .expect("response");
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();

    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/skills/import")
        .header("content-type", "application/json")
        .header("idempotency-key", format!("import-{}", fixture.suffix));
    builder = builder.header("x-request-id", format!("skill-import-{}", Uuid::now_v7()));
    let request = builder
        .body(Body::from(json!({ "document": document }).to_string()))
        .expect("request");
    let replay = timeout(WAIT, fixture.router.clone().oneshot(request))
        .await
        .expect("timed out")
        .expect("response");
    assert_eq!(replay.status(), StatusCode::CREATED);
    let replay_body: Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), usize::MAX).await.unwrap()).unwrap();

    assert_eq!(
        first_body["skills"], replay_body["skills"],
        "a replayed import must not create a second batch of skills"
    );
}

#[tokio::test]
async fn openapi_import_rejects_a_document_over_the_operation_cap() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut paths = Map::new();
    for index in 0..301 {
        paths.insert(
            format!("/op-{}-{index}", fixture.suffix),
            json!({"get": {"operationId": format!("op{}{index}", fixture.suffix)}}),
        );
    }
    let document = json!({
        "openapi": "3.0.3",
        "servers": [{"url": "https://api.example.com"}],
        "paths": Value::Object(paths),
    });

    let response = fixture
        .request(
            "POST",
            "/api/v1/admin/skills/import",
            None,
            Some(json!({ "document": document })),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        response.body
    );
    assert_eq!(response.body["error"]["code"], "import_cap_exceeded");
    assert_eq!(response.body["error"]["details"]["operation_count"], 301);
    assert_eq!(response.body["error"]["details"]["cap"], 300);
}

#[tokio::test]
async fn openapi_import_rejects_a_server_url_that_resolves_to_a_blocked_host() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    // The cloud metadata endpoint — the canonical SSRF target `security::ssrf` exists to
    // block. A literal IP host needs no DNS resolution, so this exercises the address-range
    // check directly and deterministically.
    let document = sample_document("169.254.169.254", &fixture.suffix);

    let response = fixture
        .request(
            "POST",
            "/api/v1/admin/skills/import",
            None,
            Some(json!({ "document": document })),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        response.body
    );
    assert_eq!(response.body["error"]["code"], "ssrf_blocked_host");
    // The resolved/denied address must never be echoed back to the caller.
    assert!(
        !response.body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("169.254.169.254")
    );
}

#[tokio::test]
async fn openapi_import_rejects_a_document_with_no_operations() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let document = json!({
        "openapi": "3.0.3",
        "servers": [{"url": "https://api.example.com"}],
        "paths": {}
    });

    let response = fixture
        .request(
            "POST",
            "/api/v1/admin/skills/import",
            None,
            Some(json!({ "document": document })),
        )
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"]["code"], "invalid_openapi_spec");
}

#[tokio::test]
async fn skill_http_executor_crud_lifecycle_over_http() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let document = sample_document(RESOLVABLE_TEST_HOST, &format!("crud{}", fixture.suffix));
    let imported = fixture
        .request(
            "POST",
            "/api/v1/admin/skills/import",
            None,
            Some(json!({ "document": document })),
        )
        .await;
    assert_eq!(
        imported.status,
        StatusCode::CREATED,
        "body: {}",
        imported.body
    );
    let skill = &imported.body["skills"][0];
    let skill_id = skill_id_of(skill);

    // Get.
    let fetched = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            None,
            None,
        )
        .await;
    assert_eq!(fetched.status, StatusCode::OK, "body: {}", fetched.body);
    assert_eq!(fetched.body["skill_id"], json!(skill_id.to_string()));
    assert!(fetched.etag.is_some(), "GET must return an ETag");

    // List includes it.
    let listed = fixture
        .request("GET", "/api/v1/admin/skill-executors", None, None)
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert!(
        listed.body["data"]
            .as_array()
            .expect("data array")
            .iter()
            .any(|row| row["skill_id"] == json!(skill_id.to_string())),
        "list did not contain the imported executor"
    );

    // PATCH without If-Match is rejected.
    let missing_if_match = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            None,
            Some(json!({"timeout_ms": 5000})),
        )
        .await;
    assert_eq!(missing_if_match.status, StatusCode::BAD_REQUEST);

    // PATCH bumps updated_at (and therefore the ETag).
    let patched = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            fetched.etag.as_deref(),
            Some(json!({"timeout_ms": 5000})),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "body: {}", patched.body);
    assert_eq!(patched.body["timeout_ms"], 5000);
    assert_ne!(
        patched.etag, fetched.etag,
        "a successful PATCH must change the ETag"
    );

    // A stale If-Match (the pre-patch ETag) is now a 409.
    let stale = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            fetched.etag.as_deref(),
            Some(json!({"timeout_ms": 6000})),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "body: {}", stale.body);

    // PATCHing url_template re-derives allowed_host and re-runs the SSRF check.
    let blocked_patch = fixture
        .request(
            "PATCH",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            patched.etag.as_deref(),
            Some(json!({"url_template": "https://127.0.0.1/orders"})),
        )
        .await;
    assert_eq!(blocked_patch.status, StatusCode::BAD_REQUEST);
    assert_eq!(blocked_patch.body["error"]["code"], "ssrf_blocked_host");

    // Delete requires If-Match; afterwards the executor is gone with the catalogued code.
    let deleted = fixture
        .request(
            "DELETE",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            patched.etag.as_deref(),
            None,
        )
        .await;
    assert_eq!(
        deleted.status,
        StatusCode::NO_CONTENT,
        "body: {}",
        deleted.body
    );

    let gone = fixture
        .request(
            "GET",
            &format!("/api/v1/admin/skills/{skill_id}/executor"),
            None,
            None,
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(gone.body["error"]["code"], "executor_not_found");
}
