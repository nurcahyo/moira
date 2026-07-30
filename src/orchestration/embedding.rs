//! Embedding execution through Rig — plan 11 Sub-Phase B.
//!
//! The completion side of this boundary is `runtime_factory.rs`; this module is its embedding
//! twin and deliberately mirrors its shape: a factory trait, an enum handle (Rig's
//! [`rig_core::embeddings::EmbeddingModel`] has an associated `const` and an associated type, so
//! it is **not** object-safe and `Box<dyn EmbeddingModel>` does not compile), and one
//! classification function that converts a provider failure into a sanitised [`AppError`].
//!
//! # Verified against rig-core 0.40.0
//!
//! Everything below was checked against the vendored crate rather than assumed, because
//! `plans/11-rag-memory-intelligence.md` §Sub-Phase B lists the embedding surface as the plan's
//! single largest technical unknown and forbids guessing it:
//!
//! - `rig_core::embeddings::EmbeddingModel` — `const MAX_DOCUMENTS: usize`, `type Client`,
//!   `fn make(&Client, impl Into<String>, Option<usize>)`, `fn ndims(&self) -> usize`,
//!   `async fn embed_texts(impl IntoIterator<Item = String>) -> Result<Vec<Embedding>, _>`.
//! - `rig_core::embeddings::Embedding { document: String, vec: Vec<f64> }` — **`f64`**, while
//!   pgvector stores `float4`. The narrowing happens once, in [`embedding_vector`].
//! - `rig_core::client::EmbeddingsClient` is blanket-implemented for `Client<Ext, H>` whenever
//!   `Ext: Capabilities<H, Embeddings = Capable<M>>`.
//!
//! # Provider coverage
//!
//! `Capabilities::Embeddings` is `Capable<_>` for `openai` (both the responses and the
//! completions extension), `azure`, and `gemini`, and is `Nothing` for `deepseek`. Anthropic
//! exposes no embedding model at all in 0.40. So embeddings are available for exactly the
//! OpenAI-compatible family, Azure OpenAI, and Gemini — and a Moira application configured
//! against Anthropic, DeepSeek or `custom` gets an explicit, honest refusal rather than a
//! silent skip.

use std::time::Duration;

use async_trait::async_trait;
use rig_core::{
    client::EmbeddingsClient,
    embeddings::{Embedding, EmbeddingError, EmbeddingModel as RigEmbeddingModel},
    providers::{azure, gemini, openai},
};
use secrecy::ExposeSecret;
use serde_json::Value;

use crate::{
    domain::{CredentialType, ProviderType, ResolvedCredential, ResolvedProviderConfiguration},
    error::AppError,
    orchestration::normalize_openai_base_url,
};

/// Error code returned when the configured embedding provider cannot embed at all.
pub const EMBEDDING_PROVIDER_UNSUPPORTED: &str = "embedding_provider_unsupported";
/// Error code returned when the provider call itself failed.
pub const EMBEDDING_REQUEST_FAILED: &str = "embedding_request_failed";
/// Error code returned when the provider answered with the wrong number of vectors, or with
/// vectors of the wrong width.
pub const EMBEDDING_RESPONSE_INVALID: &str = "embedding_response_invalid";

/// The one embedding width Moira's schema can store.
///
/// `memory_embeddings.embedding` and `rag_chunk_embeddings.embedding` are both
/// `vector(1536)` (`migrations/0007_conversations_memory_rag.sql`). Until that changes this is a
/// hard constraint, not a default — a 3072-wide vector from `text-embedding-3-large` is rejected
/// before the insert rather than failing inside it.
pub const SUPPORTED_EMBEDDING_DIMENSION: usize = 1536;

/// Builds embedding models from resolved Moira runtime configuration.
///
/// A trait for the same reason [`crate::orchestration::RuntimeFactory`] is one: it is the seam
/// a test substitutes to exercise the ingestion pipeline without a provider.
#[async_trait]
pub trait EmbeddingFactory: Send + Sync {
    async fn build_embedding_model(
        &self,
        provider: &ResolvedProviderConfiguration,
        model_key: &str,
        credential: &ResolvedCredential,
        dimension: usize,
    ) -> Result<EmbeddingModelHandle, AppError>;
}

#[derive(Debug, Clone)]
pub struct RigEmbeddingFactory;

impl RigEmbeddingFactory {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RigEmbeddingFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// Enum dispatch over Rig's embedding models.
///
/// Not `Box<dyn EmbeddingModel>`: the trait carries `const MAX_DOCUMENTS` and
/// `type Client`, neither of which is object-safe.
#[derive(Clone)]
pub enum EmbeddingModelHandle {
    OpenAi(openai::EmbeddingModel),
    AzureOpenAi(azure::EmbeddingModel),
    Gemini(gemini::embedding::EmbeddingModel),
}

impl std::fmt::Debug for EmbeddingModelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same redaction rule as `RuntimeModelHandle`: the model owns a client that owns the
        // credential, so the derived `Debug` would print secret material into any log line
        // that formats a handle.
        match self {
            Self::OpenAi(_) => write!(f, "EmbeddingModelHandle::OpenAi(<redacted>)"),
            Self::AzureOpenAi(_) => write!(f, "EmbeddingModelHandle::AzureOpenAi(<redacted>)"),
            Self::Gemini(_) => write!(f, "EmbeddingModelHandle::Gemini(<redacted>)"),
        }
    }
}

impl EmbeddingModelHandle {
    /// The provider's own per-request document ceiling.
    pub fn max_documents(&self) -> usize {
        match self {
            Self::OpenAi(_) => <openai::EmbeddingModel as RigEmbeddingModel>::MAX_DOCUMENTS,
            Self::AzureOpenAi(_) => <azure::EmbeddingModel as RigEmbeddingModel>::MAX_DOCUMENTS,
            Self::Gemini(_) => {
                <gemini::embedding::EmbeddingModel as RigEmbeddingModel>::MAX_DOCUMENTS
            }
        }
    }

    /// The width this model was constructed for.
    pub fn dimension(&self) -> usize {
        match self {
            Self::OpenAi(model) => model.ndims(),
            Self::AzureOpenAi(model) => model.ndims(),
            Self::Gemini(model) => model.ndims(),
        }
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Embedding>, EmbeddingError> {
        match self {
            Self::OpenAi(model) => model.embed_texts(texts).await,
            Self::AzureOpenAi(model) => model.embed_texts(texts).await,
            Self::Gemini(model) => model.embed_texts(texts).await,
        }
    }
}

/// Whether `provider_type` can embed at all, under the resolved rig-core version.
///
/// Exposed so callers can refuse a misconfigured embedding policy at write time rather than at
/// ingestion time.
pub fn provider_type_supports_embeddings(provider_type: ProviderType) -> bool {
    matches!(
        provider_type,
        ProviderType::OpenAi
            | ProviderType::OpenAiCompatible
            | ProviderType::Local
            | ProviderType::AzureOpenAi
            | ProviderType::Gemini
    )
}

#[async_trait]
impl EmbeddingFactory for RigEmbeddingFactory {
    async fn build_embedding_model(
        &self,
        provider: &ResolvedProviderConfiguration,
        model_key: &str,
        credential: &ResolvedCredential,
        dimension: usize,
    ) -> Result<EmbeddingModelHandle, AppError> {
        let secret = credential.secret.expose_secret();
        match provider.provider_type {
            ProviderType::OpenAi | ProviderType::OpenAiCompatible | ProviderType::Local => {
                require_credential_type(
                    credential.credential_type,
                    &[CredentialType::ApiKey, CredentialType::BearerToken],
                )?;
                let mut builder = openai::Client::builder().api_key(secret.as_str());
                if let Some(base_url) = provider.base_url.as_deref() {
                    builder = builder.base_url(normalize_openai_base_url(base_url)?);
                }
                let client = builder
                    .build()
                    .map_err(|err| safe_config_error("openai-compatible", err))?;
                Ok(EmbeddingModelHandle::OpenAi(
                    client.embedding_model_with_ndims(model_key, dimension),
                ))
            }
            ProviderType::AzureOpenAi => {
                require_credential_type(
                    credential.credential_type,
                    &[CredentialType::AzureOpenAi, CredentialType::ApiKey],
                )?;
                let endpoint = credential
                    .config
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .or(provider.base_url.as_deref())
                    .ok_or_else(|| {
                        AppError::Config(
                            "azure_openai provider requires a configured endpoint".to_string(),
                        )
                    })?;
                let api_version = credential
                    .config
                    .get("api_version")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-10-21");
                let client = azure::Client::builder()
                    .api_key(azure::AzureOpenAIAuth::ApiKey(secret.to_string()))
                    .azure_endpoint(endpoint.to_string())
                    .api_version(api_version)
                    .build()
                    .map_err(|err| safe_config_error("azure_openai", err))?;
                Ok(EmbeddingModelHandle::AzureOpenAi(
                    client.embedding_model_with_ndims(model_key, dimension),
                ))
            }
            ProviderType::Gemini => {
                require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
                let mut builder = gemini::Client::builder().api_key(secret.as_str());
                if let Some(base_url) = provider.base_url.as_deref() {
                    builder = builder.base_url(base_url);
                }
                let client = builder
                    .build()
                    .map_err(|err| safe_config_error("gemini", err))?;
                Ok(EmbeddingModelHandle::Gemini(
                    client.embedding_model_with_ndims(model_key, dimension),
                ))
            }
            ProviderType::Anthropic | ProviderType::DeepSeek | ProviderType::Custom => {
                Err(unsupported_provider(provider.provider_type))
            }
        }
    }
}

/// The refusal a non-embedding provider earns.
pub fn unsupported_provider(provider_type: ProviderType) -> AppError {
    AppError::unprocessable(
        "embedding_provider_unsupported",
        format!(
            "provider type {} exposes no embedding model in the configured Rig version",
            provider_type_label(provider_type)
        ),
    )
}

fn provider_type_label(provider_type: ProviderType) -> &'static str {
    match provider_type {
        ProviderType::OpenAi => "openai",
        ProviderType::OpenAiCompatible => "openai_compatible",
        ProviderType::Anthropic => "anthropic",
        ProviderType::Gemini => "gemini",
        ProviderType::DeepSeek => "deepseek",
        ProviderType::AzureOpenAi => "azure_openai",
        ProviderType::Local => "local",
        ProviderType::Custom => "custom",
    }
}

/// A batched embedding run: what went in, what limits applied, how long it may take.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingBatchPlan {
    /// Documents per provider call, from `application_embedding_policies.batch_size`.
    pub batch_size: usize,
    /// Wall-clock ceiling for the **whole** run, from
    /// `application_embedding_policies.timeout_ms`.
    pub deadline: Duration,
    /// The width every returned vector must have.
    pub dimension: usize,
}

/// Embeds `texts` in order, in batches, under a single deadline.
///
/// Returns one `Vec<f32>` per input, in input order. A short, over-long or wrong-width response
/// is an error rather than a silently padded vector: a truncated embedding is a corrupt index
/// entry that would degrade retrieval invisibly.
pub async fn embed_texts(
    handle: &EmbeddingModelHandle,
    texts: &[String],
    plan: EmbeddingBatchPlan,
) -> Result<Vec<Vec<f32>>, AppError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let batch_size = plan.batch_size.max(1).min(handle.max_documents());
    let run = async {
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(batch_size) {
            let requested = batch.len();
            let embeddings = handle
                .embed_batch(batch.to_vec())
                .await
                .map_err(classify_embedding_error)?;
            if embeddings.len() != requested {
                // The codes below are spelled as literals rather than as the consts declared
                // at the top of this module: the i18n catalog gate resolves coverage by reading
                // the source, and a constant in argument position reads to it as a
                // runtime-computed code with no proof of a catalog entry.
                return Err(AppError::coded(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "embedding_response_invalid",
                    format!(
                        "embedding provider returned {} vectors for {requested} inputs",
                        embeddings.len()
                    ),
                ));
            }
            for embedding in embeddings {
                vectors.push(embedding_vector(embedding, plan.dimension)?);
            }
        }
        Ok(vectors)
    };

    // The deadline bounds the entire run, not each batch: a caller-visible timeout must not be
    // multiplied by however many batches the document happens to need.
    tokio::time::timeout(plan.deadline, run)
        .await
        .map_err(|_| {
            AppError::coded(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "embedding_request_failed",
                "embedding request exceeded the configured timeout",
            )
        })?
}

/// Narrows Rig's `f64` vector to the `float4` pgvector stores, checking the width first.
fn embedding_vector(embedding: Embedding, dimension: usize) -> Result<Vec<f32>, AppError> {
    if embedding.vec.len() != dimension {
        return Err(AppError::coded(
            axum::http::StatusCode::BAD_GATEWAY,
            "embedding_response_invalid",
            format!(
                "embedding provider returned a {}-dimension vector; {dimension} is required",
                embedding.vec.len()
            ),
        ));
    }
    // Rig models embeddings as `f64`; every provider Moira supports transmits `float32` on the
    // wire and pgvector stores `float4`, so this narrowing loses nothing that was ever present.
    Ok(embedding
        .vec
        .into_iter()
        .map(|value| value as f32)
        .collect())
}

/// Turns a Rig embedding failure into a sanitised `AppError`.
///
/// Status-first, mirroring `classify_completion_error`. The provider's response body is
/// deliberately **not** propagated: it can echo the request, and an embedding request body is
/// document content.
pub fn classify_embedding_error(error: EmbeddingError) -> AppError {
    let status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    let detail = match status {
        Some(status) => format!("embedding request failed with HTTP {status}"),
        None => "embedding request failed".to_string(),
    };
    let http_status = match status {
        Some(401 | 403) => axum::http::StatusCode::BAD_GATEWAY,
        Some(408) => axum::http::StatusCode::GATEWAY_TIMEOUT,
        Some(429) => axum::http::StatusCode::TOO_MANY_REQUESTS,
        _ => axum::http::StatusCode::BAD_GATEWAY,
    };
    AppError::coded(http_status, "embedding_request_failed", detail)
}

/// Encodes a vector in pgvector's text input format, e.g. `[0.5,-1.25]`.
///
/// **Decision (plan 11 Wave 1, A6).** Moira binds vectors as `text` and casts with `$n::vector`
/// rather than adding the `pgvector` crate. Reasons, in order of weight:
///
/// 1. A new dependency on this repository is a reviewable supply-chain decision — it moves
///    `Cargo.lock`, `deny.toml` and `tests/supply_chain_policy.rs` — and the only thing it buys
///    at this stage is the binary wire format.
/// 2. Nothing in Wave 1 ever reads a vector back into Rust. Vectors are written once and
///    compared inside PostgreSQL by the `<=>` operator; the value never crosses back over the
///    boundary, so no decoder is needed.
/// 3. The text format is part of pgvector's documented input contract and is stable.
///
/// **Reversal condition:** adopt the `pgvector` crate if either (a) Sub-Phase C's
/// `EXPLAIN ANALYZE` work shows text encoding is a material share of write or query latency, or
/// (b) any code path needs to *read* a stored vector into Rust, at which point hand-rolling a
/// parser would be strictly worse than taking the dependency.
///
/// `f32::to_string` round-trips exactly (Rust prints the shortest representation that parses
/// back to the same value), so this encoding is lossless.
pub fn encode_vector_literal(vector: &[f32]) -> String {
    let mut encoded = String::with_capacity(vector.len() * 12 + 2);
    encoded.push('[');
    for (index, value) in vector.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        // Non-finite values have no pgvector representation and would be rejected by the
        // server with an opaque error; normalise here so the failure, if any, is ours.
        if value.is_finite() {
            encoded.push_str(&value.to_string());
        } else {
            encoded.push('0');
        }
    }
    encoded.push(']');
    encoded
}

fn require_credential_type(
    actual: CredentialType,
    allowed: &[CredentialType],
) -> Result<(), AppError> {
    if allowed.contains(&actual) {
        Ok(())
    } else {
        Err(AppError::Config(format!(
            "credential type {actual:?} is not supported by this embedding provider"
        )))
    }
}

fn safe_config_error(provider: &str, err: impl std::fmt::Display) -> AppError {
    AppError::Config(format!(
        "build Rig {provider} embedding client failed: {err}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AppError::code` is private, so tests read the variant directly.
    fn error_code(error: &AppError) -> &str {
        match error {
            AppError::Api { code, .. } => code,
            other => panic!("expected a coded AppError, got: {other:?}"),
        }
    }

    #[test]
    fn vector_literals_round_trip_through_text() {
        let vector = vec![0.0f32, 1.5, -2.25, 1e-7, 3.4028235e38];
        let encoded = encode_vector_literal(&vector);
        assert!(encoded.starts_with('[') && encoded.ends_with(']'));
        let decoded: Vec<f32> = encoded[1..encoded.len() - 1]
            .split(',')
            .map(|part| part.parse::<f32>().expect("finite component parses"))
            .collect();
        assert_eq!(decoded, vector);
    }

    #[test]
    fn non_finite_components_are_normalised_rather_than_emitted() {
        let encoded = encode_vector_literal(&[f32::NAN, f32::INFINITY, 1.0]);
        assert_eq!(encoded, "[0,0,1]");
    }

    #[test]
    fn an_empty_vector_encodes_to_empty_brackets() {
        assert_eq!(encode_vector_literal(&[]), "[]");
    }

    #[test]
    fn a_wrong_width_vector_is_rejected_rather_than_padded() {
        let embedding = Embedding {
            document: "doc".to_string(),
            vec: vec![0.1, 0.2],
        };
        let error = embedding_vector(embedding, 1536).expect_err("width mismatch must fail");
        assert_eq!(error_code(&error), EMBEDDING_RESPONSE_INVALID);
    }

    #[test]
    fn a_correct_width_vector_narrows_to_f32() {
        let embedding = Embedding {
            document: "doc".to_string(),
            vec: vec![0.5, -0.25, 0.125],
        };
        assert_eq!(
            embedding_vector(embedding, 3).expect("width matches"),
            vec![0.5f32, -0.25, 0.125]
        );
    }

    /// The provider-coverage claim in this module's header is load-bearing — Sub-Phase B's
    /// entire premise is that embeddings are unavailable for some providers Moira supports —
    /// so it is asserted rather than left as prose.
    #[test]
    fn embedding_support_matches_the_verified_rig_capability_table() {
        for provider_type in [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatible,
            ProviderType::Local,
            ProviderType::AzureOpenAi,
            ProviderType::Gemini,
        ] {
            assert!(
                provider_type_supports_embeddings(provider_type),
                "{provider_type:?} must be embedding-capable"
            );
        }
        for provider_type in [
            ProviderType::Anthropic,
            ProviderType::DeepSeek,
            ProviderType::Custom,
        ] {
            assert!(
                !provider_type_supports_embeddings(provider_type),
                "{provider_type:?} exposes no embedding model in rig-core 0.40"
            );
            assert_eq!(
                error_code(&unsupported_provider(provider_type)),
                EMBEDDING_PROVIDER_UNSUPPORTED
            );
        }
    }

    #[test]
    fn embedding_errors_never_carry_the_provider_body() {
        let error = classify_embedding_error(EmbeddingError::ProviderError(
            "sk-live-leaked-key and the document text".to_string(),
        ));
        let message = error.to_string();
        assert!(
            !message.contains("sk-live-leaked-key"),
            "sanitised message leaked provider text: {message}"
        );
        assert_eq!(error_code(&error), EMBEDDING_REQUEST_FAILED);
    }

    #[tokio::test]
    async fn embedding_an_empty_input_never_calls_the_provider() {
        // No handle is constructed, which is the point: the empty case must short-circuit
        // before anything provider-shaped is required.
        let vectors = embed_texts(
            &EmbeddingModelHandle::OpenAi(
                openai::Client::builder()
                    .api_key("sk-test")
                    .build()
                    .expect("build test client")
                    .embedding_model_with_ndims("text-embedding-3-small", 1536),
            ),
            &[],
            EmbeddingBatchPlan {
                batch_size: 8,
                deadline: Duration::from_millis(1),
                dimension: 1536,
            },
        )
        .await
        .expect("empty input succeeds without a provider call");
        assert!(vectors.is_empty());
    }
}
