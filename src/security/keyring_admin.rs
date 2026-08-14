//! The rotation verbs behind `moira keyring …` — `status`, `add`, `promote`, `rewrap`,
//! `retire`, `abandon`, `usage` and `reseal`.
//!
//! `src/security/data_keys.rs` reads the keyring. This module *changes* it. The two are split
//! because their failure postures are opposites: the loader refuses to start on anything it
//! cannot fully open, and these verbs must keep working on exactly that keyring — `abandon` is
//! only reachable when the loader has already refused. **Nothing here calls
//! [`ContentKeyring::load`]**, and that is load-bearing rather than incidental: a rotation CLI
//! that booted a keyring first would be unable to repair the one condition it exists to repair.
//!
//! # Why this is a process mode and not an admin HTTP route
//!
//! These verbs need the database and custody and nothing else. An HTTP surface would add an
//! authorization design, an OpenAPI contract and a public error taxonomy to operations that are
//! run by a human with shell access to the deployment, at a rate of single digits per year.
//! `docs/decision-encryption-at-rest.md` §9 states this; `src/app/keyring_cli.rs` is the
//! argument parsing, and this module is everything it drives.
//!
//! # The one rule the tests enforce against themselves
//!
//! **Every rotation test calls the functions on this type, never equivalent SQL of its own.**
//! A rotation suite that hand-writes the `update` the CLI is supposed to issue proves that the
//! *test* can rotate a keyring. Faking it that way is the stated reason rotation code is
//! usually broken the first time it is needed, so the fixtures below seed rows and read rows,
//! and everything between those two is a method on [`KeyringAdmin`].
//!
//! # What writes an `*_encrypted` column here
//!
//! Only [`KeyringAdmin::reseal`], and only ever by replacing an envelope that is **already** in
//! the column with the same plaintext sealed under a newer data key. **No path in this module
//! turns plaintext into ciphertext** — that is the production write path, which now exists in
//! `src/infra/repositories/conversation.rs` for all five columns (issues #139, #140, #141) and
//! is not reachable from here.
//!
//! Note what `reseal` does *not* do, because the two are easy to conflate: it does not seal a
//! plaintext row. A row still holding `content_plain` is invisible to it. Sealing existing
//! plaintext history is `seal-existing`, which is designed and deferred
//! (`docs/decision-encryption-at-rest.md` §16).
//!
//! `reseal` also never spells a column name. It builds its statement from
//! [`AadProfile::column`], so the registry in `src/security/content_envelope.rs` is the single
//! source of the identifier — which is why this module is deliberately absent from
//! `SEALED_COLUMN_SQL_SITES`, and is the shape a new sealed-column site should copy.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use aes_gcm::{Aes256Gcm, aead::KeyInit, aead::OsRng};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tracing::{info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::security::{
    AadProfile, ContentCipher, ContentEnvelopeError, ContentIdentity, DataKeyPurpose, DataKeyState,
    EnvelopeHeader, KeyCustodyError, KeyringError, MasterKeyCustody, WRAP_ALGORITHM_AES_256_GCM,
    WrappedKey, data_keys::KEYRING_BOOTSTRAP_LOCK_KEY, format_key_check_value, key_check_value,
    wrapped_data_key_aad,
};

/// Rows the loader must be able to unwrap, and therefore the rows `rewrap` must cover.
///
/// Derived from [`DataKeyState::is_unwrapped_at_load`] rather than restated, so the two can
/// never disagree about which rows a booting instance opens.
fn states_unwrapped_at_load() -> Vec<&'static str> {
    DataKeyState::ALL
        .into_iter()
        .filter(|state| state.is_unwrapped_at_load())
        .map(DataKeyState::as_str)
        .collect()
}

/// Every way a rotation verb can refuse.
///
/// Each variant is a refusal an operator has to act on, so each one names the thing they need:
/// the key, the master key, the counts, or the missing guard.
#[derive(Debug, thiserror::Error)]
pub enum KeyringAdminError {
    #[error("content keyring database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("content key custody failed: {0}")]
    Custody(#[from] KeyCustodyError),

    /// A row this build cannot interpret. Shares [`KeyringError`] rather than restating it: a
    /// keyring written by a newer binary must read identically to the loader and to the CLI.
    #[error(transparent)]
    Keyring(#[from] KeyringError),

    #[error("content data key {id} is not in content_data_keys")]
    UnknownKey { id: Uuid },

    #[error(
        "content data key {id} is in state {state:?}; `moira keyring promote` only promotes a \
         key in state \"pending\". Mint one with `moira keyring add`."
    )]
    NotPending { id: Uuid, state: &'static str },

    /// The ordering guard of `docs/decision-encryption-at-rest.md` §9, enforced rather than
    /// documented-and-hoped. It fires on exactly the condition boot fires on, which is what
    /// makes "skipping step 2 fails at step 4, loudly" true.
    #[error(
        "content data key {data_key_id} is sealed under master key {master_key_id:?}, which this \
         process does not hold, so it cannot be re-wrapped. Master key ids configured for \
         backend {backend:?}: {configured:?}, from MOIRA_CONTENT_ENCRYPTION__KEYS.\n\
         Rewrap is refused **in full** rather than in part: a keyring half under the old master \
         key and half under the new one is a keyring no single configuration can open.\n\
         Add {master_key_id:?} back to MOIRA_CONTENT_ENCRYPTION__KEYS and re-run, or — if that \
         key is gone for good — run `moira keyring abandon {data_key_id} --confirm --reason \
         \"<text>\"` first and accept that rows sealed under it become permanently unreadable."
    )]
    SourceMasterKeyNotHeld {
        data_key_id: Uuid,
        master_key_id: String,
        backend: &'static str,
        configured: Vec<String>,
    },

    #[error(
        "cannot re-wrap onto master key {master_key_id:?}: this process does not hold it. \
         Master key ids configured for backend {backend:?}: {configured:?}. Add \
         \"{master_key_id}:<base64 of 32 bytes>\" to MOIRA_CONTENT_ENCRYPTION__KEYS and restart \
         before re-wrapping onto it."
    )]
    TargetMasterKeyNotHeld {
        master_key_id: String,
        backend: &'static str,
        configured: Vec<String>,
    },

    /// `retire` is the one verb whose refusal is the point. A `retired` key is not loaded, so a
    /// row still naming it becomes unreadable the moment this succeeds.
    #[error(
        "content data key {id} cannot be retired: {total} row(s) are still sealed under it \
         ({detail}). A retired key is not loaded, so those rows would stop opening. Move them \
         first with `moira keyring reseal --from {id} --to <active-key-id>`, then retire."
    )]
    StillReferenced {
        id: Uuid,
        total: i64,
        detail: String,
    },

    #[error(
        "content data key {id} is the active key for purpose {purpose:?} and cannot be retired. \
         Promote a replacement first (`moira keyring add` then `moira keyring promote <new-id>`)."
    )]
    StillActive { id: Uuid, purpose: &'static str },

    /// Both guards of `abandon`, reported apart, because they are different mistakes.
    #[error(
        "`moira keyring abandon {id}` requires --confirm. It makes every row sealed under {id} \
         permanently unreadable, on purpose. There is no undo, and no other command in Moira \
         destroys readability."
    )]
    AbandonNotConfirmed { id: Uuid },

    #[error(
        "`moira keyring abandon {id}` requires a non-empty --reason \"<text>\". The reason is \
         stored in content_data_keys.abandon_reason and is the only record of why a key was \
         given up on."
    )]
    AbandonReasonMissing { id: Uuid },

    #[error(
        "content data key {id} is already {state:?}; `moira keyring abandon` is for a key whose \
         master key is lost, not for one that has already been dealt with."
    )]
    NotAbandonable { id: Uuid, state: &'static str },

    #[error(
        "content data key {id} is in state {state:?} and holds no openable key material, so it \
         cannot be a reseal source or target."
    )]
    NotResealable { id: Uuid, state: &'static str },

    #[error("`moira keyring reseal` needs --from and --to to be different keys ({id} was both)")]
    ResealSourceIsTarget { id: Uuid },

    /// A stored envelope the parser rejects, met mid-reseal. Named with its row so an operator
    /// can go and look at it, rather than reported as "reseal failed".
    #[error(
        "reseal stopped at {table}.{column} row {row_id}: the stored envelope is not one this \
         build can open ({source}). Nothing was written for that row."
    )]
    UnreadableEnvelope {
        table: &'static str,
        column: &'static str,
        row_id: Uuid,
        source: ContentEnvelopeError,
    },
}

/// The keyring, as an operator changes it.
///
/// Holds a pool and a custody backend and nothing else. It is deliberately constructible
/// against a keyring [`ContentKeyring::load`] would refuse — see the module header.
///
/// [`ContentKeyring::load`]: crate::security::ContentKeyring::load
pub struct KeyringAdmin {
    pool: PgPool,
    custody: Arc<dyn MasterKeyCustody>,
}

impl std::fmt::Debug for KeyringAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringAdmin")
            .field("custody", &self.custody)
            .finish_non_exhaustive()
    }
}

/// One `content_data_keys` row as the verbs read it — the loader's `KeyringRow` without the
/// loader's insistence that every row open.
#[derive(sqlx::FromRow, Clone)]
struct AdminRow {
    id: Uuid,
    key_version: i32,
    purpose: String,
    state: String,
    custody_backend: String,
    master_key_id: String,
    wrap_algorithm: String,
    wrap_nonce: Vec<u8>,
    wrapped_key: Vec<u8>,
    key_check_value: Vec<u8>,
    created_at: DateTime<Utc>,
    abandon_reason: Option<String>,
}

const ADMIN_ROW_COLUMNS: &str = "id, key_version, purpose, state, custody_backend, master_key_id, \
     wrap_algorithm, wrap_nonce, wrapped_key, key_check_value, created_at, abandon_reason";

impl AdminRow {
    fn state(&self) -> Result<DataKeyState, KeyringAdminError> {
        DataKeyState::from_db(&self.state).ok_or_else(|| {
            KeyringAdminError::Keyring(KeyringError::UnreadableRow {
                data_key_id: self.id,
                detail: format!("unknown state {:?}", self.state),
            })
        })
    }

    fn purpose(&self) -> Result<DataKeyPurpose, KeyringAdminError> {
        DataKeyPurpose::from_db(&self.purpose).ok_or_else(|| {
            KeyringAdminError::Keyring(KeyringError::UnreadableRow {
                data_key_id: self.id,
                detail: format!("unknown purpose {:?}", self.purpose),
            })
        })
    }

    fn wrapped(&self) -> WrappedKey {
        WrappedKey {
            master_key_id: self.master_key_id.clone(),
            wrap_algorithm: self.wrap_algorithm.clone(),
            nonce: self.wrap_nonce.clone(),
            wrapped: self.wrapped_key.clone(),
        }
    }

    fn aad(&self) -> String {
        wrapped_data_key_aad(self.id, &self.master_key_id, &self.wrap_algorithm)
    }
}

/// What `keyring status` prints, and what the tests assert on.
#[derive(Debug)]
pub struct KeyringStatus {
    pub backend: &'static str,
    pub active_master_key_id: String,
    pub configured_master_key_ids: Vec<String>,
    pub keys: Vec<KeyStatus>,
}

#[derive(Debug)]
pub struct KeyStatus {
    pub id: Uuid,
    pub key_version: i32,
    pub purpose: String,
    pub state: String,
    pub custody_backend: String,
    pub master_key_id: String,
    /// Lowercase hex of the stored check value. Safe to print anywhere — that is the point of
    /// it; see [`crate::security::key_check_value`].
    pub key_check_value: String,
    pub created_at: DateTime<Utc>,
    pub abandon_reason: Option<String>,
    /// Whether this process holds the master key this row names. The single most useful field
    /// on the whole listing during a rotation: it is a preview of whether boot will refuse.
    pub master_key_held: bool,
    /// Per-table reference counts, one entry per [`AadProfile`].
    pub usage: KeyUsage,
}

/// Reference counts for one data key, one entry per sealed column.
///
/// A `BTreeMap` keyed on `"table.column"` rather than five fields: the profile registry is the
/// source of the key set, so a sixth sealed column is counted by construction instead of
/// needing a sixth field here that somebody has to remember to add.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyUsage {
    pub per_column: BTreeMap<String, i64>,
}

impl KeyUsage {
    pub fn total(&self) -> i64 {
        self.per_column.values().sum()
    }

    /// `"conversation_messages.content_encrypted=3, …"`, for a refusal message.
    fn detail(&self) -> String {
        self.per_column
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(column, count)| format!("{column}={count}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `"<table>.<column>"` for one profile. The one place that spelling is formed.
fn profile_key(profile: AadProfile) -> String {
    format!("{}.{}", profile.table(), profile.column())
}

#[derive(Debug)]
pub struct AddedKey {
    pub id: Uuid,
    pub key_version: i32,
    pub purpose: DataKeyPurpose,
    pub master_key_id: String,
    pub key_check_value: String,
}

#[derive(Debug)]
pub struct Promotion {
    pub promoted: Uuid,
    pub purpose: DataKeyPurpose,
    /// The key that went `active` → `retiring`, if there was one. `None` on a keyring that had
    /// no active key — the state `abandon` on an active key leaves behind.
    pub demoted: Option<Uuid>,
}

#[derive(Debug)]
pub struct RewrapReport {
    pub target_master_key_id: String,
    pub target_backend: &'static str,
    pub rewrapped: Vec<Uuid>,
    /// Rows deliberately left alone: `retired` (not loaded, so boot never opens them) and
    /// `abandoned` (no master key exists to open them with). Reported rather than silent —
    /// an operator about to destroy the old master key must see what still names it.
    pub left_alone: Vec<(Uuid, String)>,
}

#[derive(Debug)]
pub struct Retirement {
    pub id: Uuid,
}

#[derive(Debug)]
pub struct Abandonment {
    pub id: Uuid,
    pub reason: String,
    /// The key new content would be sealed under after this. `None` means the operator has
    /// just abandoned the active key and the service cannot start until `add` + `promote`.
    pub active_content_key_remaining: Option<Uuid>,
}

/// The knobs on [`KeyringAdmin::reseal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResealOptions {
    /// Rows per pass, per sealed column.
    pub batch: u32,
    /// Pause between passes. The only throttle: this is a background maintenance job
    /// competing with live traffic for the same rows.
    pub sleep_ms: u64,
    /// Stop after this many passes and report what was done, instead of running to
    /// completion.
    ///
    /// **Not a testing hook.** Resealing a large table is a job an operator wants to bound —
    /// run one pass, watch the effect on latency, continue — and the alternative to a bound
    /// is `Ctrl-C`, which is the same thing with no report at the end. It also makes the
    /// resumability claim above a thing that can be *demonstrated*: a bounded run followed by
    /// a fresh unbounded one, on a new [`KeyringAdmin`], with no state carried between them.
    pub max_batches: Option<u32>,
}

impl Default for ResealOptions {
    fn default() -> Self {
        Self {
            batch: 500,
            sleep_ms: 0,
            max_batches: None,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResealReport {
    /// Rows whose compare-and-swap matched and was written.
    pub resealed: u64,
    /// Rows whose compare-and-swap matched **zero** rows because a concurrent writer had
    /// already changed the column. Skipped, never clobbered — and already under a newer key.
    pub skipped: u64,
    pub batches: u64,
}

impl KeyringAdmin {
    pub fn new(pool: PgPool, custody: Arc<dyn MasterKeyCustody>) -> Self {
        Self { pool, custody }
    }

    pub fn custody(&self) -> &Arc<dyn MasterKeyCustody> {
        &self.custody
    }

    // -----------------------------------------------------------------------------------
    // status / usage — read-only, and the only verbs that scan the content tables
    // -----------------------------------------------------------------------------------

    /// Every key, with its reference counts. One sequential scan per sealed column per key.
    ///
    /// Deliberately expensive and deliberately not on any request path: it is the query an
    /// operator runs *before* retiring a key, and the alternative to it is trying to decrypt
    /// every row with the key that is about to be retired.
    pub async fn status(&self) -> Result<KeyringStatus, KeyringAdminError> {
        let rows = self.all_rows().await?;
        let mut keys = Vec::with_capacity(rows.len());
        for row in rows {
            // Parsed rather than passed through, so `status` on a keyring written by a newer
            // binary refuses instead of printing a state this build would then mishandle.
            let _ = row.state()?;
            let _ = row.purpose()?;
            let usage = self.usage(row.id).await?;
            keys.push(KeyStatus {
                id: row.id,
                key_version: row.key_version,
                purpose: row.purpose.clone(),
                state: row.state.clone(),
                custody_backend: row.custody_backend.clone(),
                master_key_held: self.custody.can_unwrap(&row.master_key_id),
                master_key_id: row.master_key_id.clone(),
                key_check_value: format_key_check_value(&row.key_check_value),
                created_at: row.created_at,
                abandon_reason: row.abandon_reason.clone(),
                usage,
            });
        }
        Ok(KeyringStatus {
            backend: self.custody.backend_name(),
            active_master_key_id: self.custody.active_master_key_id().to_string(),
            configured_master_key_ids: self.custody.master_key_ids(),
            keys,
        })
    }

    /// Per-table reference counts for one key, through the migration's SQL helper.
    ///
    /// **One transaction for all five counts**, so what an operator reads is five numbers from
    /// a single instant rather than five numbers from five instants. The question this answers
    /// — "is anything still sealed under this key?" — is one an operator acts on by retiring
    /// the key, and a set of counts that were never simultaneously true is a poor basis for
    /// that. It is read-only and rolled back.
    pub async fn usage(&self, data_key_id: Uuid) -> Result<KeyUsage, KeyringAdminError> {
        let mut tx = self.pool.begin().await?;
        let usage = Self::usage_in(&mut tx, data_key_id).await?;
        tx.rollback().await?;
        Ok(usage)
    }

    /// The counts, against a caller's transaction.
    ///
    /// Taking the transaction rather than the pool is what lets [`Self::retire`] count inside
    /// the same transaction that holds the key's row `for update` — the counts and the state
    /// check then agree with each other. It also removes a way to deadlock: a `retire` that
    /// borrowed a *second* connection from the pool while holding one would wait forever on a
    /// pool of size one.
    ///
    /// The identifiers interpolated below come from [`AadProfile::table`] and
    /// [`AadProfile::column`], both `const fn`s returning `&'static str` from an exhaustive
    /// `match`. There is no runtime input in this string, and iterating the registry rather
    /// than listing five literals is what makes a sixth sealed column counted by construction.
    async fn usage_in(
        tx: &mut Transaction<'_, Postgres>,
        data_key_id: Uuid,
    ) -> Result<KeyUsage, KeyringAdminError> {
        let mut per_column = BTreeMap::new();
        for profile in AadProfile::ALL {
            let count: i64 = sqlx::query_scalar(&format!(
                "select count(*) from {} where content_envelope_data_key_id({}) = $1",
                profile.table(),
                profile.column(),
            ))
            .bind(data_key_id)
            .fetch_one(&mut **tx)
            .await?;
            per_column.insert(profile_key(profile), count);
        }
        Ok(KeyUsage { per_column })
    }

    // -----------------------------------------------------------------------------------
    // R1 — add, then promote
    // -----------------------------------------------------------------------------------

    /// Mint a data key, wrap it under the **active** master key, insert it `pending`.
    ///
    /// Trivial and reversible: a `pending` key is written under by nobody, so an `add` that is
    /// never promoted costs one row and changes no behaviour at all.
    ///
    /// Runs under the advisory lock `bootstrap_if_empty` takes, because both choose a
    /// `key_version` from `max + 1` over a `unique` column. Serialising them turns "two pods
    /// raced and one got a unique violation on a version number" — a refusal that teaches an
    /// operator nothing — into two keys with two versions.
    pub async fn add(&self, purpose: DataKeyPurpose) -> Result<AddedKey, KeyringAdminError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(KEYRING_BOOTSTRAP_LOCK_KEY)
            .execute(&mut *tx)
            .await?;

        let key_version: i32 =
            sqlx::query_scalar("select coalesce(max(key_version), 0) + 1 from content_data_keys")
                .fetch_one(&mut *tx)
                .await?;

        let id = Uuid::now_v7();
        let mut data_key = Zeroizing::new([0u8; 32]);
        data_key.copy_from_slice(Aes256Gcm::generate_key(&mut OsRng).as_slice());
        let check_value = key_check_value(&data_key);

        let master_key_id = self.custody.active_master_key_id().to_string();
        let aad = wrapped_data_key_aad(id, &master_key_id, WRAP_ALGORITHM_AES_256_GCM);
        let wrapped = self.custody.wrap(&data_key, aad.as_bytes()).await?;
        // The AAD names the algorithm, so a backend that chose a different one would produce a
        // blob nothing can ever open. Refuse before it reaches a column — the same guard
        // `bootstrap_if_empty` applies to the very first key.
        if wrapped.wrap_algorithm != WRAP_ALGORITHM_AES_256_GCM {
            return Err(KeyringAdminError::Keyring(KeyringError::UnreadableRow {
                data_key_id: id,
                detail: format!(
                    "custody backend {:?} wrapped the new data key with algorithm {:?}, but the \
                     wrap AAD was built for {WRAP_ALGORITHM_AES_256_GCM:?}",
                    self.custody.backend_name(),
                    wrapped.wrap_algorithm,
                ),
            }));
        }

        sqlx::query(
            "insert into content_data_keys (
                 id, key_version, purpose, state, custody_backend, master_key_id,
                 wrap_algorithm, wrap_nonce, wrapped_key, key_check_value
             ) values ($1, $2, $3, 'pending', $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(key_version)
        .bind(purpose.as_str())
        .bind(self.custody.backend_name())
        .bind(&wrapped.master_key_id)
        .bind(&wrapped.wrap_algorithm)
        .bind(&wrapped.nonce)
        .bind(&wrapped.wrapped)
        .bind(check_value.as_slice())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        info!(
            data_key_id = %id,
            key_version,
            purpose = purpose.as_str(),
            master_key_id = %wrapped.master_key_id,
            key_check_value = %format_key_check_value(&check_value),
            "minted a pending content data key"
        );
        Ok(AddedKey {
            id,
            key_version,
            purpose,
            master_key_id: wrapped.master_key_id,
            key_check_value: format_key_check_value(&check_value),
        })
    }

    /// **R1.** One transaction: the named `pending` key becomes `active`, and whatever was
    /// active for the same purpose becomes `retiring`.
    ///
    /// Nothing is re-encrypted and nothing needs a restart. Rows written under the demoted key
    /// stay under it forever and stay readable forever — `retiring` means "not writable, still
    /// loaded". Each instance begins sealing under the new key at its next refresh.
    ///
    /// The demote-then-promote order matters. Reversed, the second statement would collide
    /// with the still-active row on the partial unique index and every promotion would fail.
    /// In this order two racing pods produce one winner and one unique violation, which is the
    /// correct outcome and the database's to enforce rather than the application's.
    pub async fn promote(&self, id: Uuid) -> Result<Promotion, KeyringAdminError> {
        let mut tx = self.pool.begin().await?;
        let row = Self::row_for_update(&mut tx, id).await?;
        let state = row.state()?;
        let purpose = row.purpose()?;
        if state != DataKeyState::Pending {
            return Err(KeyringAdminError::NotPending {
                id,
                state: state.as_str(),
            });
        }

        let demoted: Option<Uuid> = sqlx::query_scalar(
            "update content_data_keys set state = 'retiring'
              where purpose = $1 and state = 'active' and id <> $2
              returning id",
        )
        .bind(purpose.as_str())
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

        sqlx::query(
            "update content_data_keys set state = 'active', activated_at = now()
              where id = $1 and state = 'pending'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        info!(
            promoted = %id,
            demoted = ?demoted,
            purpose = purpose.as_str(),
            "promoted a content data key; nothing was re-encrypted"
        );
        Ok(Promotion {
            promoted: id,
            purpose,
            demoted,
        })
    }

    // -----------------------------------------------------------------------------------
    // R2 / R3 — rewrap
    // -----------------------------------------------------------------------------------

    /// **R2**, within one custody backend. See [`Self::rewrap_with`].
    pub async fn rewrap(
        &self,
        target_master_key_id: &str,
    ) -> Result<RewrapReport, KeyringAdminError> {
        self.rewrap_with(self.custody.clone(), target_master_key_id)
            .await
    }

    /// **R2 and R3.** Unwrap every loadable key under the master key its row records, and
    /// re-wrap it under `target_master_key_id` in `target`. One transaction.
    ///
    /// **No row of user data is read or written.** The five `*_encrypted` columns are not
    /// touched, not scanned, and not opened: the ciphertext in them is sealed under the *data*
    /// key, and only the wrapping of that data key changes. That is the whole reason a
    /// master-key rotation is a configuration change and takes seconds on a database of any
    /// size.
    ///
    /// Running instances are unaffected. They already hold unwrapped data keys in their
    /// snapshots, and only a *booting* instance reads these rows.
    ///
    /// `target` is a separate custody object rather than `self.custody` so that **R3** — a
    /// backend that will not release key bytes, such as AWS KMS — is the same code path with a
    /// different argument: unwrap with the environment backend, wrap with the KMS backend.
    ///
    /// # What it refuses, and why in full rather than in part
    ///
    /// If any row that a booting instance would unwrap names a master key this process does
    /// not hold, the whole rewrap is refused ([`KeyringAdminError::SourceMasterKeyNotHeld`]).
    /// A partial rewrap produces a keyring half under the old master key and half under the
    /// new one — which no single configuration can open, and which is strictly worse than not
    /// having started. This is the same condition boot refuses on, which is what makes
    /// skipping step 2 of the rotation fail loudly at step 4 instead of silently.
    ///
    /// `retired` and `abandoned` rows are left alone and reported. `abandoned` especially: it
    /// names a master key that is gone by definition, and refusing on it would re-brick the
    /// deployment for exactly the key an operator has already been forced to give up on.
    pub async fn rewrap_with(
        &self,
        target: Arc<dyn MasterKeyCustody>,
        target_master_key_id: &str,
    ) -> Result<RewrapReport, KeyringAdminError> {
        if !target.can_unwrap(target_master_key_id) {
            return Err(KeyringAdminError::TargetMasterKeyNotHeld {
                master_key_id: target_master_key_id.to_string(),
                backend: target.backend_name(),
                configured: target.master_key_ids(),
            });
        }

        let mut tx = self.pool.begin().await?;
        // `for update`, so a concurrent `add` or `promote` cannot slip a row past the
        // completeness check between the read and the writes.
        let rows: Vec<AdminRow> = sqlx::query_as(&format!(
            "select {ADMIN_ROW_COLUMNS} from content_data_keys order by key_version for update"
        ))
        .fetch_all(&mut *tx)
        .await?;

        let loadable = states_unwrapped_at_load();
        let mut left_alone = Vec::new();
        let mut to_rewrap = Vec::new();
        for row in rows {
            let state = row.state()?;
            if !loadable.contains(&state.as_str()) {
                left_alone.push((row.id, state.as_str().to_string()));
                continue;
            }
            // The refusal happens before a single write, over the *whole* set — checking each
            // row as it is rewritten would leave the first half converted when the fifth row
            // turned out to be unopenable.
            if !self.custody.can_unwrap(&row.master_key_id) {
                return Err(KeyringAdminError::SourceMasterKeyNotHeld {
                    data_key_id: row.id,
                    master_key_id: row.master_key_id.clone(),
                    backend: self.custody.backend_name(),
                    configured: self.custody.master_key_ids(),
                });
            }
            to_rewrap.push(row);
        }

        let mut rewrapped = Vec::with_capacity(to_rewrap.len());
        for row in &to_rewrap {
            let data_key = self
                .custody
                .unwrap(&row.wrapped(), row.aad().as_bytes())
                .await
                .map_err(|source| KeyringError::UnopenableDataKey {
                    data_key_id: row.id,
                    master_key_id: row.master_key_id.clone(),
                    backend: self.custody.backend_name(),
                    configured: self.custody.master_key_ids(),
                    source,
                })?;

            // The unwrap said yes; this says it said yes about the *right* key. Re-wrapping an
            // unverified key would launder a custody fault into a keyring that opens cleanly
            // under the new master key and decrypts nothing.
            let computed = key_check_value(&data_key);
            if computed.as_slice() != row.key_check_value.as_slice() {
                return Err(KeyringError::KeyCheckValueMismatch {
                    data_key_id: row.id,
                    master_key_id: row.master_key_id.clone(),
                    stored: format_key_check_value(&row.key_check_value),
                    computed: format_key_check_value(&computed),
                }
                .into());
            }

            // A fresh AAD naming the *new* master key and the *target's* algorithm. Re-using
            // the old row's AAD would bind the blob to a master key id the row no longer
            // records, and it would never open. Taking the algorithm from the target rather
            // than assuming AES-256-GCM is what lets the target be a KMS backend at all —
            // the loader rebuilds this AAD from the stored `wrap_algorithm` column.
            let algorithm = target.wrap_algorithm();
            let aad = wrapped_data_key_aad(row.id, target_master_key_id, algorithm);
            let wrapped = target
                .wrap_under(target_master_key_id, &data_key, aad.as_bytes())
                .await?;
            if wrapped.master_key_id != target_master_key_id {
                return Err(KeyringAdminError::TargetMasterKeyNotHeld {
                    master_key_id: target_master_key_id.to_string(),
                    backend: target.backend_name(),
                    configured: target.master_key_ids(),
                });
            }
            // The declaration is verified, never trusted. A backend whose `wrap_algorithm()`
            // disagrees with what `wrap_under` actually produced would write a row whose AAD
            // and whose `wrap_algorithm` column name different algorithms — and every future
            // boot would fail to open it, with nothing to point at.
            if wrapped.wrap_algorithm != algorithm {
                return Err(KeyringAdminError::Keyring(KeyringError::UnreadableRow {
                    data_key_id: row.id,
                    detail: format!(
                        "custody backend {:?} declares wrap_algorithm {algorithm:?} but wrapped \
                         with {:?}; the wrap AAD was built for the declared one, so the row \
                         would never open again",
                        target.backend_name(),
                        wrapped.wrap_algorithm,
                    ),
                }));
            }

            sqlx::query(
                "update content_data_keys
                    set master_key_id = $1, custody_backend = $2, wrap_algorithm = $3,
                        wrap_nonce = $4, wrapped_key = $5
                  where id = $6",
            )
            .bind(&wrapped.master_key_id)
            .bind(target.backend_name())
            .bind(&wrapped.wrap_algorithm)
            .bind(&wrapped.nonce)
            .bind(&wrapped.wrapped)
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
            rewrapped.push(row.id);
        }
        tx.commit().await?;

        info!(
            target_master_key_id,
            target_backend = target.backend_name(),
            rewrapped = rewrapped.len(),
            left_alone = left_alone.len(),
            "re-wrapped the content keyring; no row of user data was read or written"
        );
        Ok(RewrapReport {
            target_master_key_id: target_master_key_id.to_string(),
            target_backend: target.backend_name(),
            rewrapped,
            left_alone,
        })
    }

    // -----------------------------------------------------------------------------------
    // retire / abandon
    // -----------------------------------------------------------------------------------

    /// Move a key to `retired` — out of the snapshot entirely.
    ///
    /// Refused unless the key is not active **and** every one of the five per-table reference
    /// counts is zero. Both halves matter: a retired key is not loaded, so a row still naming
    /// it stops opening the moment this succeeds.
    pub async fn retire(&self, id: Uuid) -> Result<Retirement, KeyringAdminError> {
        let mut tx = self.pool.begin().await?;
        let row = Self::row_for_update(&mut tx, id).await?;
        let state = row.state()?;
        let purpose = row.purpose()?;
        if state == DataKeyState::Active {
            return Err(KeyringAdminError::StillActive {
                id,
                purpose: purpose.as_str(),
            });
        }

        // Counted inside the same transaction that holds this key's row `for update`, so the
        // state check above and the counts below are one consistent observation rather than
        // two that were never simultaneously true.
        //
        // The count is still advisory in the wider sense — a writer could add a row a
        // microsecond after this commits. What makes retirement safe is not the count but the
        // state: nothing is ever written under a key that is not `active`, and this one is not.
        let usage = Self::usage_in(&mut tx, id).await?;
        let total = usage.total();
        if total > 0 {
            return Err(KeyringAdminError::StillReferenced {
                id,
                total,
                detail: usage.detail(),
            });
        }

        sqlx::query(
            "update content_data_keys set state = 'retired', retired_at = now() where id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        info!(data_key_id = %id, "retired a content data key; it is no longer loaded");
        Ok(Retirement { id })
    }

    /// The confession. Marks a key `abandoned`: the master key that sealed it is gone, and
    /// that loss is being acknowledged so the service can start again.
    ///
    /// **A status, never a delete.** The ciphertext stays, in every column and in this row, in
    /// case the master key resurfaces — at which point clearing the state makes every row
    /// readable again. Nothing here destroys a byte.
    ///
    /// Both guards are required and are reported apart, because they are different mistakes:
    /// `--confirm` is the acknowledgement, `--reason` is the record. Its user is a tired
    /// operator at three in the morning, which is why the refusals say what it costs.
    pub async fn abandon(
        &self,
        id: Uuid,
        confirmed: bool,
        reason: &str,
    ) -> Result<Abandonment, KeyringAdminError> {
        if !confirmed {
            return Err(KeyringAdminError::AbandonNotConfirmed { id });
        }
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(KeyringAdminError::AbandonReasonMissing { id });
        }

        let mut tx = self.pool.begin().await?;
        let row = Self::row_for_update(&mut tx, id).await?;
        let state = row.state()?;
        let purpose = row.purpose()?;
        if matches!(state, DataKeyState::Abandoned | DataKeyState::Retired) {
            return Err(KeyringAdminError::NotAbandonable {
                id,
                state: state.as_str(),
            });
        }

        sqlx::query(
            "update content_data_keys
                set state = 'abandoned', abandoned_at = now(), abandon_reason = $1
              where id = $2",
        )
        .bind(reason)
        .bind(id)
        .execute(&mut *tx)
        .await?;

        let active_content_key_remaining: Option<Uuid> = sqlx::query_scalar(
            "select id from content_data_keys where purpose = $1 and state = 'active'",
        )
        .bind(purpose.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;

        // WARN rather than INFO, always. This is the one command in Moira that makes stored
        // rows permanently unreadable, and the line it leaves in the log is part of the audit
        // trail the `--reason` guard exists to produce.
        warn!(
            data_key_id = %id,
            previous_state = state.as_str(),
            reason,
            "a content data key was ABANDONED; every row sealed under it is now permanently \
             unreadable, on purpose and by request"
        );
        if active_content_key_remaining.is_none() {
            warn!(
                purpose = purpose.as_str(),
                "no active data key remains for this purpose; Moira will refuse to start until \
                 `moira keyring add` and `moira keyring promote <id>` have been run"
            );
        }
        Ok(Abandonment {
            id,
            reason: reason.to_string(),
            active_content_key_remaining,
        })
    }

    // -----------------------------------------------------------------------------------
    // R4 — reseal
    // -----------------------------------------------------------------------------------

    /// **R4.** Re-encrypt every row sealed under `from` onto `to`, in batches.
    ///
    /// The only expensive verb, and the only way to make a `retiring` key genuinely
    /// `retired`: `retire` refuses while any row still names the key, and nothing but this
    /// moves those rows.
    ///
    /// **Resumable and idempotent by construction**, with no cursor and no job table:
    ///
    /// * the selection is `where content_envelope_data_key_id(col) = $from`, so every pass
    ///   selects only what is left to do and each pass shrinks that set. Killing the process
    ///   and re-running it converges; running it twice over is a no-op the second time.
    /// * each write is a **compare-and-swap** — `update … set col = $new where id = $1 and
    ///   col = $2`. A row a concurrent writer rewrote between the select and the update
    ///   matches zero rows, is counted as skipped rather than clobbered, and is already under
    ///   a newer key anyway. That is what makes running this under live traffic safe, and it
    ///   is the difference between "resumable" and "resumable and non-destructive".
    ///
    /// `sleep_ms` between batches is the only throttle: this is a background maintenance job
    /// competing with live traffic for the same rows.
    pub async fn reseal(
        &self,
        from: Uuid,
        to: Uuid,
        options: ResealOptions,
    ) -> Result<ResealReport, KeyringAdminError> {
        if from == to {
            return Err(KeyringAdminError::ResealSourceIsTarget { id: from });
        }
        let source = self.cipher_for(from).await?;
        let destination = self.cipher_for(to).await?;
        let batch = i64::from(options.batch.max(1));

        let mut report = ResealReport::default();
        loop {
            let mut moved_this_pass = 0u64;
            for profile in AadProfile::ALL {
                let rows = self.select_sealed_batch(profile, from, batch).await?;
                for (row_id, identity, envelope) in rows {
                    let plaintext =
                        source
                            .open(&envelope, &identity.borrow())
                            .map_err(|source| KeyringAdminError::UnreadableEnvelope {
                                table: profile.table(),
                                column: profile.column(),
                                row_id,
                                source,
                            })?;
                    let resealed =
                        destination
                            .seal(&identity.borrow(), &plaintext)
                            .map_err(|source| KeyringAdminError::UnreadableEnvelope {
                                table: profile.table(),
                                column: profile.column(),
                                row_id,
                                source,
                            })?;

                    // The compare-and-swap. `col = $3` is the whole guarantee.
                    let updated = sqlx::query(&format!(
                        "update {} set {} = $1 where id = $2 and {} = $3",
                        profile.table(),
                        profile.column(),
                        profile.column(),
                    ))
                    .bind(&resealed)
                    .bind(row_id)
                    .bind(&envelope)
                    .execute(&self.pool)
                    .await?
                    .rows_affected();

                    if updated == 0 {
                        report.skipped += 1;
                    } else {
                        report.resealed += 1;
                        moved_this_pass += 1;
                    }
                }
            }
            report.batches += 1;
            if moved_this_pass == 0 {
                break;
            }
            // Checked *after* the pass, so `max_batches: Some(1)` performs one pass rather
            // than none — and so a bounded run that happened to finish the work still reports
            // the same completion an unbounded one would.
            if options
                .max_batches
                .is_some_and(|max| report.batches >= u64::from(max))
            {
                break;
            }
            if options.sleep_ms > 0 {
                tokio::time::sleep(Duration::from_millis(options.sleep_ms)).await;
            }
        }

        info!(
            %from,
            %to,
            resealed = report.resealed,
            skipped = report.skipped,
            batches = report.batches,
            "resealed rows onto a newer content data key"
        );
        Ok(report)
    }

    // -----------------------------------------------------------------------------------
    // Shared internals
    // -----------------------------------------------------------------------------------

    async fn all_rows(&self) -> Result<Vec<AdminRow>, KeyringAdminError> {
        Ok(sqlx::query_as(&format!(
            "select {ADMIN_ROW_COLUMNS} from content_data_keys order by key_version"
        ))
        .fetch_all(&self.pool)
        .await?)
    }

    async fn row_for_update(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<AdminRow, KeyringAdminError> {
        sqlx::query_as(&format!(
            "select {ADMIN_ROW_COLUMNS} from content_data_keys where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(KeyringAdminError::UnknownKey { id })
    }

    /// Unwrap one key and build its cipher, without loading the whole keyring.
    ///
    /// The reseal path's equivalent of [`crate::security::ContentKeyring::cipher_for`], and
    /// deliberately not that method: reseal must work on a keyring the loader would refuse —
    /// one holding an abandoned key, say — and loading it first would make R4 unreachable in
    /// precisely the deployment that needs it.
    async fn cipher_for(&self, id: Uuid) -> Result<ContentCipher, KeyringAdminError> {
        let row: AdminRow = sqlx::query_as(&format!(
            "select {ADMIN_ROW_COLUMNS} from content_data_keys where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(KeyringAdminError::UnknownKey { id })?;

        let state = row.state()?;
        if !state.is_unwrapped_at_load() {
            return Err(KeyringAdminError::NotResealable {
                id,
                state: state.as_str(),
            });
        }
        let data_key = self
            .custody
            .unwrap(&row.wrapped(), row.aad().as_bytes())
            .await
            .map_err(|source| KeyringError::UnopenableDataKey {
                data_key_id: row.id,
                master_key_id: row.master_key_id.clone(),
                backend: self.custody.backend_name(),
                configured: self.custody.master_key_ids(),
                source,
            })?;
        let computed = key_check_value(&data_key);
        if computed.as_slice() != row.key_check_value.as_slice() {
            return Err(KeyringError::KeyCheckValueMismatch {
                data_key_id: row.id,
                master_key_id: row.master_key_id.clone(),
                stored: format_key_check_value(&row.key_check_value),
                computed: format_key_check_value(&computed),
            }
            .into());
        }
        Ok(ContentCipher::new(row.id, &data_key))
    }

    /// One batch of rows sealed under `data_key_id`, with the identity columns their AAD binds.
    ///
    /// The `match` is exhaustive on [`AadProfile`], so a sixth sealed column cannot be added
    /// without deciding here exactly which columns reseal has to read to rebuild its AAD.
    /// A generic "select the blob" version would compile and then fail to open a single row.
    async fn select_sealed_batch(
        &self,
        profile: AadProfile,
        data_key_id: Uuid,
        batch: i64,
    ) -> Result<Vec<(Uuid, OwnedIdentity, Vec<u8>)>, KeyringAdminError> {
        let (identity_columns, build): (&str, fn(&sqlx::postgres::PgRow) -> OwnedIdentity) =
            match profile {
                AadProfile::ConversationMessageContent => {
                    ("conversation_id, sequence_number", |row| {
                        OwnedIdentity::ConversationMessage {
                            message_id: row.get("id"),
                            conversation_id: row.get("conversation_id"),
                            sequence_number: row.get("sequence_number"),
                        }
                    })
                }
                AadProfile::ConversationSummaryText => {
                    ("conversation_id, covers_through_sequence", |row| {
                        OwnedIdentity::ConversationSummary {
                            summary_id: row.get("id"),
                            conversation_id: row.get("conversation_id"),
                            covers_through_sequence: row.get("covers_through_sequence"),
                        }
                    })
                }
                AadProfile::MemoryRecordContent => ("application_id, memory_scope", |row| {
                    OwnedIdentity::MemoryRecord {
                        memory_id: row.get("id"),
                        application_id: row.get("application_id"),
                        memory_scope: row.get("memory_scope"),
                    }
                }),
                AadProfile::RagDocumentVersionContent => ("document_id, version_number", |row| {
                    OwnedIdentity::RagDocumentVersion {
                        version_id: row.get("id"),
                        document_id: row.get("document_id"),
                        version_number: row.get("version_number"),
                    }
                }),
                AadProfile::RagChunkText => ("document_version_id, chunk_index", |row| {
                    OwnedIdentity::RagChunk {
                        chunk_id: row.get("id"),
                        document_version_id: row.get("document_version_id"),
                        chunk_index: row.get("chunk_index"),
                    }
                }),
            };

        let rows = sqlx::query(&format!(
            "select id, {identity_columns}, {column} as envelope
               from {table}
              where content_envelope_data_key_id({column}) = $1
              order by id
              limit $2",
            table = profile.table(),
            column = profile.column(),
        ))
        .bind(data_key_id)
        .bind(batch)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let id: Uuid = row.get("id");
                let envelope: Vec<u8> = row.get("envelope");
                (id, build(&row), envelope)
            })
            .collect())
    }
}

/// [`ContentIdentity`] with its one borrowed field owned, so a batch can outlive the row it was
/// read from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnedIdentity {
    ConversationMessage {
        message_id: Uuid,
        conversation_id: Uuid,
        sequence_number: i64,
    },
    ConversationSummary {
        summary_id: Uuid,
        conversation_id: Uuid,
        covers_through_sequence: i64,
    },
    MemoryRecord {
        memory_id: Uuid,
        application_id: Uuid,
        memory_scope: String,
    },
    RagDocumentVersion {
        version_id: Uuid,
        document_id: Uuid,
        version_number: i32,
    },
    RagChunk {
        chunk_id: Uuid,
        document_version_id: Uuid,
        chunk_index: i32,
    },
}

impl OwnedIdentity {
    fn borrow(&self) -> ContentIdentity<'_> {
        match self {
            Self::ConversationMessage {
                message_id,
                conversation_id,
                sequence_number,
            } => ContentIdentity::ConversationMessage {
                message_id: *message_id,
                conversation_id: *conversation_id,
                sequence_number: *sequence_number,
            },
            Self::ConversationSummary {
                summary_id,
                conversation_id,
                covers_through_sequence,
            } => ContentIdentity::ConversationSummary {
                summary_id: *summary_id,
                conversation_id: *conversation_id,
                covers_through_sequence: *covers_through_sequence,
            },
            Self::MemoryRecord {
                memory_id,
                application_id,
                memory_scope,
            } => ContentIdentity::MemoryRecord {
                memory_id: *memory_id,
                application_id: *application_id,
                memory_scope,
            },
            Self::RagDocumentVersion {
                version_id,
                document_id,
                version_number,
            } => ContentIdentity::RagDocumentVersion {
                version_id: *version_id,
                document_id: *document_id,
                version_number: *version_number,
            },
            Self::RagChunk {
                chunk_id,
                document_version_id,
                chunk_index,
            } => ContentIdentity::RagChunk {
                chunk_id: *chunk_id,
                document_version_id: *document_version_id,
                chunk_index: *chunk_index,
            },
        }
    }
}

/// Reads the data key id out of a stored envelope, in Rust.
///
/// Exposed so the property test can compare it against the migration's SQL function over
/// generated envelopes — the two are one format and the test is what keeps them one.
pub fn envelope_data_key_id(envelope: &[u8]) -> Option<Uuid> {
    EnvelopeHeader::parse(envelope)
        .ok()
        .map(|header| header.data_key_id)
}

#[cfg(test)]
mod tests;
