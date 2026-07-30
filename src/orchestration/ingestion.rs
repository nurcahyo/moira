//! The RAG ingestion plan — plan 11 Sub-Phases A and B, joined.
//!
//! A [`RagIngestionPlan`] is everything the database needs to record one document version's
//! ingestion: the chunks, their content addresses, their embeddings when there are any, and the
//! **terminal** status the version is written with.
//!
//! # Why the plan is built before the transaction opens
//!
//! Embedding is a network call. `ingest_rag_document_with_connection` runs inside the
//! `AdminCommandRunner` transaction, which already holds `select … for update` on the document
//! row and the idempotency advisory lock. Doing provider I/O there would hold both across an
//! unbounded await, on a pooled connection, once per batch. So the whole plan — chunking,
//! hashing, embedding — is computed first, and the transaction only writes.
//!
//! # Why there is no observable `chunking` → `embedding` progression
//!
//! **Decision (plan 11 Wave 1).** The version row is inserted once, already carrying its
//! terminal status. The intermediate `'chunking'` and `'embedding'` values of the
//! `rag_document_versions.ingestion_status` check constraint are not written on this path.
//!
//! Writing them would be theatre: everything here happens inside a single transaction, so no
//! other session could ever observe an intermediate value, and a sequence of updates nobody can
//! read is not honesty — it is the appearance of it. The property that actually matters is that
//! `'indexed'` is never written unless the work was done, and that is enforced by construction:
//! the status is *derived* from the plan's contents in [`RagIngestionPlan::terminal_status`],
//! not passed in.
//!
//! **Reversal condition:** the intermediate states become real, and must be written, the moment
//! ingestion spans more than one transaction — which is exactly what remote-URL ingestion
//! (`'downloading'`/`'parsing'`, excluded from this plan) and worker-resumed ingestion require.
//! At that point the states stop being decoration and start being the resume point.

use uuid::Uuid;

use crate::{
    domain::RagIngestionStatus,
    error::AppError,
    orchestration::chunking::{ChunkStrategy, ChunkingError, ChunkingLimits, chunk},
    security::request_hash,
};

/// Error code for a document that produces more chunks than the configured ceiling.
pub const RAG_DOCUMENT_TOO_LARGE: &str = "rag_document_too_large";

/// `rag_ingestion_runs.failure_class` for a document whose application asked for RAG embeddings
/// but has no usable embedding provider/model configured.
pub const FAILURE_EMBEDDING_NOT_CONFIGURED: &str = "embedding_not_configured";
/// `rag_ingestion_runs.failure_class` for a provider that refused or failed the embedding call.
pub const FAILURE_EMBEDDING_FAILED: &str = "embedding_failed";
/// `rag_ingestion_runs.failure_class` for an embedding model whose width the schema cannot
/// store. `memory_embeddings.embedding` and `rag_chunk_embeddings.embedding` are `vector(1536)`.
pub const FAILURE_EMBEDDING_DIMENSION_UNSUPPORTED: &str = "embedding_dimension_unsupported";

/// One chunk, ready to be inserted.
#[derive(Debug, Clone)]
pub struct PreparedChunk {
    pub chunk_index: i32,
    pub text: String,
    /// Content address of `text`.
    ///
    /// **Decision D-B2 (`plans/11-rag-memory-intelligence.md` §0).** This is
    /// [`crate::security::request_hash`] — a plain, unkeyed SHA-256 that is an alias for
    /// `secret_fingerprint`. The name is a misnomer here: the bytes being hashed are a document
    /// chunk, not a request.
    ///
    /// It is deliberately **not** `IdempotencyHasher::hash`, even though that is what the
    /// neighbouring `content_hash` columns use. That hasher is peppered and version-prefixed and
    /// verifies only against the *active* pepper, by design — so a pepper rotation would
    /// invalidate every stored `chunk_hash` and force every document to re-chunk and re-embed.
    /// A chunk hash has to be content-addressable indefinitely; that is its whole job.
    ///
    /// Reversal condition: move to a keyed hash if `chunk_hash` ever becomes reachable across a
    /// trust boundary — returned on a caller-visible response, accepted as a caller-supplied
    /// lookup key, or used to dedupe *across* applications. None of those is true today.
    pub chunk_hash: String,
    pub token_estimate: i64,
    pub start_offset: i32,
    pub end_offset: i32,
    pub section_title: Option<String>,
    /// `None` when the application does not have RAG embeddings enabled.
    pub embedding: Option<Vec<f32>>,
}

/// Everything one document version's ingestion produced.
#[derive(Debug, Clone)]
pub struct RagIngestionPlan {
    pub chunks: Vec<PreparedChunk>,
    pub strategy: &'static str,
    /// The `provider_models.id` the embeddings came from, for `rag_chunk_embeddings`.
    pub embedding_model_id: Option<Uuid>,
    pub embedding_dimension: Option<i32>,
    /// Set only when the run failed; `None` on success.
    pub failure_class: Option<&'static str>,
}

impl RagIngestionPlan {
    /// The plan produced for a version with no content at all.
    ///
    /// Distinct from a failure: there is genuinely nothing to index, and `'indexed'` over zero
    /// chunks would claim otherwise. `'pending'` is the honest answer — the same answer the
    /// surface gave before this plan existed, for the same reason.
    pub fn empty() -> Self {
        Self {
            chunks: Vec::new(),
            strategy: "none",
            embedding_model_id: None,
            embedding_dimension: None,
            failure_class: None,
        }
    }

    /// A plan that failed before or during embedding.
    ///
    /// The chunks are kept: they were really produced, they are really stored, and discarding
    /// them would lose the only work that succeeded. The status is what tells the truth.
    pub fn failed(chunks: Vec<PreparedChunk>, strategy: &'static str, class: &'static str) -> Self {
        Self {
            chunks,
            strategy,
            embedding_model_id: None,
            embedding_dimension: None,
            failure_class: Some(class),
        }
    }

    /// How many chunks carry an embedding.
    pub fn embedded_chunk_count(&self) -> i32 {
        // Bounded by `max_chunks_per_document`, which `RagSettings::validate` keeps below
        // `i32::MAX`, so the cast cannot wrap.
        self.chunks
            .iter()
            .filter(|chunk| chunk.embedding.is_some())
            .count() as i32
    }

    pub fn chunk_count(&self) -> i32 {
        self.chunks.len() as i32
    }

    /// The status the version row is written with.
    ///
    /// **This is the honesty invariant of the whole pipeline**, so it is derived rather than
    /// supplied: nothing can pass `Indexed` in. `'indexed'` requires at least one stored chunk
    /// and no recorded failure. A run that produced no chunks stays `'pending'`, and a run that
    /// failed is `'failed'` however much work preceded the failure.
    pub fn terminal_status(&self) -> RagIngestionStatus {
        if self.failure_class.is_some() {
            RagIngestionStatus::Failed
        } else if self.chunks.is_empty() {
            RagIngestionStatus::Pending
        } else {
            RagIngestionStatus::Indexed
        }
    }

    /// The `rag_ingestion_runs.status` matching [`Self::terminal_status`].
    pub fn run_status(&self) -> &'static str {
        match self.terminal_status() {
            RagIngestionStatus::Indexed => "completed",
            RagIngestionStatus::Failed => "failed",
            _ => "pending",
        }
    }
}

/// Chunks `content` and gives each chunk its content address.
///
/// Returns `Ok(vec![])` for content that is empty or all whitespace.
pub fn prepare_chunks(
    content: &str,
    strategy: ChunkStrategy,
    limits: ChunkingLimits,
) -> Result<Vec<PreparedChunk>, AppError> {
    let candidates = chunk(content, strategy, limits).map_err(|err| match err {
        ChunkingError::TooManyChunks { .. } => {
            AppError::unprocessable("rag_document_too_large", err.to_string())
        }
    })?;
    Ok(candidates
        .into_iter()
        .map(|candidate| PreparedChunk {
            chunk_index: candidate.chunk_index,
            chunk_hash: request_hash(candidate.text.as_bytes()),
            token_estimate: estimate_tokens(&candidate.text),
            // `rag_chunks.start_offset`/`end_offset` are `integer`. A document large enough to
            // overflow one is refused long before here by `max_chunk_chars` *
            // `max_chunks_per_document`, but saturating rather than wrapping keeps a bad
            // configuration from silently storing a negative offset.
            start_offset: candidate.start_offset.min(i32::MAX as usize) as i32,
            end_offset: candidate.end_offset.min(i32::MAX as usize) as i32,
            section_title: candidate.section_title,
            text: candidate.text,
            embedding: None,
        })
        .collect())
}

/// A coarse token estimate, matching the existing divide-by-four approximation used for
/// conversation budgeting. Stored on `rag_chunks.token_count` as an estimate, never as a
/// measurement — Moira has no tokenizer.
fn estimate_tokens(text: &str) -> i64 {
    // `usize::div_ceil` is stable; the signed one is not, so the count is rounded before the
    // cast rather than after it.
    text.chars().count().div_ceil(4) as i64
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

    fn limits() -> ChunkingLimits {
        ChunkingLimits {
            max_chunk_chars: 64,
            max_chunks_per_document: 100,
        }
    }

    fn chunks(count: usize) -> Vec<PreparedChunk> {
        (0..count)
            .map(|index| PreparedChunk {
                chunk_index: index as i32,
                text: format!("chunk {index}"),
                chunk_hash: format!("hash-{index}"),
                token_estimate: 2,
                start_offset: 0,
                end_offset: 1,
                section_title: None,
                embedding: None,
            })
            .collect()
    }

    #[test]
    fn identical_text_produces_an_identical_chunk_hash() {
        let first = prepare_chunks("Alpha.\n\nBeta.", ChunkStrategy::Paragraph, limits())
            .expect("prepare chunks");
        let second = prepare_chunks("Alpha.\n\nBeta.", ChunkStrategy::Paragraph, limits())
            .expect("prepare chunks");
        assert_eq!(
            first
                .iter()
                .map(|chunk| chunk.chunk_hash.clone())
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|chunk| chunk.chunk_hash.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn different_text_produces_a_different_chunk_hash() {
        let chunks = prepare_chunks("Alpha.\n\nBeta.", ChunkStrategy::Paragraph, limits())
            .expect("prepare chunks");
        assert_ne!(chunks[0].chunk_hash, chunks[1].chunk_hash);
    }

    /// D-B2 in executable form: the chunk hash must be the unkeyed content address, not the
    /// peppered idempotency digest, because the peppered one carries a `"{version}:"` prefix and
    /// changes under rotation.
    #[test]
    fn the_chunk_hash_is_the_unkeyed_content_address() {
        let chunks =
            prepare_chunks("Alpha.", ChunkStrategy::Paragraph, limits()).expect("prepare chunks");
        assert_eq!(chunks[0].chunk_hash, request_hash(b"Alpha."));
        assert!(
            !chunks[0].chunk_hash.contains(':'),
            "a version-prefixed digest means the peppered hasher leaked in: {}",
            chunks[0].chunk_hash
        );
    }

    #[test]
    fn a_plan_with_no_chunks_stays_pending() {
        let plan = RagIngestionPlan::empty();
        assert_eq!(plan.terminal_status(), RagIngestionStatus::Pending);
        assert_eq!(plan.run_status(), "pending");
        assert_eq!(plan.chunk_count(), 0);
    }

    #[test]
    fn a_plan_with_chunks_and_no_failure_is_indexed() {
        let plan = RagIngestionPlan {
            chunks: chunks(3),
            strategy: "paragraph",
            embedding_model_id: None,
            embedding_dimension: None,
            failure_class: None,
        };
        assert_eq!(plan.terminal_status(), RagIngestionStatus::Indexed);
        assert_eq!(plan.run_status(), "completed");
        assert_eq!(plan.embedded_chunk_count(), 0);
    }

    /// The failure path must not be able to launder itself into `'indexed'` by having produced
    /// chunks first.
    #[test]
    fn a_failed_plan_is_failed_however_many_chunks_it_produced() {
        let plan = RagIngestionPlan::failed(chunks(10), "paragraph", FAILURE_EMBEDDING_FAILED);
        assert_eq!(plan.terminal_status(), RagIngestionStatus::Failed);
        assert_eq!(plan.run_status(), "failed");
        assert_eq!(plan.chunk_count(), 10);
    }

    #[test]
    fn embedded_chunk_count_counts_only_chunks_that_carry_a_vector() {
        let mut chunks = chunks(4);
        chunks[0].embedding = Some(vec![0.0; 4]);
        chunks[2].embedding = Some(vec![0.0; 4]);
        let plan = RagIngestionPlan {
            chunks,
            strategy: "paragraph",
            embedding_model_id: Some(Uuid::now_v7()),
            embedding_dimension: Some(4),
            failure_class: None,
        };
        assert_eq!(plan.embedded_chunk_count(), 2);
        assert_eq!(plan.chunk_count(), 4);
    }

    #[test]
    fn an_over_large_document_is_refused_with_the_documented_code() {
        let content = (0..50)
            .map(|index| format!("paragraph {index}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let error = prepare_chunks(
            &content,
            ChunkStrategy::Paragraph,
            ChunkingLimits {
                max_chunk_chars: 64,
                max_chunks_per_document: 4,
            },
        )
        .expect_err("50 paragraphs must not fit under a 4-chunk ceiling");
        assert_eq!(error_code(&error), RAG_DOCUMENT_TOO_LARGE);
    }
}
