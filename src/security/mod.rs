mod api_keys;
mod auth;
mod authz;
mod crypto;
mod idempotency;
mod key_custody;
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
pub use key_custody::{
    CUSTODY_BACKEND_ENVIRONMENT, ENVIRONMENT_WRAP_BLOB_LENGTH, ENVIRONMENT_WRAP_FORMAT_V1,
    EnvironmentMasterKeyCustody, KeyCustodyError, MASTER_KEY_ID_MAX_LENGTH, MasterKeyCustody,
    MasterKeyRing, WRAP_ALGORITHM_AES_256_GCM, WrappedKey, is_valid_master_key_id,
    wrapped_data_key_aad,
};
pub use masking::{mask_plain_secret, mask_secret_value, request_hash, secret_fingerprint};
pub use ssrf::{
    HostResolver, JwksDenialReason, JwksFetchError, OutboundDenialReason, OutboundUrlDenial,
    OutboundUrlPolicy, SystemResolver, fetch_jwks_hardened, is_denied_ip, validate_jwks_url,
    validate_outbound_url,
};
