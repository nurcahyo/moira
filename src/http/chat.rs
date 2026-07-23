use axum::{
    Json,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Sse},
};

use crate::{
    app::AppState,
    domain::ChatCompletionRequest,
    error::AppError,
    orchestration::{execute_chat, resolve_provider, stream_chat},
};

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, AppError> {
    if request.messages.is_empty() {
        return Err(AppError::BadRequest(
            "messages must not be empty".to_string(),
        ));
    }

    let actor = state.caller_auth.identify(&headers).await?;
    if headers.contains_key("x-moira-provider-api-key") {
        return Err(AppError::Forbidden(
            "raw provider API key override is disabled; use an authorized stored credential"
                .to_string(),
        ));
    }
    let resolved = resolve_provider(
        state.pool()?,
        &state.runtime_cache,
        &state.cipher,
        &actor,
        request.provider_id,
        request.model.as_deref(),
        None,
    )
    .await?;

    if request.stream {
        let stream = stream_chat(&state.http, &resolved, &request).await?;
        Ok(Sse::new(stream).into_response())
    } else {
        let response = execute_chat(&state.http, &resolved, &request).await?;
        Ok(Json(response).into_response())
    }
}
