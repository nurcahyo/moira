//! Master-key custody — the pluggable seam between Moira and whatever holds the key that
//! wraps a content data key.
//!
//! Nothing in this module reads or writes an `*_encrypted` column. It ships the seam and the
//! first backend one full release ahead of any behaviour change, so operators can set the new
//! variable before the release that requires it (`docs/decision-encryption-at-rest.md` §10).
//!
//! **Why `wrap`/`unwrap` and not `get_master_key_bytes()`.** AWS KMS never releases key
//! material; its API is `Encrypt`/`Decrypt` with an `EncryptionContext`. These two methods map
//! one-to-one onto that (`aad` → `EncryptionContext`), onto Vault Transit's `encrypt`/`decrypt`,
//! and onto a local AES key. A byte-fetching seam would have admitted only environment variables
//! and Vault KV, and would have needed redesigning the day KMS arrived — which is the one thing
//! this seam exists to prevent.
//!
//! **Why `async` when the environment implementation never awaits.** Making it synchronous now
//! and asynchronous later is a breaking change to every caller at exactly the moment the swap is
//! supposed to be cheap. It is called single-digit times per process lifetime.

use std::collections::HashMap;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The only wrap algorithm the environment backend speaks. A KMS or Vault backend records its
/// own string here (`"aws-kms:symmetric_default"`, say), which is why it is carried per wrapped
/// key rather than assumed.
pub const WRAP_ALGORITHM_AES_256_GCM: &str = "AES-256-GCM";

/// Backend name of [`EnvironmentMasterKeyCustody`], recorded per keyring row so a later backend
/// swap is legible in the data.
pub const CUSTODY_BACKEND_ENVIRONMENT: &str = "environment";

/// Leading byte of the environment backend's serialised wrap output.
pub const ENVIRONMENT_WRAP_FORMAT_V1: u8 = 0x01;

/// AES-256-GCM nonce width, and the width of [`WrappedKey::nonce`] for this backend.
const WRAP_NONCE_LENGTH: usize = 12;

/// 32 key bytes + a 16-byte GCM tag.
const WRAPPED_DEK_LENGTH: usize = 48;

/// `1` version byte + `12` nonce bytes + `48` ciphertext-and-tag bytes.
///
/// Opaque outside this backend: a different backend produces different bytes, which is fine
/// because `custody_backend` and `wrap_algorithm` are recorded per keyring row.
pub const ENVIRONMENT_WRAP_BLOB_LENGTH: usize = 1 + WRAP_NONCE_LENGTH + WRAPPED_DEK_LENGTH;

/// Upper bound on a master key id. The ids reach a `varchar(128)` column and log lines.
pub const MASTER_KEY_ID_MAX_LENGTH: usize = 128;

/// Every master key this process can unwrap with, keyed by id.
///
/// A *map*, never a single key — see [`EnvironmentMasterKeyCustody`] for the defect that shape
/// exists to make unreachable.
pub type MasterKeyRing = HashMap<String, Zeroizing<[u8; 32]>>;

/// A data key sealed under a custody master key.
///
/// `Debug` is hand-written: it prints ids and lengths only. The wrapped bytes are not plaintext
/// key material, but a `Debug` that renders them turns any accidental `{:?}` on a keyring row
/// into a ciphertext dump in a log aggregator, and the id is the only part an operator ever
/// needs to read.
#[derive(Clone)]
pub struct WrappedKey {
    pub master_key_id: String,
    /// `"AES-256-GCM"` today; `"aws-kms:symmetric_default"` later.
    pub wrap_algorithm: String,
    /// Empty for backends carrying their own framing (KMS, Vault Transit), whose ciphertext
    /// already embeds every parameter needed to reverse it.
    pub nonce: Vec<u8>,
    pub wrapped: Vec<u8>,
}

impl std::fmt::Debug for WrappedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WrappedKey")
            .field("master_key_id", &self.master_key_id)
            .field("wrap_algorithm", &self.wrap_algorithm)
            .field("nonce_len", &self.nonce.len())
            .field("wrapped_len", &self.wrapped.len())
            .finish()
    }
}

/// The three failures a custody backend can have, and their shapes are deliberate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCustodyError {
    /// **Names the id.** A key id is not a secret, and it is the one thing an operator needs
    /// mid-rotation: the message has to say which key the stored blob wants.
    UnknownMasterKey { master_key_id: String },
    /// Retryable: the backend exists and the key is configured, but the backend could not be
    /// reached or refused to serve this call.
    Unavailable { backend: &'static str },
    /// Deliberately does not say why. "Wrong key" versus "tampered blob" is an oracle, so the
    /// two are reported identically.
    UnwrapFailed,
}

impl std::fmt::Display for KeyCustodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMasterKey { master_key_id } => write!(
                f,
                "master key {master_key_id:?} is not configured; add it to \
                 MOIRA_CONTENT_ENCRYPTION__KEYS as \"{master_key_id}:<base64>\""
            ),
            Self::Unavailable { backend } => {
                write!(f, "{backend} key custody backend is unavailable")
            }
            Self::UnwrapFailed => f.write_str("data key unwrap failed"),
        }
    }
}

impl std::error::Error for KeyCustodyError {}

/// The seam. Chosen for the backend that does *not* exist yet, on purpose.
#[async_trait::async_trait]
pub trait MasterKeyCustody: Send + Sync + std::fmt::Debug {
    /// `"environment"` | `"aws_kms"` | `"vault_transit"`.
    fn backend_name(&self) -> &'static str;
    /// The id new data keys are wrapped under.
    fn active_master_key_id(&self) -> &str;
    /// Whether a previously wrapped key under `master_key_id` can still be opened. Old master
    /// keys stay loaded during a rotation, which is what makes already-wrapped DEKs readable
    /// with no re-encryption.
    fn can_unwrap(&self, master_key_id: &str) -> bool;

    /// The `wrap_algorithm` this backend records on every key it wraps —
    /// `"AES-256-GCM"` here, `"aws-kms:symmetric_default"` for a KMS backend.
    ///
    /// Declared *before* a wrap because the algorithm is part of the wrapped-key AAD (see
    /// [`wrapped_data_key_aad`]), and an AAD has to be complete before the call it
    /// authenticates. Without it, a rewrap onto a backend whose algorithm string differs would
    /// authenticate `wrap_algorithm=AES-256-GCM` and then store a row saying otherwise — a
    /// blob nothing could ever open, written silently.
    ///
    /// It is a declaration, so callers verify it: [`crate::security::KeyringAdmin::rewrap_with`]
    /// refuses a backend whose [`Self::wrap_under`] returns something other than this.
    fn wrap_algorithm(&self) -> &'static str;

    /// Every master key id this backend is *configured* to open, sorted.
    ///
    /// Not "every key the backend could theoretically reach" — a KMS backend answers with the
    /// aliases it was configured with, not with an inventory call. The distinction matters
    /// because of the one thing this method exists for: when a data key cannot be unwrapped,
    /// the boot refusal has to print the ids that *are* present next to the id that is wanted.
    /// An operator staring at "master key `prod-2026-06` is not configured" cannot act on it
    /// without knowing that what they configured was `prod-2026-05` — that one line is the
    /// difference between a five-minute fix and an outage spent guessing.
    ///
    /// There is no default implementation, deliberately: a backend that answered with an empty
    /// list would silently make that refusal useless.
    fn master_key_ids(&self) -> Vec<String>;

    async fn wrap(
        &self,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError>;

    /// Wrap under a **named** master key rather than under the active one.
    ///
    /// This is what `moira keyring rewrap --to <id>` needs and what [`Self::wrap`] cannot
    /// express. Step 2 of the master-key rotation in
    /// `docs/decision-encryption-at-rest.md` §9 is deliberately performed *before*
    /// `ACTIVE_KEY_ID` moves — the new key is added to `MOIRA_CONTENT_ENCRYPTION__KEYS`, the
    /// keyring rows are re-wrapped under it, and only then is it promoted. So the one moment
    /// the target id is needed is precisely the moment it is **not** the active id, and a
    /// seam that could only wrap under "whatever is active" would force the operator to
    /// promote first and rewrap second — the ordering that leaves a booting instance unable
    /// to open rows it must open.
    ///
    /// Maps onto every backend the seam was chosen for: KMS `Encrypt` already takes a
    /// `KeyId`, and Vault Transit already names its key in the path. An implementation must
    /// refuse an id it does not hold with [`KeyCustodyError::UnknownMasterKey`] rather than
    /// silently falling back to the active key — that fallback is exactly the
    /// `LocalSecretCipher` defect this module exists not to reproduce.
    async fn wrap_under(
        &self,
        master_key_id: &str,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError>;

    async fn unwrap(
        &self,
        wrapped: &WrappedKey,
        aad: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError>;

    /// Boot probe. Must prove the backend is *usable*, not merely configured: wrap and unwrap a
    /// throwaway key under the active master key.
    ///
    /// The difference between failing here and failing on some user's first read is the whole
    /// reason it runs before the listener binds.
    async fn preflight(&self) -> Result<(), KeyCustodyError>;
}

/// **A private module, and the privacy is the entire mechanism.**
///
/// [`PreflightedCustody`]'s field is visible only in here, and the only thing in here that
/// produces one is [`PreflightedCustody::preflight`], which cannot return without having awaited
/// [`MasterKeyCustody::preflight`]. Nothing outside this module — not the rest of this file, not
/// `app::state`, not a test — can write the struct literal. That is what makes "the boot probe
/// ran" a fact the type carries rather than a call site somebody has to remember.
///
/// Do not add a `From<Arc<dyn MasterKeyCustody>>`, a `pub` field, or a second constructor: each
/// of those reopens the gap this module exists to close (issue #161).
mod preflighted {
    use std::sync::Arc;

    use super::{KeyCustodyError, MasterKeyCustody};

    /// A custody backend whose boot probe has *already run and passed*.
    ///
    /// Held by `AppState` and produced by `app::state::build_content_custody` so that skipping
    /// the probe is a compile error rather than a silent behaviour change — the failure mode
    /// being a deployment that boots happily and only discovers its master key is unusable at
    /// the first write.
    #[derive(Clone, Debug)]
    pub struct PreflightedCustody(Arc<dyn MasterKeyCustody>);

    impl PreflightedCustody {
        /// Runs [`MasterKeyCustody::preflight`] and, only on success, hands back the proof.
        ///
        /// The sole constructor. It takes the `Arc` by value rather than by reference so the
        /// unproven handle is consumed at the call site.
        pub async fn preflight(
            custody: Arc<dyn MasterKeyCustody>,
        ) -> Result<Self, KeyCustodyError> {
            custody.preflight().await?;
            Ok(Self(custody))
        }

        /// The handle underneath, for the callers that need an `Arc<dyn MasterKeyCustody>` —
        /// `ContentKeyring::load` and `KeyringAdmin::new`.
        ///
        /// Extraction is deliberately unrestricted: the invariant is about *how a custody
        /// reaches the running process*, not about hiding it afterwards.
        pub fn custody(&self) -> Arc<dyn MasterKeyCustody> {
            self.0.clone()
        }
    }

    impl std::ops::Deref for PreflightedCustody {
        type Target = dyn MasterKeyCustody;

        fn deref(&self) -> &Self::Target {
            self.0.as_ref()
        }
    }
}

pub use preflighted::PreflightedCustody;

/// Additional authenticated data binding a wrapped data key to its identity.
///
/// Semicolon-joined, matching the `credential_aad` house style in [`crate::security::crypto`].
///
/// **The custody backend name is deliberately absent, and this omission is load-bearing.** If a
/// future Vault KV deployment, a CSI driver, or an external-secrets operator serves the *same*
/// 32 bytes under the *same* key id — the common environment → Vault case — then swapping
/// custody requires **zero writes to any table**. Binding the backend name here would force a
/// rewrap of every keyring row for no cryptographic gain whatsoever. Do not "fix" this by
/// adding `custody_backend=`.
pub fn wrapped_data_key_aad(
    data_key_id: Uuid,
    master_key_id: &str,
    wrap_algorithm: &str,
) -> String {
    format!(
        "moira/data-key/v1;data_key_id={data_key_id};master_key_id={master_key_id};\
         wrap_algorithm={wrap_algorithm}"
    )
}

/// Whether `id` is a usable master key id: `[A-Za-z0-9._-]{1,128}`.
///
/// Constrained because the ids reach a `varchar(128)` column and log lines, and because neither
/// `,` nor `:` may appear in one — those two separate the entries of
/// `MOIRA_CONTENT_ENCRYPTION__KEYS`.
pub fn is_valid_master_key_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MASTER_KEY_ID_MAX_LENGTH
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Master keys held in this process, keyed by id.
///
/// **The map is the whole difference** from [`crate::security::LocalSecretCipher`], which stamps
/// `key_id` onto an `EncryptedSecret` on encrypt and never reads it on decrypt — so rotating
/// `MOIRA_SECRETS__MASTER_KEY_BASE64` today silently orphans every `provider_credentials` row.
/// This work does not fix that defect and does not claim to; `SecretCipher` and
/// `LocalSecretCipher` are left untouched. What it does is make the new subsystem structurally
/// incapable of reproducing it: [`Self::unwrap`] selects on `wrapped.master_key_id` and refuses
/// by name when that id is absent.
///
/// Key material is `Zeroizing<[u8; 32]>` — the stronger of the two patterns already in the tree,
/// chosen on purpose. `Debug` prints ids only.
pub struct EnvironmentMasterKeyCustody {
    keys: MasterKeyRing,
    active_master_key_id: String,
}

impl std::fmt::Debug for EnvironmentMasterKeyCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sorted so the rendering is stable across runs; a `HashMap` iteration order that
        // changes per process makes log diffs unreadable.
        let mut key_ids: Vec<&str> = self.keys.keys().map(String::as_str).collect();
        key_ids.sort_unstable();
        f.debug_struct("EnvironmentMasterKeyCustody")
            .field("backend", &CUSTODY_BACKEND_ENVIRONMENT)
            .field("active_master_key_id", &self.active_master_key_id)
            .field("master_key_ids", &key_ids)
            .finish()
    }
}

impl EnvironmentMasterKeyCustody {
    /// Fails if `active_master_key_id` is not one of `keys`.
    ///
    /// `Settings::validate` already rejects that combination with a message naming the field,
    /// so reaching this error means something built a custody by hand. It is still refused
    /// here: a backend whose active key is absent cannot wrap anything, and discovering that on
    /// the first write is the failure mode `preflight` exists to move to boot.
    pub fn new(
        keys: MasterKeyRing,
        active_master_key_id: impl Into<String>,
    ) -> Result<Self, KeyCustodyError> {
        let active_master_key_id = active_master_key_id.into();
        if !keys.contains_key(&active_master_key_id) {
            return Err(KeyCustodyError::UnknownMasterKey {
                master_key_id: active_master_key_id,
            });
        }
        Ok(Self {
            keys,
            active_master_key_id,
        })
    }

    /// Serialises a wrapped key this backend produced into its on-disk form.
    ///
    /// ```text
    /// [0]      0x01     wrap format version
    /// [1..13]  nonce    12 random bytes
    /// [13..61] AES-256-GCM(dek) || tag
    /// ```
    ///
    /// Self-framed, so a keyring row needs no sibling nonce column: `master_key_id`,
    /// `custody_backend` and `wrap_algorithm` are the only columns the row carries, and this
    /// blob supplies everything else needed to reverse it.
    pub fn encode_wrapped(wrapped: &WrappedKey) -> Result<Vec<u8>, KeyCustodyError> {
        if wrapped.wrap_algorithm != WRAP_ALGORITHM_AES_256_GCM
            || wrapped.nonce.len() != WRAP_NONCE_LENGTH
            || wrapped.wrapped.len() != WRAPPED_DEK_LENGTH
        {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        let mut blob = Vec::with_capacity(ENVIRONMENT_WRAP_BLOB_LENGTH);
        blob.push(ENVIRONMENT_WRAP_FORMAT_V1);
        blob.extend_from_slice(&wrapped.nonce);
        blob.extend_from_slice(&wrapped.wrapped);
        Ok(blob)
    }

    /// Reverses [`Self::encode_wrapped`]. `master_key_id` comes from the keyring row, not from
    /// the blob: which key sealed a row is a column, so that a keyring scan can answer "is every
    /// key I need configured?" without decoding anything.
    pub fn decode_wrapped(
        master_key_id: impl Into<String>,
        blob: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        if blob.len() != ENVIRONMENT_WRAP_BLOB_LENGTH || blob[0] != ENVIRONMENT_WRAP_FORMAT_V1 {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        Ok(WrappedKey {
            master_key_id: master_key_id.into(),
            wrap_algorithm: WRAP_ALGORITHM_AES_256_GCM.to_string(),
            nonce: blob[1..1 + WRAP_NONCE_LENGTH].to_vec(),
            wrapped: blob[1 + WRAP_NONCE_LENGTH..].to_vec(),
        })
    }

    fn cipher(&self, master_key_id: &str) -> Result<Aes256Gcm, KeyCustodyError> {
        let key =
            self.keys
                .get(master_key_id)
                .ok_or_else(|| KeyCustodyError::UnknownMasterKey {
                    master_key_id: master_key_id.to_string(),
                })?;
        Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| KeyCustodyError::Unavailable {
            backend: CUSTODY_BACKEND_ENVIRONMENT,
        })
    }
}

#[async_trait::async_trait]
impl MasterKeyCustody for EnvironmentMasterKeyCustody {
    fn backend_name(&self) -> &'static str {
        CUSTODY_BACKEND_ENVIRONMENT
    }

    fn active_master_key_id(&self) -> &str {
        &self.active_master_key_id
    }

    fn can_unwrap(&self, master_key_id: &str) -> bool {
        self.keys.contains_key(master_key_id)
    }

    fn wrap_algorithm(&self) -> &'static str {
        WRAP_ALGORITHM_AES_256_GCM
    }

    fn master_key_ids(&self) -> Vec<String> {
        // Sorted for the same reason `Debug` sorts: a `HashMap` iteration order that changes
        // per process makes two operators comparing two refusals unable to compare them.
        let mut ids: Vec<String> = self.keys.keys().cloned().collect();
        ids.sort_unstable();
        ids
    }

    async fn wrap(
        &self,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        // Defined in terms of `wrap_under` rather than beside it, so "the active key" is a
        // single argument to one implementation instead of a second copy of the AES-GCM
        // call that could drift from it.
        self.wrap_under(&self.active_master_key_id, dek, aad).await
    }

    async fn wrap_under(
        &self,
        master_key_id: &str,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        // Selected on the *requested* id. An id this backend does not hold is refused by
        // name here, never substituted with the active key.
        let cipher = self.cipher(master_key_id)?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let wrapped = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: dek.as_slice(),
                    aad,
                },
            )
            .map_err(|_| KeyCustodyError::Unavailable {
                backend: CUSTODY_BACKEND_ENVIRONMENT,
            })?;

        Ok(WrappedKey {
            master_key_id: master_key_id.to_string(),
            wrap_algorithm: WRAP_ALGORITHM_AES_256_GCM.to_string(),
            nonce: nonce.to_vec(),
            wrapped,
        })
    }

    async fn unwrap(
        &self,
        wrapped: &WrappedKey,
        aad: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError> {
        // Selected on the *stored* id, never on the active one. This is the line the
        // `LocalSecretCipher` defect is missing.
        let cipher = self.cipher(&wrapped.master_key_id)?;
        if wrapped.wrap_algorithm != WRAP_ALGORITHM_AES_256_GCM
            || wrapped.nonce.len() != WRAP_NONCE_LENGTH
        {
            return Err(KeyCustodyError::UnwrapFailed);
        }

        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&wrapped.nonce),
                    Payload {
                        msg: wrapped.wrapped.as_ref(),
                        aad,
                    },
                )
                .map_err(|_| KeyCustodyError::UnwrapFailed)?,
        );

        let mut dek = Zeroizing::new([0u8; 32]);
        if plaintext.len() != dek.len() {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        dek.copy_from_slice(&plaintext);
        Ok(dek)
    }

    async fn preflight(&self) -> Result<(), KeyCustodyError> {
        let mut probe = Zeroizing::new([0u8; 32]);
        probe.copy_from_slice(Aes256Gcm::generate_key(&mut OsRng).as_slice());

        // A throwaway id, so the probe's AAD is the same *shape* a real data key gets and
        // cannot collide with one.
        let aad = wrapped_data_key_aad(
            Uuid::now_v7(),
            &self.active_master_key_id,
            WRAP_ALGORITHM_AES_256_GCM,
        );
        let wrapped = self.wrap(&probe, aad.as_bytes()).await?;
        let unwrapped = self.unwrap(&wrapped, aad.as_bytes()).await?;

        // Proving the backend is *usable* means the bytes come back, not merely that no error
        // was raised.
        if unwrapped.as_slice() != probe.as_slice() {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custody(entries: &[(&str, u8)], active: &str) -> EnvironmentMasterKeyCustody {
        let keys = entries
            .iter()
            .map(|(id, byte)| ((*id).to_string(), Zeroizing::new([*byte; 32])))
            .collect();
        EnvironmentMasterKeyCustody::new(keys, active).expect("custody")
    }

    fn aad(master_key_id: &str) -> String {
        wrapped_data_key_aad(
            Uuid::from_u128(0x2f1a_5c4e_9d2b_4f7a_8c11_6b0d_5e7a_2f93),
            master_key_id,
            WRAP_ALGORITHM_AES_256_GCM,
        )
    }

    #[tokio::test]
    async fn wrap_round_trips_and_binds_the_aad() {
        let custody = custody(&[("k1", 1)], "k1");
        let dek = Zeroizing::new([9u8; 32]);
        let wrapped = custody.wrap(&dek, aad("k1").as_bytes()).await.unwrap();

        assert_eq!(wrapped.master_key_id, "k1");
        assert_eq!(wrapped.wrap_algorithm, WRAP_ALGORITHM_AES_256_GCM);
        assert_eq!(wrapped.nonce.len(), WRAP_NONCE_LENGTH);
        assert_eq!(wrapped.wrapped.len(), WRAPPED_DEK_LENGTH);
        // The wrapped bytes are not the key: a wrap that forgot to encrypt would still pass
        // every length assertion above.
        assert!(!wrapped.wrapped.windows(32).any(|w| w == dek.as_slice()));

        assert_eq!(
            custody
                .unwrap(&wrapped, aad("k1").as_bytes())
                .await
                .unwrap()
                .as_slice(),
            dek.as_slice()
        );
        // A different AAD must not open it — otherwise the data_key_id / master_key_id binding
        // is decorative and a blob could be replayed under another key's identity.
        assert_eq!(
            custody
                .unwrap(&wrapped, aad("k2").as_bytes())
                .await
                .unwrap_err(),
            KeyCustodyError::UnwrapFailed
        );
    }

    #[tokio::test]
    async fn a_non_active_but_still_configured_master_key_still_unwraps() {
        // The rotation case: `old` sealed the row, `new` is active. Nothing is re-encrypted.
        let old = custody(&[("old", 1)], "old");
        let dek = Zeroizing::new([9u8; 32]);
        let wrapped = old.wrap(&dek, aad("old").as_bytes()).await.unwrap();

        let rotated = custody(&[("old", 1), ("new", 2)], "new");
        assert_eq!(rotated.active_master_key_id(), "new");
        assert!(rotated.can_unwrap("old"));
        assert_eq!(
            rotated
                .unwrap(&wrapped, aad("old").as_bytes())
                .await
                .unwrap()
                .as_slice(),
            dek.as_slice()
        );

        // And the converse, which is what makes the assertion above mean something: a custody
        // that dropped `old` refuses by name rather than decrypting with whatever key it holds.
        let dropped = custody(&[("new", 2)], "new");
        assert!(!dropped.can_unwrap("old"));
        assert_eq!(
            dropped
                .unwrap(&wrapped, aad("old").as_bytes())
                .await
                .unwrap_err(),
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "old".to_string()
            }
        );
    }

    /// `rewrap --to <id>` runs while `<id>` is held but **not yet active** — that is the whole
    /// of step 2 of the master-key rotation. Wrapping under the active key instead would seal
    /// the keyring under the key the operator is retiring.
    #[tokio::test]
    async fn wrap_under_seals_beneath_a_named_key_that_is_not_the_active_one() {
        // Exactly the R2 step-2 configuration: both keys held, `old` still active.
        let rotating = custody(&[("old", 1), ("new", 2)], "old");
        let dek = Zeroizing::new([9u8; 32]);

        let wrapped = rotating
            .wrap_under("new", &dek, aad("new").as_bytes())
            .await
            .unwrap();
        assert_eq!(wrapped.master_key_id, "new");

        // The recorded id is not merely a label: a custody holding *only* `new` opens it, and
        // one holding only `old` does not. Without both halves this would pass against an
        // implementation that wrapped under `old` and stamped the row `"new"` — which is the
        // precise bug that makes step 4 of the rotation unrecoverable.
        let only_new = custody(&[("new", 2)], "new");
        assert_eq!(
            only_new
                .unwrap(&wrapped, aad("new").as_bytes())
                .await
                .unwrap()
                .as_slice(),
            dek.as_slice()
        );
        let only_old = custody(&[("old", 1)], "old");
        assert_eq!(
            only_old
                .unwrap(&wrapped, aad("new").as_bytes())
                .await
                .unwrap_err(),
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "new".to_string()
            }
        );

        // And `wrap` is still "wrap under the active key" — the two must not have drifted.
        let active = rotating.wrap(&dek, aad("old").as_bytes()).await.unwrap();
        assert_eq!(active.master_key_id, "old");
    }

    #[tokio::test]
    async fn wrap_under_refuses_an_id_it_does_not_hold_rather_than_using_the_active_key() {
        let custody = custody(&[("k1", 1)], "k1");
        // Silently wrapping under `k1` here would write a row claiming `absent` sealed it,
        // and boot would then refuse for a key that was never used.
        assert_eq!(
            custody
                .wrap_under(
                    "absent",
                    &Zeroizing::new([9u8; 32]),
                    aad("absent").as_bytes()
                )
                .await
                .unwrap_err(),
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "absent".to_string()
            }
        );
    }

    #[tokio::test]
    async fn unwrap_with_an_unconfigured_master_key_names_the_id() {
        let custody = custody(&[("k1", 1)], "k1");
        let wrapped = WrappedKey {
            master_key_id: "retired-2025".to_string(),
            wrap_algorithm: WRAP_ALGORITHM_AES_256_GCM.to_string(),
            nonce: vec![0; WRAP_NONCE_LENGTH],
            wrapped: vec![0; WRAPPED_DEK_LENGTH],
        };

        let err = custody
            .unwrap(&wrapped, aad("retired-2025").as_bytes())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "retired-2025".to_string()
            }
        );
        // The id is the one thing an operator needs mid-rotation, so it has to survive into the
        // rendered message and not only into the variant.
        let rendered = err.to_string();
        assert!(
            rendered.contains("retired-2025")
                && rendered.contains("MOIRA_CONTENT_ENCRYPTION__KEYS"),
            "unhelpful unknown-key message: {rendered}"
        );
    }

    #[tokio::test]
    async fn unwrap_failure_never_says_why() {
        let sealer = custody(&[("k1", 1)], "k1");
        let dek = Zeroizing::new([9u8; 32]);
        let wrapped = sealer.wrap(&dek, aad("k1").as_bytes()).await.unwrap();

        // Wrong key, same id.
        let imposter = custody(&[("k1", 200)], "k1");
        let wrong_key = imposter
            .unwrap(&wrapped, aad("k1").as_bytes())
            .await
            .unwrap_err();

        // Tampered blob, right key.
        let mut tampered = wrapped.clone();
        tampered.wrapped[0] ^= 0xff;
        let tampered = sealer
            .unwrap(&tampered, aad("k1").as_bytes())
            .await
            .unwrap_err();

        // Identical, deliberately: telling the two apart is an oracle.
        assert_eq!(wrong_key, KeyCustodyError::UnwrapFailed);
        assert_eq!(tampered, KeyCustodyError::UnwrapFailed);
        assert_eq!(wrong_key.to_string(), tampered.to_string());
    }

    #[tokio::test]
    async fn preflight_proves_the_active_key_round_trips() {
        assert!(custody(&[("k1", 1)], "k1").preflight().await.is_ok());
    }

    #[tokio::test]
    async fn preflight_fails_when_the_active_key_cannot_wrap() {
        // The only way to reach an unusable active key past `new`'s guard is to build the
        // struct with one, which is what a future backend's `Unavailable` would look like.
        let custody = EnvironmentMasterKeyCustody {
            keys: HashMap::new(),
            active_master_key_id: "absent".to_string(),
        };
        assert_eq!(
            custody.preflight().await.unwrap_err(),
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "absent".to_string()
            }
        );
    }

    /// The failing probe hands back an error and *nothing else*: there is no second way to
    /// obtain a [`PreflightedCustody`], which is what makes the boot probe unskippable rather
    /// than merely conventional (#161).
    #[tokio::test]
    async fn a_preflighted_custody_exists_only_after_a_passing_probe() {
        let proven =
            PreflightedCustody::preflight(std::sync::Arc::new(custody(&[("k1", 1)], "k1")))
                .await
                .expect("a usable backend preflights");
        assert_eq!(proven.backend_name(), CUSTODY_BACKEND_ENVIRONMENT);
        assert_eq!(proven.custody().active_master_key_id(), "k1");

        let refused =
            PreflightedCustody::preflight(std::sync::Arc::new(EnvironmentMasterKeyCustody {
                keys: HashMap::new(),
                active_master_key_id: "absent".to_string(),
            }))
            .await
            .unwrap_err();
        assert_eq!(
            refused,
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "absent".to_string()
            }
        );
    }

    #[test]
    fn new_refuses_an_active_id_that_is_not_configured() {
        let keys = HashMap::from([("k1".to_string(), Zeroizing::new([1u8; 32]))]);
        assert_eq!(
            EnvironmentMasterKeyCustody::new(keys, "k2").unwrap_err(),
            KeyCustodyError::UnknownMasterKey {
                master_key_id: "k2".to_string()
            }
        );
    }

    #[tokio::test]
    async fn the_wrap_blob_is_sixty_one_self_framed_bytes() {
        let custody = custody(&[("k1", 1)], "k1");
        let dek = Zeroizing::new([9u8; 32]);
        let wrapped = custody.wrap(&dek, aad("k1").as_bytes()).await.unwrap();

        let blob = EnvironmentMasterKeyCustody::encode_wrapped(&wrapped).unwrap();
        assert_eq!(blob.len(), ENVIRONMENT_WRAP_BLOB_LENGTH);
        assert_eq!(blob.len(), 61);
        assert_eq!(blob[0], ENVIRONMENT_WRAP_FORMAT_V1);
        assert_eq!(&blob[1..13], wrapped.nonce.as_slice());
        assert_eq!(&blob[13..], wrapped.wrapped.as_slice());

        let decoded = EnvironmentMasterKeyCustody::decode_wrapped("k1", &blob).unwrap();
        assert_eq!(
            custody
                .unwrap(&decoded, aad("k1").as_bytes())
                .await
                .unwrap()
                .as_slice(),
            dek.as_slice()
        );

        // An unknown format version is refused, not ignored: silently accepting one is how a
        // format change becomes an unreadable row nobody notices until a read.
        let mut future_version = blob.clone();
        future_version[0] = 0x02;
        assert!(EnvironmentMasterKeyCustody::decode_wrapped("k1", &future_version).is_err());
        assert!(EnvironmentMasterKeyCustody::decode_wrapped("k1", &blob[..60]).is_err());
    }

    #[test]
    fn the_wrapped_dek_aad_omits_the_custody_backend() {
        let id = Uuid::from_u128(1);
        let rendered = wrapped_data_key_aad(id, "k1", WRAP_ALGORITHM_AES_256_GCM);

        assert_eq!(
            rendered,
            format!(
                "moira/data-key/v1;data_key_id={id};master_key_id=k1;wrap_algorithm=AES-256-GCM"
            )
        );
        // Load-bearing: binding the backend would force a rewrap of every keyring row the day
        // the same bytes are served from Vault under the same id. See the function's doc.
        assert!(!rendered.contains("custody"));
        assert!(!rendered.contains(CUSTODY_BACKEND_ENVIRONMENT));
    }

    #[test]
    fn debug_prints_ids_and_never_key_bytes() {
        let custody = custody(&[("k1", 0xab), ("k2", 0xcd)], "k2");
        let rendered = format!("{custody:?}");

        assert!(rendered.contains("k1") && rendered.contains("k2"));
        assert!(rendered.contains("active_master_key_id: \"k2\""));
        // The two keys are all-0xab and all-0xcd. Any rendering of the material — byte array,
        // hex, or base64 — trips one of these.
        for needle in ["171", "205", "ab", "cd", "q6ur", "zc3N"] {
            assert!(
                !rendered.contains(needle),
                "custody Debug leaked {needle:?}: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn wrapped_key_debug_prints_lengths_and_never_the_blob() {
        let custody = custody(&[("k1", 1)], "k1");
        let wrapped = custody
            .wrap(&Zeroizing::new([9u8; 32]), aad("k1").as_bytes())
            .await
            .unwrap();

        let rendered = format!("{wrapped:?}");
        assert!(rendered.contains("k1") && rendered.contains("AES-256-GCM"));
        assert!(rendered.contains("nonce_len: 12") && rendered.contains("wrapped_len: 48"));
        // A derived `Debug` would have rendered both byte vectors as `field: [..]`. Asserting on
        // a sample byte instead would be flaky — a random `0x01` also matches the `12` in
        // `nonce_len` — so assert the shape no byte-vector rendering can avoid.
        assert!(!rendered.contains('['), "custody blob rendered: {rendered}");
    }

    #[test]
    fn the_configured_master_key_ids_are_listed_sorted_and_without_material() {
        let rotating = custody(&[("zulu", 1), ("alpha", 2), ("mike", 3)], "mike");
        assert_eq!(rotating.master_key_ids(), ["alpha", "mike", "zulu"]);
        // The list is what the keyring's boot refusal prints, so it has to be stable across
        // processes; an unsorted `HashMap` walk would make two replicas print two orders for
        // the same configuration.
        assert_eq!(rotating.master_key_ids(), rotating.master_key_ids());
        assert_eq!(
            custody(&[("only", 1)], "only").master_key_ids(),
            ["only".to_string()]
        );
    }

    #[test]
    fn master_key_ids_are_constrained_to_the_column_and_the_separators() {
        assert!(is_valid_master_key_id("dev-local"));
        assert!(is_valid_master_key_id("prod.2026_07-a"));
        assert!(is_valid_master_key_id(
            &"a".repeat(MASTER_KEY_ID_MAX_LENGTH)
        ));

        assert!(!is_valid_master_key_id(""));
        assert!(!is_valid_master_key_id(
            &"a".repeat(MASTER_KEY_ID_MAX_LENGTH + 1)
        ));
        // The two separators of MOIRA_CONTENT_ENCRYPTION__KEYS, which is what makes
        // `split_once(':')` unambiguous.
        assert!(!is_valid_master_key_id("a:b"));
        assert!(!is_valid_master_key_id("a,b"));
        assert!(!is_valid_master_key_id("a b"));
        assert!(!is_valid_master_key_id("kunci-é"));
    }
}
