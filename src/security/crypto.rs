use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

pub const LOCAL_AES_256_GCM: &str = "AES-256-GCM";
pub const ENVELOPE_VERSION_V1: i32 = 1;

pub trait SecretCipher: Send + Sync {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<EncryptedSecret, AppError>;
    fn decrypt(&self, secret: &EncryptedSecret, aad: &[u8]) -> Result<Vec<u8>, AppError>;
}

#[derive(Clone)]
pub struct LocalSecretCipher {
    cipher: Aes256Gcm,
    key_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedSecret {
    pub algorithm: String,
    pub version: i32,
    pub key_id: String,
    pub encrypted_data_key: Option<Vec<u8>>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

impl LocalSecretCipher {
    pub fn new(key: [u8; 32], key_id: impl Into<String>) -> Self {
        Self {
            cipher: Aes256Gcm::new_from_slice(&key).expect("32 byte key"),
            key_id: key_id.into(),
        }
    }
}

impl SecretCipher for LocalSecretCipher {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<EncryptedSecret, AppError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| AppError::Internal("encrypt provider credential".to_string()))?;

        Ok(EncryptedSecret {
            algorithm: LOCAL_AES_256_GCM.to_string(),
            version: ENVELOPE_VERSION_V1,
            key_id: self.key_id.clone(),
            encrypted_data_key: None,
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    fn decrypt(&self, secret: &EncryptedSecret, aad: &[u8]) -> Result<Vec<u8>, AppError> {
        if secret.algorithm != LOCAL_AES_256_GCM || secret.version != ENVELOPE_VERSION_V1 {
            return Err(AppError::Forbidden(
                "unsupported credential encryption envelope".to_string(),
            ));
        }
        self.cipher
            .decrypt(
                Nonce::from_slice(&secret.nonce),
                Payload {
                    msg: secret.ciphertext.as_ref(),
                    aad,
                },
            )
            .map_err(|_| AppError::Forbidden("credential decryption failed".to_string()))
    }
}

/// Which field of a decrypted credential payload holds the provider secret.
///
/// One mapping, one place. Both provider-facing execution paths — completion
/// (`crate::application::execution`) and embedding (`crate::application::conversation`) — read
/// through this, because two copies of a credential-field mapping that drift is a class of bug
/// that presents as "the wrong secret was sent to the provider".
///
/// `None` means the credential type carries no single bearer-style secret and is therefore not
/// usable as a provider credential at all.
pub fn credential_secret_field(
    credential_type: crate::domain::CredentialType,
) -> Option<&'static str> {
    use crate::domain::CredentialType;
    match credential_type {
        CredentialType::ApiKey | CredentialType::AzureOpenAi => Some("api_key"),
        CredentialType::BearerToken => Some("bearer_token"),
        CredentialType::Oauth2 => Some("access_token"),
        CredentialType::BasicAuth
        | CredentialType::CustomHeaders
        | CredentialType::ServiceAccount => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CredentialAadParts<'a> {
    pub credential_id: uuid::Uuid,
    pub provider_id: uuid::Uuid,
    pub credential_type: &'a str,
    pub scope_type: &'a str,
    pub external_tenant_id: Option<&'a str>,
    pub application_id: Option<uuid::Uuid>,
    pub external_user_id: Option<&'a str>,
    pub encryption_version: i32,
}

pub fn credential_aad(parts: CredentialAadParts<'_>) -> String {
    format!(
        "credential_id={credential_id};provider_id={provider_id};credential_type={credential_type};scope_type={scope_type};external_tenant_id={};application_id={};external_user_id={};encryption_version={encryption_version}",
        parts.external_tenant_id.unwrap_or(""),
        parts
            .application_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        parts.external_user_id.unwrap_or(""),
        credential_id = parts.credential_id,
        provider_id = parts.provider_id,
        credential_type = parts.credential_type,
        scope_type = parts.scope_type,
        encryption_version = parts.encryption_version
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_round_trip_binds_aad() {
        let cipher = LocalSecretCipher::new([42; 32], "test");
        let encrypted = cipher.encrypt(b"sk-test", b"provider-a").unwrap();

        assert_eq!(
            cipher.decrypt(&encrypted, b"provider-a").unwrap(),
            b"sk-test".to_vec()
        );
        assert!(cipher.decrypt(&encrypted, b"provider-b").is_err());
        assert_ne!(encrypted.ciphertext, b"sk-test");
        assert_eq!(encrypted.version, ENVELOPE_VERSION_V1);
    }
}
