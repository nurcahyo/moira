mod admin;
mod conversation;
mod health;
mod observability;
mod openapi;
mod public;

use std::{sync::Arc, time::Duration};

use axum::{Extension, Router, extract::DefaultBodyLimit, http::StatusCode};
use tower_http::timeout::TimeoutLayer;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app::AppState, config::Settings};

/// Fixed body limit for the conversation / memory / RAG surface.
///
/// Plan 03 (P1-3) deliberately does **not** introduce a new configuration field for
/// this surface: the value simply matches the effective default these routes already
/// had (`PublicApiSettings.maximum_request_bytes`'s 1 MiB default), so no legitimate
/// payload that works today can start failing. Making it configurable is an explicit
/// deferred follow-up.
pub const CONVERSATION_BODY_LIMIT_BYTES: usize = 1024 * 1024;

/// Body limit for the `/api/v1/admin/*` surface.
///
/// Admin payloads are structurally larger than public ones — routing policies, agent
/// profile prompts, provider runtime policies and RAG document bodies are all
/// operator-authored JSON documents rather than a single prompt — so the admin cap is
/// set above the public one. 2 MiB matches Axum's own built-in default body limit, so
/// this is a ceiling that already applied to every unlayered Axum route, not a novel
/// number. The largest admin request body in the existing test corpus is well under
/// 1 KiB (the largest generated payload anywhere in `tests/` is a 128-character
/// repeat), so this cannot regress a known-legitimate payload.
///
/// **Needs ops sign-off**: 2 MiB is a judgement call, not a measured requirement.
pub const ADMIN_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Headroom added on top of `RuntimeSettings.maximum_execution_timeout_seconds` when
/// sizing the HTTP-level timeout, so the HTTP deadline always sits *above* the
/// execution deadline and an execution timeout surfaces as its own specific error
/// rather than as a generic transport timeout.
///
/// **Needs product/ops sign-off**: 30 s is the plan's proposed buffer, not a derived
/// value.
pub const NON_STREAMING_TIMEOUT_BUFFER_SECONDS: u64 = 30;

/// Per-route-group request policy (plan 03 / P1-3).
///
/// Replaces the single global `DefaultBodyLimit::max(512 * 1024)` that used to be
/// applied in `build_router` and that was disconnected from
/// `PublicApiSettings.maximum_request_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterPolicy {
    /// Applied to `/api/v1/responses*` and `/v1/responses`; sourced from
    /// `PublicApiSettings.maximum_request_bytes`.
    pub public_body_limit_bytes: usize,
    /// Applied to the conversation / memory / RAG surface.
    pub conversation_body_limit_bytes: usize,
    /// Applied to `/api/v1/admin/*`.
    pub admin_body_limit_bytes: usize,
    /// Applied to every non-SSE route group. SSE routes are exempt by construction.
    pub non_streaming_timeout: Duration,
}

impl RouterPolicy {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            public_body_limit_bytes: usize::try_from(settings.public_api.maximum_request_bytes)
                .ok()
                .filter(|bytes| *bytes > 0)
                .unwrap_or(CONVERSATION_BODY_LIMIT_BYTES),
            conversation_body_limit_bytes: CONVERSATION_BODY_LIMIT_BYTES,
            admin_body_limit_bytes: ADMIN_BODY_LIMIT_BYTES,
            non_streaming_timeout: Duration::from_secs(
                settings
                    .runtime
                    .maximum_execution_timeout_seconds
                    .saturating_add(NON_STREAMING_TIMEOUT_BUFFER_SECONDS),
            ),
        }
    }
}

impl Default for RouterPolicy {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

pub fn router(policy: RouterPolicy) -> Router<AppState> {
    let (router, openapi) = documented_router(policy).split_for_parts();
    #[cfg(feature = "test-routes")]
    let router = router.merge(middleware_probe_routes());
    router.layer(Extension(Arc::new(openapi)))
}

/// The timeout applied to the `test-routes` slow probe.
///
/// Deliberately tiny and deliberately *not* [`RouterPolicy::non_streaming_timeout`]: the
/// production value is `maximum_execution_timeout_seconds + 30`, i.e. never below 30 s,
/// and it always sits above every execution deadline by construction — so no production
/// route can be made to reach it inside a test. The production value and its placement
/// are asserted by `the_non_streaming_timeout_sits_above_the_execution_deadline`
/// (`src/lib.rs`); this probe exists only so an integration test can observe what the
/// *envelope mapper* does to a real `TimeoutLayer` rejection over a real socket.
#[cfg(feature = "test-routes")]
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Undocumented probe routes, compiled only under the off-by-default `test-routes`
/// feature (plan 03 / P1-3, Wave 3).
///
/// They are merged **after** `split_for_parts`, so they contribute nothing to the
/// OpenAPI document and cannot perturb the zero-diff spec requirement. `cargo build
/// --release --locked` does not enable the feature, so they cannot exist in a shipped
/// binary.
///
/// Why they are needed: `tests/*.rs` links `moira` compiled *without* `cfg(test)`, so a
/// `#[cfg(test)]` route is unreachable from an integration test, and neither
/// `CatchPanicLayer` nor the 504 branch of `normalize_infrastructure_error` is
/// reachable from any production route inside a test (nothing panics on demand, and the
/// production HTTP timeout is always above the execution deadline).
#[cfg(feature = "test-routes")]
fn middleware_probe_routes() -> Router<AppState> {
    use axum::routing::get;

    Router::new()
        .route("/internal/test/panic", get(probe_panic))
        .merge(
            Router::new()
                .route("/internal/test/slow", get(probe_slow))
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::GATEWAY_TIMEOUT,
                    PROBE_TIMEOUT,
                )),
        )
}

/// Panics with a payload built from fragments the test asserts never reach the client.
#[cfg(feature = "test-routes")]
async fn probe_panic() -> StatusCode {
    panic!("moira probe panic: credential row 42 pepper v1 unwrap on None");
}

/// Never completes, so the probe timeout above always wins. No `sleep`.
#[cfg(feature = "test-routes")]
async fn probe_slow() -> StatusCode {
    std::future::pending::<()>().await;
    StatusCode::OK
}

fn documented_router(policy: RouterPolicy) -> OpenApiRouter<AppState> {
    // `TimeoutLayer` here only bounds the time taken to *produce* a response head; it
    // never bounds an already-returned streaming body. The SSE group is nevertheless
    // left entirely unlayered so that no future change to this stack can accidentally
    // cap a long-lived stream.
    let timeout =
        TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, policy.non_streaming_timeout);

    let mut router = OpenApiRouter::with_openapi(openapi::MoiraApiDoc::openapi())
        .merge(operational_routes().layer(timeout))
        .merge(
            public_execution_routes()
                .layer(DefaultBodyLimit::max(policy.public_body_limit_bytes))
                .layer(timeout),
        )
        // No `TimeoutLayer`: SSE connections are long-lived by design.
        .merge(streaming_routes().layer(DefaultBodyLimit::max(policy.public_body_limit_bytes)))
        .merge(
            conversation_routes()
                .layer(DefaultBodyLimit::max(policy.conversation_body_limit_bytes))
                .layer(timeout),
        )
        .merge(
            admin_routes()
                .layer(DefaultBodyLimit::max(policy.admin_body_limit_bytes))
                .layer(timeout),
        );
    openapi::finalize_document(router.get_openapi_mut());
    router
}

fn operational_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health::healthz))
        .routes(routes!(health::readyz))
        .routes(routes!(observability::metrics))
        .routes(routes!(openapi::openapi_json))
        .routes(routes!(openapi::docs))
}

fn public_execution_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(public::create_response))
        .routes(routes!(public::get_response))
        .routes(routes!(public::list_executions))
        .routes(routes!(public::get_execution))
        .routes(routes!(public::list_usage))
        .routes(routes!(public::list_models))
        .routes(routes!(public::list_routes))
        .routes(routes!(public::capabilities))
}

/// Every route that can emit `text/event-stream`.
///
/// `/api/v1/responses/stream` always streams; `/v1/responses` streams whenever the
/// caller sets `stream: true` (`src/http/public.rs` — the OpenAI compatibility route
/// documents both `application/json` and `text/event-stream` for its 200 response).
fn streaming_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(public::stream_response))
        .routes(routes!(public::openai_responses_compat))
}

fn conversation_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            conversation::list_conversations,
            conversation::create_conversation
        ))
        .routes(routes!(
            conversation::get_conversation,
            conversation::patch_conversation,
            conversation::delete_conversation
        ))
        .routes(routes!(conversation::archive_conversation))
        .routes(routes!(conversation::restore_conversation))
        .routes(routes!(
            conversation::list_messages,
            conversation::create_message
        ))
        .routes(routes!(
            conversation::list_memories,
            conversation::create_memory
        ))
        .routes(routes!(
            conversation::get_memory,
            conversation::patch_memory,
            conversation::delete_memory
        ))
        .routes(routes!(
            conversation::get_conversation_policy,
            conversation::put_conversation_policy
        ))
        .routes(routes!(
            conversation::get_memory_policy,
            conversation::put_memory_policy
        ))
        .routes(routes!(
            conversation::get_retrieval_policy,
            conversation::put_retrieval_policy
        ))
        .routes(routes!(
            conversation::get_embedding_policy,
            conversation::put_embedding_policy
        ))
        .routes(routes!(
            conversation::list_rag_collections,
            conversation::create_rag_collection
        ))
        .routes(routes!(
            conversation::get_rag_collection,
            conversation::patch_rag_collection,
            conversation::delete_rag_collection
        ))
        .routes(routes!(conversation::enable_rag_collection))
        .routes(routes!(conversation::disable_rag_collection))
        .routes(routes!(
            conversation::list_rag_documents,
            conversation::create_rag_document
        ))
        .routes(routes!(
            conversation::get_rag_document,
            conversation::delete_rag_document
        ))
        .routes(routes!(conversation::ingest_rag_document))
        .routes(routes!(conversation::reindex_rag_document))
}

fn admin_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(admin::get_setup_status))
        .routes(routes!(admin::list_applications, admin::create_application))
        .routes(routes!(
            admin::get_application,
            admin::patch_application,
            admin::delete_application
        ))
        .routes(routes!(admin::enable_application))
        .routes(routes!(admin::disable_application))
        .routes(routes!(
            admin::get_application_execution_policy,
            admin::put_application_execution_policy
        ))
        .routes(routes!(admin::list_providers, admin::create_provider))
        .routes(routes!(
            admin::get_provider,
            admin::patch_provider,
            admin::delete_provider
        ))
        .routes(routes!(admin::enable_provider))
        .routes(routes!(admin::disable_provider))
        .routes(routes!(
            admin::get_provider_runtime_policy,
            admin::put_provider_runtime_policy
        ))
        .routes(routes!(
            admin::list_provider_models,
            admin::create_provider_model
        ))
        .routes(routes!(
            admin::list_route_definitions,
            admin::create_route_definition
        ))
        .routes(routes!(
            admin::get_route_definition,
            admin::patch_route_definition,
            admin::delete_route_definition
        ))
        .routes(routes!(admin::enable_route_definition))
        .routes(routes!(admin::disable_route_definition))
        .routes(routes!(
            admin::list_routing_policies,
            admin::create_routing_policy
        ))
        .routes(routes!(
            admin::get_routing_policy,
            admin::patch_routing_policy,
            admin::delete_routing_policy
        ))
        .routes(routes!(admin::enable_routing_policy))
        .routes(routes!(admin::disable_routing_policy))
        .routes(routes!(
            admin::list_agent_profiles,
            admin::create_agent_profile
        ))
        .routes(routes!(
            admin::get_agent_profile,
            admin::patch_agent_profile,
            admin::delete_agent_profile
        ))
        .routes(routes!(admin::enable_agent_profile))
        .routes(routes!(admin::disable_agent_profile))
        .routes(routes!(admin::diagnose_runtime))
        .routes(routes!(
            admin::get_provider_model,
            admin::patch_provider_model,
            admin::delete_provider_model
        ))
        .routes(routes!(admin::enable_provider_model))
        .routes(routes!(admin::disable_provider_model))
        .routes(routes!(admin::list_credentials, admin::create_credential))
        .routes(routes!(
            admin::get_credential,
            admin::patch_credential,
            admin::delete_credential
        ))
        .routes(routes!(admin::rotate_credential))
        .routes(routes!(admin::enable_credential))
        .routes(routes!(admin::disable_credential))
        .routes(routes!(
            admin::upsert_user_credential,
            admin::delete_user_credential
        ))
        .routes(routes!(admin::list_user_credentials))
        .routes(routes!(admin::list_system_keys, admin::create_system_key))
        .routes(routes!(admin::get_system_key, admin::delete_system_key))
        .routes(routes!(admin::rotate_system_key))
        .routes(routes!(admin::revoke_system_key))
        .routes(routes!(
            admin::list_consumer_keys,
            admin::create_consumer_key
        ))
        .routes(routes!(admin::get_consumer_key, admin::delete_consumer_key))
        .routes(routes!(admin::rotate_consumer_key))
        .routes(routes!(admin::revoke_consumer_key))
        .routes(routes!(
            admin::list_trusted_jwt_issuers,
            admin::create_trusted_jwt_issuer
        ))
        .routes(routes!(
            admin::get_trusted_jwt_issuer,
            admin::patch_trusted_jwt_issuer,
            admin::delete_trusted_jwt_issuer
        ))
        .routes(routes!(admin::refresh_trusted_jwt_issuer))
        .routes(routes!(admin::enable_trusted_jwt_issuer))
        .routes(routes!(admin::disable_trusted_jwt_issuer))
        .routes(routes!(admin::list_audit_events))
        .routes(routes!(admin::get_audit_event))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use serde_json::Value;

    use super::*;

    const HTTP_METHODS: [&str; 8] = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];

    #[test]
    fn generated_openapi_covers_every_registered_route() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let paths = value["paths"].as_object().expect("OpenAPI paths");
        let expected: BTreeSet<_> = [
            "/health/live",
            "/health/ready",
            "/metrics",
            "/openapi.json",
            "/docs",
            "/api/v1/responses",
            "/api/v1/responses/stream",
            "/api/v1/responses/{response_id}",
            "/api/v1/executions",
            "/api/v1/executions/{execution_id}",
            "/api/v1/usage",
            "/api/v1/models",
            "/api/v1/routes",
            "/api/v1/capabilities",
            "/api/v1/conversations",
            "/api/v1/conversations/{id}",
            "/api/v1/conversations/{id}/archive",
            "/api/v1/conversations/{id}/restore",
            "/api/v1/conversations/{id}/messages",
            "/api/v1/memories",
            "/api/v1/memories/{id}",
            "/v1/responses",
            "/api/v1/admin/setup/status",
            "/api/v1/admin/applications",
            "/api/v1/admin/applications/{id}",
            "/api/v1/admin/applications/{id}/enable",
            "/api/v1/admin/applications/{id}/disable",
            "/api/v1/admin/applications/{id}/execution-policy",
            "/api/v1/admin/applications/{application_id}/conversation-policy",
            "/api/v1/admin/applications/{application_id}/memory-policy",
            "/api/v1/admin/applications/{application_id}/retrieval-policy",
            "/api/v1/admin/applications/{application_id}/embedding-policy",
            "/api/v1/admin/providers",
            "/api/v1/admin/providers/{id}",
            "/api/v1/admin/providers/{id}/enable",
            "/api/v1/admin/providers/{id}/disable",
            "/api/v1/admin/providers/{provider_id}/runtime-policy",
            "/api/v1/admin/providers/{provider_id}/models",
            "/api/v1/admin/routes",
            "/api/v1/admin/routes/{id}",
            "/api/v1/admin/routes/{id}/enable",
            "/api/v1/admin/routes/{id}/disable",
            "/api/v1/admin/routing-policies",
            "/api/v1/admin/routing-policies/{id}",
            "/api/v1/admin/routing-policies/{id}/enable",
            "/api/v1/admin/routing-policies/{id}/disable",
            "/api/v1/admin/agent-profiles",
            "/api/v1/admin/agent-profiles/{id}",
            "/api/v1/admin/agent-profiles/{id}/enable",
            "/api/v1/admin/agent-profiles/{id}/disable",
            "/api/v1/admin/runtime/diagnose",
            "/api/v1/admin/rag-collections",
            "/api/v1/admin/rag-collections/{id}",
            "/api/v1/admin/rag-collections/{id}/enable",
            "/api/v1/admin/rag-collections/{id}/disable",
            "/api/v1/admin/rag-collections/{collection_id}/documents",
            "/api/v1/admin/rag-documents/{id}",
            "/api/v1/admin/rag-documents/{id}/ingest",
            "/api/v1/admin/rag-documents/{id}/reindex",
            "/api/v1/admin/provider-models/{id}",
            "/api/v1/admin/provider-models/{id}/enable",
            "/api/v1/admin/provider-models/{id}/disable",
            "/api/v1/admin/provider-credentials",
            "/api/v1/admin/provider-credentials/{id}",
            "/api/v1/admin/provider-credentials/{id}/rotate",
            "/api/v1/admin/provider-credentials/{id}/enable",
            "/api/v1/admin/provider-credentials/{id}/disable",
            "/api/v1/admin/users/{external_user_id}/provider-credentials/{id}",
            "/api/v1/admin/users/{external_user_id}/provider-credentials",
            "/api/v1/admin/system-keys",
            "/api/v1/admin/system-keys/{id}",
            "/api/v1/admin/system-keys/{id}/rotate",
            "/api/v1/admin/system-keys/{id}/revoke",
            "/api/v1/admin/consumer-keys",
            "/api/v1/admin/consumer-keys/{id}",
            "/api/v1/admin/consumer-keys/{id}/rotate",
            "/api/v1/admin/consumer-keys/{id}/revoke",
            "/api/v1/admin/jwt-issuers",
            "/api/v1/admin/jwt-issuers/{id}",
            "/api/v1/admin/jwt-issuers/{id}/refresh-jwks",
            "/api/v1/admin/jwt-issuers/{id}/enable",
            "/api/v1/admin/jwt-issuers/{id}/disable",
            "/api/v1/admin/audit-events",
            "/api/v1/admin/audit-events/{id}",
        ]
        .into_iter()
        .collect();
        let actual: BTreeSet<_> = paths.keys().map(String::as_str).collect();
        assert_eq!(actual, expected);

        let mut operation_ids = HashSet::new();
        let mut operation_count = 0;
        for item in paths.values() {
            for method in HTTP_METHODS {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                operation_count += 1;
                let operation_id = operation["operationId"].as_str().expect("operation id");
                assert!(
                    operation_ids.insert(operation_id),
                    "duplicate operation id: {operation_id}"
                );
            }
        }
        assert_eq!(operation_count, 131);
    }

    #[test]
    fn public_document_filters_admin_paths_and_keeps_operational_paths() {
        let document =
            openapi::public_document(documented_router(RouterPolicy::default()).into_openapi());
        assert!(
            document
                .paths
                .paths
                .keys()
                .all(|path| !path.starts_with("/api/v1/admin/"))
        );
        for path in [
            "/health/live",
            "/metrics",
            "/openapi.json",
            "/docs",
            "/api/v1/responses",
            "/api/v1/conversations",
            "/api/v1/memories",
            "/v1/responses",
        ] {
            assert!(document.paths.paths.contains_key(path), "missing {path}");
        }
    }

    #[test]
    fn generated_openapi_contains_security_content_types_and_parameters() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        assert_eq!(value["openapi"], "3.1.0");

        let schemes = value["components"]["securitySchemes"]
            .as_object()
            .expect("security schemes");
        for name in ["bearerAuth", "systemKeyAuth", "consumerKeyAuth"] {
            assert!(schemes.contains_key(name), "missing {name}");
        }

        assert!(value["paths"]["/api/v1/responses/stream"]["post"]["responses"]["200"]["content"]
            ["text/event-stream"]
            .is_object());
        assert!(
            value["paths"]["/metrics"]["get"]["responses"]["200"]["content"]["text/plain"]
                .is_object()
        );
        assert!(parameter_named(
            &value["paths"]["/api/v1/executions"]["get"],
            "limit"
        ));
        assert!(parameter_named(
            &value["paths"]["/api/v1/admin/applications/{id}"]["patch"],
            "If-Match"
        ));
        assert!(parameter_named(
            &value["paths"]["/api/v1/responses"]["post"],
            "Idempotency-Key"
        ));
        assert!(
            value["paths"]["/api/v1/admin/applications"]["post"]["responses"]
                .as_object()
                .unwrap()
                .contains_key("201")
        );
        assert!(
            value["paths"]["/api/v1/admin/applications/{id}"]["delete"]["responses"]
                .as_object()
                .unwrap()
                .contains_key("204")
        );

        let upsert = &value["paths"]["/api/v1/admin/users/{external_user_id}/provider-credentials/{id}"]
            ["put"];
        assert!(upsert["responses"].as_object().unwrap().contains_key("201"));
        assert!(!parameter_named(upsert, "If-Match"));
        assert_eq!(
            upsert["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == "id")
                .unwrap()["description"],
            "Provider identifier"
        );
    }

    #[test]
    fn every_operation_documents_request_ids_and_protected_operations_document_auth() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let paths = value["paths"].as_object().expect("OpenAPI paths");
        let public_operations = [
            ("/health/live", "get"),
            ("/health/ready", "get"),
            ("/metrics", "get"),
            ("/openapi.json", "get"),
            ("/docs", "get"),
        ];

        for (path, item) in paths {
            for method in HTTP_METHODS {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                assert!(
                    parameter_named(operation, "X-Request-Id"),
                    "{method} {path} is missing the X-Request-Id parameter"
                );
                for (status, response) in operation["responses"]
                    .as_object()
                    .expect("operation responses")
                {
                    assert!(
                        response["headers"]["X-Request-Id"].is_object(),
                        "{method} {path} response {status} is missing X-Request-Id"
                    );
                }

                if !public_operations.contains(&(path.as_str(), method)) {
                    let security = operation["security"]
                        .as_array()
                        .expect("protected operation security");
                    let alternatives: BTreeSet<_> = security
                        .iter()
                        .flat_map(|requirement| {
                            requirement
                                .as_object()
                                .into_iter()
                                .flat_map(|requirement| requirement.keys().map(String::as_str))
                        })
                        .collect();
                    let expected = if path == "/api/v1/admin/setup/status" {
                        BTreeSet::from(["bearerAuth", "systemKeyAuth"])
                    } else {
                        BTreeSet::from(["bearerAuth", "consumerKeyAuth", "systemKeyAuth"])
                    };
                    assert_eq!(
                        alternatives, expected,
                        "unexpected security alternatives for {method} {path}"
                    );
                }
            }
        }
    }

    #[test]
    fn setup_status_contract_is_typed_and_exact() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operation = &value["paths"]["/api/v1/admin/setup/status"]["get"];
        assert_eq!(operation["operationId"], "get_setup_status");
        let responses = operation["responses"].as_object().unwrap();
        assert_eq!(
            responses
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["200", "401", "403", "500", "503"])
        );
        assert_eq!(
            responses["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SetupStatusResponse"
        );

        for schema in [
            "SetupStatus",
            "SetupDeploymentEnvironment",
            "SetupCheckState",
            "SetupCheckName",
            "SetupChecks",
            "SetupStatusResponse",
        ] {
            assert!(
                value["components"]["schemas"][schema].is_object(),
                "missing setup schema {schema}"
            );
        }
    }

    #[test]
    fn once_only_key_responses_use_the_secret_envelope() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method, status) in [
            ("/api/v1/admin/system-keys", "post", "201"),
            ("/api/v1/admin/system-keys/{id}/rotate", "post", "200"),
            ("/api/v1/admin/consumer-keys", "post", "201"),
            ("/api/v1/admin/consumer-keys/{id}/rotate", "post", "200"),
        ] {
            assert_eq!(
                value["paths"][path][method]["responses"][status]["content"]["application/json"]["schema"]
                    ["$ref"],
                "#/components/schemas/ApiKeySecretResponse",
                "unexpected once-only key response for {method} {path}"
            );
        }

        let secret_schema = &value["components"]["schemas"]["ApiKeySecretResponse"];
        assert!(secret_schema["properties"]["secret"].is_object());
        assert!(secret_schema["properties"]["secret_retrievable"].is_object());
    }

    #[test]
    fn atomic_admin_idempotency_contract_is_explicit() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operations = [
            ("/api/v1/admin/applications", "post", "201", false, false),
            ("/api/v1/admin/providers", "post", "201", false, false),
            (
                "/api/v1/admin/providers/{provider_id}/models",
                "post",
                "201",
                false,
                false,
            ),
            (
                "/api/v1/admin/provider-credentials",
                "post",
                "201",
                false,
                false,
            ),
            (
                "/api/v1/admin/provider-credentials/{id}/rotate",
                "post",
                "200",
                true,
                false,
            ),
            ("/api/v1/admin/system-keys", "post", "201", false, true),
            (
                "/api/v1/admin/system-keys/{id}/rotate",
                "post",
                "200",
                false,
                true,
            ),
            ("/api/v1/admin/consumer-keys", "post", "201", false, true),
            (
                "/api/v1/admin/consumer-keys/{id}/rotate",
                "post",
                "200",
                false,
                true,
            ),
            ("/api/v1/admin/jwt-issuers", "post", "201", false, false),
        ];

        for (path, method, success_status, requires_if_match, once_only_secret) in operations {
            let operation = &value["paths"][path][method];
            assert!(
                parameter_named(operation, "Idempotency-Key"),
                "{method} {path} is missing Idempotency-Key"
            );
            assert_eq!(
                parameter_required(operation, "If-Match"),
                requires_if_match,
                "unexpected If-Match contract for {method} {path}"
            );

            let responses = operation["responses"]
                .as_object()
                .expect("operation responses");
            assert!(
                responses.contains_key(success_status),
                "{method} {path} is missing success status {success_status}"
            );
            assert!(
                responses.contains_key("409"),
                "{method} {path} must explicitly document idempotency conflicts"
            );
            assert_eq!(
                responses["409"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorResponse",
                "{method} {path} must use ErrorResponse for idempotency conflicts"
            );
            let conflict_description = responses["409"]["description"]
                .as_str()
                .expect("409 description");
            for code in ["idempotency_conflict", "idempotency_in_progress"] {
                assert!(
                    conflict_description.contains(code),
                    "{method} {path} 409 description is missing {code}"
                );
            }

            if once_only_secret {
                let success_description = responses[success_status]["description"]
                    .as_str()
                    .expect("success description");
                assert!(
                    success_description.contains("replay")
                        && success_description.contains("secret_retrievable"),
                    "{method} {path} must document sanitized secret replays"
                );
            }
        }

        let error_schema = &value["components"]["schemas"]["ErrorResponse"];
        assert!(
            error_schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "error"))
        );
        let error_detail_schema = &value["components"]["schemas"]["ErrorDetail"];
        assert!(
            error_detail_schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "request_id")),
            "replayed deterministic errors must carry the current request ID"
        );
    }

    #[test]
    fn every_local_schema_reference_resolves() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let schemas = value["components"]["schemas"]
            .as_object()
            .expect("component schemas");
        assert_refs_resolve(&value, schemas);
    }

    /// The four RAG write operations that carry `Idempotency-Key` and now implement
    /// real replay (`plans/02b-idempotency-replay.md`). 02a's interim disclaimer
    /// (Sentence B) used to live on these operations too; it is gone as of 02b.
    const RAG_WRITE_OPERATIONS: [(&str, &str); 4] = [
        ("/api/v1/admin/rag-collections", "post"),
        (
            "/api/v1/admin/rag-collections/{collection_id}/documents",
            "post",
        ),
        ("/api/v1/admin/rag-documents/{id}/ingest", "post"),
        ("/api/v1/admin/rag-documents/{id}/reindex", "post"),
    ];

    /// The seven conversation/memory/policy operations that carry only the short
    /// Sentence A (no `Idempotency-Key`, no Sentence B).
    const OTHER_PREVIEW_OPERATIONS: [(&str, &str); 7] = [
        ("/api/v1/conversations", "post"),
        ("/api/v1/conversations/{id}/messages", "post"),
        ("/api/v1/memories", "post"),
        (
            "/api/v1/admin/applications/{application_id}/conversation-policy",
            "put",
        ),
        (
            "/api/v1/admin/applications/{application_id}/memory-policy",
            "put",
        ),
        (
            "/api/v1/admin/applications/{application_id}/retrieval-policy",
            "put",
        ),
        (
            "/api/v1/admin/applications/{application_id}/embedding-policy",
            "put",
        ),
    ];

    const SENTENCE_A_RAG_WRITE: &str = "Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md.";
    const SENTENCE_A_SHORT: &str = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.";
    const SENTENCE_B_INTERIM_IDEMPOTENCY: &str = "Idempotency-Key is accepted but replay is not implemented yet; retrying can duplicate side effects.";
    /// The truthful `Idempotency-Key` parameter description 02b installs on all four
    /// RAG write operations, replacing 02a's "not implemented yet" text
    /// (`plans/02b-idempotency-replay.md`, Architecture -> "API & OpenAPI changes").
    const IDEMPOTENCY_KEY_PARAMETER_DESCRIPTION: &str = "Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409.";

    #[test]
    fn rag_write_routes_still_declare_the_idempotency_key_parameter() {
        // Guard against a stale "remove the parameter" instinct: CONVENTIONS.md §0
        // decision D1 settled that P0-2 is fixed by implementing real replay (02b),
        // not by removing Idempotency-Key from these routes.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            assert!(
                parameter_named(operation, "Idempotency-Key"),
                "{method} {path} must still declare Idempotency-Key"
            );
            assert_eq!(
                parameter_description(operation, "Idempotency-Key"),
                Some(IDEMPOTENCY_KEY_PARAMETER_DESCRIPTION),
                "{method} {path} Idempotency-Key description drifted"
            );
        }
    }

    #[test]
    fn rag_write_routes_declare_the_idempotency_replay_contract() {
        // A sibling to `atomic_admin_idempotency_contract_is_explicit`, kept separate
        // because these four routes are not admin-command routes in the operations
        // list that test enumerates.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            assert!(
                parameter_named(operation, "Idempotency-Key"),
                "{method} {path} must declare Idempotency-Key"
            );
            let description =
                parameter_description(operation, "Idempotency-Key").unwrap_or_else(|| {
                    panic!("{method} {path} is missing the Idempotency-Key parameter")
                });
            assert!(
                !description.to_lowercase().contains("not implemented"),
                "{method} {path} Idempotency-Key description still disclaims replay: {description}"
            );

            let responses = operation["responses"]
                .as_object()
                .expect("operation responses");
            assert!(
                responses.contains_key("409"),
                "{method} {path} must explicitly document a 409 idempotency response"
            );
            assert_eq!(
                responses["409"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorResponse",
                "{method} {path} must use ErrorResponse for its 409 response"
            );
        }
    }

    #[test]
    fn rag_write_route_descriptions_no_longer_disclaim_idempotency() {
        // The paired half of 02a's `rag_write_routes_carry_the_interim_idempotency_disclaimer`,
        // which 02b deletes: Sentence B is gone, Sentence A survives verbatim.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            let description = operation["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} is missing an operation description"));
            assert!(
                !description.contains(SENTENCE_B_INTERIM_IDEMPOTENCY),
                "{method} {path} still carries 02a's interim idempotency disclaimer"
            );
            assert!(
                description.contains(SENTENCE_A_RAG_WRITE),
                "{method} {path} must still carry Sentence A verbatim"
            );
        }
    }

    #[test]
    fn rag_collection_document_route_keeps_its_collection_id_path_parameter() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operation =
            &value["paths"]["/api/v1/admin/rag-collections/{collection_id}/documents"]["post"];
        let parameter = operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "collection_id")
            .expect("collection_id path parameter must survive");
        assert_eq!(parameter["in"], "path");
        assert_eq!(parameter["required"], true);
        assert_eq!(parameter["description"], "RAG collection identifier");
    }

    #[test]
    fn conversation_memory_rag_operations_document_the_mvp_preview_boundary() {
        // Assert Sentence A only (the permanent boundary text). Sentence B is
        // deliberately checked by a separate test that plan 02b deletes.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            let description = operation["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} is missing an operation description"));
            assert!(
                !description.is_empty(),
                "{method} {path} description must not be empty"
            );
            assert!(
                description.contains("used to influence model responses"),
                "{method} {path} description is missing the invariant phrase"
            );
            assert!(
                description.contains(SENTENCE_A_RAG_WRITE),
                "{method} {path} description is missing Sentence A verbatim"
            );
        }
        for (path, method) in OTHER_PREVIEW_OPERATIONS {
            let operation = &value["paths"][path][method];
            let description = operation["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} is missing an operation description"));
            assert!(
                !description.is_empty(),
                "{method} {path} description must not be empty"
            );
            assert!(
                description.contains("used to influence model responses"),
                "{method} {path} description is missing the invariant phrase"
            );
            assert!(
                description.contains(SENTENCE_A_SHORT),
                "{method} {path} description is missing the short Sentence A verbatim"
            );
        }
    }

    #[test]
    fn rag_document_record_schema_exposes_ingestion_status() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let property =
            &value["components"]["schemas"]["RagDocumentRecord"]["properties"]["ingestion_status"];
        assert!(
            property.is_object(),
            "RagDocumentRecord.ingestion_status schema is missing"
        );
        // utoipa represents Option<RagIngestionStatus> as a `oneOf` of `null` and a
        // `$ref` to the enum schema rather than a bare `$ref`.
        let one_of = property["oneOf"]
            .as_array()
            .expect("ingestion_status must be a oneOf wrapping the optional enum");
        assert!(
            one_of.iter().any(|variant| variant["type"] == "null"),
            "ingestion_status oneOf must allow null"
        );
        assert!(
            one_of
                .iter()
                .any(|variant| variant["$ref"] == "#/components/schemas/RagIngestionStatus"),
            "ingestion_status oneOf must reference RagIngestionStatus"
        );

        let enum_schema = &value["components"]["schemas"]["RagIngestionStatus"];
        let variants: BTreeSet<_> = enum_schema["enum"]
            .as_array()
            .expect("RagIngestionStatus must enumerate its variants")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            variants,
            BTreeSet::from([
                "pending",
                "downloading",
                "parsing",
                "chunking",
                "embedding",
                "indexed",
                "failed",
                "superseded",
            ])
        );
    }

    #[test]
    fn public_response_schema_documents_always_empty_citations() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let description = value["components"]["schemas"]["PublicResponse"]["properties"]
            ["citations"]["description"]
            .as_str()
            .expect("PublicResponse.citations must carry a description");
        assert!(!description.is_empty());
        assert!(
            description.to_lowercase().contains("empty"),
            "citations description must state that the array is always empty: {description}"
        );
        assert!(
            description.to_lowercase().contains("not wired"),
            "citations description must state that retrieval is not wired: {description}"
        );
    }

    fn parameter_named(operation: &Value, name: &str) -> bool {
        operation["parameters"]
            .as_array()
            .is_some_and(|parameters| parameters.iter().any(|parameter| parameter["name"] == name))
    }

    fn parameter_description<'a>(operation: &'a Value, name: &str) -> Option<&'a str> {
        operation["parameters"].as_array().and_then(|parameters| {
            parameters
                .iter()
                .find(|parameter| parameter["name"] == name)
                .and_then(|parameter| parameter["description"].as_str())
        })
    }

    fn parameter_required(operation: &Value, name: &str) -> bool {
        operation["parameters"]
            .as_array()
            .is_some_and(|parameters| {
                parameters
                    .iter()
                    .find(|parameter| parameter["name"] == name)
                    .is_some_and(|parameter| parameter["required"] == true)
            })
    }

    fn assert_refs_resolve(value: &Value, schemas: &serde_json::Map<String, Value>) {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                    && let Some(name) = reference.strip_prefix("#/components/schemas/")
                {
                    assert!(
                        schemas.contains_key(name),
                        "unresolved schema ref: {reference}"
                    );
                }
                for nested in object.values() {
                    assert_refs_resolve(nested, schemas);
                }
            }
            Value::Array(values) => {
                for nested in values {
                    assert_refs_resolve(nested, schemas);
                }
            }
            _ => {}
        }
    }
}
