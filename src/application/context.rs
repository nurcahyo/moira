use std::net::IpAddr;

use axum::http::HeaderMap;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: String,
    pub source_ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub idempotency_key: Option<String>,
}

impl RequestContext {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            request_id: header(headers, "x-request-id")
                .unwrap_or_else(|| Uuid::now_v7().to_string()),
            source_ip: header(headers, "x-forwarded-for")
                .and_then(|value| {
                    value
                        .split(',')
                        .next()
                        .map(str::trim)
                        .map(ToOwned::to_owned)
                })
                .or_else(|| header(headers, "x-real-ip"))
                .and_then(|value| value.parse().ok()),
            user_agent: header(headers, axum::http::header::USER_AGENT.as_str()),
            idempotency_key: header(headers, "idempotency-key"),
        }
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
