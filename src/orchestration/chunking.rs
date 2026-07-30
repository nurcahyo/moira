//! Document chunking — plan 11 Sub-Phase A.
//!
//! Pure text transformation: no database access, no provider calls, no clock. Given a document
//! body and a strategy it returns the ordered chunks that `rag_chunks` rows are built from.
//! It lives in `orchestration` rather than `infra` or `domain` for exactly that reason
//! (`docs/project-structure.md`): it is Moira behaviour, not persistence and not a DTO.
//!
//! # UTF-8 safety
//!
//! Every boundary this module produces is a `char` boundary. The content is arbitrary
//! user-supplied text, so a byte-slice split would panic on a multi-byte sequence — or worse,
//! silently store a mangled chunk if it were ever done with `get_unchecked`. All splitting goes
//! through [`str::char_indices`], and the `start_offset`/`end_offset` a chunk carries are byte
//! offsets into the *original* content that are guaranteed to be char boundaries, so
//! `&content[start..end]` is always valid.
//!
//! # Determinism
//!
//! The same `(content, strategy, limits)` triple always yields the same chunks, byte for byte.
//! That is what makes `rag_chunks.chunk_hash` a stable content address across re-ingestion —
//! see `chunk_hash` in `crate::infra::repositories::conversation`.

/// How a document body is divided into chunks.
///
/// Deliberately *not* token-based. Moira has no tokenizer: `estimate_tokens` in
/// `crate::application::conversation` is a divide-by-four approximation, and naming a
/// character window "tokens" would make the limits read as a guarantee they are not.
/// Tokenizer-aware budgeting is plan 11 Sub-Phase D's problem, and when it lands this enum
/// gains a variant rather than having its existing ones silently re-interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkStrategy {
    /// Split on blank lines. Paragraphs longer than the chunk ceiling are windowed.
    Paragraph,
    /// Like [`ChunkStrategy::Paragraph`], but ATX headings (`#` … `######`) start a new chunk
    /// and name the `section_title` of every chunk beneath them.
    Markdown,
    /// A fixed sliding window with overlap, ignoring structure entirely.
    FixedWindow {
        /// Window width in `char`s. Clamped to at least 1.
        window_chars: usize,
        /// How many `char`s each window repeats from its predecessor. Clamped below
        /// `window_chars` so the window always advances.
        overlap_chars: usize,
    },
}

impl ChunkStrategy {
    /// The strategy's stable wire name, stored on `rag_chunks.metadata`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Markdown => "markdown",
            Self::FixedWindow { .. } => "fixed_window",
        }
    }

    /// Picks a strategy from a document's declared MIME type.
    ///
    /// Unknown types fall to [`ChunkStrategy::Paragraph`], which degrades to a windowed split
    /// for content that has no blank lines at all — never to "one chunk containing everything".
    pub fn for_mime_type(mime_type: Option<&str>) -> Self {
        let mime = mime_type.unwrap_or("").trim().to_ascii_lowercase();
        let mime = mime.split(';').next().unwrap_or("").trim().to_string();
        match mime.as_str() {
            "text/markdown" | "text/x-markdown" | "application/markdown" => Self::Markdown,
            _ => Self::Paragraph,
        }
    }
}

/// Bounds a single document's chunk output.
#[derive(Debug, Clone, Copy)]
pub struct ChunkingLimits {
    /// Maximum `char`s in one chunk. Clamped to at least 1.
    pub max_chunk_chars: usize,
    /// Maximum chunks one document version may produce.
    pub max_chunks_per_document: usize,
}

/// One chunk, before it is given an identifier or a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkCandidate {
    /// The chunk text, exactly as it will be hashed and stored.
    pub text: String,
    /// Byte offset of `text` in the original content. Always a `char` boundary.
    pub start_offset: usize,
    /// Byte offset one past the end of `text`. Always a `char` boundary.
    pub end_offset: usize,
    /// The nearest preceding Markdown heading, when [`ChunkStrategy::Markdown`] is in use.
    pub section_title: Option<String>,
    /// Zero-based position in the document. Dense and gap-free.
    pub chunk_index: i32,
}

/// Why chunking refused to produce output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkingError {
    /// The document would exceed [`ChunkingLimits::max_chunks_per_document`].
    ///
    /// Refused rather than truncated: silently dropping the tail of a document would make
    /// retrieval quietly incomplete, which is the class of dishonesty this plan exists to
    /// remove.
    TooManyChunks { produced: usize, limit: usize },
}

impl std::fmt::Display for ChunkingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyChunks { produced, limit } => write!(
                f,
                "document produces {produced} chunks, which exceeds the {limit}-chunk ceiling"
            ),
        }
    }
}

impl std::error::Error for ChunkingError {}

/// Divides `content` into chunks.
///
/// Returns an empty vector for content that is empty or entirely whitespace — an honest
/// "nothing to index" rather than a single blank chunk.
pub fn chunk(
    content: &str,
    strategy: ChunkStrategy,
    limits: ChunkingLimits,
) -> Result<Vec<ChunkCandidate>, ChunkingError> {
    let max_chunk_chars = limits.max_chunk_chars.max(1);
    let spans = match strategy {
        ChunkStrategy::Paragraph => structural_spans(content, false, max_chunk_chars),
        ChunkStrategy::Markdown => structural_spans(content, true, max_chunk_chars),
        ChunkStrategy::FixedWindow {
            window_chars,
            overlap_chars,
        } => {
            let window = window_chars.max(1).min(max_chunk_chars);
            // `saturating_sub(1)` keeps the stride at least one char, so a caller passing
            // `overlap >= window` gets a slow split rather than an infinite loop.
            let overlap = overlap_chars.min(window.saturating_sub(1));
            window_spans(content, 0, content.len(), window, overlap)
                .into_iter()
                .map(|(start, end)| SectionSpan {
                    start,
                    end,
                    section_title: None,
                })
                .collect()
        }
    };

    if spans.len() > limits.max_chunks_per_document {
        return Err(ChunkingError::TooManyChunks {
            produced: spans.len(),
            limit: limits.max_chunks_per_document,
        });
    }

    Ok(spans
        .into_iter()
        .enumerate()
        .map(|(index, span)| ChunkCandidate {
            text: content[span.start..span.end].to_string(),
            start_offset: span.start,
            end_offset: span.end,
            section_title: span.section_title,
            // `i32` because `rag_chunks.chunk_index` is `integer`. The cast cannot lose
            // information: `max_chunks_per_document` is validated below `i32::MAX` by
            // `RagSettings::validate`.
            chunk_index: index as i32,
        })
        .collect())
}

#[derive(Debug, Clone)]
struct SectionSpan {
    start: usize,
    end: usize,
    section_title: Option<String>,
}

/// Paragraph and Markdown share one walk: Markdown differs only in that a heading line both
/// closes the current block and renames the section.
fn structural_spans(content: &str, markdown: bool, max_chunk_chars: usize) -> Vec<SectionSpan> {
    let mut spans = Vec::new();
    let mut section_title: Option<String> = None;
    let mut block_start: Option<usize> = None;
    let mut block_end = 0usize;

    let flush = |spans: &mut Vec<SectionSpan>,
                 block_start: &mut Option<usize>,
                 block_end: usize,
                 title: &Option<String>| {
        if let Some(start) = block_start.take() {
            for (start, end) in window_spans(content, start, block_end, max_chunk_chars, 0) {
                spans.push(SectionSpan {
                    start,
                    end,
                    section_title: title.clone(),
                });
            }
        }
    };

    for (line_start, line) in line_spans(content) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(&mut spans, &mut block_start, block_end, &section_title);
            continue;
        }
        if markdown && let Some(heading) = atx_heading(trimmed) {
            flush(&mut spans, &mut block_start, block_end, &section_title);
            section_title = Some(heading);
            // The heading line itself is kept as the opening of the section's first chunk, so
            // no byte of the document is dropped and the offsets stay contiguous.
        }
        if block_start.is_none() {
            block_start = Some(line_start + leading_whitespace_bytes(line));
        }
        block_end = line_start + line.trim_end().len();
    }
    flush(&mut spans, &mut block_start, block_end, &section_title);
    spans
}

/// Yields `(byte offset, line)` for every line, without allocating.
fn line_spans(content: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0usize;
    content.split_inclusive('\n').map(move |line| {
        let start = offset;
        offset += line.len();
        (start, line.trim_end_matches('\n').trim_end_matches('\r'))
    })
}

fn leading_whitespace_bytes(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Recognises an ATX heading (`#` … `######` followed by whitespace) and returns its text.
fn atx_heading(trimmed_line: &str) -> Option<String> {
    let hashes = trimmed_line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed_line[hashes..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    if title.is_empty() {
        None
    } else {
        // `rag_chunks.section_title` is `varchar(512)`; truncate on a char boundary so a long
        // heading cannot make the insert fail.
        Some(truncate_chars(title, 512))
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        Some((byte, _)) => value[..byte].to_string(),
        None => value.to_string(),
    }
}

/// Splits `content[start..end]` into windows of at most `window_chars` characters.
///
/// Every returned boundary is a `char` boundary because it comes from `char_indices`.
/// Leading and trailing whitespace of each window is trimmed away, and windows that are
/// entirely whitespace are dropped.
fn window_spans(
    content: &str,
    start: usize,
    end: usize,
    window_chars: usize,
    overlap_chars: usize,
) -> Vec<(usize, usize)> {
    if start >= end {
        return Vec::new();
    }
    let slice = &content[start..end];
    let boundaries: Vec<usize> = slice
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(slice.len()))
        .collect();
    let char_count = boundaries.len() - 1;
    if char_count == 0 {
        return Vec::new();
    }
    let window = window_chars.max(1);
    let stride = window.saturating_sub(overlap_chars).max(1);

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while cursor < char_count {
        let stop = (cursor + window).min(char_count);
        let raw_start = start + boundaries[cursor];
        let raw_end = start + boundaries[stop];
        if let Some(span) = trim_span(content, raw_start, raw_end) {
            spans.push(span);
        }
        if stop == char_count {
            break;
        }
        cursor += stride;
    }
    spans
}

/// Narrows `[start, end)` to its non-whitespace core, or `None` when there is none.
fn trim_span(content: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let slice = &content[start..end];
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lead = slice.len() - slice.trim_start().len();
    Some((start + lead, start + lead + trimmed.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_chunk_chars: usize) -> ChunkingLimits {
        ChunkingLimits {
            max_chunk_chars,
            max_chunks_per_document: 1_000,
        }
    }

    #[test]
    fn empty_and_whitespace_content_produce_no_chunks() {
        for content in ["", "   ", "\n\n\t\n"] {
            let chunks = chunk(content, ChunkStrategy::Paragraph, limits(64)).expect("chunk");
            assert!(chunks.is_empty(), "content {content:?} produced {chunks:?}");
        }
    }

    #[test]
    fn paragraph_strategy_splits_on_blank_lines() {
        let content = "First paragraph.\n\nSecond paragraph.\n\n\nThird.";
        let chunks = chunk(content, ChunkStrategy::Paragraph, limits(1_000)).expect("chunk");
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["First paragraph.", "Second paragraph.", "Third."]
        );
        assert_eq!(
            chunks.iter().map(|c| c.chunk_index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    /// The offsets are the contract that lets a caller re-derive a chunk from the stored
    /// document body. A chunk whose text does not equal `content[start..end]` is a silent
    /// corruption of provenance, so it is asserted directly rather than inferred.
    #[test]
    fn every_chunk_offset_pair_slices_back_to_its_own_text() {
        let content = "Ünïcödé heading\n\nA paragraph with emoji 🚀🚀🚀 and accents éàü.\n\nTail.";
        for strategy in [
            ChunkStrategy::Paragraph,
            ChunkStrategy::Markdown,
            ChunkStrategy::FixedWindow {
                window_chars: 7,
                overlap_chars: 3,
            },
        ] {
            let chunks = chunk(content, strategy, limits(11)).expect("chunk");
            assert!(!chunks.is_empty(), "{strategy:?} produced nothing");
            for candidate in &chunks {
                assert!(
                    content.is_char_boundary(candidate.start_offset),
                    "{strategy:?} start offset is not a char boundary: {candidate:?}"
                );
                assert!(
                    content.is_char_boundary(candidate.end_offset),
                    "{strategy:?} end offset is not a char boundary: {candidate:?}"
                );
                assert_eq!(
                    &content[candidate.start_offset..candidate.end_offset],
                    candidate.text,
                    "{strategy:?} chunk text disagrees with its offsets: {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn no_chunk_exceeds_the_character_ceiling() {
        let content = "🚀".repeat(50);
        let chunks = chunk(&content, ChunkStrategy::Paragraph, limits(7)).expect("chunk");
        assert!(chunks.len() > 1, "expected the long run to be windowed");
        for candidate in &chunks {
            assert!(
                candidate.text.chars().count() <= 7,
                "chunk exceeds the ceiling: {candidate:?}"
            );
        }
        assert_eq!(
            chunks
                .iter()
                .map(|c| c.text.clone())
                .collect::<String>()
                .chars()
                .count(),
            50,
            "windowing without overlap must lose no characters"
        );
    }

    #[test]
    fn markdown_strategy_attaches_the_nearest_preceding_heading() {
        let content =
            "# Title\n\nIntro text.\n\n## Section A\n\nBody A.\n\n## Section B\n\nBody B.";
        let chunks = chunk(content, ChunkStrategy::Markdown, limits(1_000)).expect("chunk");
        let pairs: Vec<(Option<&str>, &str)> = chunks
            .iter()
            .map(|c| (c.section_title.as_deref(), c.text.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Some("Title"), "# Title"),
                (Some("Title"), "Intro text."),
                (Some("Section A"), "## Section A"),
                (Some("Section A"), "Body A."),
                (Some("Section B"), "## Section B"),
                (Some("Section B"), "Body B."),
            ]
        );
    }

    #[test]
    fn paragraph_strategy_never_attaches_a_section_title() {
        let content = "# Title\n\nBody.";
        let chunks = chunk(content, ChunkStrategy::Paragraph, limits(1_000)).expect("chunk");
        assert!(chunks.iter().all(|c| c.section_title.is_none()));
    }

    #[test]
    fn a_seventh_hash_is_not_a_heading() {
        assert_eq!(atx_heading("####### seven"), None);
        assert_eq!(atx_heading("#no-space"), None);
        assert_eq!(atx_heading("### "), None);
        assert_eq!(atx_heading("## Real ##"), Some("Real".to_string()));
    }

    #[test]
    fn fixed_window_overlaps_by_the_requested_amount() {
        let content = "abcdefghij";
        let chunks = chunk(
            content,
            ChunkStrategy::FixedWindow {
                window_chars: 4,
                overlap_chars: 2,
            },
            limits(1_000),
        )
        .expect("chunk");
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["abcd", "cdef", "efgh", "ghij"]);
    }

    /// A caller that asks for more overlap than window would otherwise never advance.
    #[test]
    fn overlap_at_or_above_the_window_still_terminates() {
        let chunks = chunk(
            "abcdefghij",
            ChunkStrategy::FixedWindow {
                window_chars: 3,
                overlap_chars: 99,
            },
            limits(1_000),
        )
        .expect("chunk");
        assert!(!chunks.is_empty());
        assert!(chunks.len() <= 10);
    }

    #[test]
    fn chunking_is_deterministic() {
        let content = "# H\n\nOne.\n\nTwo two two.\n\n### H3\n\nThree 🚀.";
        let first = chunk(content, ChunkStrategy::Markdown, limits(9)).expect("chunk");
        let second = chunk(content, ChunkStrategy::Markdown, limits(9)).expect("chunk");
        assert_eq!(first, second);
    }

    #[test]
    fn exceeding_the_document_ceiling_is_refused_not_truncated() {
        let content = (0..20)
            .map(|index| format!("paragraph {index}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let error = chunk(
            &content,
            ChunkStrategy::Paragraph,
            ChunkingLimits {
                max_chunk_chars: 1_000,
                max_chunks_per_document: 5,
            },
        )
        .expect_err("20 paragraphs must not fit under a 5-chunk ceiling");
        assert_eq!(
            error,
            ChunkingError::TooManyChunks {
                produced: 20,
                limit: 5
            }
        );
    }

    #[test]
    fn mime_type_selects_the_strategy() {
        assert_eq!(
            ChunkStrategy::for_mime_type(Some("text/markdown; charset=utf-8")),
            ChunkStrategy::Markdown
        );
        assert_eq!(
            ChunkStrategy::for_mime_type(Some("TEXT/X-MARKDOWN")),
            ChunkStrategy::Markdown
        );
        assert_eq!(
            ChunkStrategy::for_mime_type(Some("text/plain")),
            ChunkStrategy::Paragraph
        );
        assert_eq!(ChunkStrategy::for_mime_type(None), ChunkStrategy::Paragraph);
    }

    /// Content with no blank line at all must still be windowed rather than returned whole,
    /// otherwise a one-line 10 MB document would become a single chunk no embedding call could
    /// accept.
    #[test]
    fn structureless_content_is_still_windowed() {
        let content = "x".repeat(1_000);
        let chunks = chunk(&content, ChunkStrategy::Paragraph, limits(100)).expect("chunk");
        assert_eq!(chunks.len(), 10);
    }

    #[test]
    fn crlf_line_endings_do_not_leak_into_chunk_text() {
        let content = "First.\r\n\r\nSecond.\r\n";
        let chunks = chunk(content, ChunkStrategy::Paragraph, limits(1_000)).expect("chunk");
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["First.", "Second."]);
    }
}
