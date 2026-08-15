//! The two secrets `moira-runner` handles, each in a newtype that cannot be printed.
//!
//! # Why newtypes rather than discipline
//!
//! Both secrets travel through code that logs: `tracing` fields, `anyhow` contexts, `?`
//! operators onto error types that derive `Debug`, and the `{:?}` a future maintainer adds
//! while debugging. A `String` is one careless interpolation away from a token in a log
//! aggregator, and that is not a mistake code review reliably catches — it is the mistake
//! `secrecy` exists for, and the same reasoning that put `Zeroizing<[u8; 32]>` behind
//! `src/security/key_custody.rs`.
//!
//! So neither type implements `Display`, and both implement `Debug` by hand to print a
//! placeholder. Reading the value requires calling a method whose name says what you are
//! doing, which is a thing a reviewer can see.

use std::fmt;

use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// The OAuth token minted by `claude setup-token` inside a runner container.
///
/// This is the whole point of the service and the most sensitive value it touches: it is a
/// live credential for the operator's Claude account. It leaves the process exactly once, in
/// the body of `GET /v1/runners/{id}/token`, and nowhere else.
pub struct MintedToken(String);

impl MintedToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The only way to read the token. Named so that every call site is greppable and so
    /// that a reviewer seeing it asks where the value is going.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length in bytes, for a metric or an assertion that does not need the value.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for MintedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MintedToken(<redacted>)")
    }
}

impl Drop for MintedToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The bearer token every caller must present on every route except `GET /healthz`.
///
/// Comparison is constant-time. A byte-by-byte `==` on a secret answers "how many leading
/// bytes did you get right" through its timing, which turns a 32-byte secret into 32
/// sequential one-byte searches. That is not a theoretical concern for a service whose whole
/// job is to hand out a credential.
pub struct ControlToken(String);

impl ControlToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Constant-time equality against a candidate presented by a caller.
    ///
    /// `ConstantTimeEq` on byte slices is only defined for equal lengths, so the length
    /// check is unavoidable and is done first. That leaks the *length* of the configured
    /// token and nothing else, which is the standard, accepted trade — the alternative
    /// (hashing both sides first) buys nothing here because the length is already implied by
    /// the configuration's documented minimum.
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let presented = candidate.as_bytes();
        if expected.len() != presented.len() {
            return false;
        }
        expected.ct_eq(presented).into()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of distinct bytes, used by [`super::config`] to reject a trivial token such as
    /// `aaaaaaaa…`. Deliberately here rather than in the config module so the value never
    /// has to be handed out to compute it.
    pub fn distinct_bytes(&self) -> usize {
        let mut seen = [false; 256];
        let mut count = 0;
        for byte in self.0.bytes() {
            if !seen[byte as usize] {
                seen[byte as usize] = true;
                count += 1;
            }
        }
        count
    }

    /// Whether the token is one of the placeholders that ship in example files and
    /// tutorials. An operator who leaves the sample value in place has a service with no
    /// authentication at all, and the failure is silent — so it is refused at startup.
    pub fn is_well_known_placeholder(&self) -> bool {
        const PLACEHOLDERS: &[&str] = &[
            "changeme",
            "change-me",
            "placeholder",
            "secret",
            "password",
            "token",
            "moira",
            "test",
            "example",
        ];
        let lowered = self.0.to_ascii_lowercase();
        PLACEHOLDERS
            .iter()
            .any(|candidate| lowered.contains(candidate))
    }
}

impl fmt::Debug for ControlToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ControlToken(<redacted>)")
    }
}

impl Drop for ControlToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A container's raw tty stream.
///
/// Not a secret in itself, but it *contains* both the authorization URL and — on success —
/// the minted token, so it must never be logged wholesale. It is a newtype for the same
/// reason as the two above: `debug!(?transcript)` should print nothing useful rather than
/// the credential.
pub struct TtyTranscript(String);

impl TtyTranscript {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The only way to read the stream. Callers are [`super::scrape`] and nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for TtyTranscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TtyTranscript(<redacted {} bytes>)", self.0.len())
    }
}

impl Drop for TtyTranscript {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_token_debug_never_shows_the_value() {
        let token = MintedToken::new("sk-ant-oat01-VERY-SECRET-VALUE");

        assert_eq!(format!("{token:?}"), "MintedToken(<redacted>)");
        assert!(!format!("{token:?}").contains("SECRET"));
        assert_eq!(token.expose(), "sk-ant-oat01-VERY-SECRET-VALUE");
    }

    /// The realistic leak is not `{:?}` on the token itself — it is `{:?}` on a struct that
    /// happens to hold one, which is what `#[derive(Debug)]` produces everywhere.
    #[test]
    fn minted_token_stays_redacted_inside_a_derived_debug() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Envelope {
            id: &'static str,
            token: MintedToken,
        }

        let rendered = format!(
            "{:?}",
            Envelope {
                id: "runner-1",
                token: MintedToken::new("sk-ant-oat01-VERY-SECRET-VALUE"),
            }
        );

        assert!(rendered.contains("runner-1"));
        assert!(!rendered.contains("SECRET"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn control_token_debug_never_shows_the_value() {
        let token = ControlToken::new("unit-fixture-control-aaaaaaaaaaaaaaaa");

        assert_eq!(format!("{token:?}"), "ControlToken(<redacted>)");
        assert!(!format!("{token:?}").contains("abcdef"));
    }

    #[test]
    fn transcript_debug_reports_only_a_length() {
        let transcript = TtyTranscript::new("sk-ant-oat01-VERY-SECRET-VALUE");

        assert_eq!(
            format!("{transcript:?}"),
            "TtyTranscript(<redacted 30 bytes>)"
        );
        assert!(!format!("{transcript:?}").contains("SECRET"));
    }

    #[test]
    fn control_token_matches_only_the_exact_value() {
        let token = ControlToken::new("unit-fixture-control-aaaaaaaaaaaaaaaa");

        assert!(token.matches("unit-fixture-control-aaaaaaaaaaaaaaaa"));
        // Same length, one byte different — the case a timing attack exploits.
        assert!(!token.matches("unit-fixture-control-aaaaaaaaaaaaaaab"));
        // A prefix must not match: a length-prefix comparison would let a caller walk the
        // secret one byte at a time.
        assert!(!token.matches("unit-fixture-control-"));
        assert!(!token.matches("unit-fixture-control-aaaaaaaaaaaaaaaaa"));
        assert!(!token.matches(""));
    }

    #[test]
    fn distinct_bytes_counts_the_alphabet_not_the_length() {
        assert_eq!(ControlToken::new("aaaaaaaa").distinct_bytes(), 1);
        assert_eq!(ControlToken::new("abababab").distinct_bytes(), 2);
        assert_eq!(ControlToken::new("abcdefgh").distinct_bytes(), 8);
        assert_eq!(ControlToken::new("").distinct_bytes(), 0);
    }

    #[test]
    fn placeholders_are_recognised_case_insensitively_and_as_substrings() {
        assert!(ControlToken::new("ChangeMe-please-aaaaaaaaaaaaaaaa").is_well_known_placeholder());
        assert!(ControlToken::new("my-super-secret-value-here-12345").is_well_known_placeholder());
        assert!(
            !ControlToken::new("unit-fixture-control-aaaaaaaaaaaaaaaa").is_well_known_placeholder()
        );
    }
}
