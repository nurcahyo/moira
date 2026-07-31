use aes_gcm::aead::rand_core::{OsRng, RngCore};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng as PasswordOsRng},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::SecretString;

use crate::error::AppError;

use super::masking::secret_fingerprint;

/// Every namespace [`ApiKeyHasher::generate`] is called with, in one place.
///
/// It exists so [`MIN_API_KEY_PREFIX_LENGTH`] can be *derived* from the longest of them
/// rather than from a number someone measured once. A call site naming a namespace that is
/// not listed here is caught by `every_generate_call_site_names_a_registered_namespace`,
/// which walks `src/` for the same reason the i18n catalog gate does: a literal at a call
/// site is invisible to a constant maintained by hand.
pub const KEY_NAMESPACES: &[&str] = &["moira_sys", "moira_cons", "moira_inv"];

/// Random characters a generated key prefix must retain **after** its namespace and the
/// `_` separator.
///
/// The prefix is a *plaintext lookup key*: `AuthService::verify_api_key` and
/// `AdminIdentityService::resolve_invite` both select the candidate row by it and only then
/// run Argon2. Two properties are exactly as strong as the random part is long.
///
/// 1. **Uniqueness.** `admin_invites_token_prefix_active_unique` and the equivalents on
///    `system_api_keys` / `consumer_api_keys` are unique over live rows, so a prefix
///    collision is a failed write, not a retryable one.
/// 2. **The anonymous preview's cost bound.** `POST /api/v1/admin/admin-invites/preview` is
///    unauthenticated and its whole CPU-exhaustion argument is that "a caller who does not
///    already hold a valid prefix causes zero Argon2 work" — which is true only for as long
///    as guessing a live prefix is infeasible.
///
/// Eight base64url characters is 64⁸ ≈ 2.8 × 10¹⁴. The shipped `api_keys.prefix_length` of
/// 20 leaves nine for the longest namespace.
pub const MIN_RANDOM_PREFIX_CHARS: usize = 8;

/// Smallest `api_keys.prefix_length` that keeps [`MIN_RANDOM_PREFIX_CHARS`] random
/// characters for **every** namespace in [`KEY_NAMESPACES`].
///
/// Derived rather than written down: `"moira_cons"` is a character longer than
/// `"moira_inv"`, and a hand-maintained floor stops being one on the day a longer namespace
/// is added — silently, because nothing would fail.
pub const MIN_API_KEY_PREFIX_LENGTH: usize = min_api_key_prefix_length();

/// Whether `namespace` is one of [`KEY_NAMESPACES`].
///
/// `const` so that a namespace held in a constant can prove its own registration at
/// **compile time**. That is not decoration: the source walker in this module's tests can
/// only see namespaces spelled inline at a `generate` call site, and
/// `ADMIN_INVITE_NAMESPACE` is not one of them — it is a constant, precisely so the schema
/// value and the code agree. Without this the invite namespace would be the one namespace
/// no gate covered, which is the shape of hole that produced finding F13 and the
/// uncatalogued `validate_override` codes.
pub const fn is_registered_key_namespace(namespace: &str) -> bool {
    let mut index = 0;
    while index < KEY_NAMESPACES.len() {
        if const_str_eq(KEY_NAMESPACES[index], namespace) {
            return true;
        }
        index += 1;
    }
    false
}

const fn const_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn min_api_key_prefix_length() -> usize {
    let mut longest = 0;
    let mut index = 0;
    while index < KEY_NAMESPACES.len() {
        let candidate = KEY_NAMESPACES[index].len();
        if candidate > longest {
            longest = candidate;
        }
        index += 1;
    }
    // `+ 1` for the `_` `generate` inserts between the namespace and the random material.
    longest + 1 + MIN_RANDOM_PREFIX_CHARS
}

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
    /// # The floor, and why configuration no longer reaches it
    ///
    /// This used to clamp at a bare `.max(12)`, a number that knew nothing about the
    /// namespace it would be prefixing. `moira_inv_` is ten characters, so a configured
    /// `prefix_length` of 12 left **two** random base64url characters: 4096 distinct
    /// prefixes, colliding on `admin_invites_token_prefix_active_unique` — an unmapped
    /// unique violation, i.e. a `500` — and collapsing the anonymous preview's "no Argon2
    /// work without a valid prefix" bound to a 4096-guess search.
    ///
    /// The shipped default is 20, so this was configuration-only. It is now refused at
    /// startup by `Settings::validate` rather than clamped here, on the same reasoning
    /// `validated_invite_lifetime` refuses rather than clamps: an operator who believes
    /// they configured one thing and silently received another finds out at the worst
    /// possible moment, and a clamp makes the misconfiguration *invisible* instead of
    /// merely harmless.
    ///
    /// The floor stays, retargeted at [`MIN_API_KEY_PREFIX_LENGTH`], so that a direct
    /// library construction — a test, a future caller that does not come through
    /// `Settings` — still cannot produce a hasher whose prefixes are guessable. It is a
    /// backstop that configuration can no longer reach, not the gate.
    pub fn new(
        pepper: impl Into<Vec<u8>>,
        pepper_version: impl Into<String>,
        prefix_length: usize,
    ) -> Self {
        Self {
            pepper: pepper.into(),
            pepper_version: pepper_version.into(),
            prefix_length: prefix_length.max(MIN_API_KEY_PREFIX_LENGTH),
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

    /// The bound is **measured**, not asserted against arithmetic repeated from the
    /// constant's own definition.
    ///
    /// A test that recomputed `namespace.len() + 1 + MIN_RANDOM_PREFIX_CHARS` would agree
    /// with the constant however wrong both were. This one generates a real key at the
    /// floor and counts what is actually left after the namespace — which is the quantity
    /// the uniqueness index and the preview's cost bound both depend on.
    #[test]
    fn the_minimum_prefix_length_leaves_enough_random_material_for_every_namespace() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", MIN_API_KEY_PREFIX_LENGTH);
        for namespace in KEY_NAMESPACES {
            let generated = hasher.generate(namespace).expect("generate a key");
            let random = generated
                .key_prefix
                .strip_prefix(&format!("{namespace}_"))
                .unwrap_or_else(|| {
                    panic!("{namespace}: the prefix must still contain the whole namespace")
                });
            assert!(
                random.chars().count() >= MIN_RANDOM_PREFIX_CHARS,
                "{namespace}: the prefix retains only {} random characters, below the \
                 {MIN_RANDOM_PREFIX_CHARS} the unique index and the anonymous preview's \
                 cost bound both need",
                random.chars().count()
            );
        }
    }

    /// The defect the floor now prevents, stated as a measurement rather than as prose.
    ///
    /// At the old `.max(12)` a `moira_inv` prefix retained two characters. Asserting the
    /// *number* here is what makes the change verifiable: 64² = 4096 is a search space, not
    /// a secret.
    #[test]
    fn the_old_clamp_left_a_guessable_prefix_and_the_new_floor_does_not() {
        let old_clamp = 12;
        let namespace = "moira_inv";
        let random_at_old_clamp = old_clamp - (namespace.len() + 1);
        assert_eq!(
            random_at_old_clamp, 2,
            "the historical clamp left two base64url characters — 4096 distinct prefixes"
        );
        assert!(
            MIN_API_KEY_PREFIX_LENGTH > old_clamp,
            "the floor must exceed the clamp it replaced"
        );
    }

    /// Every `generate` call site in `src/` must name a registered namespace.
    ///
    /// [`MIN_API_KEY_PREFIX_LENGTH`] is derived from the longest entry in
    /// [`KEY_NAMESPACES`], so an *unregistered* namespace longer than `moira_cons` would
    /// silently push the random tail below [`MIN_RANDOM_PREFIX_CHARS`] with every gate
    /// still green — the constant would be right about the list and wrong about the tree.
    /// This is the same walking technique, and the same class of blind spot, as
    /// `every_coded_error_literal_in_src_has_a_catalog_entry`.
    #[test]
    fn every_generate_call_site_names_a_registered_namespace() {
        // Assembled with `concat!` because this file is itself walked: a literal spelled
        // out here would be found in its own source and parsed as a call site.
        const NEEDLE: &str = concat!(".generate", "(\"");

        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_sources_under(&manifest.join("src"), &mut files);
        assert!(
            files.len() > 20,
            "the source walker found only {} files under src/ — a broken walker asserts nothing",
            files.len()
        );

        let mut found = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            let relative = file
                .strip_prefix(manifest)
                .unwrap_or(file)
                .display()
                .to_string();
            let mut cursor = 0usize;
            while let Some(offset) = source[cursor..].find(NEEDLE) {
                let start = cursor + offset + NEEDLE.len();
                cursor = start;
                let Some(end) = source[start..].find('"') else {
                    continue;
                };
                found.push((relative.clone(), source[start..start + end].to_string()));
            }
        }

        // Vacuity guard: the namespaces reach `generate` through a `const` at one of the
        // three sites, so this counts the ones spelled inline. Zero means the needle
        // stopped matching, which would make the assertion below prove nothing.
        assert!(
            !found.is_empty(),
            "no `generate(\"…\")` call site was found in src/ — the needle has drifted"
        );

        let unregistered: Vec<String> = found
            .iter()
            .filter(|(_, namespace)| !KEY_NAMESPACES.contains(&namespace.as_str()))
            .map(|(file, namespace)| format!("{namespace:?} in {file}"))
            .collect();
        assert!(
            unregistered.is_empty(),
            "these key namespaces are not in KEY_NAMESPACES, so MIN_API_KEY_PREFIX_LENGTH \
             was not computed with them in mind: {unregistered:?}"
        );
    }

    /// Every `.rs` file under `src/`, sorted and depth-first so a failure names the same
    /// file on every machine.
    fn rust_sources_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                rust_sources_under(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
}
