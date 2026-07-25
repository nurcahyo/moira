use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{ConversationService, RequestContext},
    domain::{
        ConversationCreateRequest, ConversationMessageCreateRequest, ConversationMessageQuery,
        ConversationMessageRecord, ConversationPatchRequest, ConversationPolicyPutRequest,
        ConversationPolicyRecord, ConversationQuery, ConversationRecord, ConversationStatus,
        EmbeddingPolicyPutRequest, EmbeddingPolicyRecord, ListResponse, MemoryCreateRequest,
        MemoryPatchRequest, MemoryPolicyPutRequest, MemoryPolicyRecord, MemoryQuery, MemoryRecord,
        RagCollectionCreateRequest, RagCollectionPatchRequest, RagCollectionQuery,
        RagCollectionRecord, RagCollectionStatus, RagDocumentCreateRequest,
        RagDocumentIngestRequest, RagDocumentRecord, RetrievalPolicyPutRequest,
        RetrievalPolicyRecord,
    },
    error::{AppError, ErrorResponse},
};

async fn public_actor(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::security::Actor, AppError> {
    state.auth.authenticate_caller(state.pool()?, headers).await
}

async fn admin_actor(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::security::Actor, AppError> {
    state.auth.authenticate_admin(state.pool()?, headers).await
}

fn json_headers(request_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(header::HeaderName::from_static("x-request-id"), value);
    }
    headers
}

fn etag_headers(version: i64) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("\"{version}\"")) {
        headers.insert(header::ETAG, value);
    }
    headers
}

#[utoipa::path(
    post,
    path = "/api/v1/conversations",
    tag = "conversations",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = ConversationCreateRequest,
    responses(
        (status = 201, description = "Conversation created", body = ConversationRecord),
        (status = "4XX", description = "Request, authentication, policy, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ConversationCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<ConversationRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .create_conversation(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        json_headers(&ctx.request_id),
        Json(record),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/conversations",
    tag = "conversations",
    params(ConversationQuery),
    responses(
        (status = 200, description = "Paginated conversations", body = ListResponse<ConversationRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_conversations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ConversationQuery>,
) -> Result<(HeaderMap, Json<ListResponse<ConversationRecord>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let records = ConversationService::new(&state)?
        .list_conversations(&actor, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(records)))
}

#[utoipa::path(
    get,
    path = "/api/v1/conversations/{id}",
    tag = "conversations",
    params(("id" = String, Path, description = "Conversation identifier")),
    responses(
        (
            status = 200,
            description = "Conversation",
            body = ConversationRecord,
            headers(("ETag" = String, description = "Current resource version"))
        ),
        (status = "4XX", description = "Identifier, authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<ConversationRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .get_conversation(&actor, &ctx, &id)
        .await?;
    let mut headers = json_headers(&ctx.request_id);
    headers.extend(etag_headers(record.version));
    Ok((headers, Json(record)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/conversations/{id}",
    tag = "conversations",
    request_body = ConversationPatchRequest,
    params(("id" = String, Path, description = "Conversation identifier")),
    responses(
        (
            status = 200,
            description = "Conversation updated",
            body = ConversationRecord,
            headers(("ETag" = String, description = "Current resource version"))
        ),
        (status = "4XX", description = "Request, authentication, policy, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ConversationPatchRequest>,
) -> Result<(HeaderMap, Json<ConversationRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .patch_conversation(&actor, &ctx, &id, request)
        .await?;
    let mut headers = json_headers(&ctx.request_id);
    headers.extend(etag_headers(record.version));
    Ok((headers, Json(record)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/conversations/{id}",
    tag = "conversations",
    params(("id" = String, Path, description = "Conversation identifier")),
    responses(
        (status = 204, description = "Conversation deleted"),
        (status = "4XX", description = "Authentication, authorization, policy, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, StatusCode), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    ConversationService::new(&state)?
        .set_conversation_status(&actor, &ctx, &id, ConversationStatus::Deleted)
        .await?;
    Ok((json_headers(&ctx.request_id), StatusCode::NO_CONTENT))
}

#[utoipa::path(
    post,
    path = "/api/v1/conversations/{id}/archive",
    tag = "conversations",
    params(("id" = String, Path, description = "Conversation identifier")),
    responses(
        (status = 200, description = "Conversation archived", body = ConversationRecord),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn archive_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<ConversationRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .set_conversation_status(&actor, &ctx, &id, ConversationStatus::Archived)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(record)))
}

#[utoipa::path(
    post,
    path = "/api/v1/conversations/{id}/restore",
    tag = "conversations",
    params(("id" = String, Path, description = "Conversation identifier")),
    responses(
        (status = 200, description = "Conversation restored", body = ConversationRecord),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn restore_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<ConversationRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .set_conversation_status(&actor, &ctx, &id, ConversationStatus::Active)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(record)))
}

#[utoipa::path(
    get,
    path = "/api/v1/conversations/{id}/messages",
    tag = "conversation-messages",
    params(
        ("id" = String, Path, description = "Conversation identifier"),
        ConversationMessageQuery
    ),
    responses(
        (status = 200, description = "Paginated conversation messages", body = ListResponse<ConversationMessageRecord>),
        (status = "4XX", description = "Query, authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ConversationMessageQuery>,
) -> Result<(HeaderMap, Json<ListResponse<ConversationMessageRecord>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let records = ConversationService::new(&state)?
        .list_messages(&actor, &id, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(records)))
}

#[utoipa::path(
    post,
    path = "/api/v1/conversations/{id}/messages",
    tag = "conversation-messages",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = ConversationMessageCreateRequest,
    params(("id" = String, Path, description = "Conversation identifier")),
    responses(
        (status = 201, description = "Conversation message created", body = ConversationMessageRecord),
        (status = "4XX", description = "Request, authentication, policy, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ConversationMessageCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<ConversationMessageRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .create_message(&actor, &ctx, &id, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        json_headers(&ctx.request_id),
        Json(record),
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/memories",
    tag = "memories",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = MemoryCreateRequest,
    responses(
        (status = 201, description = "Memory created", body = MemoryRecord),
        (status = "4XX", description = "Request, authentication, policy, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MemoryCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<MemoryRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .create_memory(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        json_headers(&ctx.request_id),
        Json(record),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/memories",
    tag = "memories",
    params(MemoryQuery),
    responses(
        (status = 200, description = "Paginated memories", body = ListResponse<MemoryRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MemoryQuery>,
) -> Result<(HeaderMap, Json<ListResponse<MemoryRecord>>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let records = ConversationService::new(&state)?
        .list_memories(&actor, &query)
        .await?;
    Ok((json_headers(&ctx.request_id), Json(records)))
}

#[utoipa::path(
    get,
    path = "/api/v1/memories/{id}",
    tag = "memories",
    params(("id" = String, Path, description = "Memory identifier")),
    responses(
        (
            status = 200,
            description = "Memory",
            body = MemoryRecord,
            headers(("ETag" = String, description = "Current resource version"))
        ),
        (status = "4XX", description = "Identifier, authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<MemoryRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .get_memory(&actor, &id)
        .await?;
    let mut headers = json_headers(&ctx.request_id);
    headers.extend(etag_headers(record.version));
    Ok((headers, Json(record)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/memories/{id}",
    tag = "memories",
    request_body = MemoryPatchRequest,
    params(("id" = String, Path, description = "Memory identifier")),
    responses(
        (
            status = 200,
            description = "Memory updated",
            body = MemoryRecord,
            headers(("ETag" = String, description = "Current resource version"))
        ),
        (status = "4XX", description = "Request, authentication, policy, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<MemoryPatchRequest>,
) -> Result<(HeaderMap, Json<MemoryRecord>), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .patch_memory(&actor, &ctx, &id, request)
        .await?;
    let mut headers = json_headers(&ctx.request_id);
    headers.extend(etag_headers(record.version));
    Ok((headers, Json(record)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/memories/{id}",
    tag = "memories",
    params(("id" = String, Path, description = "Memory identifier")),
    responses(
        (status = 204, description = "Memory deleted"),
        (status = "4XX", description = "Authentication, authorization, policy, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, StatusCode), AppError> {
    let actor = public_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    ConversationService::new(&state)?
        .delete_memory(&actor, &ctx, &id)
        .await?;
    Ok((json_headers(&ctx.request_id), StatusCode::NO_CONTENT))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/applications/{application_id}/conversation-policy",
    tag = "admin-policies",
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Conversation policy", body = ConversationPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_conversation_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<ConversationPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ConversationService::new(&state)?
        .get_conversation_policy(&actor, application_id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/applications/{application_id}/conversation-policy",
    tag = "admin-policies",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = ConversationPolicyPutRequest,
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Conversation policy updated", body = ConversationPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn put_conversation_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
    Json(request): Json<ConversationPolicyPutRequest>,
) -> Result<(HeaderMap, Json<ConversationPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .put_conversation_policy(&actor, &ctx, application_id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/applications/{application_id}/memory-policy",
    tag = "admin-policies",
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Memory policy", body = MemoryPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_memory_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<MemoryPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ConversationService::new(&state)?
        .get_memory_policy(&actor, application_id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/applications/{application_id}/memory-policy",
    tag = "admin-policies",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = MemoryPolicyPutRequest,
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Memory policy updated", body = MemoryPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn put_memory_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
    Json(request): Json<MemoryPolicyPutRequest>,
) -> Result<(HeaderMap, Json<MemoryPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .put_memory_policy(&actor, &ctx, application_id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/applications/{application_id}/retrieval-policy",
    tag = "admin-policies",
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Retrieval policy", body = RetrievalPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_retrieval_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<RetrievalPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ConversationService::new(&state)?
        .get_retrieval_policy(&actor, application_id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/applications/{application_id}/retrieval-policy",
    tag = "admin-policies",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = RetrievalPolicyPutRequest,
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Retrieval policy updated", body = RetrievalPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn put_retrieval_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
    Json(request): Json<RetrievalPolicyPutRequest>,
) -> Result<(HeaderMap, Json<RetrievalPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .put_retrieval_policy(&actor, &ctx, application_id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/applications/{application_id}/embedding-policy",
    tag = "admin-policies",
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Embedding policy", body = EmbeddingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_embedding_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<EmbeddingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ConversationService::new(&state)?
        .get_embedding_policy(&actor, application_id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/applications/{application_id}/embedding-policy",
    tag = "admin-policies",
    description = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.",
    request_body = EmbeddingPolicyPutRequest,
    params(("application_id" = Uuid, Path, description = "Application identifier")),
    responses(
        (status = 200, description = "Embedding policy updated", body = EmbeddingPolicyRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn put_embedding_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
    Json(request): Json<EmbeddingPolicyPutRequest>,
) -> Result<(HeaderMap, Json<EmbeddingPolicyRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .put_embedding_policy(&actor, &ctx, application_id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/rag-collections",
    tag = "admin-rag",
    description = "Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md.",
    request_body = RagCollectionCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409.")),
    responses(
        (status = 201, description = "RAG collection created", body = RagCollectionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, or conflict error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_rag_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RagCollectionCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<RagCollectionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .create_rag_collection(&actor, &ctx, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/rag-collections",
    tag = "admin-rag",
    params(RagCollectionQuery),
    responses(
        (status = 200, description = "Paginated RAG collections", body = ListResponse<RagCollectionRecord>),
        (status = "4XX", description = "Query, authentication, or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_rag_collections(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RagCollectionQuery>,
) -> Result<Json<ListResponse<RagCollectionRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    ConversationService::new(&state)?
        .list_rag_collections(&actor, &query)
        .await
        .map(Json)
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/rag-collections/{id}",
    tag = "admin-rag",
    params(("id" = String, Path, description = "RAG collection identifier")),
    responses(
        (status = 200, description = "RAG collection", body = RagCollectionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_rag_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<RagCollectionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ConversationService::new(&state)?
        .get_rag_collection(&actor, &id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/rag-collections/{id}",
    tag = "admin-rag",
    request_body = RagCollectionPatchRequest,
    params(("id" = String, Path, description = "RAG collection identifier")),
    responses(
        (status = 200, description = "RAG collection updated", body = RagCollectionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn patch_rag_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RagCollectionPatchRequest>,
) -> Result<(HeaderMap, Json<RagCollectionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .patch_rag_collection(&actor, &ctx, &id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/rag-collections/{id}",
    tag = "admin-rag",
    params(("id" = String, Path, description = "RAG collection identifier")),
    responses(
        (status = 204, description = "RAG collection deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_rag_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    ConversationService::new(&state)?
        .set_rag_collection_status(&actor, &ctx, &id, RagCollectionStatus::Deleted)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/rag-collections/{id}/enable",
    tag = "admin-rag",
    params(("id" = String, Path, description = "RAG collection identifier")),
    responses(
        (status = 200, description = "RAG collection enabled", body = RagCollectionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn enable_rag_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<RagCollectionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .set_rag_collection_status(&actor, &ctx, &id, RagCollectionStatus::Active)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/rag-collections/{id}/disable",
    tag = "admin-rag",
    params(("id" = String, Path, description = "RAG collection identifier")),
    responses(
        (status = 200, description = "RAG collection disabled", body = RagCollectionRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn disable_rag_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<RagCollectionRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .set_rag_collection_status(&actor, &ctx, &id, RagCollectionStatus::Disabled)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/rag-collections/{collection_id}/documents",
    tag = "admin-rag",
    description = "Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md.",
    request_body = RagDocumentCreateRequest,
    params(
        ("collection_id" = String, Path, description = "RAG collection identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409.")
    ),
    responses(
        (status = 201, description = "RAG document created", body = RagDocumentRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn create_rag_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(collection_id): Path<String>,
    Json(request): Json<RagDocumentCreateRequest>,
) -> Result<(StatusCode, HeaderMap, Json<RagDocumentRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .create_rag_document(&actor, &ctx, &collection_id, request)
        .await?;
    Ok((
        StatusCode::CREATED,
        etag_headers(record.version),
        Json(record),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/rag-collections/{collection_id}/documents",
    tag = "admin-rag",
    params(("collection_id" = String, Path, description = "RAG collection identifier")),
    responses(
        (status = 200, description = "RAG documents", body = ListResponse<RagDocumentRecord>),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn list_rag_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(collection_id): Path<String>,
) -> Result<Json<ListResponse<RagDocumentRecord>>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    ConversationService::new(&state)?
        .list_rag_documents(&actor, &collection_id, 50)
        .await
        .map(Json)
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/rag-documents/{id}",
    tag = "admin-rag",
    params(("id" = String, Path, description = "RAG document identifier")),
    responses(
        (status = 200, description = "RAG document", body = RagDocumentRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = "4XX", description = "Authentication, authorization, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_rag_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<RagDocumentRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let record = ConversationService::new(&state)?
        .get_rag_document(&actor, &id)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/rag-documents/{id}",
    tag = "admin-rag",
    params(("id" = String, Path, description = "RAG document identifier")),
    responses(
        (status = 204, description = "RAG document deleted"),
        (status = "4XX", description = "Authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn delete_rag_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    ConversationService::new(&state)?
        .delete_rag_document(&actor, &ctx, &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/rag-documents/{id}/ingest",
    tag = "admin-rag",
    description = "Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md.",
    request_body = RagDocumentIngestRequest,
    params(
        ("id" = String, Path, description = "RAG document identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409.")
    ),
    responses(
        (status = 200, description = "RAG document ingestion scheduled or completed", body = RagDocumentRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn ingest_rag_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RagDocumentIngestRequest>,
) -> Result<(HeaderMap, Json<RagDocumentRecord>), AppError> {
    let actor = admin_actor(&state, &headers).await?;
    let ctx = RequestContext::from_headers(&headers);
    let record = ConversationService::new(&state)?
        .ingest_rag_document(&actor, &ctx, &id, request)
        .await?;
    Ok((etag_headers(record.version), Json(record)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/rag-documents/{id}/reindex",
    tag = "admin-rag",
    description = "Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md.",
    request_body = RagDocumentIngestRequest,
    params(
        ("id" = String, Path, description = "RAG document identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409.")
    ),
    responses(
        (status = 200, description = "RAG document reindexing scheduled or completed", body = RagDocumentRecord, headers(("ETag" = String, description = "Current resource version"))),
        (status = 409, description = "idempotency_conflict or idempotency_in_progress", body = ErrorResponse),
        (status = "4XX", description = "Request, authentication, authorization, conflict, or not-found error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn reindex_rag_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RagDocumentIngestRequest>,
) -> Result<(HeaderMap, Json<RagDocumentRecord>), AppError> {
    ingest_rag_document(State(state), headers, Path(id), Json(request)).await
}
