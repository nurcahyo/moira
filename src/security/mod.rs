mod api_keys;
mod auth;
mod authz;
mod crypto;
mod idempotency;
mod masking;
mod ssrf;

pub use api_keys::{
    ApiKeyHasher, GeneratedApiKey, KEY_NAMESPACES, MIN_API_KEY_PREFIX_LENGTH,
    MIN_RANDOM_PREFIX_CHARS, is_registered_key_namespace,
};
/// Re-exported for `src/http/identity.rs` (plan 07 module 11), which reads
/// `X-Moira-System-Key` directly instead of going through `authenticate_admin`. Mirroring
/// the parsing there instead would be a second implementation of a one-line rule.
pub(crate) use auth::header_string;
pub use auth::{
    Actor, ActorType, AdminAuthenticator, AuthService, CallerAuthenticator, JwksCache,
    TrustedJwtIdentity,
};
pub use authz::AuthorizationService;
pub use crypto::{
    CredentialAadParts, ENVELOPE_VERSION_V1, EncryptedSecret, LOCAL_AES_256_GCM, LocalSecretCipher,
    SecretCipher, credential_aad, credential_secret_field,
};
pub use idempotency::IdempotencyHasher;
pub use masking::{mask_plain_secret, mask_secret_value, request_hash, secret_fingerprint};
pub use ssrf::{
    JwksDenialReason, JwksFetchError, fetch_jwks_hardened, is_denied_ip, validate_jwks_url,
};
