use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn secret_fingerprint(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    URL_SAFE_NO_PAD.encode(digest)
}

pub fn request_hash(bytes: &[u8]) -> String {
    secret_fingerprint(bytes)
}

pub fn mask_secret_value(value: &Value) -> String {
    match value {
        Value::String(secret) => mask_plain_secret(secret),
        _ => "structured-secret".to_string(),
    }
}

/// Renders a secret as `****` plus at most its last four characters.
///
/// The suffix is only revealed when it is *strictly shorter* than the secret.
/// A four-character window over a secret of four characters or fewer would
/// otherwise reproduce the whole plaintext — `"abcd"` masking to `"****abcd"` —
/// and the result is persisted to `credentials.masked_secret` and returned by
/// the admin credential read APIs, so that is full disclosure rather than
/// masking. Short secrets therefore mask to a bare `****` with no hint.
///
/// The window is sized in `chars`, not bytes, so multi-byte input neither
/// widens it nor panics on a slice boundary.
pub fn mask_plain_secret(secret: &str) -> String {
    let visible_suffix: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if visible_suffix.is_empty() || visible_suffix.chars().count() == secret.chars().count() {
        "****".to_string()
    } else {
        format!("****{visible_suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_keeps_only_suffix() {
        assert_eq!(mask_plain_secret("sk-123456"), "****3456");
        assert_eq!(mask_plain_secret(""), "****");
    }

    #[test]
    fn fingerprints_are_stable_without_exposing_secret() {
        let one = secret_fingerprint(b"secret");
        let two = secret_fingerprint(b"secret");
        assert_eq!(one, two);
        assert!(!one.contains("secret"));
    }

    /// The property `mask_plain_secret` exists to guarantee, over the input
    /// shapes `masking_keeps_only_suffix` never reaches: secrets shorter than
    /// the four-character suffix window, a secret exactly as long as it, and
    /// multi-byte input where `.chars()` — not bytes — sizes the window.
    #[test]
    fn masked_secret_never_contains_the_original_plaintext() {
        for secret in [
            "a",
            "ab",
            "abc",
            "abcd",
            "秘密",
            "sk-ключ-秘密鍵",
            "sk-live-0123456789abcdef0123456789abcdef",
        ] {
            let masked = mask_plain_secret(secret);
            assert!(
                masked.starts_with("****"),
                "{secret:?} must be masked, got {masked:?}"
            );
            assert!(
                !masked.contains(secret),
                "masked form {masked:?} still contains the whole plaintext {secret:?}"
            );
        }

        // The window is four *characters* wide even when each one is multi-byte.
        assert_eq!(mask_plain_secret("sk-ключ-秘密鍵"), "****-秘密鍵");
    }

    #[test]
    fn structured_secret_values_are_masked_without_serializing_their_contents() {
        let structured = [
            serde_json::json!({"api_key": "sk-live-0123456789abcdef", "region": "eu"}),
            serde_json::json!(["sk-live-0123456789abcdef", "AKIAIOSFODNN7EXAMPLE"]),
            serde_json::json!({"nested": {"bearer_token": "AKIAIOSFODNN7EXAMPLE"}}),
            serde_json::json!(1234567890123456i64),
            serde_json::json!(true),
            serde_json::json!(null),
        ];
        for value in structured {
            let masked = mask_secret_value(&value);
            assert_eq!(
                masked, "structured-secret",
                "value {value:?} must be opaque"
            );
            for fragment in [
                "sk-live",
                "0123456789abcdef",
                "AKIAIOSFODNN7EXAMPLE",
                "api_key",
                "bearer_token",
                "1234567890123456",
                "true",
                "null",
            ] {
                assert!(
                    !masked.contains(fragment),
                    "masked form {masked:?} leaked {fragment:?} from {value:?}"
                );
            }
        }

        // The string branch keeps delegating to `mask_plain_secret`.
        assert_eq!(
            mask_secret_value(&Value::String("sk-123456".to_string())),
            "****3456"
        );
    }

    #[test]
    fn secret_fingerprint_is_stable_and_does_not_embed_the_input() {
        let secret = "sk-live-0123456789abcdef";
        let fingerprint = secret_fingerprint(secret.as_bytes());

        assert_eq!(fingerprint, secret_fingerprint(secret.as_bytes()));
        assert_eq!(fingerprint, request_hash(secret.as_bytes()));
        assert_ne!(fingerprint, secret_fingerprint(b"sk-live-0123456789abcdeg"));

        // URL-safe, unpadded base64 of a 32-byte SHA-256 digest: fixed width,
        // no padding, so the length cannot leak the input length.
        assert_eq!(fingerprint.len(), 43);
        assert_eq!(secret_fingerprint(b"").len(), fingerprint.len());
        assert!(!fingerprint.contains('='));

        assert!(!fingerprint.contains(secret));
        let characters: Vec<char> = secret.chars().collect();
        for window in characters.windows(4) {
            let fragment: String = window.iter().collect();
            assert!(
                !fingerprint.contains(&fragment),
                "fingerprint {fingerprint:?} embeds {fragment:?} from the input"
            );
        }
    }
}
