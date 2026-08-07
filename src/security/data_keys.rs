//! The content keyring: `content_data_keys` rows, unwrapped once at boot, held in process as a
//! snapshot of **cipher objects rather than key bytes**.
//!
//! Nothing here reads or writes a `*_encrypted` column. This is the third step of the
//! envelope-encryption train (`docs/decision-encryption-at-rest.md` §7 and §10); the write and
//! read paths arrive later. What ships now is the thing both of them will depend on, one full
//! release ahead, so that a keyring problem is found by an operator rolling out a release that
//! changes nothing rather than by a user whose message will not decrypt.
//!
//! # The three properties this module exists to hold
//!
//! **Custody is never called on the request path.** The keyring is unwrapped once, at boot, and
//! a decrypt afterwards is a `HashMap` lookup plus an `Arc` clone. That is what makes a later
//! swap to AWS KMS or Vault Transit an operational non-event instead of a per-row network call,
//! and [`ContentKeyring::database_loads`] plus the counting tests below defend it rather than a
//! comment.
//!
//! **Any unwrap failure at boot aborts.** A keyring that only partly opens means some stored
//! rows are unreadable, and learning that on a user's request is the lazy failure this design
//! refuses. The refusal names the key, the master key it wants, the master keys that *are*
//! configured, the variable to put one in, and **both** remedies — because without an escape
//! hatch a lost master key bricks the service forever, which is strictly worse than an audited
//! acknowledgement of data loss.
//!
//! **A failed refresh is not a failure.** The previous snapshot is still correct, merely stale,
//! so a refresh that cannot reach the database keeps serving from what it has and raises a
//! counter. Readiness reads the cached snapshot and never the database, so a database blip
//! cannot take a healthy replica out of rotation on a question it already knows the answer to.
//!
//! # Why this module holds a `PgPool`, when `security` otherwise holds none
//!
//! It is the one place where the trust boundary and the persistence boundary are the same
//! object: these rows are *sealed key material*, and the rule that they are unwrapped exactly
//! once, at boot, under a custody backend is a security rule rather than a storage one. Putting
//! the loader in `infra` and the policy here would split one invariant across two layers and
//! leave neither able to enforce it. The queries below are the only SQL in `src/security`, they
//! touch exactly one table, and they are deliberately kept trivial.
//!
//! # What is *not* here
//!
//! No rotation, no promotion, no `abandon`. Those are the CLI's. This module reads the keyring,
//! mints the very first key when there is none at all, and refuses to start when the keyring it
//! reads is not one it can fully open.

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use aes_gcm::{Aes256Gcm, aead::KeyInit, aead::OsRng};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    config::ContentEncryptionSettings,
    error::AppError,
    infra::metrics::MetricsRegistry,
    security::{
        AadProfile, ContentCipher, KeyCustodyError, MasterKeyCustody, WRAP_ALGORITHM_AES_256_GCM,
        WrappedKey, wrapped_data_key_aad,
    },
};

/// The fixed label a key check value is computed over.
///
/// Versioned in the string itself: changing the construction changes this label, so a keyring
/// written by an older build fails the check loudly instead of comparing two different things
/// and calling them equal.
pub const KEY_CHECK_VALUE_LABEL: &[u8] = b"moira/content-kcv/v1";

/// Width of a key check value, and of the `content_data_keys.key_check_value` column.
///
/// Eight bytes of a truncated HMAC under a 256-bit key: enough that two operators comparing
/// keyrings over a chat window will never see a collision, far too little to attack, and safe to
/// print anywhere — which is the point, because a value nobody dares log is a value nobody ever
/// compares.
pub const KEY_CHECK_VALUE_LEN: usize = 8;

/// Serialises the very first insert into `content_data_keys`.
///
/// House style, matching `infra::repositories::identity::OWNERSHIP_LOCK_KEY`: eight ASCII bytes,
/// so a `pg_locks` row is readable by eye rather than being an opaque integer.
const KEYRING_BOOTSTRAP_LOCK_KEY: i64 = i64::from_be_bytes(*b"moirakyr");

/// The `purpose` new content writes are sealed under.
const PURPOSE_CONTENT: &str = "content";

/// `key_check_value = HMAC-SHA256(dek, KEY_CHECK_VALUE_LABEL)[..8]`.
///
/// It proves a successful unwrap produced the *right* key rather than merely *a* key. Without
/// it, a custody backend that silently returned the wrong 32 bytes — a mis-keyed KMS alias, a
/// master key list edited into the wrong order — would be discovered by an AES-GCM tag failure
/// on some row, which reads as "your data is corrupt or somebody tampered with it" and sends the
/// operator down entirely the wrong path.
pub fn key_check_value(data_key: &Zeroizing<[u8; 32]>) -> [u8; KEY_CHECK_VALUE_LEN] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(data_key.as_slice())
        .expect("HMAC accepts a key of any length");
    mac.update(KEY_CHECK_VALUE_LABEL);
    let tag = mac.finalize().into_bytes();
    let mut value = [0u8; KEY_CHECK_VALUE_LEN];
    value.copy_from_slice(&tag[..KEY_CHECK_VALUE_LEN]);
    value
}

/// Lowercase hex, for log lines and `keyring status`.
pub fn format_key_check_value(value: &[u8]) -> String {
    let mut rendered = String::with_capacity(value.len() * 2);
    for byte in value {
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

/// What a data key is for.
///
/// Two purposes from day one because the second — deduplicating memory content by keyed digest —
/// must never share a key with content encryption: a digest key is an offline verifier for
/// anyone holding it, so its blast radius differs in kind. Declaring it now costs a `varchar`
/// check and makes the single-active index per *purpose* rather than global, which is the shape
/// that does not have to change later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKeyPurpose {
    Content,
    MemoryDedupe,
}

impl DataKeyPurpose {
    pub const ALL: [Self; 2] = [Self::Content, Self::MemoryDedupe];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Content => PURPOSE_CONTENT,
            Self::MemoryDedupe => "memory_dedupe",
        }
    }

    /// Resolves a stored value. `None` is a **refusal**, never a default: a purpose this build
    /// does not know is a keyring written by a newer binary, and guessing would seal content
    /// under a key reserved for something else.
    pub fn from_db(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|it| it.as_str() == value)
    }
}

/// A data key's position in its lifecycle. The migration's CHECK carries the same five.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKeyState {
    /// Minted, not yet writing.
    Pending,
    /// New writes use it. Exactly one per purpose, enforced by a partial unique index.
    Active,
    /// Not writable, still loaded, still readable — **forever**. Retiring a key is not a
    /// countdown to unreadability; it is a countdown to nothing new being written under it.
    Retiring,
    /// Not loaded. A row referencing it fails cleanly rather than lazily.
    Retired,
    /// The key is permanently lost and that loss has been explicitly acknowledged.
    Abandoned,
}

impl DataKeyState {
    pub const ALL: [Self; 5] = [
        Self::Pending,
        Self::Active,
        Self::Retiring,
        Self::Retired,
        Self::Abandoned,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Retiring => "retiring",
            Self::Retired => "retired",
            Self::Abandoned => "abandoned",
        }
    }

    /// Resolves a stored value; `None` is a refusal, for the reason [`DataKeyPurpose::from_db`]
    /// gives.
    pub fn from_db(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|it| it.as_str() == value)
    }

    /// Whether a snapshot load unwraps this key.
    ///
    /// `Abandoned` is deliberately **false**. It is carried as a marker so a row naming it earns
    /// a distinct refusal instead of "unknown key", but it is never unwrapped: attempting that
    /// would abort boot for precisely the key an operator has already confessed to losing, which
    /// is the deadlock `abandon` exists to break.
    pub const fn is_unwrapped_at_load(self) -> bool {
        matches!(self, Self::Pending | Self::Active | Self::Retiring)
    }

    /// Whether the snapshot carries an entry for this key at all.
    pub const fn is_carried_in_snapshot(self) -> bool {
        !matches!(self, Self::Retired)
    }

    /// Whether new content may be sealed under this key.
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// One keyring row, as the snapshot holds it.
///
/// `Debug` is hand-written and prints ids, states and the key check value only. A derived one
/// would render the [`ContentCipher`] — whose own `Debug` is safe, but the rule in this
/// subsystem is that no type grows a rendering of key material by inheriting one.
#[derive(Clone)]
pub struct KeyEntry {
    pub id: Uuid,
    pub key_version: i32,
    pub purpose: DataKeyPurpose,
    pub state: DataKeyState,
    pub custody_backend: String,
    pub master_key_id: String,
    pub key_check_value: [u8; KEY_CHECK_VALUE_LEN],
    pub created_at: DateTime<Utc>,
    /// Present for every state in [`DataKeyState::is_unwrapped_at_load`], `None` for
    /// `abandoned`. The `Option` *is* the abandonment marker: there is no second flag that could
    /// disagree with it.
    cipher: Option<Arc<ContentCipher>>,
}

impl KeyEntry {
    /// The cipher, or the typed refusal an abandoned key earns.
    pub fn cipher(&self) -> Result<Arc<ContentCipher>, KeyringError> {
        self.cipher
            .clone()
            .ok_or(KeyringError::AbandonedDataKey { id: self.id })
    }
}

impl std::fmt::Debug for KeyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyEntry")
            .field("id", &self.id)
            .field("key_version", &self.key_version)
            .field("purpose", &self.purpose)
            .field("state", &self.state)
            .field("custody_backend", &self.custody_backend)
            .field("master_key_id", &self.master_key_id)
            .field(
                "key_check_value",
                &format_key_check_value(&self.key_check_value),
            )
            .field("loaded", &self.cipher.is_some())
            .finish()
    }
}

/// An immutable view of the keyring, swapped in whole.
///
/// Held behind `RwLock<Arc<_>>`: a reader clones an `Arc` in nanoseconds and never blocks a
/// refresh, and a refresh never mutates a snapshot a reader is holding. No new crate is needed
/// for that, which is why there is not one.
pub struct KeyringSnapshot {
    /// The key new content is sealed under.
    ///
    /// A [`ContentCipher`] rather than the `(Uuid, cipher)` pair this is sometimes drawn as: the
    /// cipher already carries its `data_key_id` and stamps it into every envelope it seals, so a
    /// separate copy of the id would be a second source of truth that can only ever be wrong.
    active_content: Arc<ContentCipher>,
    /// Every non-retired key, abandoned ones included and marked.
    by_id: HashMap<Uuid, KeyEntry>,
    generation: u64,
    loaded_at: Instant,
}

impl KeyringSnapshot {
    /// The cipher new writes use.
    pub fn active_content(&self) -> Arc<ContentCipher> {
        self.active_content.clone()
    }

    pub fn active_content_key_id(&self) -> Uuid {
        self.active_content.data_key_id()
    }

    /// Monotonic across the life of one [`ContentKeyring`]. The single-flight gate compares it to
    /// decide whether a load already happened while it waited.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn loaded_at(&self) -> Instant {
        self.loaded_at
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn entry(&self, id: Uuid) -> Option<&KeyEntry> {
        self.by_id.get(&id)
    }

    /// Every entry, ordered by version — for `keyring status`, for `Debug`, and for the tests.
    pub fn entries(&self) -> Vec<&KeyEntry> {
        let mut entries: Vec<&KeyEntry> = self.by_id.values().collect();
        entries.sort_unstable_by_key(|entry| entry.key_version);
        entries
    }

    /// `None` means "this snapshot has never heard of that key", which is the only condition that
    /// justifies going back to the database. An abandoned key is `Some(Err(_))` — known, and
    /// known to be unopenable — so it never triggers a reload.
    fn resolve(&self, id: Uuid) -> Option<Result<Arc<ContentCipher>, KeyringError>> {
        self.by_id.get(&id).map(KeyEntry::cipher)
    }
}

impl std::fmt::Debug for KeyringSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringSnapshot")
            .field("active_content_key_id", &self.active_content_key_id())
            .field("generation", &self.generation)
            .field("keys", &self.entries())
            .finish()
    }
}

/// Every way the keyring can refuse.
#[derive(Debug, thiserror::Error)]
pub enum KeyringError {
    #[error("content keyring database error: {0}")]
    Database(#[from] sqlx::Error),

    /// The FATAL boot refusal. Its `Display` **is** the deliverable — an operator reading it at
    /// three in the morning gets the key, the master key it wants, what is actually loaded, where
    /// to put one, and the only two ways out. The test asserting this text asserts the remedy.
    #[error(
        "FATAL: content data key {data_key_id} is sealed under master key {master_key_id:?}, \
         which this process cannot open ({source}).\n\
         Master key ids configured for backend {backend:?}: {configured:?}, from \
         MOIRA_CONTENT_ENCRYPTION__KEYS.\n\
         Moira refuses to start with a keyring it can only partly open: every row sealed under \
         {data_key_id} would fail on a user's request instead of here.\n\
         There are exactly two remedies:\n  \
         1. Restore the master key — add \"{master_key_id}:<base64 of 32 bytes>\" to \
         MOIRA_CONTENT_ENCRYPTION__KEYS and restart. Nothing is re-encrypted.\n  \
         2. Abandon the data key — run `moira keyring abandon {data_key_id} --confirm --reason \
         \"<text>\"`. Moira then starts, every other key keeps working, and rows sealed under \
         {data_key_id} become permanently unreadable. That is data loss, on purpose, recorded."
    )]
    UnopenableDataKey {
        data_key_id: Uuid,
        master_key_id: String,
        backend: &'static str,
        configured: Vec<String>,
        source: KeyCustodyError,
    },

    /// The unwrap succeeded and produced the wrong key. Distinguished from
    /// [`Self::UnopenableDataKey`] because the remedy differs in kind: the master key list is not
    /// missing an entry, it holds the wrong bytes under a name that is present.
    #[error(
        "FATAL: content data key {data_key_id} unwrapped under master key {master_key_id:?}, but \
         the resulting key does not match the stored key check value (stored {stored}, computed \
         {computed}). The master key configured under this id in MOIRA_CONTENT_ENCRYPTION__KEYS \
         is not the one that sealed this data key. Restore the correct bytes for \
         {master_key_id:?}, or run `moira keyring abandon {data_key_id} --confirm --reason \
         \"<text>\"` to acknowledge the loss."
    )]
    KeyCheckValueMismatch {
        data_key_id: Uuid,
        master_key_id: String,
        stored: String,
        computed: String,
    },

    /// A non-empty keyring with nothing to write under. Bootstrap deliberately does not fire here
    /// — see [`ContentKeyring::bootstrap_if_empty`] — so this is a refusal, not a repair.
    #[error(
        "FATAL: content keyring holds {loaded} key(s) but none is 'active' for purpose \
         {purpose:?}; new content could not be sealed. Promote one with \
         `moira keyring promote <id>`."
    )]
    NoActiveKey {
        purpose: &'static str,
        loaded: usize,
    },

    /// A row this build cannot interpret — an unknown `state` or `purpose`, or a key check value
    /// of the wrong width. Almost always a keyring written by a newer binary.
    #[error("content keyring row {data_key_id} is not one this build understands: {detail}")]
    UnreadableRow { data_key_id: Uuid, detail: String },

    /// No key by that id, after one refresh. A retired key looks exactly like this, which is what
    /// "a row referencing it fails cleanly" means.
    #[error("content data key {data_key_id} is not in the keyring")]
    UnknownDataKey { data_key_id: Uuid },

    /// Known, and known to be unopenable.
    #[error(
        "content data key {id} was abandoned; the master key that sealed it is gone and the loss \
         was acknowledged. Rows sealed under it are permanently unreadable."
    )]
    AbandonedDataKey { id: Uuid },

    #[error("content key custody failed: {0}")]
    Custody(#[from] KeyCustodyError),
}

impl From<KeyringError> for AppError {
    /// Every variant is a configuration or custody fault rather than a caller fault, and every one
    /// of them reachable today is reached during boot, where `AppError::Config` is what `main`
    /// turns into a non-zero exit. The request-path variants ([`KeyringError::UnknownDataKey`],
    /// [`KeyringError::AbandonedDataKey`]) acquire their own typed API mapping in the PR that
    /// gives them a call site; mapping them speculatively now would fix a public error contract
    /// nothing has exercised.
    fn from(error: KeyringError) -> Self {
        AppError::Config(error.to_string())
    }
}

/// The database-held keyring and its in-process snapshot.
pub struct ContentKeyring {
    /// `std` rather than `tokio`: every access is a clone of an `Arc` with no `.await` inside the
    /// critical section, so an async lock would buy nothing and cost a task wake-up.
    snapshot: RwLock<Arc<KeyringSnapshot>>,
    custody: Arc<dyn MasterKeyCustody>,
    pool: PgPool,
    /// One permit. The single-flight gate: concurrent misses queue here, and all but the first
    /// find the generation already advanced and never issue a second query.
    refresh_gate: Semaphore,
    refresh_interval: Duration,
    min_refresh: Duration,
    rotation_age: Duration,
    metrics: MetricsRegistry,
    /// Queries against `content_data_keys` that actually ran. Public because the property it
    /// measures — "custody is never called on the request path, and a miss storm costs one
    /// query" — is defended by counting rather than by comment.
    database_loads: AtomicU64,
    /// Misses that found the generation already advanced — the single-flight gate doing its job.
    ///
    /// Counted separately from [`Self::floor_throttles`] because a load count alone cannot tell
    /// the two apart, and a test that cannot tell them apart is not testing single flight: with
    /// the generation check deleted, the floor collapses a concurrent burst just as well and the
    /// load count stays at one. These two counters are what make that mutation visible.
    single_flight_collapses: AtomicU64,
    /// Misses refused by `min_refresh_seconds` without a load.
    floor_throttles: AtomicU64,
    generation: AtomicU64,
    /// Milliseconds since [`Self::origin`] at the last on-demand refresh, or [`NEVER`].
    last_on_demand_ms: AtomicU64,
    origin: Instant,
}

/// Sentinel for "no on-demand refresh has happened yet", so the first miss after boot is never
/// throttled by a floor measured from a refresh that did not occur.
const NEVER: u64 = u64::MAX;

impl ContentKeyring {
    /// Boot steps 3 through 5 of `docs/decision-encryption-at-rest.md` §10, in order.
    ///
    /// Runs before the listener binds. `custody.preflight()` (step 2) has already happened in
    /// `AppState::new`; this is what comes after it.
    pub async fn load(
        pool: PgPool,
        custody: Arc<dyn MasterKeyCustody>,
        settings: &ContentEncryptionSettings,
        metrics: MetricsRegistry,
    ) -> Result<Self, KeyringError> {
        match Self::load_inner(pool, custody, settings, metrics).await {
            Ok(keyring) => Ok(keyring),
            Err(error) => {
                // Logged *and* returned. The log line is what an operator sees in `kubectl
                // logs`; the returned error is what becomes the non-zero exit that stops the pod
                // ever becoming Ready. Either alone leaves half the failure invisible.
                error!(%error, "content keyring refused to load; Moira will not start");
                Err(error)
            }
        }
    }

    async fn load_inner(
        pool: PgPool,
        custody: Arc<dyn MasterKeyCustody>,
        settings: &ContentEncryptionSettings,
        metrics: MetricsRegistry,
    ) -> Result<Self, KeyringError> {
        let database_loads = AtomicU64::new(0);

        // Step 3, then step 4 only if step 3 found nothing at all. The order matters: a
        // deployment whose keyring exists but cannot be opened must get the refusal, never a
        // silent second key. Bootstrap is reachable only from a strictly empty table, and it
        // re-checks that emptiness under the advisory lock it takes.
        let mut rows = Self::fetch_rows(&pool, &database_loads).await?;
        if rows.is_empty() {
            Self::bootstrap_if_empty(&pool, custody.as_ref()).await?;
            rows = Self::fetch_rows(&pool, &database_loads).await?;
        }

        let snapshot = Self::assemble(rows, custody.as_ref(), 0).await?;

        let keyring = Self {
            snapshot: RwLock::new(Arc::new(snapshot)),
            custody,
            pool,
            refresh_gate: Semaphore::new(1),
            refresh_interval: Duration::from_secs(settings.refresh_seconds.max(1)),
            min_refresh: Duration::from_secs(settings.min_refresh_seconds.max(1)),
            rotation_age: Duration::from_secs(
                u64::from(settings.data_key_rotation_days).saturating_mul(86_400),
            ),
            metrics,
            database_loads,
            single_flight_collapses: AtomicU64::new(0),
            floor_throttles: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            last_on_demand_ms: AtomicU64::new(NEVER),
            origin: Instant::now(),
        };
        keyring.announce();
        Ok(keyring)
    }

    /// The current snapshot. A clone of an `Arc`; nothing here can block on the database.
    ///
    /// This is what readiness reads.
    pub fn snapshot(&self) -> Arc<KeyringSnapshot> {
        self.snapshot
            .read()
            .expect("the content keyring snapshot lock is never held across a panic")
            .clone()
    }

    /// The cipher new content is sealed under.
    pub fn active_content_cipher(&self) -> Arc<ContentCipher> {
        self.snapshot().active_content()
    }

    /// Queries against `content_data_keys` issued by this process, boot included.
    pub fn database_loads(&self) -> u64 {
        self.database_loads.load(Ordering::SeqCst)
    }

    /// Misses that a concurrent load had already satisfied by the time they took the gate.
    pub fn single_flight_collapses(&self) -> u64 {
        self.single_flight_collapses.load(Ordering::SeqCst)
    }

    /// Misses refused without a load because `min_refresh_seconds` had not elapsed.
    pub fn floor_throttles(&self) -> u64 {
        self.floor_throttles.load(Ordering::SeqCst)
    }

    /// Resolve the cipher an envelope's header names.
    ///
    /// A hit is a `HashMap` lookup and an `Arc` clone — **no custody call and no query**. A miss
    /// takes the single-flight gate, reloads at most once and retries at most once; every
    /// concurrent miss behind the same gate observes the load the first one performed rather than
    /// issuing its own.
    pub async fn cipher_for(&self, data_key_id: Uuid) -> Result<Arc<ContentCipher>, KeyringError> {
        let snapshot = self.snapshot();
        if let Some(resolved) = snapshot.resolve(data_key_id) {
            return resolved;
        }

        let refreshed = self.refresh_for_miss(snapshot.generation()).await;
        refreshed
            .resolve(data_key_id)
            .unwrap_or(Err(KeyringError::UnknownDataKey { data_key_id }))
    }

    /// Reload from the database and swap the snapshot in.
    ///
    /// On failure the previous snapshot is **kept**: it is still correct, merely stale, and a
    /// keyring that emptied itself because a connection blipped would turn a database hiccup into
    /// unreadable content. See [`Self::refresh_or_serve_on`], which is what every caller except a
    /// test uses.
    pub async fn refresh(&self) -> Result<Arc<KeyringSnapshot>, KeyringError> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let rows = Self::fetch_rows(&self.pool, &self.database_loads).await?;
        let snapshot = Arc::new(Self::assemble(rows, self.custody.as_ref(), generation).await?);
        *self
            .snapshot
            .write()
            .expect("the content keyring snapshot lock is never held across a panic") =
            snapshot.clone();
        self.metrics.record_content_keyring_refresh(true);
        self.metrics.set_content_keyring_age_seconds(0.0);
        Ok(snapshot)
    }

    /// The background tick.
    ///
    /// Detached, like the runtime-config listener: there is nothing to join on, and a keyring that
    /// stopped refreshing keeps serving correctly from its last snapshot — the same posture a
    /// failed refresh has.
    pub fn spawn_refresh(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let keyring = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(keyring.refresh_interval);
            // The first tick fires immediately; boot has just loaded, so spend it.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                keyring.refresh_or_serve_on().await;
            }
        })
    }

    /// One refresh, with the failure absorbed. The unit the background tick runs.
    async fn refresh_or_serve_on(&self) -> Arc<KeyringSnapshot> {
        match self.refresh().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let stale = self.snapshot();
                let age = stale.loaded_at().elapsed();
                self.metrics.record_content_keyring_refresh(false);
                self.metrics
                    .set_content_keyring_age_seconds(age.as_secs_f64());
                warn!(
                    %error,
                    generation = stale.generation(),
                    keys_loaded = stale.len(),
                    keyring_age_seconds = age.as_secs_f64(),
                    "content keyring refresh failed; continuing on the previous snapshot"
                );
                stale
            }
        }
    }

    /// The single-flight body.
    ///
    /// Two gates, answering different questions. The **generation check** collapses a concurrent
    /// burst: everyone queued behind the permit observes the generation the first holder advanced
    /// and returns without querying, so 100 simultaneous misses cost one load. The **floor**
    /// collapses a serial stream: misses arriving one after another for a key that genuinely does
    /// not exist would otherwise each pay for their own query, because by then the generation has
    /// caught up with them.
    ///
    /// The floor is measured from the last *on-demand* refresh, never from boot, so the first miss
    /// a process ever sees always gets its reload.
    async fn refresh_for_miss(&self, observed_generation: u64) -> Arc<KeyringSnapshot> {
        let _permit = self
            .refresh_gate
            .acquire()
            .await
            .expect("the content keyring refresh gate is never closed");

        let current = self.snapshot();
        if current.generation() != observed_generation {
            self.single_flight_collapses.fetch_add(1, Ordering::SeqCst);
            return current;
        }

        let now_ms = u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        let last = self.last_on_demand_ms.load(Ordering::SeqCst);
        if last != NEVER && Duration::from_millis(now_ms.saturating_sub(last)) < self.min_refresh {
            self.floor_throttles.fetch_add(1, Ordering::SeqCst);
            return current;
        }
        self.last_on_demand_ms.store(now_ms, Ordering::SeqCst);

        self.refresh_or_serve_on().await
    }

    /// The one structured startup line, plus the rotation warning.
    ///
    /// Every field on it is an id, a count or a check value. **No key material, and nothing
    /// derived from any**: the key check value is a truncated HMAC of a fixed label, which is
    /// exactly why it is the field an operator is given to compare keyrings with.
    fn announce(&self) {
        let snapshot = self.snapshot();
        let active = snapshot.active_content_key_id();
        let entry = snapshot
            .entry(active)
            .expect("the active key is in the snapshot it was chosen from");
        let age = Utc::now().signed_duration_since(entry.created_at);
        let age_days = age.num_days().max(0);

        info!(
            custody_backend = self.custody.backend_name(),
            master_key_id = self.custody.active_master_key_id(),
            active_data_key_id = %active,
            active_data_key_version = entry.key_version,
            active_data_key_age_days = age_days,
            active_key_check_value = %format_key_check_value(&entry.key_check_value),
            keys_loaded = snapshot.len(),
            // From the profile registry rather than a literal, so a sixth sealed column cannot be
            // added without this line counting it.
            sealed_columns = AadProfile::ALL.len(),
            "content keyring loaded"
        );

        let overdue = age
            .to_std()
            .is_ok_and(|age| !self.rotation_age.is_zero() && age > self.rotation_age);
        if overdue {
            warn!(
                active_data_key_id = %active,
                active_data_key_age_days = age_days,
                rotation_days = self.rotation_age.as_secs() / 86_400,
                "the active content data key is older than the configured rotation interval; \
                 rotation may be wedged"
            );
        }
    }

    /// Mint the very first key — and **only** the very first.
    ///
    /// The strictly-empty bound is what makes this convenience safe rather than dangerous. An
    /// existing deployment booted against the wrong master key has rows in this table, so it
    /// reaches the refusal in [`Self::assemble`] and never a silent second key that would split
    /// its content across two keyrings. The advisory lock is what makes "strictly empty" true at
    /// insert time rather than merely at check time: two pods starting together both see an empty
    /// table, and the second re-reads it inside the lock and finds the first one's row.
    ///
    /// The partial unique index is the backstop under both. Even if this function were wrong, a
    /// second `active` row for `content` cannot exist.
    async fn bootstrap_if_empty(
        pool: &PgPool,
        custody: &dyn MasterKeyCustody,
    ) -> Result<bool, KeyringError> {
        let mut tx = pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(KEYRING_BOOTSTRAP_LOCK_KEY)
            .execute(&mut *tx)
            .await?;

        // `count(*)` over the whole table, deliberately: not "no active content key", not "no
        // content-purpose key". Any row at all means somebody else already decided what this
        // keyring is, and this process is not entitled to add to it.
        let existing: i64 = sqlx::query_scalar("select count(*) from content_data_keys")
            .fetch_one(&mut *tx)
            .await?;
        if existing != 0 {
            tx.rollback().await?;
            return Ok(false);
        }

        let id = Uuid::now_v7();
        let mut data_key = Zeroizing::new([0u8; 32]);
        data_key.copy_from_slice(Aes256Gcm::generate_key(&mut OsRng).as_slice());
        let check_value = key_check_value(&data_key);

        let aad = wrapped_data_key_aad(
            id,
            custody.active_master_key_id(),
            WRAP_ALGORITHM_AES_256_GCM,
        );
        let wrapped = custody.wrap(&data_key, aad.as_bytes()).await?;
        // The AAD names the algorithm, so a backend choosing a different one than the AAD assumed
        // would produce a blob nothing can ever open. Refuse before it reaches a column.
        if wrapped.wrap_algorithm != WRAP_ALGORITHM_AES_256_GCM {
            return Err(KeyringError::UnreadableRow {
                data_key_id: id,
                detail: format!(
                    "custody backend {:?} wrapped the first data key with algorithm {:?}, but the \
                     wrap AAD was built for {WRAP_ALGORITHM_AES_256_GCM:?}",
                    custody.backend_name(),
                    wrapped.wrap_algorithm,
                ),
            });
        }

        sqlx::query(
            "insert into content_data_keys (
                 id, key_version, purpose, state, custody_backend, master_key_id,
                 wrap_algorithm, wrap_nonce, wrapped_key, key_check_value, activated_at
             ) values ($1, 1, $2, 'active', $3, $4, $5, $6, $7, $8, now())",
        )
        .bind(id)
        .bind(PURPOSE_CONTENT)
        .bind(custody.backend_name())
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
            custody_backend = custody.backend_name(),
            master_key_id = custody.active_master_key_id(),
            key_check_value = %format_key_check_value(&check_value),
            "content keyring was empty; minted the first content data key"
        );
        Ok(true)
    }

    async fn fetch_rows(pool: &PgPool, loads: &AtomicU64) -> Result<Vec<KeyringRow>, sqlx::Error> {
        loads.fetch_add(1, Ordering::SeqCst);
        sqlx::query_as::<_, KeyringRow>(
            "select id, key_version, purpose, state, custody_backend, master_key_id,
                    wrap_algorithm, wrap_nonce, wrapped_key, key_check_value, created_at
               from content_data_keys
              where state <> 'retired'
              order by key_version",
        )
        .fetch_all(pool)
        .await
    }

    /// Unwrap every loadable row, verify each key check value, and pick the active content key.
    ///
    /// **Every failure here is a refusal.** There is no "skip the ones that did not open": that
    /// is precisely the partly-openable keyring this design exists to reject.
    async fn assemble(
        rows: Vec<KeyringRow>,
        custody: &dyn MasterKeyCustody,
        generation: u64,
    ) -> Result<KeyringSnapshot, KeyringError> {
        let mut by_id = HashMap::with_capacity(rows.len());
        let mut active_content: Option<Arc<ContentCipher>> = None;

        for row in rows {
            let purpose = DataKeyPurpose::from_db(&row.purpose).ok_or_else(|| {
                KeyringError::UnreadableRow {
                    data_key_id: row.id,
                    detail: format!("unknown purpose {:?}", row.purpose),
                }
            })?;
            let state =
                DataKeyState::from_db(&row.state).ok_or_else(|| KeyringError::UnreadableRow {
                    data_key_id: row.id,
                    detail: format!("unknown state {:?}", row.state),
                })?;
            if !state.is_carried_in_snapshot() {
                continue;
            }
            let stored_check_value: [u8; KEY_CHECK_VALUE_LEN] =
                row.key_check_value.as_slice().try_into().map_err(|_| {
                    KeyringError::UnreadableRow {
                        data_key_id: row.id,
                        detail: format!(
                            "key check value is {} bytes, expected {KEY_CHECK_VALUE_LEN}",
                            row.key_check_value.len()
                        ),
                    }
                })?;

            let cipher = if state.is_unwrapped_at_load() {
                let aad = wrapped_data_key_aad(row.id, &row.master_key_id, &row.wrap_algorithm);
                let wrapped = WrappedKey {
                    master_key_id: row.master_key_id.clone(),
                    wrap_algorithm: row.wrap_algorithm.clone(),
                    nonce: row.wrap_nonce.clone(),
                    wrapped: row.wrapped_key.clone(),
                };
                let data_key =
                    custody
                        .unwrap(&wrapped, aad.as_bytes())
                        .await
                        .map_err(|source| KeyringError::UnopenableDataKey {
                            data_key_id: row.id,
                            master_key_id: row.master_key_id.clone(),
                            backend: custody.backend_name(),
                            configured: custody.master_key_ids(),
                            source,
                        })?;

                // The unwrap said yes. This says it said yes about the *right* key.
                let computed = key_check_value(&data_key);
                if computed != stored_check_value {
                    return Err(KeyringError::KeyCheckValueMismatch {
                        data_key_id: row.id,
                        master_key_id: row.master_key_id.clone(),
                        stored: format_key_check_value(&stored_check_value),
                        computed: format_key_check_value(&computed),
                    });
                }
                Some(Arc::new(ContentCipher::new(row.id, &data_key)))
            } else {
                None
            };

            if purpose == DataKeyPurpose::Content && state.is_writable() {
                active_content.clone_from(&cipher);
            }

            by_id.insert(
                row.id,
                KeyEntry {
                    id: row.id,
                    key_version: row.key_version,
                    purpose,
                    state,
                    custody_backend: row.custody_backend,
                    master_key_id: row.master_key_id,
                    key_check_value: stored_check_value,
                    created_at: row.created_at,
                    cipher,
                },
            );
        }

        let active_content = active_content.ok_or(KeyringError::NoActiveKey {
            purpose: PURPOSE_CONTENT,
            loaded: by_id.len(),
        })?;

        Ok(KeyringSnapshot {
            active_content,
            by_id,
            generation,
            loaded_at: Instant::now(),
        })
    }
}

impl std::fmt::Debug for ContentKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentKeyring")
            .field("custody", &self.custody)
            .field("snapshot", &self.snapshot())
            .field("database_loads", &self.database_loads())
            .finish_non_exhaustive()
    }
}

/// One `content_data_keys` row, exactly as stored.
///
/// `wrap_nonce` and `wrapped_key` are separate columns rather than one self-framed blob because
/// the wrap format belongs to the *backend*, not to the keyring: a KMS ciphertext carries its own
/// framing and leaves `wrap_nonce` empty, and a row has to stay readable by a `select` either way.
#[derive(sqlx::FromRow)]
struct KeyringRow {
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
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::security::{
        ContentIdentity, EnvelopeHeader, EnvironmentMasterKeyCustody, MIN_ENVELOPE_LEN,
    };

    // ---------------------------------------------------------------------------------
    // A custody that counts, and can be made to fail on demand.
    // ---------------------------------------------------------------------------------

    /// Wraps the real environment backend so the tests exercise real AES-GCM, and adds the two
    /// things a fake is for: a call counter, and a set of master key ids it refuses to open.
    ///
    /// Deliberately not a hand-rolled cipher. A fake that "unwrapped" by returning the bytes it
    /// was given would let a keyring that never actually decrypts anything pass every test below.
    #[derive(Debug)]
    struct CountingCustody {
        inner: EnvironmentMasterKeyCustody,
        unwrap_calls: AtomicU64,
    }

    impl CountingCustody {
        fn new(ids: &[(&str, u8)], active: &str) -> Self {
            let keys = ids
                .iter()
                .map(|(id, byte)| ((*id).to_string(), Zeroizing::new([*byte; 32])))
                .collect();
            Self {
                inner: EnvironmentMasterKeyCustody::new(keys, active).expect("custody"),
                unwrap_calls: AtomicU64::new(0),
            }
        }

        fn unwrap_calls(&self) -> u64 {
            self.unwrap_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl MasterKeyCustody for CountingCustody {
        fn backend_name(&self) -> &'static str {
            self.inner.backend_name()
        }
        fn active_master_key_id(&self) -> &str {
            self.inner.active_master_key_id()
        }
        fn can_unwrap(&self, master_key_id: &str) -> bool {
            self.inner.can_unwrap(master_key_id)
        }
        fn master_key_ids(&self) -> Vec<String> {
            self.inner.master_key_ids()
        }
        async fn wrap(
            &self,
            dek: &Zeroizing<[u8; 32]>,
            aad: &[u8],
        ) -> Result<WrappedKey, KeyCustodyError> {
            self.inner.wrap(dek, aad).await
        }
        async fn unwrap(
            &self,
            wrapped: &WrappedKey,
            aad: &[u8],
        ) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError> {
            self.unwrap_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.unwrap(wrapped, aad).await
        }
        async fn preflight(&self) -> Result<(), KeyCustodyError> {
            self.inner.preflight().await
        }
    }

    // ---------------------------------------------------------------------------------
    // Fixtures.
    // ---------------------------------------------------------------------------------

    /// A long floor and a long tick, deliberately. Every assertion below is about *whether* a
    /// load happened, never about waiting for one, so a short interval would only buy flakes.
    fn settings() -> ContentEncryptionSettings {
        ContentEncryptionSettings {
            refresh_seconds: 300,
            min_refresh_seconds: 30,
            ..ContentEncryptionSettings::default()
        }
    }

    fn metrics() -> MetricsRegistry {
        MetricsRegistry::new("moira-test", None)
    }

    /// A pool onto a private schema holding its own `content_data_keys`.
    ///
    /// The lib-test database is shared by every test in this binary, and half of these tests
    /// assert on a *strictly empty* table. `like public.content_data_keys including all` clones
    /// the columns, defaults, checks, primary key and the partial unique index from the real
    /// migration rather than restating them, so what these tests run against is the schema the
    /// migration produced — including the single-active index — and not a hand-written copy that
    /// could drift from it.
    async fn isolated_pool(database: &crate::test_support::LibTestDatabase) -> PgPool {
        let schema = format!("keyring_{}", Uuid::now_v7().simple());
        sqlx::query(&format!("create schema \"{schema}\""))
            .execute(database.pool())
            .await
            .expect("create schema");
        sqlx::query(&format!(
            "create table \"{schema}\".content_data_keys \
             (like public.content_data_keys including all)"
        ))
        .execute(database.pool())
        .await
        .expect("clone content_data_keys");

        PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |conn, _| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query(&format!("set search_path to \"{schema}\", public"))
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(database.url())
            .await
            .expect("isolated pool")
    }

    /// Inserts a key sealed under `sealer`'s **active** master key.
    ///
    /// A row sealed under an older master key is produced by handing this a custody whose active
    /// key is that older one — which is exactly the shape a master-key rotation leaves behind, and
    /// is why the fixture never relabels a blob it did not seal.
    async fn insert_key(
        pool: &PgPool,
        sealer: &dyn MasterKeyCustody,
        version: i32,
        state: DataKeyState,
        dek_byte: u8,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let data_key = Zeroizing::new([dek_byte; 32]);
        let check_value = key_check_value(&data_key);
        let aad = wrapped_data_key_aad(
            id,
            sealer.active_master_key_id(),
            WRAP_ALGORITHM_AES_256_GCM,
        );
        let wrapped = sealer.wrap(&data_key, aad.as_bytes()).await.expect("wrap");

        sqlx::query(
            "insert into content_data_keys (
                 id, key_version, purpose, state, custody_backend, master_key_id,
                 wrap_algorithm, wrap_nonce, wrapped_key, key_check_value, activated_at
             ) values ($1, $2, 'content', $3, 'environment', $4, $5, $6, $7, $8, now())",
        )
        .bind(id)
        .bind(version)
        .bind(state.as_str())
        .bind(&wrapped.master_key_id)
        .bind(&wrapped.wrap_algorithm)
        .bind(&wrapped.nonce)
        .bind(&wrapped.wrapped)
        .bind(check_value.as_slice())
        .execute(pool)
        .await
        .expect("insert key");
        id
    }

    async fn key_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar("select count(*) from content_data_keys")
            .fetch_one(pool)
            .await
            .expect("count")
    }

    // ---------------------------------------------------------------------------------
    // Group 1 — assemble, unwrap and key-check-verify from fixture rows.
    // ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn assembles_unwraps_and_verifies_every_loadable_state() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let current = CountingCustody::new(&[("k1", 1)], "k1");
        // Sealed under a master key that is no longer active but is still configured — the
        // rotation case, and the reason `retiring` stays loaded forever.
        let previous = CountingCustody::new(&[("old", 2)], "old");

        let pending = insert_key(&pool, &current, 1, DataKeyState::Pending, 10).await;
        let active = insert_key(&pool, &current, 2, DataKeyState::Active, 11).await;
        let retiring = insert_key(&pool, &previous, 3, DataKeyState::Retiring, 12).await;
        let retired = insert_key(&pool, &current, 4, DataKeyState::Retired, 13).await;
        let abandoned = insert_key(&pool, &current, 5, DataKeyState::Abandoned, 14).await;

        let custody = Arc::new(CountingCustody::new(&[("k1", 1), ("old", 2)], "k1"));
        let keyring = ContentKeyring::load(pool, custody.clone(), &settings(), metrics())
            .await
            .expect("load");
        let snapshot = keyring.snapshot();

        // Four carried, one dropped. A `retired` key is absent, which is what makes a row naming
        // it fail cleanly rather than decrypt.
        assert_eq!(snapshot.len(), 4);
        assert!(snapshot.entry(retired).is_none());
        assert_eq!(snapshot.active_content_key_id(), active);

        // Three unwrapped, and exactly three: `abandoned` is carried without ever being handed to
        // custody, and `retired` is not read at all. The count *is* the assertion — a keyring that
        // also tried to unwrap the abandoned key would still have produced a usable snapshot.
        assert_eq!(custody.unwrap_calls(), 3);
        for id in [pending, active, retiring] {
            assert!(snapshot.entry(id).expect("entry").cipher().is_ok());
        }
        assert!(matches!(
            snapshot.entry(abandoned).expect("entry").cipher(),
            Err(KeyringError::AbandonedDataKey { id }) if id == abandoned
        ));

        // The unwrap produced the *right* key, not merely a key: seal with the cipher the keyring
        // resolved, and open it with one built from the DEK the fixture wrote.
        let cipher = keyring.cipher_for(active).await.expect("active cipher");
        let identity = ContentIdentity::ConversationMessage {
            message_id: Uuid::from_u128(1),
            conversation_id: Uuid::from_u128(2),
            sequence_number: 7,
        };
        let envelope = cipher.seal(&identity, b"keyring round trip").expect("seal");
        let expected = ContentCipher::new(active, &Zeroizing::new([11u8; 32]));
        assert_eq!(
            expected
                .open(&envelope, &identity)
                .expect("open")
                .as_slice(),
            b"keyring round trip"
        );

        // And the key sealed under the *retired* master key opened too, without re-encrypting
        // anything — the property that makes a master-key rotation a configuration change.
        let retiring_cipher = keyring.cipher_for(retiring).await.expect("retiring cipher");
        assert_eq!(retiring_cipher.data_key_id(), retiring);
    }

    #[tokio::test]
    async fn a_key_check_value_that_does_not_match_refuses_the_whole_keyring() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        let id = insert_key(&pool, custody.as_ref(), 1, DataKeyState::Active, 20).await;

        // The one edit a keyring without a key check value could not notice: the row still
        // unwraps, under a configured master key, to a perfectly valid 32-byte AES key.
        sqlx::query("update content_data_keys set key_check_value = $1 where id = $2")
            .bind(vec![0xffu8; KEY_CHECK_VALUE_LEN])
            .bind(id)
            .execute(&pool)
            .await
            .expect("corrupt the check value");

        let error = ContentKeyring::load(pool, custody, &settings(), metrics())
            .await
            .expect_err("a mismatched key check value must refuse the keyring");
        assert!(
            matches!(&error, KeyringError::KeyCheckValueMismatch { data_key_id, .. }
                if *data_key_id == id),
            "{error:?}"
        );
        let rendered = error.to_string();
        assert!(rendered.contains(&id.to_string()), "{rendered}");
        assert!(
            rendered.contains("MOIRA_CONTENT_ENCRYPTION__KEYS"),
            "{rendered}"
        );
        assert!(rendered.contains("abandon"), "{rendered}");
    }

    #[tokio::test]
    async fn a_keyring_with_no_active_content_key_refuses_rather_than_bootstrapping() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        insert_key(&pool, custody.as_ref(), 1, DataKeyState::Retiring, 30).await;

        let error = ContentKeyring::load(pool.clone(), custody, &settings(), metrics())
            .await
            .expect_err("no active key must refuse");
        assert!(
            matches!(error, KeyringError::NoActiveKey { .. }),
            "{error:?}"
        );
        // And it did not "repair" itself by minting a second key beside the one that is there.
        assert_eq!(key_count(&pool).await, 1);
    }

    // ---------------------------------------------------------------------------------
    // Group 2 — an unknown key id triggers exactly one refresh, then succeeds.
    // ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn an_unknown_key_id_triggers_exactly_one_refresh_then_succeeds() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        insert_key(&pool, custody.as_ref(), 1, DataKeyState::Active, 40).await;

        let keyring = ContentKeyring::load(pool.clone(), custody.clone(), &settings(), metrics())
            .await
            .expect("load");
        let loads_after_boot = keyring.database_loads();

        // A key that arrived after this process booted — a rotation, seen from the replica that
        // did not perform it.
        let latecomer = insert_key(&pool, custody.as_ref(), 2, DataKeyState::Retiring, 41).await;
        assert!(keyring.snapshot().entry(latecomer).is_none());

        let cipher = keyring.cipher_for(latecomer).await.expect("resolved");
        // Bound to the id that was asked for, not to whichever key the reload made active.
        assert_eq!(cipher.data_key_id(), latecomer);
        assert_eq!(
            keyring.database_loads() - loads_after_boot,
            1,
            "one miss must cost exactly one load"
        );

        // The second read of the same key costs nothing: the whole point of the snapshot.
        let loads = keyring.database_loads();
        keyring.cipher_for(latecomer).await.expect("cached");
        assert_eq!(keyring.database_loads(), loads);
        // And the retry really did decrypt: this cipher opens what a cipher built from the
        // fixture's DEK sealed.
        let identity = ContentIdentity::RagChunk {
            chunk_id: Uuid::from_u128(21),
            document_version_id: Uuid::from_u128(22),
            chunk_index: 3,
        };
        let sealed = ContentCipher::new(latecomer, &Zeroizing::new([41u8; 32]))
            .seal(&identity, b"arrived late")
            .expect("seal");
        assert_eq!(
            cipher.open(&sealed, &identity).expect("open").as_slice(),
            b"arrived late"
        );
    }

    #[tokio::test]
    async fn a_key_that_is_still_missing_after_a_reload_fails_cleanly_and_is_then_floored() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        insert_key(&pool, custody.as_ref(), 1, DataKeyState::Active, 42).await;
        let keyring = ContentKeyring::load(pool, custody, &settings(), metrics())
            .await
            .expect("load");
        let loads_after_boot = keyring.database_loads();

        let absent = Uuid::now_v7();
        let error = keyring
            .cipher_for(absent)
            .await
            .expect_err("must not resolve");
        assert!(
            matches!(error, KeyringError::UnknownDataKey { data_key_id } if data_key_id == absent),
            "{error:?}"
        );
        // It did try — a `cipher_for` that returned `UnknownDataKey` without reloading would pass
        // the assertion above and be wrong.
        assert_eq!(keyring.database_loads() - loads_after_boot, 1);

        // The floor holds the second miss off the database. Without it, a stream of reads for a
        // retired key issues one query each.
        let loads = keyring.database_loads();
        let _ = keyring.cipher_for(absent).await;
        assert_eq!(
            keyring.database_loads(),
            loads,
            "min_refresh_seconds must floor a repeated miss"
        );
        // And it was the *floor* that refused it, not the single-flight gate: nothing was
        // running concurrently here, so a collapse would mean the generation moved on its own.
        assert_eq!(keyring.floor_throttles(), 1);
        assert_eq!(keyring.single_flight_collapses(), 0);
    }

    // ---------------------------------------------------------------------------------
    // Group 3 — a failed refresh retains the previous snapshot.
    // ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_failed_refresh_keeps_the_previous_snapshot_and_keeps_decrypting() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        let active = insert_key(&pool, custody.as_ref(), 1, DataKeyState::Active, 50).await;

        let keyring = ContentKeyring::load(pool.clone(), custody, &settings(), metrics())
            .await
            .expect("load");
        let before = keyring.snapshot();

        let identity = ContentIdentity::MemoryRecord {
            memory_id: Uuid::from_u128(9),
            application_id: Uuid::from_u128(10),
            memory_scope: "user",
        };
        let sealed = before
            .active_content()
            .seal(&identity, b"still readable")
            .expect("seal");

        // Break the *source*, not the keyring: a stored key check value the loader cannot verify
        // fails the reload the same way a keyring written by a broken rotation would.
        sqlx::query("update content_data_keys set key_check_value = $1 where id = $2")
            .bind(vec![0x00u8; KEY_CHECK_VALUE_LEN])
            .bind(active)
            .execute(&pool)
            .await
            .expect("break the row");

        let error = keyring.refresh().await.expect_err("the reload must fail");
        assert!(
            matches!(error, KeyringError::KeyCheckValueMismatch { .. }),
            "{error:?}"
        );

        let after = keyring.snapshot();
        assert_eq!(after.generation(), before.generation());
        assert_eq!(after.active_content_key_id(), active);
        // The property, not a proxy for it: content sealed before the failed refresh still opens
        // after it, through the keyring rather than through the `before` handle.
        assert_eq!(
            keyring
                .cipher_for(active)
                .await
                .expect("cipher survived")
                .open(&sealed, &identity)
                .expect("open")
                .as_slice(),
            b"still readable"
        );

        // The tick absorbs the same failure rather than propagating it.
        let served = keyring.refresh_or_serve_on().await;
        assert_eq!(served.active_content_key_id(), active);

        // And a database that has gone away is absorbed identically — the failure mode the
        // "serve on" rule was actually written for.
        pool.close().await;
        assert!(keyring.refresh().await.is_err());
        assert_eq!(
            keyring.refresh_or_serve_on().await.active_content_key_id(),
            active
        );
        assert_eq!(
            keyring
                .cipher_for(active)
                .await
                .expect("a closed pool cannot make a loaded key unreadable")
                .data_key_id(),
            active
        );
    }

    // ---------------------------------------------------------------------------------
    // Group 4 — single flight.
    // ---------------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_hundred_concurrent_unknown_key_opens_cost_exactly_one_database_load() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        insert_key(&pool, custody.as_ref(), 1, DataKeyState::Active, 60).await;

        let keyring = Arc::new(
            ContentKeyring::load(pool.clone(), custody.clone(), &settings(), metrics())
                .await
                .expect("load"),
        );
        let loads_after_boot = keyring.database_loads();
        let unwraps_after_boot = custody.unwrap_calls();

        // A real latecomer, so the hundred misses end in a *success*. A test where every task
        // fails would pass just as happily against a keyring that never reloaded at all.
        let latecomer = insert_key(&pool, custody.as_ref(), 2, DataKeyState::Retiring, 61).await;

        // All hundred released at once, so they genuinely queue on the same permit rather than
        // arriving one after another and being collapsed by the floor instead — which would make
        // this a test of the floor wearing the single-flight test's name.
        let barrier = Arc::new(tokio::sync::Barrier::new(100));
        let mut tasks = Vec::with_capacity(100);
        for _ in 0..100 {
            let keyring = Arc::clone(&keyring);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                keyring
                    .cipher_for(latecomer)
                    .await
                    .map(|cipher| cipher.data_key_id())
            }));
        }
        for task in tasks {
            assert_eq!(task.await.expect("join").expect("resolved"), latecomer);
        }

        assert_eq!(
            keyring.database_loads() - loads_after_boot,
            1,
            "100 concurrent misses must collapse to one load"
        );
        // …and they must have been collapsed by the **single-flight gate**, not by the floor.
        //
        // This pair is the difference between a test with teeth and one without. Deleting the
        // generation check entirely leaves the load count at one — `min_refresh_seconds` refuses
        // the other 99 just as effectively — so the count alone proves nothing about single
        // flight. Measured: with the generation check disabled, `floor_throttles` becomes 99 and
        // this assertion is the only one that fails.
        assert_eq!(
            keyring.floor_throttles(),
            0,
            "the burst was collapsed by the refresh floor, not by the single-flight gate"
        );
        assert!(
            keyring.single_flight_collapses() > 0,
            "no miss observed the concurrent load; this did not exercise single flight"
        );

        // The load unwrapped two keys once, not two hundred times. Custody is never on the
        // request path, and this is the count that says so — the assertions above would still
        // pass if every task had called custody after a shared load.
        assert_eq!(custody.unwrap_calls() - unwraps_after_boot, 2);
    }

    // ---------------------------------------------------------------------------------
    // Group 5 — the boot refusal, and its text.
    // ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn boot_refuses_when_a_data_key_cannot_be_unwrapped_and_says_how_to_recover() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;

        // Seal under `lost-2026-01`, then boot a process that no longer holds it. That is the
        // real shape of the failure: a master key dropped from the list during a rotation.
        let sealer = CountingCustody::new(&[("lost-2026-01", 7)], "lost-2026-01");
        let active = insert_key(&pool, &sealer, 1, DataKeyState::Active, 70).await;
        let booting = Arc::new(CountingCustody::new(&[("current", 8)], "current"));

        let error = ContentKeyring::load(pool.clone(), booting, &settings(), metrics())
            .await
            .expect_err("a keyring that cannot be fully opened must abort boot");
        assert!(
            matches!(&error, KeyringError::UnopenableDataKey { data_key_id, .. }
                if *data_key_id == active),
            "{error:?}"
        );

        let text = error.to_string();
        // Every one of these is a fact the operator cannot proceed without, and every one has to
        // survive into the rendered string rather than only into the variant.
        assert!(text.contains(&active.to_string()), "no data key id: {text}");
        assert!(
            text.contains("lost-2026-01"),
            "no wanted master key: {text}"
        );
        assert!(
            text.contains("current"),
            "no list of what *is* configured: {text}"
        );
        assert!(
            text.contains("MOIRA_CONTENT_ENCRYPTION__KEYS"),
            "no variable to put the key in: {text}"
        );
        // Remedy one: restore, stated so it can be carried out without a second document.
        assert!(
            text.contains("Restore the master key"),
            "no restore remedy: {text}"
        );
        assert!(
            text.contains("<base64 of 32 bytes>"),
            "restore remedy is not actionable: {text}"
        );
        // Remedy two: the confession, in full, with both of its guards and its cost.
        assert!(
            text.contains(&format!("moira keyring abandon {active}")),
            "no abandon remedy naming the key: {text}"
        );
        assert!(
            text.contains("--confirm"),
            "abandon remedy omits --confirm: {text}"
        );
        assert!(
            text.contains("--reason"),
            "abandon remedy omits --reason: {text}"
        );
        assert!(
            text.contains("permanently unreadable"),
            "abandon remedy does not say what it costs: {text}"
        );

        // A refusal, not a repair: nothing was minted beside the key it could not open.
        assert_eq!(key_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn the_boot_refusal_never_renders_key_material() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let sealer = CountingCustody::new(&[("gone", 0xab)], "gone");
        insert_key(&pool, &sealer, 1, DataKeyState::Active, 0xcd).await;
        let booting = Arc::new(CountingCustody::new(&[("current", 8)], "current"));

        let text = ContentKeyring::load(pool, booting, &settings(), metrics())
            .await
            .expect_err("must refuse")
            .to_string();
        // The master key is all-0xab and the data key all-0xcd. Any rendering of either — hex,
        // decimal or base64 — trips one of these.
        for needle in [
            "abababab", "cdcdcdcd", "171, 171", "205, 205", "q6ur", "zc3N",
        ] {
            assert!(
                !text.contains(needle),
                "boot refusal leaked {needle:?}: {text}"
            );
        }
    }

    // ---------------------------------------------------------------------------------
    // Group 6 — bootstrap fires on a strictly empty table, and only then.
    // ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn bootstrap_mints_exactly_one_active_key_on_a_strictly_empty_table() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        assert_eq!(key_count(&pool).await, 0);

        let keyring = ContentKeyring::load(pool.clone(), custody.clone(), &settings(), metrics())
            .await
            .expect("bootstrap");
        assert_eq!(key_count(&pool).await, 1);

        let snapshot = keyring.snapshot();
        let entry = snapshot
            .entry(snapshot.active_content_key_id())
            .expect("the minted key");
        assert_eq!(entry.state, DataKeyState::Active);
        assert_eq!(entry.purpose, DataKeyPurpose::Content);
        assert_eq!(entry.key_version, 1);
        assert_eq!(entry.master_key_id, "k1");
        assert_eq!(entry.custody_backend, "environment");

        // The minted key is real: drawn from `OsRng`, wrapped, stored, read back, unwrapped, and
        // the check value the loader computed matched the one the mint stored — that last part is
        // implied by `load` having succeeded at all.
        let identity = ContentIdentity::RagDocumentVersion {
            version_id: Uuid::from_u128(3),
            document_id: Uuid::from_u128(4),
            version_number: 1,
        };
        let sealed = snapshot
            .active_content()
            .seal(&identity, b"minted")
            .expect("seal");
        assert_eq!(
            snapshot
                .active_content()
                .open(&sealed, &identity)
                .expect("open")
                .as_slice(),
            b"minted"
        );

        // A second boot against the same table adds nothing and adopts the same key.
        let again = ContentKeyring::load(pool.clone(), custody, &settings(), metrics())
            .await
            .expect("second boot");
        assert_eq!(key_count(&pool).await, 1);
        assert_eq!(again.snapshot().active_content_key_id(), entry.id);
    }

    #[tokio::test]
    async fn bootstrap_does_not_fire_on_a_table_holding_any_row_at_all() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));

        // Not an active content key, and not even a *content* key: one `memory_dedupe` row is
        // enough. "Strictly empty" means the table, not "no key I could have used".
        let id = Uuid::now_v7();
        let data_key = Zeroizing::new([80u8; 32]);
        let aad = wrapped_data_key_aad(id, "k1", WRAP_ALGORITHM_AES_256_GCM);
        let wrapped = custody.wrap(&data_key, aad.as_bytes()).await.expect("wrap");
        sqlx::query(
            "insert into content_data_keys (
                 id, key_version, purpose, state, custody_backend, master_key_id,
                 wrap_algorithm, wrap_nonce, wrapped_key, key_check_value, activated_at
             ) values ($1, 1, 'memory_dedupe', 'active', 'environment', 'k1', $2, $3, $4, $5, now())",
        )
        .bind(id)
        .bind(&wrapped.wrap_algorithm)
        .bind(&wrapped.nonce)
        .bind(&wrapped.wrapped)
        .bind(key_check_value(&data_key).as_slice())
        .execute(&pool)
        .await
        .expect("seed a non-content key");

        let error = ContentKeyring::load(pool.clone(), custody, &settings(), metrics())
            .await
            .expect_err("a non-empty keyring with no active content key must refuse");
        assert!(
            matches!(error, KeyringError::NoActiveKey { .. }),
            "{error:?}"
        );
        // The whole point: it refused instead of minting a second keyring beside the first.
        assert_eq!(key_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn bootstrap_does_not_fire_on_a_table_of_only_retired_keys() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = Arc::new(CountingCustody::new(&[("k1", 1)], "k1"));
        insert_key(&pool, custody.as_ref(), 1, DataKeyState::Retired, 100).await;

        // The case the *inner* emptiness check exists for, and the only one that reaches it.
        // `retired` rows are excluded from the load query, so the outer "did the load return
        // nothing" guard says yes and bootstrap is entered — and is then refused by the
        // `count(*)` **over the whole table** it takes under the advisory lock. Narrow that
        // count to "no active content key" and this deployment silently mints a second keyring
        // beside rows that may still reference the retired one.
        let error = ContentKeyring::load(pool.clone(), custody, &settings(), metrics())
            .await
            .expect_err("a keyring of retired keys is not an empty keyring");
        assert!(
            matches!(error, KeyringError::NoActiveKey { loaded, .. } if loaded == 0),
            "{error:?}"
        );
        assert_eq!(key_count(&pool).await, 1, "nothing may have been minted");
    }

    #[tokio::test]
    async fn the_database_refuses_a_second_active_key_for_one_purpose() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = isolated_pool(&database).await;
        let custody = CountingCustody::new(&[("k1", 1)], "k1");
        insert_key(&pool, &custody, 1, DataKeyState::Active, 90).await;

        // The invariant is the *database's*, not the loader's. Two pods racing to promote get one
        // winner and one unique violation, and no amount of application-side care is what makes
        // that true.
        let error = insert_raw(&pool, &custody, 2, "content", "active", 91)
            .await
            .expect_err("a second active content key must be refused by the database");
        assert!(
            error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation),
            "not a unique violation: {error}"
        );

        // The clone above carries the index but not its name — `like ... including all` lets
        // PostgreSQL generate one — so the *migration's* index is pinned separately, by name and
        // by definition. Both halves are needed: the insert above proves an index like this is
        // enforced, and this proves the one the migration created is that index.
        let definition: String = sqlx::query_scalar(
            "select indexdef from pg_indexes
              where schemaname = 'public'
                and tablename = 'content_data_keys'
                and indexname = 'content_data_keys_single_active_idx'",
        )
        .fetch_one(database.pool())
        .await
        .expect("the migration's partial unique index exists");
        assert!(
            definition.starts_with("CREATE UNIQUE INDEX"),
            "{definition}"
        );
        assert!(definition.contains("(purpose)"), "{definition}");
        assert!(definition.contains("'active'"), "{definition}");

        // A second *purpose* may have its own active key — the uniqueness is per purpose, which
        // is the whole reason the index is on `(purpose)` and not on a constant.
        insert_raw(&pool, &custody, 3, "memory_dedupe", "active", 92)
            .await
            .expect("a second purpose may have its own active key");
    }

    /// Inserts without going through [`insert_key`], so a test can assert on the database's own
    /// refusal rather than on a helper's `expect`.
    async fn insert_raw(
        pool: &PgPool,
        sealer: &dyn MasterKeyCustody,
        version: i32,
        purpose: &str,
        state: &str,
        dek_byte: u8,
    ) -> Result<(), sqlx::Error> {
        let id = Uuid::now_v7();
        let data_key = Zeroizing::new([dek_byte; 32]);
        let aad = wrapped_data_key_aad(
            id,
            sealer.active_master_key_id(),
            WRAP_ALGORITHM_AES_256_GCM,
        );
        let wrapped = sealer.wrap(&data_key, aad.as_bytes()).await.expect("wrap");
        sqlx::query(
            "insert into content_data_keys (
                 id, key_version, purpose, state, custody_backend, master_key_id,
                 wrap_algorithm, wrap_nonce, wrapped_key, key_check_value
             ) values ($1, $2, $3, $4, 'environment', $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(version)
        .bind(purpose)
        .bind(state)
        .bind(&wrapped.master_key_id)
        .bind(&wrapped.wrap_algorithm)
        .bind(&wrapped.nonce)
        .bind(&wrapped.wrapped)
        .bind(key_check_value(&data_key).as_slice())
        .execute(pool)
        .await
        .map(|_| ())
    }

    // ---------------------------------------------------------------------------------
    // The SQL helper and the Rust parser are one format, checked against each other.
    // ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn the_sql_helper_names_the_same_data_key_the_rust_parser_does() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let pool = database.pool();

        let key_id = Uuid::now_v7();
        let cipher = ContentCipher::new(key_id, &Zeroizing::new([3u8; 32]));
        let identity = ContentIdentity::ConversationSummary {
            summary_id: Uuid::from_u128(11),
            conversation_id: Uuid::from_u128(12),
            covers_through_sequence: 5,
        };
        let envelope = cipher
            .seal(&identity, b"agree on the offsets")
            .expect("seal");

        // A real envelope, from the real writer, read by the migration's function. Any drift in
        // magic, version or the key-id offset fails here rather than in a retirement audit.
        let named: Option<Uuid> = sqlx::query_scalar("select content_envelope_data_key_id($1)")
            .bind(&envelope)
            .fetch_one(pool)
            .await
            .expect("query the helper");
        assert_eq!(named, Some(key_id));
        assert_eq!(
            EnvelopeHeader::parse(&envelope).expect("parse").data_key_id,
            key_id
        );

        // Null, never a wrong answer, for anything that is not a v1 envelope. Each of these trips
        // exactly one of the helper's three checks.
        let mut wrong_magic = envelope.clone();
        wrong_magic[0] ^= 0xff;
        let mut wrong_version = envelope.clone();
        wrong_version[4] = 0x02;
        for (label, bytes) in [
            ("wrong magic", wrong_magic),
            ("wrong version", wrong_version),
            ("too short", envelope[..MIN_ENVELOPE_LEN - 1].to_vec()),
        ] {
            let named: Option<Uuid> = sqlx::query_scalar("select content_envelope_data_key_id($1)")
                .bind(&bytes)
                .fetch_one(pool)
                .await
                .expect("query the helper");
            assert_eq!(named, None, "{label} must not be named");
        }
    }

    // ---------------------------------------------------------------------------------
    // Pure units.
    // ---------------------------------------------------------------------------------

    #[test]
    fn the_key_check_value_is_pinned_to_its_label_and_its_width() {
        // A golden construction, so a "harmless" change to the label or the truncation is a
        // failing test rather than a keyring that silently stops matching the database's copy.
        let dek = Zeroizing::new([0x5au8; 32]);
        let value = key_check_value(&dek);
        assert_eq!(value.len(), KEY_CHECK_VALUE_LEN);
        assert_eq!(KEY_CHECK_VALUE_LABEL, b"moira/content-kcv/v1");

        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(dek.as_slice()).expect("mac");
        mac.update(b"moira/content-kcv/v1");
        assert_eq!(value.as_slice(), &mac.finalize().into_bytes()[..8]);

        // A different key gives a different value — otherwise the check value would prove nothing
        // about which key came back.
        assert_ne!(key_check_value(&Zeroizing::new([0x5bu8; 32])), value);
        // And it is not the key: a truncated HMAC, not a prefix of the material.
        assert!(!dek.as_slice().starts_with(&value));
    }

    #[test]
    fn states_and_purposes_round_trip_through_their_stored_strings() {
        for state in DataKeyState::ALL {
            assert_eq!(DataKeyState::from_db(state.as_str()), Some(state));
        }
        for purpose in DataKeyPurpose::ALL {
            assert_eq!(DataKeyPurpose::from_db(purpose.as_str()), Some(purpose));
        }
        // A value from a newer binary is refused, never defaulted.
        assert_eq!(DataKeyState::from_db("rotating"), None);
        assert_eq!(DataKeyPurpose::from_db("embeddings"), None);

        // The lifecycle table of `docs/decision-encryption-at-rest.md` §7, as assertions rather
        // than as a comment.
        assert!(DataKeyState::Active.is_writable());
        for state in [
            DataKeyState::Pending,
            DataKeyState::Retiring,
            DataKeyState::Retired,
            DataKeyState::Abandoned,
        ] {
            assert!(!state.is_writable(), "{state:?} must not be writable");
        }
        assert!(!DataKeyState::Retired.is_carried_in_snapshot());
        assert!(DataKeyState::Abandoned.is_carried_in_snapshot());
        assert!(!DataKeyState::Abandoned.is_unwrapped_at_load());
        assert!(!DataKeyState::Retired.is_unwrapped_at_load());
        for state in [
            DataKeyState::Pending,
            DataKeyState::Active,
            DataKeyState::Retiring,
        ] {
            assert!(state.is_unwrapped_at_load(), "{state:?} must be loaded");
        }
    }

    #[test]
    fn a_key_entry_debug_prints_ids_and_check_values_and_never_material() {
        let entry = KeyEntry {
            id: Uuid::from_u128(1),
            key_version: 4,
            purpose: DataKeyPurpose::Content,
            state: DataKeyState::Active,
            custody_backend: "environment".to_string(),
            master_key_id: "k1".to_string(),
            key_check_value: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            created_at: Utc::now(),
            cipher: Some(Arc::new(ContentCipher::new(
                Uuid::from_u128(1),
                &Zeroizing::new([0xbeu8; 32]),
            ))),
        };
        let rendered = format!("{entry:?}");

        assert!(rendered.contains("key_check_value: \"0102030405060708\""));
        assert!(rendered.contains("state: Active") && rendered.contains("master_key_id: \"k1\""));
        assert!(rendered.contains("loaded: true"));
        // The key is all-0xbe. A derived `Debug` on `KeyEntry` would have rendered the cipher, and
        // any rendering of the material trips one of these.
        for needle in ["bebebe", "190, 190", "vr6+"] {
            assert!(
                !rendered.contains(needle),
                "KeyEntry Debug leaked: {rendered}"
            );
        }
    }

    #[test]
    fn key_check_values_render_as_lowercase_hex() {
        assert_eq!(format_key_check_value(&[0x00, 0xff, 0x0a]), "00ff0a");
        assert_eq!(format_key_check_value(&[]), "");
    }
}
