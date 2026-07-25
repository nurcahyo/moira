mod admin;
mod conversation;
mod health;
mod observability;
mod openapi;
mod public;

use std::sync::Arc;

use axum::{Extension, Router};
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    let (router, openapi) = documented_router().split_for_parts();
    router.layer(Extension(Arc::new(openapi)))
}

fn documented_router() -> OpenApiRouter<AppState> {
    let mut router = OpenApiRouter::with_openapi(openapi::MoiraApiDoc::openapi())
        .routes(routes!(health::healthz))
        .routes(routes!(health::readyz))
        .routes(routes!(observability::metrics))
        .routes(routes!(openapi::openapi_json))
        .routes(routes!(openapi::docs))
        .routes(routes!(public::create_response))
        .routes(routes!(public::stream_response))
        .routes(routes!(public::get_response))
        .routes(routes!(public::list_executions))
        .routes(routes!(public::get_execution))
        .routes(routes!(public::list_usage))
        .routes(routes!(public::list_models))
        .routes(routes!(public::list_routes))
        .routes(routes!(public::capabilities))
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
        .routes(routes!(public::openai_responses_compat))
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
        .routes(routes!(admin::get_audit_event));
    openapi::finalize_document(router.get_openapi_mut());
    router
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let document = openapi::public_document(documented_router().into_openapi());
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
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
