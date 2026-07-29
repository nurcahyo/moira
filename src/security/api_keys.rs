use aes_gcm::aead::rand_core::{OsRng, RngCore};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng as PasswordOsRng},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::SecretString;

use crate::error::AppError;

use super::masking::secret_fingerprint;

#[derive(Debug, Clone)]
pub struct ApiKeyHasher {
    pepper: Vec<u8>,
    pepper_version: String,
    prefix_length: usize,
}

/// A freshly minted API key. `raw_key` is the only place the plaintext exists — everything
/// persisted is derived from it.
///
/// **`raw_key` is `SecretString` to make disclosure a compile error, not a review question.**
/// Plan 05 found a QA probe that had written `json!({ "raw": generated.raw_key })` into
/// `audit_logs.metadata`, which the admin audit API serialises verbatim; it survived two cleanup
/// commits and was caught only by a leak test. A `String` there is one careless `json!` away from
/// doing it again, and the next one might not be planted by someone who wanted it found.
///
/// The guarantee does not depend on feature flags: `secrecy` implements
/// `Serialize for Secret<T> where T: SerializableSecret`, and `String` never implements that
/// marker — only `CloneableSecret` and `DebugSecret`. So `Secret<String>` cannot be serialised at
/// all, and `#[derive(Debug, Clone)]` below keeps working with `Debug` redacted.
///
/// Reading the plaintext now requires `expose_secret()`, which is greppable: every legitimate
/// disclosure is one search away from an auditor.
#[derive(Debug, Clone)]
pub struct GeneratedApiKey {
    pub raw_key: SecretString,
    pub key_prefix: String,
    pub key_hash: String,
    pub fingerprint: String,
    pub pepper_version: String,
}

impl ApiKeyHasher {
    pub fn new(
        pepper: impl Into<Vec<u8>>,
        pepper_version: impl Into<String>,
        prefix_length: usize,
    ) -> Self {
        Self {
            pepper: pepper.into(),
            pepper_version: pepper_version.into(),
            prefix_length: prefix_length.max(12),
        }
    }

    pub fn generate(&self, namespace: &str) -> Result<GeneratedApiKey, AppError> {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let raw_key = format!("{namespace}_{}", URL_SAFE_NO_PAD.encode(bytes));
        let key_hash = self.hash(&raw_key)?;
        let key_prefix = self.prefix(&raw_key);
        let fingerprint = secret_fingerprint(raw_key.as_bytes());

        Ok(GeneratedApiKey {
            raw_key: SecretString::new(raw_key),
            key_prefix,
            key_hash,
            fingerprint,
            pepper_version: self.pepper_version.clone(),
        })
    }

    pub fn hash(&self, raw_key: &str) -> Result<String, AppError> {
        let salt = SaltString::generate(&mut PasswordOsRng);
        Argon2::default()
            .hash_password(self.peppered(raw_key).as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|err| AppError::Internal(format!("hash api key: {err}")))
    }

    pub fn verify(&self, raw_key: &str, encoded_hash: &str) -> Result<bool, AppError> {
        let parsed = PasswordHash::new(encoded_hash)
            .map_err(|err| AppError::Internal(format!("parse api key hash: {err}")))?;
        Ok(Argon2::default()
            .verify_password(self.peppered(raw_key).as_bytes(), &parsed)
            .is_ok())
    }

    pub fn prefix(&self, raw_key: &str) -> String {
        raw_key.chars().take(self.prefix_length).collect()
    }

    pub fn fingerprint(&self, raw_key: &str) -> String {
        secret_fingerprint(raw_key.as_bytes())
    }

    fn peppered(&self, raw_key: &str) -> String {
        format!("{raw_key}:{}", URL_SAFE_NO_PAD.encode(&self.pepper))
    }
}

#[cfg(test)]
mod tests {
    // Test-local: the production paths in this module never read the plaintext back, so importing
    // this at module scope would be an unused import in a non-test build.
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn generated_key_verifies_with_argon2id_hash() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20);
        let generated = hasher.generate("moira_sys").unwrap();

        assert!(
            hasher
                .verify(generated.raw_key.expose_secret(), &generated.key_hash)
                .unwrap()
        );
        assert!(!hasher.verify("wrong", &generated.key_hash).unwrap());
        assert_eq!(generated.key_prefix.len(), 20);
        assert_eq!(generated.pepper_version, "v1");
    }
}
