use std::sync::Arc;

use axum::{Extension, Json, extract::State, http::HeaderMap, response::Html};
use serde_json::Value;
use utoipa::{
    Modify, OpenApi,
    openapi::{
        OpenApi as OpenApiDocument, RefOr, Required,
        header::Header,
        path::{Operation, ParameterBuilder, ParameterIn},
        schema::{Object, Type},
        security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

use crate::{
    app::AppState,
    domain::{
        AuditResult, ConversationStatus, CredentialType, MemoryStatus, MemoryType,
        RagCollectionStatus, ResponseText, ScopeType,
    },
    error::{AppError, ErrorDetail, ErrorResponse},
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Moira API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Moira public responses, conversations, memory, retrieval, RAG, discovery, usage, observability, and administrative runtime configuration API."
    ),
    components(schemas(
        AuditResult,
        ConversationStatus,
        CredentialType,
        ErrorDetail,
        ErrorResponse,
        MemoryStatus,
        MemoryType,
        ResponseText,
        RagCollectionStatus,
        ScopeType
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "health", description = "Liveness and readiness probes"),
        (name = "observability", description = "Operational metrics"),
        (name = "documentation", description = "OpenAPI document and interactive reference"),
        (name = "responses", description = "Native response execution"),
        (name = "executions", description = "Execution history"),
        (name = "usage", description = "Usage records"),
        (name = "discovery", description = "Model, route, and capability discovery"),
        (name = "compatibility", description = "OpenAI-compatible API subset"),
        (name = "conversations", description = "Public conversations"),
        (name = "conversation-messages", description = "Conversation messages"),
        (name = "memories", description = "Explicit memory"),
        (name = "admin-applications", description = "Application administration"),
        (name = "admin-policies", description = "Application policy administration"),
        (name = "admin-providers", description = "Provider administration"),
        (name = "admin-provider-models", description = "Provider model administration"),
        (name = "admin-credentials", description = "Provider credential administration"),
        (name = "admin-api-keys", description = "System and consumer API key administration"),
        (name = "admin-jwt-issuers", description = "Trusted JWT issuer administration"),
        (name = "admin-audit", description = "Immutable audit event access"),
        (name = "admin-routes", description = "Route definition administration"),
        (name = "admin-routing-policies", description = "Routing policy administration"),
        (name = "admin-agent-profiles", description = "Agent profile administration"),
        (name = "admin-runtime", description = "Runtime policy and diagnostics"),
        (name = "admin-rag", description = "RAG collection and document administration")
    )
)]
pub struct MoiraApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut OpenApiDocument) {
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
        components.add_security_scheme(
            "systemKeyAuth",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-Moira-System-Key"))),
        );
        components.add_security_scheme(
            "consumerKeyAuth",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-Consumer-Key"))),
        );
    }
}

#[utoipa::path(
    get,
    path = "/openapi.json",
    tag = "documentation",
    responses(
        (status = 200, description = "Generated OpenAPI 3.1 document", content_type = "application/json"),
        (status = 401, description = "Admin documentation authentication failed", body = ErrorResponse),
        (status = 403, description = "Admin documentation authorization failed", body = ErrorResponse),
        (status = 503, description = "Database required for admin documentation authentication is unavailable", body = ErrorResponse),
        (status = 500, description = "OpenAPI serialization failed", body = ErrorResponse)
    )
)]
pub async fn openapi_json(
    Extension(openapi): Extension<Arc<OpenApiDocument>>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let document = if state.settings.docs.expose_admin {
        let actor = state
            .auth
            .authenticate_admin(state.pool()?, &headers)
            .await?;
        state.authz.require(&actor, "moira:admin")?;
        (*openapi).clone()
    } else {
        public_document((*openapi).clone())
    };

    serde_json::to_value(document)
        .map(Json)
        .map_err(|err| AppError::Internal(format!("build openapi: {err}")))
}

#[utoipa::path(
    get,
    path = "/docs",
    tag = "documentation",
    responses(
        (status = 200, description = "Interactive Scalar API reference", body = String, content_type = "text/html")
    )
)]
pub async fn docs() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Moira API Docs</title>
  <script id="api-reference" data-url="/openapi.json"></script>
  <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
</head>
<body></body>
</html>"#,
    )
}

pub(crate) fn public_document(mut document: OpenApiDocument) -> OpenApiDocument {
    document
        .paths
        .paths
        .retain(|path, _| !path.starts_with("/api/v1/admin/"));
    document
}

pub(crate) fn finalize_document(document: &mut OpenApiDocument) {
    for path_item in document.paths.paths.values_mut() {
        for operation in [
            &mut path_item.get,
            &mut path_item.put,
            &mut path_item.post,
            &mut path_item.delete,
            &mut path_item.options,
            &mut path_item.head,
            &mut path_item.patch,
            &mut path_item.trace,
        ]
        .into_iter()
        .flatten()
        {
            document_request_id(operation);
        }
    }
}

fn document_request_id(operation: &mut Operation) {
    let parameters = operation.parameters.get_or_insert_default();
    if !parameters
        .iter()
        .any(|parameter| parameter.name.eq_ignore_ascii_case("x-request-id"))
    {
        parameters.push(
            ParameterBuilder::new()
                .name("X-Request-Id")
                .parameter_in(ParameterIn::Header)
                .required(Required::False)
                .schema(Some(Object::with_type(Type::String)))
                .description(Some(
                    "Optional caller-provided correlation identifier; generated when absent",
                ))
                .build(),
        );
    }

    for response in operation.responses.responses.values_mut() {
        if let RefOr::T(response) = response {
            response
                .headers
                .entry("X-Request-Id".to_string())
                .or_insert(
                    Header::builder()
                        .description(Some("Request correlation identifier"))
                        .build(),
                );
        }
    }
}
