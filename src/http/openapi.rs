use std::sync::Arc;

use axum::{Extension, Json, extract::State, http::HeaderMap, response::Html};
use serde_json::Value;
use utoipa::{
    Modify, OpenApi,
    openapi::{
        ContentBuilder, OpenApi as OpenApiDocument, Ref, RefOr, Required, ResponseBuilder,
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
        (name = "admin-invites", description = "Admin invitation lifecycle"),
        (name = "admin-identities", description = "Admin identity grant administration"),
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
            // Order matters: the `504` is added first so `document_request_id` then stamps
            // the correlation header onto it like every other response. Reversing these two
            // would ship one response object in the whole document without `X-Request-Id`.
            document_gateway_timeout(operation);
            document_request_id(operation);
        }
    }
}

/// Description of the global `504`, naming **both** conditions that produce it.
///
/// One status, two meanings, so the description says both — the same rule this commit applies
/// to `POST /v1/responses`'s `404`. A caller separates them by `error.code`, not by status:
/// the middleware timeout is normalised to `request_timeout` by
/// `normalize_infrastructure_error` (`src/lib.rs`), and the execution timeouts keep their own
/// `provider_timeout` / `deadline_exceeded` codes because that mapper leaves an
/// `application/json` body alone.
const GATEWAY_TIMEOUT_DESCRIPTION: &str = "The request exceeded a server-side deadline. Raised for every route by the middleware \
     response-head timeout (`request_timeout`), and additionally by execution operations when \
     the upstream provider or the execution deadline expires (`provider_timeout`, \
     `deadline_exceeded`).";

/// Issue #157, item 1: `504` was reachable and declared nowhere.
///
/// **Why here and not on the two execution operations.** `504` is reachable two ways —
/// globally, because `documented_router` layers `TimeoutLayer::with_status_code(GATEWAY_TIMEOUT,
/// ..)` onto *every* route group (`src/http/mod.rs`), and per-operation, because
/// `failure_http_status` maps `ProviderTimeout | DeadlineExceeded` to it
/// (`src/application/public.rs`). Declaring it on the execution operations alone would leave
/// the other 150 operations missing a status they can all return, which is the inconsistent
/// half-measure #157 warned about. `.agents/skills/moira-openapi/SKILL.md` puts cross-cutting
/// enrichment here and forbids duplicating it per-operation, which is already how the
/// `X-Request-Id` parameter and response header are handled — so this reuses that shape rather
/// than inventing a second one, down to the "only if the operation does not already say
/// something" guard.
///
/// **Why some operations are skipped.** An operation that already declares `504`, or a `5XX`
/// range, or a `default` response, has already told a generated client what to expect for this
/// status. Adding an explicit `504` next to a `5XX` that covers it would be duplication, not
/// coverage — and it would let an operation that deliberately describes its own timeout be
/// silently overwritten by this generic text. The 131 admin operations declare `5XX
/// Infrastructure or internal error` and are therefore left alone; the 21 operations that
/// enumerate their statuses explicitly are the ones that genuinely had no shape for a `504`.
fn document_gateway_timeout(operation: &mut Operation) {
    let responses = &mut operation.responses.responses;
    if responses.contains_key("504")
        || responses.contains_key("5XX")
        || responses.contains_key("default")
    {
        return;
    }
    responses.insert(
        "504".to_string(),
        RefOr::T(
            ResponseBuilder::new()
                .description(GATEWAY_TIMEOUT_DESCRIPTION)
                .content(
                    "application/json",
                    ContentBuilder::new()
                        .schema(Some(Ref::from_schema_name("ErrorResponse")))
                        .build(),
                )
                .build(),
        ),
    );
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
