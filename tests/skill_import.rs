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

use std::{collections::HashMap, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{
    app::AppState, config::Settings, domain::SkillCredentialOutcome,
    infra::repositories::PgAgentPlatformRepository,
};
use serde_json::{Map, Value, json};
use sqlx::Row;
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
    /// Only the legacy-row and inventory tests use this, and only to *create* pre-rule state
    /// — a `skill_http_executors` row whose binding no admin route would accept today, or a
    /// `providers.base_url` the admin write path normalises away. That state is by definition
    /// unreachable through the routes, so writing it any other way would be testing a shape
    /// the fixture invented rather than the one a real upgrade inherits. The inventory test
    /// also runs the documented operator query through it.
    pool: sqlx::PgPool,
    /// Kept so a test can reach `AppState::cipher` and call the execution-path resolver
    /// directly; the router owns its own clone.
    state: AppState,
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
        let state = AppState::new(settings, Some(pool.clone()))
            .await
            .expect("test app state");
        let router = moira::build_router(state.clone()).expect("test router");
        Some(Self {
            router,
            suffix: Uuid::now_v7().simple().to_string(),
            pool,
            state,
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
/// A third public IP literal, for the legacy-row test: it needs a host that is neither the
/// executor's own nor the one the already-bound foreign credential's provider serves, so that
/// "cannot be repointed" is proved against a genuinely new destination.
const THIRD_PUBLIC_TEST_HOST: &str = "9.9.9.9";

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

/// The repair to round one's own admitted defect: a `skill_http_executors` row stored **before**
/// the binding rule existed must stay editable.
///
/// Round one re-validated on every patch, so a legacy row was un-patchable even for an
/// unrelated `timeout_ms` edit. That is not a security property. The row's credential is
/// already refused at execution time (`resolve_skill_credential` returns `HostNotEntitled`
/// before it decrypts anything), so the write refusal moved no secret — it only stopped an
/// operator cleaning up, on the same endpoint that told them to.
///
/// The rule is now "do not make it worse": the binding may stay exactly where it is, and
/// everything else about the row is free. Both refusals below prove the security half is
/// intact, and the last step proves the documented repair actually lands.
#[tokio::test]
async fn a_row_that_predates_the_binding_rule_stays_editable_but_cannot_be_repointed() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let (skill_id, _etag) = fixture
        .imported_executor(RESOLVABLE_TEST_HOST, "legacy")
        .await;
    let path = format!("/api/v1/admin/skills/{skill_id}/executor");

    // The pre-rule state: a credential whose provider serves a different host, bound straight
    // in the table the way a pre-rule admin write, a migration or a restore left it.
    let foreign = fixture
        .credential_on_provider_at(OTHER_PUBLIC_TEST_HOST, "legacyforeign")
        .await;
    sqlx::query("update skill_http_executors set credential_id = $1 where skill_id = $2")
        .bind(foreign)
        .bind(skill_id)
        .execute(&fixture.pool)
        .await
        .expect("seed the pre-rule binding");

    let current = fixture.request("GET", &path, None, None).await;
    assert_eq!(current.status, StatusCode::OK, "body: {}", current.body);
    assert_eq!(current.body["credential_id"], json!(foreign.to_string()));
    let etag = current.etag.expect("GET must return an ETag");

    // 1. An unrelated field edit lands. This is the assertion round one fails.
    let edited = fixture
        .request(
            "PATCH",
            &path,
            Some(&etag),
            Some(json!({ "timeout_ms": 4_321 })),
        )
        .await;
    assert_eq!(
        edited.status,
        StatusCode::OK,
        "a legacy row must stay editable for fields that are not the binding: {}",
        edited.body
    );
    assert_eq!(edited.body["timeout_ms"], json!(4_321));
    assert_eq!(
        edited.body["credential_id"],
        json!(foreign.to_string()),
        "the unrelated edit must not have quietly re-bound anything"
    );
    let etag = edited.etag.expect("PATCH must return an ETag");

    // 2. The binding itself still cannot move to another non-entitled credential.
    let other = fixture
        .credential_on_provider_at(THIRD_PUBLIC_TEST_HOST, "legacyother")
        .await;
    let repointed = fixture
        .request(
            "PATCH",
            &path,
            Some(&etag),
            Some(json!({ "credential_id": other })),
        )
        .await;
    assert_eq!(
        repointed.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a broken row is not a licence to point it somewhere new: {}",
        repointed.body
    );
    assert_eq!(
        repointed.body["error"]["code"],
        "skill_credential_host_mismatch"
    );

    // 3. …and neither can the host move under it.
    let moved = fixture
        .request(
            "PATCH",
            &path,
            Some(&etag),
            Some(json!({
                "url_template": format!("https://{THIRD_PUBLIC_TEST_HOST}/v1/orders")
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
        moved.body["error"]["code"],
        "skill_credential_host_mismatch"
    );

    // 4. The documented repair — bind a credential the executor's host is entitled to — works.
    let own = fixture
        .credential_on_provider_at(RESOLVABLE_TEST_HOST, "legacyrepair")
        .await;
    let repaired = fixture
        .request(
            "PATCH",
            &path,
            Some(&etag),
            Some(json!({ "credential_id": own })),
        )
        .await;
    assert_eq!(
        repaired.status,
        StatusCode::OK,
        "the repair path must land: {}",
        repaired.body
    );
    assert_eq!(repaired.body["credential_id"], json!(own.to_string()));
}

/// The pre-deploy inventory query, read out of `docs/agent-platform.md` itself so the query
/// this test proves and the query an operator is handed cannot drift apart.
fn documented_inventory_query() -> String {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/agent-platform.md"
    ))
    .expect("docs/agent-platform.md is readable");
    let mut blocks: Vec<String> = Vec::new();
    let mut open: Option<String> = None;
    for line in doc.lines() {
        let Some(body) = open.as_mut() else {
            if line.trim_end() == "```sql" {
                open = Some(String::new());
            }
            continue;
        };
        if line.trim_end() == "```" {
            blocks.push(open.take().expect("an open block"));
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    assert_eq!(
        blocks.len(),
        1,
        "docs/agent-platform.md must carry exactly one ```sql block: the pre-deploy inventory \
         query this test executes. If you added a second one, teach this helper which block to \
         run rather than dropping the check — an inventory query nothing executes is how the \
         soft-deleted-provider case went unreported in the first place."
    );
    // `fetch_all` speaks the extended protocol, which takes one statement; the document ends
    // the query with a semicolon because an operator pastes it into `psql`.
    blocks
        .pop()
        .expect("one block")
        .trim()
        .trim_end_matches(';')
        .to_string()
}

impl Fixture {
    async fn provider_of(&self, credential_id: Uuid) -> Uuid {
        sqlx::query_scalar::<_, Uuid>("select provider_id from provider_credentials where id = $1")
            .bind(credential_id)
            .fetch_one(&self.pool)
            .await
            .expect("the credential's provider id")
    }

    /// Soft-deletes a provider through the real admin route, so the row ends in exactly the
    /// state a real `DELETE` leaves — `deleted_at` set, and its credentials untouched.
    async fn soft_delete_provider(&self, provider_id: Uuid) {
        let path = format!("/api/v1/admin/providers/{provider_id}");
        let current = self.request("GET", &path, None, None).await;
        assert_eq!(current.status, StatusCode::OK, "body: {}", current.body);
        let etag = current.etag.expect("GET must return an ETag");
        let deleted = self.request("DELETE", &path, Some(&etag), None).await;
        assert!(
            deleted.status.is_success(),
            "soft delete the provider: {} {}",
            deleted.status,
            deleted.body
        );
    }

    /// Writes a `providers.base_url` the admin write path would never store — it refuses
    /// userinfo outright and trims whitespace — because that is precisely the population the
    /// inventory query exists for: rows from a migration, a restore or a direct `psql` edit.
    async fn force_provider_base_url(&self, provider_id: Uuid, base_url: &str) {
        sqlx::query("update providers set base_url = $1 where id = $2")
            .bind(base_url)
            .bind(provider_id)
            .execute(&self.pool)
            .await
            .expect("overwrite the provider base_url");
    }

    /// Binds a credential to a skill's executor through the admin route, which enforces the
    /// entitlement rule — so anything bound this way was entitled at bind time.
    async fn bind_credential(&self, skill_id: Uuid, etag: &str, credential_id: Uuid) {
        let bound = self
            .request(
                "PATCH",
                &format!("/api/v1/admin/skills/{skill_id}/executor"),
                Some(etag),
                Some(json!({ "credential_id": credential_id })),
            )
            .await;
        assert_eq!(
            bound.status,
            StatusCode::OK,
            "bind the credential: {}",
            bound.body
        );
    }

    async fn resolve_skill_credential(
        &self,
        credential_id: Uuid,
        allowed_host: &str,
    ) -> SkillCredentialOutcome {
        PgAgentPlatformRepository::new(self.pool.clone())
            .resolve_skill_credential(&self.state.cipher, credential_id, allowed_host)
            .await
            .expect("resolving a skill credential is not an error")
    }
}

/// A public IPv6 literal, so an executor can be created at a bracketed host without DNS and
/// without tripping the SSRF address rules. Its only job is to make the inventory query's
/// host extraction meet the one form a `split_part(…, ':', 1)` cannot survive.
const PUBLIC_IPV6_TEST_HOST: &str = "[2001:4860:4860::8888]";

/// The pre-deploy inventory query is the **only** tool an operator has for finding the rows
/// this rule breaks, so it is executed here rather than trusted, against the four shapes it
/// has to classify. Two of them are the round-two defects:
///
/// 1. **A soft-deleted provider (`reason = 'provider_deleted'`).** The execution-time lookup
///    requires the owning `providers` row to be live, so the credential is refused — a
///    whole-execution failure, because `skill_refs` resolution is fail-closed. Its own row is
///    still active and unexpired, and its `base_url` still names the executor's host, so the
///    host comparison the query used to be cannot see it at all. Recovery already worked; the
///    row was simply undiscoverable, and that was the whole defect.
/// 2. **Two rows the code accepts and a string-based host extraction reports anyway** — a
///    `base_url` carrying userinfo and stray whitespace, and one carrying a bracketed IPv6
///    literal with a port. `credential_binding_permits_host` parses both with
///    `Url::host_str()` and permits them. A false positive is the safe direction for *finding*
///    rows, but it is not free here: repair 1 in the same document moves a live completion
///    endpoint, so sending an operator to a row that was never broken has a cost.
///
/// The fourth is the ordinary host mismatch the query already found, kept so a fix to the
/// other three cannot quietly cost the original coverage.
#[tokio::test]
async fn the_documented_inventory_query_finds_every_row_the_binding_rule_refuses() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    // 1. Entitled at bind time, then its provider is soft-deleted out from under it.
    let (deleted_skill, deleted_etag) = fixture
        .imported_executor(RESOLVABLE_TEST_HOST, "invdeleted")
        .await;
    let deleted_credential = fixture
        .credential_on_provider_at(RESOLVABLE_TEST_HOST, "invdeleted")
        .await;
    fixture
        .bind_credential(deleted_skill, &deleted_etag, deleted_credential)
        .await;
    let deleted_provider = fixture.provider_of(deleted_credential).await;
    fixture.soft_delete_provider(deleted_provider).await;

    // 2. Entitled, and stays entitled — but stored in a form a naive extraction mangles.
    let (userinfo_skill, userinfo_etag) = fixture
        .imported_executor(OTHER_PUBLIC_TEST_HOST, "invuserinfo")
        .await;
    let userinfo_credential = fixture
        .credential_on_provider_at(OTHER_PUBLIC_TEST_HOST, "invuserinfo")
        .await;
    fixture
        .bind_credential(userinfo_skill, &userinfo_etag, userinfo_credential)
        .await;
    fixture
        .force_provider_base_url(
            fixture.provider_of(userinfo_credential).await,
            &format!("  https://api.vendor.example@{OTHER_PUBLIC_TEST_HOST}/v1  "),
        )
        .await;

    // 3. The same, for a bracketed IPv6 literal with an explicit port.
    let (ipv6_skill, ipv6_etag) = fixture
        .imported_executor(PUBLIC_IPV6_TEST_HOST, "invipv6")
        .await;
    let ipv6_credential = fixture
        .credential_on_provider_at(PUBLIC_IPV6_TEST_HOST, "invipv6")
        .await;
    fixture
        .bind_credential(ipv6_skill, &ipv6_etag, ipv6_credential)
        .await;
    fixture
        .force_provider_base_url(
            fixture.provider_of(ipv6_credential).await,
            &format!("https://{PUBLIC_IPV6_TEST_HOST}:8443/v1"),
        )
        .await;

    // 4. The plain host mismatch — bound the way an upgrade inherits it, since no admin route
    //    accepts this binding today.
    let (mismatch_skill, _) = fixture
        .imported_executor(THIRD_PUBLIC_TEST_HOST, "invmismatch")
        .await;
    let mismatch_credential = fixture
        .credential_on_provider_at(OTHER_PUBLIC_TEST_HOST, "invmismatch")
        .await;
    sqlx::query("update skill_http_executors set credential_id = $1 where skill_id = $2")
        .bind(mismatch_credential)
        .bind(mismatch_skill)
        .execute(&fixture.pool)
        .await
        .expect("seed the pre-rule binding");

    // What the code itself does with each, so the query is being checked against behaviour
    // rather than against another reading of the same SQL.
    assert!(
        matches!(
            fixture
                .resolve_skill_credential(deleted_credential, RESOLVABLE_TEST_HOST)
                .await,
            SkillCredentialOutcome::ProviderDeleted
        ),
        "a live credential on a soft-deleted provider must resolve to ProviderDeleted, not to \
         the Unusable that told the operator it was 'missing, expired or revoked'"
    );
    assert!(
        matches!(
            fixture
                .resolve_skill_credential(userinfo_credential, OTHER_PUBLIC_TEST_HOST)
                .await,
            SkillCredentialOutcome::Resolved(_)
        ),
        "Url::host_str() puts a userinfo value in the userinfo, so this binding is entitled \
         and the inventory query must not claim otherwise"
    );
    assert!(
        matches!(
            fixture
                .resolve_skill_credential(ipv6_credential, PUBLIC_IPV6_TEST_HOST)
                .await,
            SkillCredentialOutcome::Resolved(_)
        ),
        "a bracketed IPv6 host with a port is entitled; only the port differs"
    );
    assert!(
        matches!(
            fixture
                .resolve_skill_credential(mismatch_credential, THIRD_PUBLIC_TEST_HOST)
                .await,
            SkillCredentialOutcome::HostNotEntitled
        ),
        "the plain mismatch must still be refused"
    );

    let rows = sqlx::query(&documented_inventory_query())
        .fetch_all(&fixture.pool)
        .await
        .expect("the documented inventory query must run as written");
    let reported: HashMap<Uuid, String> = rows
        .iter()
        .map(|row| {
            (
                row.get::<Uuid, _>("skill_id"),
                row.get::<String, _>("reason"),
            )
        })
        .collect();

    assert_eq!(
        reported.get(&deleted_skill).map(String::as_str),
        Some("provider_deleted"),
        "the one discovery tool an operator has must find the row whose provider was deleted; \
         its base_url still names the executor's host, so nothing else can. Reported: {reported:?}"
    );
    assert_eq!(
        reported.get(&mismatch_skill).map(String::as_str),
        Some("host_mismatch"),
        "the original coverage must survive the fix. Reported: {reported:?}"
    );
    assert!(
        !reported.contains_key(&userinfo_skill),
        "a base_url whose real host is the executor's must not be reported just because it \
         carries userinfo and whitespace — repair 1 moves a live completion endpoint, so a \
         false positive here is not free. Reported: {reported:?}"
    );
    assert!(
        !reported.contains_key(&ipv6_skill),
        "splitting a bracketed IPv6 authority on ':' yields '[' and reports every IPv6 \
         provider as broken. Reported: {reported:?}"
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
