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

pub fn mask_plain_secret(secret: &str) -> String {
    let visible_suffix: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if visible_suffix.is_empty() {
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
}
