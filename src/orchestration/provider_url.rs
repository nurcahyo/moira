use crate::error::AppError;

/// Normalise a provider base URL onto the OpenAI-compatible `/v1` prefix that
/// Rig's OpenAI client expects. Live consumer: `runtime_factory.rs` on the
/// OpenAI and Azure provider paths.
pub fn normalize_openai_base_url(base_url: &str) -> Result<String, AppError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| AppError::BadRequest(format!("invalid provider base_url: {err}")))?;
    if parsed.path().trim_end_matches('/').ends_with("/v1") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}/v1"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_vllm_base_url_to_openai_v1() {
        assert_eq!(
            normalize_openai_base_url("http://192.168.1.13:8000").unwrap(),
            "http://192.168.1.13:8000/v1"
        );
        assert_eq!(
            normalize_openai_base_url("http://localhost:8000/v1/").unwrap(),
            "http://localhost:8000/v1"
        );
    }
}
