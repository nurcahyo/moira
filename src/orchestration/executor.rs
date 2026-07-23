use std::{convert::Infallible, time::Duration};

use axum::response::sse::Event;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};

use crate::{
    domain::{ChatCompletionRequest, ProviderKind},
    error::AppError,
    orchestration::resolver::{ResolvedProvider, normalize_openai_base_url},
};

pub async fn execute_chat(
    http: &Client,
    resolved: &ResolvedProvider,
    request: &ChatCompletionRequest,
) -> Result<Value, AppError> {
    ensure_openai_compatible(&resolved.provider.kind)?;
    let rig_client = rig_openai_client(resolved)?;
    let url = format!("{}/chat/completions", rig_client.base_url());
    let body = openai_body(resolved, request, false);
    let response = http
        .post(url)
        .bearer_auth(&resolved.api_key)
        .timeout(Duration::from_millis(resolved.provider.timeout_ms as u64))
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(AppError::Upstream(format!("status {status}: {body}")));
    }

    serde_json::from_str(&body).map_err(|err| AppError::Upstream(format!("invalid JSON: {err}")))
}

pub async fn stream_chat(
    http: &Client,
    resolved: &ResolvedProvider,
    request: &ChatCompletionRequest,
) -> Result<impl Stream<Item = Result<Event, Infallible>> + Send + 'static, AppError> {
    ensure_openai_compatible(&resolved.provider.kind)?;
    let rig_client = rig_openai_client(resolved)?;
    let url = format!("{}/chat/completions", rig_client.base_url());
    let response = http
        .post(url)
        .bearer_auth(&resolved.api_key)
        .timeout(Duration::from_millis(resolved.provider.timeout_ms as u64))
        .json(&openai_body(resolved, request, true))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AppError::Upstream(format!("status {status}: {body}")));
    }

    Ok(response.bytes_stream().map(|chunk| {
        let data = match chunk {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(err) => format!("event: error\ndata: {err}\n\n"),
        };
        Ok(Event::default().event("provider_chunk").data(data))
    }))
}

fn ensure_openai_compatible(kind: &ProviderKind) -> Result<(), AppError> {
    match kind {
        ProviderKind::OpenAiCompatible | ProviderKind::OpenAi | ProviderKind::Local => Ok(()),
        _ => Err(AppError::BadRequest(format!(
            "provider kind {kind:?} is registered but not executable in V1"
        ))),
    }
}

fn rig_openai_client(
    resolved: &ResolvedProvider,
) -> Result<rig_core::providers::openai::CompletionsClient, AppError> {
    let base_url = normalize_openai_base_url(&resolved.provider.base_url)?;
    rig_core::providers::openai::Client::builder()
        .api_key(resolved.api_key.as_str())
        .base_url(base_url)
        .build()
        .map(|client| client.completions_api())
        .map_err(|err| AppError::Config(format!("build Rig OpenAI-compatible client: {err}")))
}

fn openai_body(
    resolved: &ResolvedProvider,
    request: &ChatCompletionRequest,
    force_stream: bool,
) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), json!(resolved.model));
    body.insert("messages".to_string(), json!(request.messages));
    body.insert("stream".to_string(), json!(force_stream));
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(max_tokens) = request.max_tokens {
        body.insert("max_tokens".to_string(), json!(max_tokens));
    }
    for (key, value) in &request.extra_body {
        body.entry(key.clone()).or_insert_with(|| value.clone());
    }
    Value::Object(body)
}

#[cfg(test)]
fn flatten_messages_for_rig(messages: &[crate::domain::ChatMessage]) -> String {
    messages
        .iter()
        .map(|message| format!("{}: {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::Value;
    use uuid::Uuid;

    use crate::domain::{ChatMessage, ProviderConfig};

    #[test]
    fn message_flattening_keeps_roles_for_rig_boundary() {
        let text = flatten_messages_for_rig(&[ChatMessage {
            role: "user".to_string(),
            content: json!("hello"),
            extra: Default::default(),
        }]);
        assert!(text.contains("user"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn rig_client_uses_normalized_openai_base_url() {
        let now = Utc::now();
        let resolved = ResolvedProvider {
            provider: ProviderConfig {
                id: Uuid::new_v4(),
                tenant_id: None,
                application_id: None,
                name: "local-vllm".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: "http://192.168.1.13:8000".to_string(),
                default_model: "Qwen/Qwen3-4B".to_string(),
                enabled: true,
                timeout_ms: 120_000,
                metadata: Value::Object(Default::default()),
                created_at: now,
                updated_at: now,
            },
            model: "Qwen/Qwen3-4B".to_string(),
            api_key: "dummy".to_string(),
        };

        let client = rig_openai_client(&resolved).unwrap();
        assert_eq!(client.base_url(), "http://192.168.1.13:8000/v1");
    }
}
