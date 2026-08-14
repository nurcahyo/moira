//! Retrieval ranking — plan 11 Sub-Phase C.
//!
//! The SQL that *finds* candidates lives in `src/infra/repositories/conversation.rs`, because
//! the cross-tenant isolation predicate has to be evaluated in the same query as the vector
//! `ORDER BY` and must never be re-expressible anywhere else. This module owns everything that
//! happens to a candidate **after** the database has already scoped it: scoring, thresholds,
//! diversity and caps. All of it is pure, so all of it is unit-testable without a database.
//!
//! # What "hybrid" means here, precisely
//!
//! **Decision (plan 11 Wave 2).** Recall is *semantic only*: the candidate set is whatever the
//! HNSW-ordered query returns for the query embedding, over-fetched by
//! [`CANDIDATE_OVERFETCH`]. The keyword, recency and importance weights from
//! `application_retrieval_policies` are then applied as a **re-rank over that candidate set**.
//!
//! The consequence, stated rather than buried: a chunk that matches the query lexically but not
//! semantically is **not retrieved**. There is no independent keyword recall path.
//!
//! Why not a real `ts_rank` union: `rag_chunks.chunk_text_plain` and `memory_records.content_plain`
//! are both nullable — the conversation-content-persistence policy can store only the
//! `*_encrypted` variant — and neither has a full-text index. A `to_tsvector(...)` term inside
//! the retrieval query would therefore be an unindexed expression over the whole table, scanned
//! on every response, for a column that is frequently null. That is a worse failure than the
//! honest limitation above.
//!
//! **Reversal condition:** add a GIN index on the plaintext columns and a genuine
//! `plainto_tsquery` recall arm the moment either (a) a retrieval-quality measurement shows
//! lexical-only matches are being missed in practice, or (b) the persistence policy guarantees
//! plaintext is present. At that point [`lexical_overlap_score`] should be deleted, not kept
//! alongside it.

use uuid::Uuid;

/// How many rows the SQL asks for per result the policy will keep.
///
/// The re-rank can only reorder what it is given, so a multiplier of 1 would make the keyword,
/// recency and importance weights decorative — they could never promote a row the vector order
/// had already cut. Four is small enough to stay inside one HNSW page-ful and large enough for
/// the re-rank to be able to change the answer.
pub const CANDIDATE_OVERFETCH: i64 = 4;

/// Hard ceiling on candidate rows per retrieval arm, whatever the policy asks for.
///
/// `maximum_memory_results` / `maximum_chunk_results` are operator-set `integer` columns with
/// only a `> 0` check, so without this a typo could ask PostgreSQL to sort two billion rows
/// inside a request path.
pub const MAX_CANDIDATE_ROWS: i64 = 512;

/// The blend weights, read from `application_retrieval_policies`.
#[derive(Debug, Clone, Copy)]
pub struct RetrievalWeights {
    pub semantic: f64,
    pub keyword: f64,
    pub recency: f64,
    pub importance: f64,
}

/// Thresholds and caps for one retrieval arm.
#[derive(Debug, Clone, Copy)]
pub struct RetrievalLimits {
    pub maximum_results: usize,
    /// A candidate must score **strictly above** this to be returned.
    pub minimum_score: f64,
    /// `maximum_chunks_per_document`; ignored for memories.
    pub maximum_per_group: usize,
    pub diversity_enabled: bool,
}

/// One `memory_records` row the isolation-scoped query returned.
#[derive(Debug, Clone)]
pub struct MemoryCandidate {
    pub memory_uuid: Uuid,
    pub public_id: String,
    pub memory_type: String,
    pub memory_key: Option<String>,
    /// `None` when the application persists memory content encrypted only.
    pub content: Option<String>,
    pub importance: f64,
    /// pgvector cosine distance, in `[0, 2]`.
    pub distance: f64,
    pub age_seconds: f64,
}

/// One `rag_chunks` row the isolation-scoped query returned.
#[derive(Debug, Clone)]
pub struct RagChunkCandidate {
    pub chunk_uuid: Uuid,
    pub public_id: String,
    pub document_uuid: Uuid,
    pub document_public_id: String,
    pub document_title: Option<String>,
    pub section_title: Option<String>,
    pub chunk_index: i32,
    /// `None` when the row holds no body in either column.
    ///
    /// Since issue #141 a sealed chunk is **opened** on this path, so `None` no longer covers
    /// "stored encrypted" — it did before, and a collection under `encrypted_content` therefore
    /// retrieved as a page of textless chunks with nothing saying so.
    /// `ContentWrite::under_policy_for_rag` never writes a bodyless chunk, so this is currently
    /// unreachable in practice and is kept because the columns are nullable and pre-existing
    /// rows were never checked.
    pub text: Option<String>,
    pub distance: f64,
    pub age_seconds: f64,
}

/// A candidate plus the score that decided its fate.
#[derive(Debug, Clone)]
pub struct Scored<T> {
    pub item: T,
    pub score: f64,
}

/// The four signals a blended score is built from.
///
/// `importance` is `Option` because RAG chunks genuinely have none — `rag_chunks` carries no
/// importance column and inventing one would be fabrication. An absent component drops out of
/// both the numerator and the denominator, so the result stays in `[0, 1]` and a policy that
/// puts weight on importance does not silently deflate every chunk score.
#[derive(Debug, Clone, Copy)]
pub struct ScoreComponents {
    pub semantic: f64,
    pub keyword: f64,
    pub recency: f64,
    pub importance: Option<f64>,
}

/// Maps pgvector's cosine distance onto `[0, 1]`, monotonically decreasing.
///
/// `<=>` returns `1 - cosine_similarity`, so it spans `[0, 2]` and 1.0 means orthogonal. The
/// affine map keeps the whole range usable rather than clamping every obtuse pair to zero —
/// which matters because `minimum_memory_score` / `minimum_chunk_score` are declared over
/// `[0, 1]` and an operator setting `0.5` means "no worse than orthogonal".
pub fn semantic_score(distance: f64) -> f64 {
    if !distance.is_finite() {
        return 0.0;
    }
    ((2.0 - distance) / 2.0).clamp(0.0, 1.0)
}

/// Fraction of the query's distinct terms that appear in `text`.
///
/// A coverage measure, not TF-IDF: it answers "how much of what was asked about is present",
/// which is the question a re-rank needs, and it cannot be gamed by repeating a term. Returns
/// `0.0` when the text is absent (encrypted-only persistence) so an unreadable candidate is
/// ranked on its semantic score alone rather than being excluded outright.
pub fn lexical_overlap_score(query: &str, text: Option<&str>) -> f64 {
    let Some(text) = text else {
        return 0.0;
    };
    let query_terms: Vec<String> = normalized_terms(query);
    if query_terms.is_empty() {
        return 0.0;
    }
    let text_terms: std::collections::BTreeSet<String> =
        normalized_terms(text).into_iter().collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut matched = 0usize;
    for term in query_terms {
        if !seen.insert(term.clone()) {
            continue;
        }
        if text_terms.contains(&term) {
            matched += 1;
        }
    }
    let distinct = seen.len();
    if distinct == 0 {
        0.0
    } else {
        matched as f64 / distinct as f64
    }
}

fn normalized_terms(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Recency on `[0, 1]`, monotonically decreasing in age, halving each week.
///
/// A hyperbola rather than an exponential: it decays fast enough to matter within a session and
/// never reaches zero, so a genuinely relevant year-old memory is demoted rather than erased.
pub fn recency_score(age_seconds: f64) -> f64 {
    if !age_seconds.is_finite() || age_seconds <= 0.0 {
        return 1.0;
    }
    let weeks = age_seconds / (7.0 * 24.0 * 60.0 * 60.0);
    1.0 / (1.0 + weeks)
}

/// Weighted mean of the components that are present.
///
/// Normalised by the weights actually used, so the result is in `[0, 1]` whatever the operator
/// set the four weights to — the columns are `double precision` with only a `>= 0` check, so
/// they do not sum to one and cannot be assumed to. A policy with every weight at zero falls
/// back to the pure semantic score rather than returning zero for everything, because "all
/// weights zero" is a misconfiguration and silently rejecting every candidate would look
/// identical to an empty corpus.
pub fn blend(components: ScoreComponents, weights: RetrievalWeights) -> f64 {
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    for (value, weight) in [
        (Some(components.semantic), weights.semantic),
        (Some(components.keyword), weights.keyword),
        (Some(components.recency), weights.recency),
        (components.importance, weights.importance),
    ] {
        let (Some(value), true) = (value, weight > 0.0) else {
            continue;
        };
        numerator += value * weight;
        denominator += weight;
    }
    if denominator <= 0.0 {
        return components.semantic.clamp(0.0, 1.0);
    }
    (numerator / denominator).clamp(0.0, 1.0)
}

/// Scores, filters and caps memory candidates.
///
/// Candidates arrive already ordered by distance and already scoped by the SQL; nothing here
/// can widen the scope, only narrow it.
pub fn rank_memories(
    query: &str,
    candidates: Vec<MemoryCandidate>,
    weights: RetrievalWeights,
    limits: RetrievalLimits,
) -> Vec<Scored<MemoryCandidate>> {
    let mut scored: Vec<Scored<MemoryCandidate>> = candidates
        .into_iter()
        .map(|candidate| {
            let score = blend(
                ScoreComponents {
                    semantic: semantic_score(candidate.distance),
                    keyword: lexical_overlap_score(query, candidate.content.as_deref()),
                    recency: recency_score(candidate.age_seconds),
                    importance: Some(candidate.importance.clamp(0.0, 1.0)),
                },
                weights,
            );
            Scored {
                item: candidate,
                score,
            }
        })
        .filter(|scored| scored.score > limits.minimum_score)
        .collect();
    sort_by_score(&mut scored, |scored| scored.item.public_id.as_str());
    scored.truncate(limits.maximum_results);
    scored
}

/// Scores, filters, diversifies and caps RAG chunk candidates.
pub fn rank_chunks(
    query: &str,
    candidates: Vec<RagChunkCandidate>,
    weights: RetrievalWeights,
    limits: RetrievalLimits,
) -> Vec<Scored<RagChunkCandidate>> {
    let mut scored: Vec<Scored<RagChunkCandidate>> = candidates
        .into_iter()
        .map(|candidate| {
            let score = blend(
                ScoreComponents {
                    semantic: semantic_score(candidate.distance),
                    keyword: lexical_overlap_score(query, candidate.text.as_deref()),
                    recency: recency_score(candidate.age_seconds),
                    importance: None,
                },
                weights,
            );
            Scored {
                item: candidate,
                score,
            }
        })
        .filter(|scored| scored.score > limits.minimum_score)
        .collect();
    sort_by_score(&mut scored, |scored| scored.item.public_id.as_str());

    // Diversity is applied *before* the global cap, not after: capping first and then
    // de-duplicating would return fewer than `maximum_results` rows whenever one document
    // dominates, which is the opposite of what the setting is for.
    if limits.diversity_enabled && limits.maximum_per_group > 0 {
        let mut per_document: std::collections::HashMap<Uuid, usize> =
            std::collections::HashMap::new();
        scored.retain(|entry| {
            let count = per_document.entry(entry.item.document_uuid).or_insert(0);
            if *count >= limits.maximum_per_group {
                false
            } else {
                *count += 1;
                true
            }
        });
    }
    scored.truncate(limits.maximum_results);
    scored
}

/// Descending by score, ties broken by public id.
///
/// The tiebreak is not cosmetic: without it two candidates with identical scores order by
/// whatever the HNSW scan happened to emit, which makes the `context_plans` provenance row and
/// therefore the response's citations non-deterministic for the same input.
fn sort_by_score<T, F>(scored: &mut [Scored<T>], key: F)
where
    F: Fn(&Scored<T>) -> &str,
{
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| key(left).cmp(key(right)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights(semantic: f64, keyword: f64, recency: f64, importance: f64) -> RetrievalWeights {
        RetrievalWeights {
            semantic,
            keyword,
            recency,
            importance,
        }
    }

    fn limits(maximum_results: usize, minimum_score: f64) -> RetrievalLimits {
        RetrievalLimits {
            maximum_results,
            minimum_score,
            maximum_per_group: 0,
            diversity_enabled: false,
        }
    }

    fn memory(public_id: &str, distance: f64, importance: f64) -> MemoryCandidate {
        MemoryCandidate {
            memory_uuid: Uuid::now_v7(),
            public_id: public_id.to_string(),
            memory_type: "fact".to_string(),
            memory_key: None,
            content: Some("the user prefers dark mode".to_string()),
            importance,
            distance,
            age_seconds: 0.0,
        }
    }

    fn chunk(public_id: &str, document: Uuid, distance: f64) -> RagChunkCandidate {
        RagChunkCandidate {
            chunk_uuid: Uuid::now_v7(),
            public_id: public_id.to_string(),
            document_uuid: document,
            document_public_id: format!("doc_{document}"),
            document_title: Some("Handbook".to_string()),
            section_title: None,
            chunk_index: 0,
            text: Some("retention policy details".to_string()),
            distance,
            age_seconds: 0.0,
        }
    }

    #[test]
    fn cosine_distance_maps_to_a_monotonically_decreasing_score() {
        let distances = [0.0, 0.25, 0.5, 1.0, 1.5, 2.0];
        let scores: Vec<f64> = distances.iter().copied().map(semantic_score).collect();
        assert_eq!(scores.first().copied(), Some(1.0));
        assert_eq!(scores.last().copied(), Some(0.0));
        for window in scores.windows(2) {
            assert!(
                window[0] > window[1],
                "score must strictly decrease with distance: {window:?}"
            );
        }
        // An orthogonal pair sits exactly halfway, which is what makes the default
        // `minimum_*_score = 0.5` mean "no worse than orthogonal".
        assert!((semantic_score(1.0) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_non_finite_distance_scores_zero_rather_than_panicking() {
        assert_eq!(semantic_score(f64::NAN), 0.0);
        assert_eq!(semantic_score(f64::INFINITY), 0.0);
    }

    #[test]
    fn only_semantic_weight_yields_the_pure_semantic_score() {
        let components = ScoreComponents {
            semantic: 0.8,
            keyword: 0.0,
            recency: 0.1,
            importance: Some(0.2),
        };
        assert!((blend(components, weights(1.0, 0.0, 0.0, 0.0)) - 0.8).abs() < 1e-12);
        // And the same holds for any positive semantic weight, because the blend is normalised.
        assert!((blend(components, weights(7.0, 0.0, 0.0, 0.0)) - 0.8).abs() < 1e-12);
    }

    #[test]
    fn the_blend_is_the_weight_normalised_mean_of_the_present_components() {
        let components = ScoreComponents {
            semantic: 1.0,
            keyword: 0.0,
            recency: 1.0,
            importance: Some(0.0),
        };
        // (1*1 + 0*1 + 1*1 + 0*1) / 4
        assert!((blend(components, weights(1.0, 1.0, 1.0, 1.0)) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn an_absent_importance_component_drops_out_of_the_denominator() {
        let present = ScoreComponents {
            semantic: 1.0,
            keyword: 1.0,
            recency: 1.0,
            importance: Some(1.0),
        };
        let absent = ScoreComponents {
            importance: None,
            ..present
        };
        // A chunk with no importance signal must not be penalised against a memory that
        // scores perfectly on every component it has.
        assert!((blend(present, weights(1.0, 1.0, 1.0, 1.0)) - 1.0).abs() < 1e-12);
        assert!((blend(absent, weights(1.0, 1.0, 1.0, 1.0)) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn all_zero_weights_fall_back_to_the_semantic_score() {
        let components = ScoreComponents {
            semantic: 0.42,
            keyword: 1.0,
            recency: 1.0,
            importance: Some(1.0),
        };
        assert!((blend(components, weights(0.0, 0.0, 0.0, 0.0)) - 0.42).abs() < 1e-12);
    }

    #[test]
    fn lexical_overlap_is_the_covered_fraction_of_distinct_query_terms() {
        assert!(
            (lexical_overlap_score("dark mode", Some("prefers dark mode")) - 1.0).abs() < 1e-12
        );
        assert!((lexical_overlap_score("dark mode", Some("prefers dark")) - 0.5).abs() < 1e-12);
        assert_eq!(
            lexical_overlap_score("dark mode", Some("nothing here")),
            0.0
        );
    }

    #[test]
    fn lexical_overlap_cannot_be_inflated_by_repeating_a_term() {
        let once = lexical_overlap_score("retention policy", Some("retention"));
        let many = lexical_overlap_score(
            "retention policy",
            Some("retention retention retention retention"),
        );
        assert!((once - many).abs() < 1e-12, "{once} vs {many}");
    }

    #[test]
    fn lexical_overlap_of_unreadable_content_is_zero_not_a_panic() {
        assert_eq!(lexical_overlap_score("anything", None), 0.0);
        assert_eq!(lexical_overlap_score("", Some("anything")), 0.0);
    }

    #[test]
    fn recency_decreases_with_age_and_never_reaches_zero() {
        let day = 24.0 * 60.0 * 60.0;
        let fresh = recency_score(0.0);
        let week = recency_score(7.0 * day);
        let year = recency_score(365.0 * day);
        assert_eq!(fresh, 1.0);
        assert!((week - 0.5).abs() < 1e-12, "one week must halve: {week}");
        assert!(year > 0.0 && year < week);
    }

    #[test]
    fn a_candidate_scoring_exactly_the_minimum_is_excluded() {
        // Pure semantic scoring makes the score exactly 0.5 at distance 1.0, so this pins the
        // `>` vs `>=` question at the boundary rather than near it.
        let ranked = rank_memories(
            "unrelated",
            vec![memory("mem_a", 1.0, 0.5)],
            weights(1.0, 0.0, 0.0, 0.0),
            limits(10, 0.5),
        );
        assert!(ranked.is_empty(), "the threshold must be exclusive");

        let ranked = rank_memories(
            "unrelated",
            vec![memory("mem_a", 0.999, 0.5)],
            weights(1.0, 0.0, 0.0, 0.0),
            limits(10, 0.5),
        );
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn results_are_capped_at_the_configured_maximum() {
        let candidates = (0..10)
            .map(|index| memory(&format!("mem_{index:02}"), 0.0, 1.0))
            .collect();
        let ranked = rank_memories(
            "dark mode",
            candidates,
            weights(1.0, 0.0, 0.0, 0.0),
            limits(3, 0.0),
        );
        assert_eq!(ranked.len(), 3);
    }

    #[test]
    fn ranking_is_by_score_descending_with_a_deterministic_tiebreak() {
        let ranked = rank_memories(
            "dark mode",
            vec![
                memory("mem_c", 0.5, 1.0),
                memory("mem_a", 0.5, 1.0),
                memory("mem_b", 0.0, 1.0),
            ],
            weights(1.0, 0.0, 0.0, 0.0),
            limits(10, 0.0),
        );
        let order: Vec<&str> = ranked
            .iter()
            .map(|entry| entry.item.public_id.as_str())
            .collect();
        assert_eq!(order, vec!["mem_b", "mem_a", "mem_c"]);
    }

    #[test]
    fn the_diversity_cap_limits_chunks_per_document() {
        let crowded = Uuid::now_v7();
        let other = Uuid::now_v7();
        let ranked = rank_chunks(
            "retention policy",
            vec![
                chunk("chunk_a", crowded, 0.0),
                chunk("chunk_b", crowded, 0.01),
                chunk("chunk_c", crowded, 0.02),
                chunk("chunk_d", other, 0.03),
            ],
            weights(1.0, 0.0, 0.0, 0.0),
            RetrievalLimits {
                maximum_results: 10,
                minimum_score: 0.0,
                maximum_per_group: 2,
                diversity_enabled: true,
            },
        );
        let documents: Vec<Uuid> = ranked
            .iter()
            .map(|entry| entry.item.document_uuid)
            .collect();
        assert_eq!(documents.iter().filter(|id| **id == crowded).count(), 2);
        assert_eq!(documents.iter().filter(|id| **id == other).count(), 1);
    }

    #[test]
    fn diversity_runs_before_the_global_cap_so_the_cap_is_still_filled() {
        let crowded = Uuid::now_v7();
        let other = Uuid::now_v7();
        let ranked = rank_chunks(
            "retention policy",
            vec![
                chunk("chunk_a", crowded, 0.00),
                chunk("chunk_b", crowded, 0.01),
                chunk("chunk_c", crowded, 0.02),
                chunk("chunk_d", other, 0.03),
            ],
            weights(1.0, 0.0, 0.0, 0.0),
            RetrievalLimits {
                maximum_results: 2,
                minimum_score: 0.0,
                maximum_per_group: 1,
                diversity_enabled: true,
            },
        );
        assert_eq!(
            ranked.len(),
            2,
            "the cap must still be filled after de-duping"
        );
        assert_eq!(ranked[0].item.document_uuid, crowded);
        assert_eq!(ranked[1].item.document_uuid, other);
    }

    #[test]
    fn disabling_diversity_lets_one_document_take_every_slot() {
        let crowded = Uuid::now_v7();
        let ranked = rank_chunks(
            "retention policy",
            vec![
                chunk("chunk_a", crowded, 0.0),
                chunk("chunk_b", crowded, 0.01),
                chunk("chunk_c", crowded, 0.02),
            ],
            weights(1.0, 0.0, 0.0, 0.0),
            RetrievalLimits {
                maximum_results: 10,
                minimum_score: 0.0,
                maximum_per_group: 1,
                diversity_enabled: false,
            },
        );
        assert_eq!(ranked.len(), 3);
    }

    #[test]
    fn the_keyword_weight_can_reorder_the_vector_order() {
        // This is the whole justification for over-fetching: if the re-rank could never change
        // the answer, the keyword weight would be decoration.
        let lexical_match = MemoryCandidate {
            content: Some("dark mode preference".to_string()),
            ..memory("mem_lexical", 0.6, 0.5)
        };
        let closer_but_unrelated = MemoryCandidate {
            content: Some("completely different subject".to_string()),
            ..memory("mem_vector", 0.5, 0.5)
        };
        let ranked = rank_memories(
            "dark mode",
            vec![closer_but_unrelated, lexical_match],
            weights(1.0, 2.0, 0.0, 0.0),
            limits(10, 0.0),
        );
        assert_eq!(ranked[0].item.public_id, "mem_lexical");
    }
}
