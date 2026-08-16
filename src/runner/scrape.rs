//! Turning a container's raw tty stream into the two values the control plane needs.
//!
//! # The problem, stated from a real capture rather than from first principles
//!
//! The container runs `claude setup-token` under a daemon-allocated pty, so what comes back is
//! not a log — it is a **terminal recording**. The structure below is not inferred: it was
//! captured byte for byte from `moira-claude-runner:local` on 2026-08-16 while building this
//! module, and the first version of this file was wrong about it in a way its own unit tests
//! could not have caught.
//!
//! What the CLI actually emits for the authorization URL is an **OSC 8 hyperlink**, five times
//! over, once per wrapped display row:
//!
//! ```text
//! ESC ] 8 ; id=10jb8ff ; <THE COMPLETE URL> BEL   ESC [ 37m <one 80-column slice> ESC [ 39m   ESC ] 8 ; ; BEL   \r \r \n
//! ```
//!
//! Two consequences, and both of them broke the obvious implementation:
//!
//! 1. **The complete URL is in the escape sequence, and the wrapped fragments are the
//!    *visible* text.** A stripper that removes OSC sequences throws away the one clean copy
//!    and leaves only the fragments. That is what [`osc8_targets`] recovers.
//! 2. **Rows are separated by `\r\r\n`, not `\r\n`.** Splitting on `['\r', '\n']` turns that
//!    into a spurious *empty* line between every pair of fragments, and an empty line is
//!    exactly what a naive continuation rule stops at. Measured symptom: the service returned
//!    `…&client_id=9d1c250a-e61b-44d9-88` — truncated at the first wrap, 80 characters in,
//!    with the `state` and `code_challenge` parameters missing. It looked like a URL. The
//!    authorization server would have rejected it with an opaque error.
//!
//! A third detail is worth recording because it silently invalidates a plausible heuristic:
//! **Ink positions words with cursor-forward sequences instead of spaces.** The prompt line
//! `Paste code here if prompted >` is emitted as
//! `ESC[2G Paste ESC[8G code ESC[13G here ESC[18G if ESC[21G prompted ESC[30G >`, so after
//! stripping it reads `Pastecodehereifprompted>` — with **no spaces at all**. Any rule that
//! ends a value at "the first line containing a space" would run straight through it. What
//! ends it here is `>`, which cannot appear in a URL.
//!
//! # The algorithm
//!
//! 1. **[`osc8_targets`] first.** If the stream carries an OSC 8 hyperlink whose target starts
//!    with the wanted prefix, that is the answer: complete, unwrapped, and not reconstructed.
//! 2. **Otherwise fall back to stripping and rejoining.** [`strip_ansi`] removes escape
//!    sequences and the C0 controls that are not line structure; lines are split on *runs* of
//!    `\r`/`\n` so `\r\r\n` is one break; the line containing the prefix starts the value and
//!    each following line is appended for as long as it is composed entirely of characters
//!    that may appear in a URL; the result is truncated at its first character that cannot.
//!
//! The fallback is not dead code kept for symmetry — the minted token is printed as plain
//! text, not as a hyperlink, so [`extract_token`] uses that path exclusively.
//!
//! # What this deliberately does not do
//!
//! It does not reconstruct the terminal's screen buffer by interpreting cursor movement. A
//! full terminal emulator would be more faithful, and is a large dependency plus a large
//! surface for a credential to end up somewhere unexpected. The two steps above are sufficient
//! for the two strings that matter, and their failure mode is a value that does not match
//! rather than a value that is silently wrong.

use super::token::{MintedToken, TtyTranscript};

/// Characters that may appear in a URL, per RFC 3986's unreserved + reserved sets plus `%`.
///
/// Used as the continuation test in [`rejoin_wrapped`]. It is deliberately generous: a false
/// *accept* on a continuation line is corrected by the final truncation, whereas a false
/// *reject* silently truncates the URL at a wrap boundary, which is the failure this module
/// exists to prevent.
fn is_url_char(candidate: char) -> bool {
    candidate.is_ascii_alphanumeric()
        || matches!(
            candidate,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

/// Removes ANSI escape sequences and the C0 control characters that are not line structure.
///
/// Handles the three shapes a pty stream actually contains:
///
/// * **CSI** — `ESC [` , parameter and intermediate bytes, then a final byte in `0x40..=0x7E`.
///   This is colour, cursor movement and erase-line, which is the overwhelming majority.
/// * **OSC** — `ESC ]` … terminated by `BEL` or by `ESC \` (a String Terminator). Terminal
///   title sets arrive this way and their payload is arbitrary text, so an unterminated OSC
///   must not swallow the rest of the stream — it is bounded by end-of-input.
/// * **Two-character escapes** — `ESC` followed by one byte, e.g. `ESC ( B`-style charset
///   selection and `ESC 7` / `ESC 8` cursor save/restore.
///
/// `\n`, `\r` and `\t` survive because step 2 of the algorithm needs the line structure. Every
/// other C0 control is dropped, `BEL` included.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(current) = chars.next() {
        if current != '\u{1b}' {
            // Keep line structure and printable text; drop the rest of C0 and DEL.
            if current == '\n' || current == '\r' || current == '\t' || !current.is_control() {
                out.push(current);
            }
            continue;
        }

        match chars.peek().copied() {
            // CSI: consume through the first byte in the final-byte range.
            Some('[') => {
                chars.next();
                for candidate in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&candidate) {
                        break;
                    }
                }
            }
            // OSC (and the other string-terminated sequences, which share the terminator):
            // consume through BEL or ESC \.
            Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                chars.next();
                while let Some(candidate) = chars.next() {
                    if candidate == '\u{07}' {
                        break;
                    }
                    if candidate == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-character escape: drop ESC and the byte after it.
            Some(_) => {
                chars.next();
            }
            // A trailing ESC at end of input: nothing to drop.
            None => {}
        }
    }

    out
}

/// Collects the target URIs of every OSC 8 hyperlink in a **raw**, un-stripped stream.
///
/// The sequence is `ESC ] 8 ; <params> ; <URI> ST`, where `ST` is either `BEL` or `ESC \`.
/// `params` is a `key=value:key=value` list — `id=10jb8ff` in the measured capture — and is
/// discarded; only the URI is wanted. A closing `ESC ] 8 ; ; ST` has an empty URI and is
/// skipped by the emptiness check.
///
/// This must run **before** [`strip_ansi`], which deletes the whole sequence. That ordering is
/// the entire reason the function exists as a separate step rather than as part of the
/// stripper.
///
/// A target containing a character that cannot appear in a URL is dropped rather than
/// returned: an unterminated OSC would otherwise swallow the rest of the stream — which,
/// after a successful exchange, is the stream that contains the minted token.
pub fn osc8_targets(raw: &str) -> Vec<String> {
    const INTRODUCER: &str = "\u{1b}]8;";
    let mut targets = Vec::new();
    let mut rest = raw;

    while let Some(offset) = rest.find(INTRODUCER) {
        rest = &rest[offset + INTRODUCER.len()..];
        // Skip the parameter list up to its terminating ';'. If there is none, the sequence is
        // malformed and there is nothing further to find.
        let Some(separator) = rest.find(';') else {
            break;
        };
        let after_params = &rest[separator + 1..];

        // The URI runs to BEL or to ESC \, whichever comes first.
        let bel = after_params.find('\u{07}');
        let st = after_params.find("\u{1b}\\");
        let end = match (bel, st) {
            (Some(bel), Some(st)) => bel.min(st),
            (Some(bel), None) => bel,
            (None, Some(st)) => st,
            (None, None) => break,
        };

        let target = &after_params[..end];
        if !target.is_empty() && target.chars().all(is_url_char) {
            targets.push(target.to_string());
        }
        rest = &after_params[end..];
    }

    targets
}

/// Finds a value beginning with `prefix` and rejoins the lines the terminal wrapped it across.
///
/// Returns `None` when no line contains the prefix. See the module docs for the continuation
/// rule and why it is shaped the way it is.
///
/// # Splitting on *runs* of `\r`/`\n` is load-bearing
///
/// The measured separator between two wrapped display rows is `\r\r\n`, not `\r\n`. Splitting
/// on individual `\r` and `\n` characters therefore produces spurious empty entries between
/// every pair of fragments — and an empty line is exactly what would otherwise end a
/// continuation. That is not hypothetical: it is the bug that shipped in the first draft of
/// this file and returned a URL truncated 80 characters in, with `state` and `code_challenge`
/// missing.
///
/// The cost of collapsing runs is that a genuinely blank line no longer ends a value. That is
/// acceptable because the real terminator is stronger: the first line containing a character
/// that cannot appear in a URL. In the measured output that is the `>` of the paste prompt.
pub fn rejoin_wrapped(stripped: &str, prefix: &str) -> Option<String> {
    let lines: Vec<&str> = stripped
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let (index, start) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| line.find(prefix).map(|offset| (index, offset)))?;

    let mut value = String::from(&lines[index][start..]);
    for line in lines.iter().skip(index + 1) {
        if !line.chars().all(is_url_char) {
            break;
        }
        value.push_str(line);
    }

    // The first line may carry trailing prose after the value (`… &state=abc  (opens in a
    // browser)`), and a generously accepted continuation line may carry a stray trailing
    // character. Truncating at the first non-URL character fixes both.
    let end = value
        .char_indices()
        .find(|(_, candidate)| !is_url_char(*candidate))
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    value.truncate(end);

    if value.len() <= prefix.len() {
        // Nothing beyond the prefix survived — a bare prefix is not a usable value, and
        // returning it would move a runner into `awaiting_authorization` with a URL that
        // goes nowhere.
        return None;
    }
    Some(value)
}

/// The authorization URL, or `None` if the CLI has not printed it yet.
///
/// `prefix` is [`super::config::RunnerConfig::authorization_url_prefix`], default
/// `https://claude.com/cai/oauth/authorize`.
///
/// The OSC 8 hyperlink target is preferred because it is the complete URL as the CLI meant it,
/// not a reconstruction. The rejoin is the fallback for a build that stops emitting hyperlinks
/// — and the fallback is the *only* path once the terminal width is narrow enough that even
/// the hyperlink's visible text is useless, so both are kept and both are tested.
pub fn extract_authorization_url(transcript: &TtyTranscript, prefix: &str) -> Option<String> {
    let raw = transcript.expose();
    if let Some(target) = osc8_targets(raw)
        .into_iter()
        .find(|target| target.starts_with(prefix))
    {
        return Some(target);
    }
    rejoin_wrapped(&strip_ansi(raw), prefix)
}

/// The minted token, or `None` if the exchange has not produced one.
///
/// # This is the unproven part of the design, stated plainly
///
/// The spike on issue #272 exercised the whole exchange with a deliberately bogus code and
/// got an OAuth 400 back, which proves the plumbing — the attach write reaches the prompt and
/// the CLI performs the exchange. It never ran a *valid* code, so the exact shape of the
/// success output is **not measured**. `prefix` is therefore configurable
/// ([`super::config::RunnerConfig::token_prefix`], default `sk-ant-`) so an operator can
/// correct it without a rebuild, and the tests below assert the mechanism — ANSI stripping,
/// wrap rejoining, refusing a bare prefix — not the format.
pub fn extract_token(transcript: &TtyTranscript, prefix: &str) -> Option<MintedToken> {
    rejoin_wrapped(&strip_ansi(transcript.expose()), prefix).map(MintedToken::new)
}

/// Whether the transcript contains any of the configured failure markers.
///
/// Matched on the stripped text, because a marker can be split by a colour change: the CLI
/// renders `OAuth error` with the word `error` in red, which puts a CSI sequence in the
/// middle of the phrase in the raw stream.
pub fn contains_failure_marker(transcript: &TtyTranscript, markers: &[String]) -> bool {
    let stripped = strip_ansi(transcript.expose());
    markers
        .iter()
        .any(|marker| !marker.is_empty() && stripped.contains(marker.as_str()))
}

/// Whether the CLI has rendered its paste prompt, i.e. whether there is a reader waiting.
///
/// # Why this gates the state machine rather than being a nicety
///
/// The authorization URL and the paste prompt render at *roughly* the same moment, so it is
/// tempting to treat "I have scraped the URL" as "the CLI is ready for input". They are not
/// the same event, and writing into the gap delivers bytes to a container whose reader is not
/// attached yet. `awaiting_authorization` therefore means **the prompt is on screen**, which
/// is the only observation that actually licenses a write.
///
/// # Matching has to survive Ink's layout
///
/// Ink positions words with cursor-forward sequences instead of spaces, so the prompt arrives
/// as `ESC[2G Paste ESC[8G code ESC[13G here …` and strips to `Pastecodehereifprompted>` —
/// with no whitespace at all. Matching therefore happens on the stripped text with **all
/// whitespace removed and case folded**, so the same marker matches both that rendering and a
/// plain-text build that emits real spaces.
pub fn contains_paste_prompt(transcript: &TtyTranscript, marker: &str) -> bool {
    if marker.is_empty() {
        return false;
    }
    let normalise = |text: &str| {
        text.chars()
            .filter(|candidate| !candidate.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    normalise(&strip_ansi(transcript.expose())).contains(&normalise(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL_PREFIX: &str = "https://claude.com/cai/oauth/authorize";

    /// The URL `claude setup-token` 2.1.233 actually emitted, with the three per-session
    /// values replaced.
    ///
    /// The parameter names, their order, the encodings and the overall length are exactly as
    /// captured; `client_id`, `code_challenge` and `state` carry placeholders of the same
    /// length as the originals. The originals were a live PKCE challenge and a live CSRF
    /// state, and a repository does not need either — but the *shape* is what the wrap
    /// arithmetic depends on, so the lengths are preserved.
    const CAPTURED_URL: &str = "https://claude.com/cai/oauth/authorize?code=true\
        &client_id=00000000-1111-2222-3333-444444444444\
        &response_type=code\
        &redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback\
        &scope=user%3Ainference\
        &code_challenge=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
        &code_challenge_method=S256\
        &state=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    /// Rebuilds the exact frame `claude setup-token` writes for the authorization URL.
    ///
    /// Per wrapped display row, verbatim from the capture:
    ///
    /// ```text
    /// ESC]8;id=10jb8ff;<COMPLETE URL>BEL  ESC[37m<80-column slice>ESC[39m  ESC]8;;BEL  \r\r\n
    /// ```
    ///
    /// Then two blank rows and the paste prompt — which Ink positions with cursor-forward
    /// sequences rather than spaces, so it strips to `Pastecodehereifprompted>`.
    fn captured_frame(url: &str) -> String {
        let mut frame = String::new();
        frame.push_str("\u{1b}7\u{1b}[r\u{1b}8\u{1b}[?25h\u{1b}[?25l");
        frame.push_str("\u{1b}[31mWelcome\u{1b}[9Gto\u{1b}[12GClaude\u{1b}[19GCode\r\r\n\r\r\n");
        frame.push_str(
            "\u{1b}[37mBrowser didn't open? Use the url below to sign in (c to copy)\
             \u{1b}[39m\r\r\n\r\r\n",
        );

        // 80 columns, which is the width the daemon gives a container with no size negotiated.
        for slice in url.as_bytes().chunks(80) {
            let slice = std::str::from_utf8(slice).expect("the URL is ASCII");
            frame.push_str("\u{1b}]8;id=10jb8ff;");
            frame.push_str(url);
            frame.push('\u{07}');
            frame.push_str("\u{1b}[37m");
            frame.push_str(slice);
            frame.push_str("\u{1b}[39m\u{1b}]8;;\u{07}\r\r\n");
        }

        frame.push_str("\r\r\n\r\r\n");
        frame.push_str(
            "\u{1b}[2GPaste\u{1b}[8Gcode\u{1b}[13Ghere\u{1b}[18Gif\u{1b}[21Gprompted\
             \u{1b}[30G>\r\r\n",
        );
        frame
    }

    /// The regression test for the bug that shipped in the first draft and was caught only by
    /// running the real image: the service returned `…&client_id=00000000-1111-2222-33`,
    /// truncated at the first wrap, with `state` and `code_challenge` gone.
    #[test]
    fn the_real_captured_frame_yields_the_complete_url() {
        let transcript = TtyTranscript::new(captured_frame(CAPTURED_URL));

        let url = extract_authorization_url(&transcript, URL_PREFIX).expect("url is present");

        assert_eq!(url, CAPTURED_URL);
        assert!(url.contains("state=BBBB"), "the CSRF state survived");
        assert!(
            url.contains("code_challenge=AAAA"),
            "the PKCE challenge survived"
        );
        assert!(!url.contains('\u{1b}'));
    }

    /// The same frame with every OSC 8 sequence removed, which is what a build that stopped
    /// emitting hyperlinks would produce. The fallback has to reassemble the fragments.
    #[test]
    fn the_same_frame_without_hyperlinks_is_rejoined_from_the_wrapped_fragments() {
        let frame = captured_frame(CAPTURED_URL);
        // Strip only the hyperlinks, leaving the wrapped visible text and everything else.
        let without_links = frame
            .split("\u{1b}]8;")
            .enumerate()
            .map(|(index, part)| {
                if index == 0 {
                    part.to_string()
                } else {
                    // Drop the params and URI up to the BEL that terminates them.
                    part.split_once('\u{07}')
                        .map(|(_, rest)| rest.to_string())
                        .unwrap_or_default()
                }
            })
            .collect::<String>();
        assert!(!without_links.contains("\u{1b}]8;"));

        let url = extract_authorization_url(&TtyTranscript::new(without_links), URL_PREFIX)
            .expect("the fallback rejoins the fragments");

        assert_eq!(url, CAPTURED_URL);
    }

    #[test]
    fn osc8_targets_skips_the_closing_sequence_and_a_malformed_one() {
        let raw = "\u{1b}]8;id=1;https://example.com/a\u{07}text\u{1b}]8;;\u{07}\
                   \u{1b}]8;;https://example.com/b\u{1b}\\more";

        assert_eq!(
            osc8_targets(raw),
            vec![
                "https://example.com/a".to_string(),
                // The ESC \ terminator works as well as BEL, and an empty target (the closing
                // sequence) is skipped.
                "https://example.com/b".to_string(),
            ]
        );

        // An unterminated OSC yields nothing rather than swallowing the rest of the stream,
        // which after a successful exchange is the stream carrying the minted token.
        assert!(osc8_targets("\u{1b}]8;id=1;https://example.com/never-ends").is_empty());
        assert!(osc8_targets("no escapes here").is_empty());
    }

    /// Ink positions words with cursor-forward sequences instead of spaces, so the paste prompt
    /// strips to a run with no whitespace in it at all. Any continuation rule that ended a
    /// value at "the first line containing a space" would run straight through it.
    #[test]
    fn the_paste_prompt_strips_to_a_spaceless_run_and_still_ends_the_value() {
        let prompt = "\u{1b}[2GPaste\u{1b}[8Gcode\u{1b}[13Ghere\u{1b}[18Gif\u{1b}[21Gprompted\
                      \u{1b}[30G>";
        assert_eq!(strip_ansi(prompt), "Pastecodehereifprompted>");

        // What ends it is `>`, which cannot appear in a URL.
        let transcript = TtyTranscript::new(format!(
            "https://claude.com/cai/oauth/authorize?state=abc\r\r\n{prompt}\r\r\n"
        ));
        assert_eq!(
            extract_authorization_url(&transcript, URL_PREFIX).expect("url"),
            "https://claude.com/cai/oauth/authorize?state=abc"
        );
    }

    #[test]
    fn strip_ansi_removes_csi_colour_and_cursor_sequences() {
        let raw = "\u{1b}[2K\u{1b}[1;36mhello\u{1b}[0m\u{1b}[H\u{1b}[3Aworld";
        assert_eq!(strip_ansi(raw), "helloworld");
    }

    #[test]
    fn strip_ansi_removes_osc_sequences_with_either_terminator() {
        assert_eq!(strip_ansi("\u{1b}]0;a title\u{07}body"), "body");
        assert_eq!(strip_ansi("\u{1b}]0;a title\u{1b}\\body"), "body");
        // An unterminated OSC is bounded by end of input rather than swallowing everything
        // that follows in some later chunk.
        assert_eq!(strip_ansi("\u{1b}]0;never ends"), "");
    }

    #[test]
    fn strip_ansi_keeps_line_structure_and_drops_other_controls() {
        assert_eq!(
            strip_ansi("a\r\nb\nc\rd\te\u{07}f\u{00}g"),
            "a\r\nb\nc\rd\tefg"
        );
    }

    #[test]
    fn strip_ansi_survives_a_two_character_escape_and_a_trailing_escape() {
        assert_eq!(strip_ansi("\u{1b}7keep\u{1b}8me\u{1b}"), "keepme");
    }

    /// The measured shape: the Ink UI hard-wraps the URL at terminal width, so the query
    /// string is cut mid-parameter with no continuation marker of any kind.
    #[test]
    fn a_url_hard_wrapped_across_three_lines_is_rejoined() {
        let transcript = TtyTranscript::new(concat!(
            "\u{1b}[2K Browse to:\r\n",
            "\u{1b}[36mhttps://claude.com/cai/oauth/authorize?code=true&client_id=9d1c\u{1b}[0m\r\n",
            "3f2a&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.co\r\n",
            "m%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&state=abc-123\r\n",
            "\r\n",
            "Paste code here if prompted > \r\n",
        ));

        let url = extract_authorization_url(&transcript, URL_PREFIX).expect("url is present");

        assert_eq!(
            url,
            "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c3f2a\
             &response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth\
             %2Fcode%2Fcallback&scope=user%3Ainference&state=abc-123"
        );
        // The property that actually matters: the state parameter survived the wrap. A
        // naive single-line match returns a URL truncated at `client_id=9d1c`, which the
        // authorization server rejects with an opaque error.
        assert!(url.contains("state=abc-123"));
    }

    #[test]
    fn the_prompt_line_after_the_url_does_not_get_glued_on() {
        // The prompt contains spaces and `>`, neither of which is a URL character, so the
        // continuation rule stops there even with no blank line in between.
        let transcript = TtyTranscript::new(concat!(
            "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c\r\n",
            "3f2a\r\n",
            "Paste code here if prompted > \r\n",
        ));

        let url = extract_authorization_url(&transcript, URL_PREFIX).expect("url");
        assert_eq!(
            url,
            "https://claude.com/cai/oauth/authorize?state=abc&client_id=9d1c3f2a"
        );
    }

    #[test]
    fn box_drawing_borders_end_the_value() {
        let transcript = TtyTranscript::new(concat!(
            "│ https://claude.com/cai/oauth/authorize?state=abc \r\n",
            "└──────────────────────────────────────────────────┘\r\n",
        ));

        assert_eq!(
            extract_authorization_url(&transcript, URL_PREFIX).expect("url"),
            "https://claude.com/cai/oauth/authorize?state=abc"
        );
    }

    #[test]
    fn trailing_prose_on_the_same_line_is_truncated_away() {
        let transcript = TtyTranscript::new(
            "Open https://claude.com/cai/oauth/authorize?state=abc in a browser",
        );

        assert_eq!(
            extract_authorization_url(&transcript, URL_PREFIX).expect("url"),
            "https://claude.com/cai/oauth/authorize?state=abc"
        );
    }

    #[test]
    fn an_absent_url_is_none_rather_than_an_empty_string() {
        let transcript = TtyTranscript::new("Starting…\r\nLoading configuration\r\n");
        assert!(extract_authorization_url(&transcript, URL_PREFIX).is_none());
    }

    #[test]
    fn a_bare_prefix_with_nothing_after_it_is_not_a_usable_url() {
        // A half-rendered frame can contain the prefix and nothing else. Returning it would
        // advance the runner to `awaiting_authorization` with a URL that goes nowhere.
        let transcript = TtyTranscript::new("https://claude.com/cai/oauth/authorize\r\n\r\n");
        assert!(extract_authorization_url(&transcript, URL_PREFIX).is_none());
    }

    /// The token is scraped by the same rejoin as the URL, because it is printed as plain
    /// text rather than as a hyperlink — so the OSC 8 fast path does not apply to it.
    ///
    /// Every fake credential in this crate's tests is deliberately low-entropy
    /// (`…-fake-aaaa…` rather than a realistic random string). Two reasons: `gitleaks` runs
    /// as a required check and its `generic-api-key` rule fires on a high-entropy value near
    /// the word `token`, and a reader skimming the file should never have to wonder whether
    /// one of these is real.
    #[test]
    fn a_token_wrapped_across_lines_is_rejoined_and_stays_redacted() {
        let transcript = TtyTranscript::new(concat!(
            "\u{1b}[32mSuccess!\u{1b}[0m Your token:\r\n",
            "sk-ant-oat01-fake-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n",
            "aaaaaaaaaaaaaaaaaaaaaaaa\r\n",
            "\r\n",
        ));

        let token = extract_token(&transcript, "sk-ant-").expect("token");

        assert_eq!(
            token.expose(),
            "sk-ant-oat01-fake-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
             aaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(format!("{token:?}"), "MintedToken(<redacted>)");
    }

    #[test]
    fn no_token_before_the_exchange_completes() {
        let transcript = TtyTranscript::new(
            "https://claude.com/cai/oauth/authorize?state=abc\r\nPaste code here if prompted > ",
        );
        assert!(extract_token(&transcript, "sk-ant-").is_none());
    }

    #[test]
    fn failure_markers_are_found_across_a_colour_change() {
        // The CLI renders the word `error` in red, which puts a CSI sequence inside the
        // phrase in the raw stream — so matching must happen after stripping.
        let transcript = TtyTranscript::new(
            "OAuth \u{1b}[31merror\u{1b}[0m: token exchange failed with status code 400",
        );

        assert!(contains_failure_marker(
            &transcript,
            &["OAuth error".to_string()]
        ));
        assert!(!contains_failure_marker(
            &TtyTranscript::new("all good"),
            &["OAuth error".to_string()]
        ));
        // An empty marker must not match everything.
        assert!(!contains_failure_marker(
            &TtyTranscript::new("all good"),
            &[String::new()]
        ));
    }
}
