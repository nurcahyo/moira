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
    fn every_local_schema_reference_resolves() {
        let value = serde_json::to_value(documented_router().into_openapi()).unwrap();
        let schemas = value["components"]["schemas"]
            .as_object()
            .expect("component schemas");
        assert_refs_resolve(&value, schemas);
    }

    fn parameter_named(operation: &Value, name: &str) -> bool {
        operation["parameters"]
            .as_array()
            .is_some_and(|parameters| parameters.iter().any(|parameter| parameter["name"] == name))
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
