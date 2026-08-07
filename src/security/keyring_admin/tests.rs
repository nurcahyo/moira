//! The rotation suite — **the named gate that fails rather than skips.**
//!
//! `docs/decision-encryption-at-rest.md` §12 names the largest risk to the whole encryption
//! plan, and it is not a missing test: it is a **skipped** one. Every other database-backed
//! test in this crate opens with `let Some(db) = test_database().await else { return }`, which
//! reports success when `MOIRA_TEST_ALLOW_NO_DATABASE=1` is set. These do not. They call
//! [`crate::test_support::rotation_gate_database`], which returns a database or panics — see
//! its doc for the argument, and `scripts/rotation-gate.sh` for the other half, which asserts
//! that a non-zero number of these tests actually executed.
//!
//! # Two rules these tests hold themselves to
//!
//! **Everything between "seed rows" and "read rows" is a method on [`KeyringAdmin`].** Not one
//! test issues the `update content_data_keys …` that a verb is supposed to issue. A rotation
//! suite that hand-writes the rotation proves the test can rotate a keyring, which is not the
//! question. The only raw SQL below inserts content rows, reads them back, and — in exactly
//! one test — holds a row lock to make a race deterministic.
//!
//! **Reads go through [`ContentKeyring`], the same object the request path will use.** A test
//! that decrypted with a `ContentCipher` it built itself would pass against a keyring that
//! resolves the wrong key.

use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicU64, atomic::Ordering},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::{
    config::ContentEncryptionSettings,
    infra::metrics::MetricsRegistry,
    security::{
        CUSTODY_BACKEND_ENVIRONMENT, ContentKeyring, EnvironmentMasterKeyCustody, KeyringSnapshot,
        MIN_ENVELOPE_LEN,
    },
    test_support::rotation_gate_database,
};

// =======================================================================================
// Fixtures
// =======================================================================================

/// A private schema holding clones of `content_data_keys` **and all five content tables**.
///
/// `like public.<table> including all` clones columns, defaults, `not null`, primary keys,
/// indexes and — the part that matters — the CHECK constraints, including migration `0027`'s
/// five `*_single_form` exclusivity checks. So what these tests run against is the schema the
/// migrations produced rather than a hand-written copy that could drift from it.
///
/// What `LIKE` deliberately does **not** copy is foreign keys, and that is what makes seeding
/// possible at all: a `conversation_messages` row needs a `conversations` row, which needs an
/// `applications` row, and so on down eight tables that have nothing to do with rotation.
///
/// `content_envelope_data_key_id` stays in `public` and is reached through the search path, so
/// the audit queries under test are the migration's function and not a copy of it.
struct Fixture {
    pool: PgPool,
}

async fn fixture() -> Fixture {
    let database = rotation_gate_database().await;
    let schema = format!("rotation_{}", Uuid::now_v7().simple());
    sqlx::query(&format!("create schema \"{schema}\""))
        .execute(database.pool())
        .await
        .expect("create schema");

    let mut tables = vec!["content_data_keys"];
    tables.extend(AadProfile::ALL.iter().map(|profile| profile.table()));
    tables.dedup();
    for table in tables {
        sqlx::query(&format!(
            "create table \"{schema}\".{table} (like public.{table} including all)"
        ))
        .execute(database.pool())
        .await
        .unwrap_or_else(|error| panic!("clone {table}: {error}"));
    }

    let search_path = schema.clone();
    // Six, and the number is reasoned rather than round. The heaviest test here is the
    // concurrent-promotion race: a transaction holding the gate, two spawned promotions queued
    // behind it, and the `pg_stat_activity` poll that has to keep answering *while* all three
    // are blocked — four at once, plus headroom. Sizing it generously would be the wrong kind
    // of safe: `cargo test` runs the whole lib binary's tests in parallel threads, each holding
    // one of these pools plus the harness's own, and the ceiling they collectively approach is
    // the server's `max_connections`. A poll that cannot get a connection because this test's
    // own pool is exhausted deadlocks the wait loop it depends on.
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .after_connect(move |conn, _| {
            let search_path = search_path.clone();
            Box::pin(async move {
                sqlx::query(&format!("set search_path to \"{search_path}\", public"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(database.url())
        .await
        .expect("isolated pool");

    Fixture { pool }
}

fn settings() -> ContentEncryptionSettings {
    // A long floor and a long tick. Every assertion below is about *whether* a load happened,
    // never about waiting for one.
    ContentEncryptionSettings {
        refresh_seconds: 300,
        min_refresh_seconds: 30,
        ..ContentEncryptionSettings::default()
    }
}

fn metrics() -> MetricsRegistry {
    MetricsRegistry::new("moira-rotation-test", None)
}

/// The real environment backend. Deliberately not a stub: a fake that "unwrapped" by returning
/// the bytes it was handed would let a rotation that never actually re-encrypts anything pass
/// every test in this file.
fn env_custody(entries: &[(&str, u8)], active: &str) -> Arc<EnvironmentMasterKeyCustody> {
    let keys = entries
        .iter()
        .map(|(id, byte)| ((*id).to_string(), Zeroizing::new([*byte; 32])))
        .collect();
    Arc::new(EnvironmentMasterKeyCustody::new(keys, active).expect("custody"))
}

fn admin(fixture: &Fixture, custody: Arc<dyn MasterKeyCustody>) -> KeyringAdmin {
    KeyringAdmin::new(fixture.pool.clone(), custody)
}

async fn keyring(fixture: &Fixture, custody: Arc<dyn MasterKeyCustody>) -> Arc<ContentKeyring> {
    Arc::new(
        ContentKeyring::load(fixture.pool.clone(), custody, &settings(), metrics())
            .await
            .expect("the keyring must load"),
    )
}

// =======================================================================================
// Two more custody backends, so R3 is exercised rather than described
// =======================================================================================

/// A *structurally different* implementation serving the *same bytes under the same key id*.
///
/// This is the common R3 case: environment variable → Vault KV, an external-secrets operator,
/// a CSI driver. It shares no code with [`EnvironmentMasterKeyCustody`] — its own AES-GCM
/// calls, its own storage (a sorted `Vec`, not a `HashMap`), its own backend name — and the
/// swap must still be **zero writes and zero re-encryption**, which works only because the
/// backend name is absent from the wrapped-key AAD.
#[derive(Debug)]
struct InMemoryCustody {
    keys: Vec<(String, Zeroizing<[u8; 32]>)>,
    active: String,
}

impl InMemoryCustody {
    fn new(entries: &[(&str, u8)], active: &str) -> Arc<Self> {
        Arc::new(Self {
            keys: entries
                .iter()
                .map(|(id, byte)| ((*id).to_string(), Zeroizing::new([*byte; 32])))
                .collect(),
            active: active.to_string(),
        })
    }

    fn key(&self, id: &str) -> Result<Aes256Gcm, KeyCustodyError> {
        let (_, bytes) = self
            .keys
            .iter()
            .find(|(name, _)| name == id)
            .ok_or_else(|| KeyCustodyError::UnknownMasterKey {
                master_key_id: id.to_string(),
            })?;
        Aes256Gcm::new_from_slice(bytes.as_slice()).map_err(|_| KeyCustodyError::Unavailable {
            backend: "in_memory",
        })
    }
}

#[async_trait::async_trait]
impl MasterKeyCustody for InMemoryCustody {
    fn backend_name(&self) -> &'static str {
        "in_memory"
    }
    fn active_master_key_id(&self) -> &str {
        &self.active
    }
    fn can_unwrap(&self, master_key_id: &str) -> bool {
        self.keys.iter().any(|(name, _)| name == master_key_id)
    }
    fn wrap_algorithm(&self) -> &'static str {
        WRAP_ALGORITHM_AES_256_GCM
    }
    fn master_key_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.keys.iter().map(|(name, _)| name.clone()).collect();
        ids.sort_unstable();
        ids
    }
    async fn wrap(
        &self,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        self.wrap_under(&self.active.clone(), dek, aad).await
    }
    async fn wrap_under(
        &self,
        master_key_id: &str,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        let cipher = self.key(master_key_id)?;
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
                backend: "in_memory",
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
        let cipher = self.key(&wrapped.master_key_id)?;
        let plain = Zeroizing::new(
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
        if plain.len() != dek.len() {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        dek.copy_from_slice(&plain);
        Ok(dek)
    }
    async fn preflight(&self) -> Result<(), KeyCustodyError> {
        Ok(())
    }
}

/// A backend that **will not release key bytes** — the AWS KMS shape.
///
/// Nothing outside it can obtain its master key: there is no method on the trait that would
/// return one, which is the entire reason the seam is `wrap`/`unwrap` rather than
/// `get_master_key_bytes()`. It also differs from the environment backend on the wire: its
/// ciphertext is self-framed with the nonce inside `wrapped`, `nonce` is empty exactly as a
/// KMS ciphertext leaves it, and its `wrap_algorithm` is its own string — so a rewrap onto it
/// exercises the algorithm-carrying AAD rather than assuming AES-256-GCM.
#[derive(Debug)]
struct FakeKmsCustody {
    /// Never handed out. `Zeroizing` and private, and no accessor exists.
    material: BTreeMap<String, Zeroizing<[u8; 32]>>,
    active: String,
    wraps: AtomicU64,
    unwraps: AtomicU64,
}

const FAKE_KMS_ALGORITHM: &str = "fake-kms:symmetric_default";

impl FakeKmsCustody {
    fn new(entries: &[(&str, u8)], active: &str) -> Arc<Self> {
        Arc::new(Self {
            material: entries
                .iter()
                .map(|(id, byte)| ((*id).to_string(), Zeroizing::new([*byte; 32])))
                .collect(),
            active: active.to_string(),
            wraps: AtomicU64::new(0),
            unwraps: AtomicU64::new(0),
        })
    }

    fn cipher(&self, id: &str) -> Result<Aes256Gcm, KeyCustodyError> {
        let bytes = self
            .material
            .get(id)
            .ok_or_else(|| KeyCustodyError::UnknownMasterKey {
                master_key_id: id.to_string(),
            })?;
        Aes256Gcm::new_from_slice(bytes.as_slice()).map_err(|_| KeyCustodyError::Unavailable {
            backend: "fake_kms",
        })
    }
}

#[async_trait::async_trait]
impl MasterKeyCustody for FakeKmsCustody {
    fn backend_name(&self) -> &'static str {
        "fake_kms"
    }
    fn active_master_key_id(&self) -> &str {
        &self.active
    }
    fn can_unwrap(&self, master_key_id: &str) -> bool {
        self.material.contains_key(master_key_id)
    }
    fn wrap_algorithm(&self) -> &'static str {
        FAKE_KMS_ALGORITHM
    }
    fn master_key_ids(&self) -> Vec<String> {
        self.material.keys().cloned().collect()
    }
    async fn wrap(
        &self,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        self.wrap_under(&self.active.clone(), dek, aad).await
    }
    async fn wrap_under(
        &self,
        master_key_id: &str,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        self.wraps.fetch_add(1, Ordering::SeqCst);
        let cipher = self.cipher(master_key_id)?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let body = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: dek.as_slice(),
                    aad,
                },
            )
            .map_err(|_| KeyCustodyError::Unavailable {
                backend: "fake_kms",
            })?;
        // Self-framed, like a real KMS ciphertext: everything needed to reverse it is inside
        // `wrapped`, and `nonce` stays empty.
        let mut framed = nonce.to_vec();
        framed.extend_from_slice(&body);
        Ok(WrappedKey {
            master_key_id: master_key_id.to_string(),
            wrap_algorithm: FAKE_KMS_ALGORITHM.to_string(),
            nonce: Vec::new(),
            wrapped: framed,
        })
    }
    async fn unwrap(
        &self,
        wrapped: &WrappedKey,
        aad: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError> {
        self.unwraps.fetch_add(1, Ordering::SeqCst);
        if wrapped.wrap_algorithm != FAKE_KMS_ALGORITHM || wrapped.wrapped.len() < 12 {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        let cipher = self.cipher(&wrapped.master_key_id)?;
        let (nonce, body) = wrapped.wrapped.split_at(12);
        let plain = Zeroizing::new(
            cipher
                .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad })
                .map_err(|_| KeyCustodyError::UnwrapFailed)?,
        );
        let mut dek = Zeroizing::new([0u8; 32]);
        if plain.len() != dek.len() {
            return Err(KeyCustodyError::UnwrapFailed);
        }
        dek.copy_from_slice(&plain);
        Ok(dek)
    }
    async fn preflight(&self) -> Result<(), KeyCustodyError> {
        Ok(())
    }
}

// =======================================================================================
// Seeding and reading content rows
//
// The only raw SQL in this file that is not an assertion. It stands in for the write path,
// which arrives in a later PR of the train.
// =======================================================================================

/// One seeded content row, with everything needed to re-open it.
struct SeededRow {
    id: Uuid,
    profile: AadProfile,
    identity: OwnedIdentity,
    plaintext: Vec<u8>,
    /// The data key the envelope named when it was written.
    data_key_id: Uuid,
}

/// Seals `plaintext` with `cipher` and inserts it into the profile's table and column.
///
/// Every identity value is generated here and bound into the AAD, exactly as the write path
/// will: the AAD is what stops an attacker with database write access lifting one tenant's
/// ciphertext into another's row.
async fn seed(
    pool: &PgPool,
    cipher: &ContentCipher,
    profile: AadProfile,
    plaintext: &[u8],
) -> SeededRow {
    let id = Uuid::now_v7();
    let identity = match profile {
        AadProfile::ConversationMessageContent => OwnedIdentity::ConversationMessage {
            message_id: id,
            conversation_id: Uuid::now_v7(),
            sequence_number: 1,
        },
        AadProfile::ConversationSummaryText => OwnedIdentity::ConversationSummary {
            summary_id: id,
            conversation_id: Uuid::now_v7(),
            covers_through_sequence: 7,
        },
        AadProfile::MemoryRecordContent => OwnedIdentity::MemoryRecord {
            memory_id: id,
            application_id: Uuid::now_v7(),
            // `application`, because the CHECK on `memory_scope` demands a matching
            // `conversation_id` / `external_user_id` / `external_tenant_id` for the other
            // three, and this fixture is about the sealed column rather than about scoping.
            memory_scope: "application".to_string(),
        },
        AadProfile::RagDocumentVersionContent => OwnedIdentity::RagDocumentVersion {
            version_id: id,
            document_id: Uuid::now_v7(),
            version_number: 1,
        },
        AadProfile::RagChunkText => OwnedIdentity::RagChunk {
            chunk_id: id,
            document_version_id: Uuid::now_v7(),
            chunk_index: 0,
        },
    };
    let envelope = cipher
        .seal(&identity.borrow(), plaintext)
        .expect("seal the fixture row");
    let data_key_id = EnvelopeHeader::parse(&envelope)
        .expect("the fixture sealed a parseable envelope")
        .data_key_id;

    match &identity {
        OwnedIdentity::ConversationMessage {
            conversation_id,
            sequence_number,
            ..
        } => sqlx::query(
            "insert into conversation_messages
                     (id, public_id, conversation_id, role, sequence_number,
                      content_encrypted, content_hash)
                 values ($1, $2, $3, 'user', $4, $5, 'fixture')",
        )
        .bind(id)
        .bind(format!("msg_{id}"))
        .bind(conversation_id)
        .bind(sequence_number)
        .bind(&envelope),
        OwnedIdentity::ConversationSummary {
            conversation_id,
            covers_through_sequence,
            ..
        } => sqlx::query(
            "insert into conversation_summaries
                 (id, conversation_id, summary_version, covers_through_sequence,
                  summary_text_encrypted, summary_hash)
             values ($1, $2, 1, $3, $4, 'fixture')",
        )
        .bind(id)
        .bind(conversation_id)
        .bind(covers_through_sequence)
        .bind(&envelope),
        OwnedIdentity::MemoryRecord {
            application_id,
            memory_scope,
            ..
        } => sqlx::query(
            "insert into memory_records
                 (id, public_id, application_id, memory_scope, memory_type,
                  content_encrypted, content_hash)
             values ($1, $2, $3, $4, 'fact', $5, 'fixture')",
        )
        .bind(id)
        .bind(format!("mem_{id}"))
        .bind(application_id)
        .bind(memory_scope)
        .bind(&envelope),
        OwnedIdentity::RagDocumentVersion {
            document_id,
            version_number,
            ..
        } => sqlx::query(
            "insert into rag_document_versions
                 (id, document_id, version_number, content_encrypted, content_hash)
             values ($1, $2, $3, $4, 'fixture')",
        )
        .bind(id)
        .bind(document_id)
        .bind(version_number)
        .bind(&envelope),
        OwnedIdentity::RagChunk {
            document_version_id,
            chunk_index,
            ..
        } => sqlx::query(
            "insert into rag_chunks
                 (id, public_id, document_version_id, collection_id, chunk_index,
                  chunk_text_encrypted, chunk_hash)
             values ($1, $2, $3, $4, $5, $6, 'fixture')",
        )
        .bind(id)
        .bind(format!("chunk_{id}"))
        .bind(document_version_id)
        .bind(Uuid::now_v7())
        .bind(chunk_index)
        .bind(&envelope),
    }
    .execute(pool)
    .await
    .unwrap_or_else(|error| panic!("seed {}: {error}", profile.table()));

    SeededRow {
        id,
        profile,
        identity,
        plaintext: plaintext.to_vec(),
        data_key_id,
    }
}

/// One row in every sealed column, so an assertion over "all five" cannot be vacuous.
async fn seed_all_five(pool: &PgPool, cipher: &ContentCipher, label: &str) -> Vec<SeededRow> {
    let mut rows = Vec::new();
    for profile in AadProfile::ALL {
        let plaintext = format!("{label} / {} / {}", profile.table(), profile.column());
        rows.push(seed(pool, cipher, profile, plaintext.as_bytes()).await);
    }
    assert_eq!(rows.len(), AadProfile::ALL.len());
    rows
}

async fn stored_envelope(pool: &PgPool, row: &SeededRow) -> Vec<u8> {
    sqlx::query_scalar(&format!(
        "select {} from {} where id = $1",
        row.profile.column(),
        row.profile.table()
    ))
    .bind(row.id)
    .fetch_one(pool)
    .await
    .expect("read the stored envelope")
}

/// Reads a row **through the keyring**, exactly as the request path will: parse the header,
/// resolve the data key by the id the header names, open with the AAD the row's identity
/// rebuilds.
async fn read_through_keyring(
    pool: &PgPool,
    keyring: &ContentKeyring,
    row: &SeededRow,
) -> Result<Vec<u8>, KeyringError> {
    let envelope = stored_envelope(pool, row).await;
    let header = EnvelopeHeader::parse(&envelope).expect("stored envelope parses");
    let cipher = keyring.cipher_for(header.data_key_id).await?;
    Ok(cipher
        .open(&envelope, &row.identity.borrow())
        .expect("the resolved key must open the row it was named by")
        .to_vec())
}

/// `"table.column"` → (row count, SHA-256 over every stored value in id order).
///
/// The count is carried beside the digest deliberately. Two empty columns have the same
/// digest, so "identical before and after" over an empty table is true and means nothing —
/// which is precisely how a byte-identity assertion becomes decoration.
async fn column_fingerprints(pool: &PgPool) -> BTreeMap<String, (i64, String)> {
    let mut out = BTreeMap::new();
    for profile in AadProfile::ALL {
        let rows: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(&format!(
            "select id, {} from {} where {} is not null order by id",
            profile.column(),
            profile.table(),
            profile.column()
        ))
        .fetch_all(pool)
        .await
        .expect("read a sealed column");

        let mut hasher = Sha256::new();
        for (id, value) in &rows {
            hasher.update(id.as_bytes());
            hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(value);
        }
        out.insert(
            profile_key(profile),
            (
                i64::try_from(rows.len()).unwrap_or(i64::MAX),
                format!("{:x}", hasher.finalize()),
            ),
        );
    }
    out
}

/// Every keyring row's `(master_key_id, wrapped_key, custody_backend, wrap_algorithm)`.
async fn wrappings(pool: &PgPool) -> BTreeMap<Uuid, (String, Vec<u8>, String, String)> {
    sqlx::query_as::<_, (Uuid, String, Vec<u8>, String, String)>(
        "select id, master_key_id, wrapped_key, custody_backend, wrap_algorithm
           from content_data_keys order by key_version",
    )
    .fetch_all(pool)
    .await
    .expect("read the keyring")
    .into_iter()
    .map(|(id, master, wrapped, backend, algorithm)| (id, (master, wrapped, backend, algorithm)))
    .collect()
}

/// Blocks until `at_least` backends are queued, directly or transitively, behind the lock
/// `holder_pid` is holding.
///
/// This is what makes the two race tests below deterministic instead of timed, and it is worth
/// the recursive CTE for two measured reasons.
///
/// **A row-lock wait is not an ungranted lock on the relation.** The waiter queues on the
/// holder's `transactionid`, so `pg_locks where not granted and relation = …` never matches
/// and a test built on it waits forever. `pg_blocking_pids` answers the question directly.
///
/// **The queue is a chain, not a star.** With two writers behind one held row, PostgreSQL
/// reports the first as waiting on the holder's `transactionid` and the second as waiting on a
/// `tuple` lock held by the *first waiter* — so `holder = any(pg_blocking_pids(pid))` counts
/// one, forever, while both are plainly blocked. Walking the chain from the holder is what
/// makes "both promotions are queued" a thing this test can actually observe.
///
/// Rooting the walk at `holder_pid` is also what keeps it honest: the lib-test database is
/// shared by every test in this process, and a global count of blocked backends would be
/// satisfied by an unrelated suite's contention.
///
/// The panic on the way out is the point. A timed-out wait means the race never happened, and
/// the assertions that follow would then pass while proving nothing.
async fn wait_until_blocked_by(pool: &PgPool, holder_pid: i32, at_least: i64, what: &str) {
    const QUEUE_BEHIND: &str = "with recursive queue(pid) as (
             select $1::int
           union
             select waiting.pid
               from pg_stat_activity waiting
               join queue on queue.pid = any(pg_blocking_pids(waiting.pid))
              where waiting.wait_event_type = 'Lock'
         )
         select count(*) - 1 from queue";

    for _ in 0..600 {
        let blocked: i64 = sqlx::query_scalar(QUEUE_BEHIND)
            .bind(holder_pid)
            .fetch_one(pool)
            .await
            .expect("ask PostgreSQL who is queued behind the held lock");
        if blocked >= at_least {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("{what}");
}

async fn state_of(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("select state from content_data_keys where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read a key state")
}

// =======================================================================================
// R1 — data-key rotation
// =======================================================================================

/// **R1 online.** Five rows under key A, rotate, five rows under key B, and all ten still read.
///
/// The rotation runs through [`KeyringAdmin::add`] and [`KeyringAdmin::promote`] — the same
/// functions `moira keyring add` and `moira keyring promote` call — and never through SQL of
/// this test's own.
///
/// The teeth are the per-row key assertion, not the read. A rotation that did nothing at all
/// would leave all ten rows readable under key A and pass a test that only checked plaintext.
#[tokio::test]
async fn r1_rotates_the_data_key_online_and_every_row_stays_readable() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");

    // Bootstrap mints key A. The first instance is the one that will rotate.
    let instance = keyring(&fixture, custody.clone()).await;
    let key_a = instance.snapshot().active_content_key_id();
    let mut before = Vec::new();
    for index in 0..5 {
        before.push(
            seed(
                &fixture.pool,
                &instance.active_content_cipher(),
                AadProfile::ConversationMessageContent,
                format!("written under A #{index}").as_bytes(),
            )
            .await,
        );
    }

    // R1, through the CLI's own functions.
    let admin = admin(&fixture, custody.clone());
    let added = admin.add(DataKeyPurpose::Content).await.expect("add");
    assert_eq!(state_of(&fixture.pool, added.id).await, "pending");
    let promotion = admin.promote(added.id).await.expect("promote");
    assert_eq!(promotion.promoted, added.id);
    assert_eq!(promotion.demoted, Some(key_a));

    // "Not writable, still loaded": A is retiring, not retired, and is still in the snapshot.
    assert_eq!(state_of(&fixture.pool, key_a).await, "retiring");
    let refreshed = instance.refresh().await.expect("refresh");
    assert_eq!(refreshed.active_content_key_id(), added.id);
    assert!(
        refreshed
            .entry(key_a)
            .expect("A is still carried")
            .cipher()
            .is_ok()
    );

    let mut after = Vec::new();
    for index in 0..5 {
        after.push(
            seed(
                &fixture.pool,
                &instance.active_content_cipher(),
                AadProfile::ConversationMessageContent,
                format!("written under B #{index}").as_bytes(),
            )
            .await,
        );
    }

    // Every header parsed, and the first five carry A while the last five carry B. Without
    // this split the whole test passes against a `promote` that changed nothing.
    for row in &before {
        assert_eq!(row.data_key_id, key_a, "a pre-rotation row moved keys");
    }
    for row in &after {
        assert_eq!(
            row.data_key_id, added.id,
            "a post-rotation row kept the old key"
        );
    }
    assert_ne!(key_a, added.id);

    // All ten read, through the keyring, by the key each one names.
    for row in before.iter().chain(after.iter()) {
        assert_eq!(
            read_through_keyring(&fixture.pool, &instance, row)
                .await
                .expect("every row must open"),
            row.plaintext
        );
    }
}

/// **R1 cold cache.** A process that has never seen the old key unwraps it from
/// `content_data_keys` and reads pre-rotation rows.
///
/// The snapshot is not "cleared" — a fresh [`ContentKeyring`] is built over the same pool,
/// which is what a replacement pod actually is. The counter is what gives this teeth: the new
/// instance unwraps both keys from the database rather than inheriting anything.
#[tokio::test]
async fn r1_cold_cache_re_unwraps_the_retiring_key_and_reads_pre_rotation_rows() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");
    let first = keyring(&fixture, custody.clone()).await;
    let key_a = first.snapshot().active_content_key_id();
    let old_rows = seed_all_five(&fixture.pool, &first.active_content_cipher(), "cold").await;

    let admin = admin(&fixture, custody.clone());
    let key_b = admin.add(DataKeyPurpose::Content).await.expect("add").id;
    admin.promote(key_b).await.expect("promote");
    drop(first);

    // A brand-new process. Nothing is carried over: it reads the table and unwraps from cold.
    let cold = keyring(&fixture, custody).await;
    assert_eq!(cold.snapshot().active_content_key_id(), key_b);
    assert_eq!(
        cold.database_loads(),
        1,
        "boot must be the only load so far; a second means the snapshot was not populated"
    );

    for row in &old_rows {
        assert_eq!(row.data_key_id, key_a);
        assert_eq!(
            read_through_keyring(&fixture.pool, &cold, row)
                .await
                .expect("a pre-rotation row must open from a cold cache"),
            row.plaintext
        );
    }
    // Still one load. Reading five rows sealed under the *retiring* key cost no query and no
    // custody call: they were unwrapped once at boot, which is the property that makes a
    // later swap to KMS an operational non-event.
    assert_eq!(cold.database_loads(), 1);
}

/// **Two-instance rolling deploy, without containers.** Two keyrings over one pool.
///
/// The stale instance is the interesting one: mid-deploy it still writes under the old key —
/// which is legal, because `retiring` means "not writable *by new snapshots*, still loaded and
/// still readable" — and then meets a row sealed under a key it has never heard of.
#[tokio::test]
async fn two_instances_over_one_pool_survive_a_rotation_performed_by_one_of_them() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");

    let rotating = keyring(&fixture, custody.clone()).await;
    let stale = keyring(&fixture, custody.clone()).await;
    let key_a = rotating.snapshot().active_content_key_id();
    assert_eq!(stale.snapshot().active_content_key_id(), key_a);
    let stale_generation = stale.snapshot().generation();

    let admin = admin(&fixture, custody.clone());
    let key_b = admin.add(DataKeyPurpose::Content).await.expect("add").id;
    admin.promote(key_b).await.expect("promote");
    rotating
        .refresh()
        .await
        .expect("the rotating instance refreshes");

    // The stale instance has not refreshed. Its write is under A, and it is a legal row: A is
    // `retiring`, which is exactly the state that keeps a mid-deploy write readable forever.
    let legal_old_row = seed(
        &fixture.pool,
        &stale.active_content_cipher(),
        AadProfile::MemoryRecordContent,
        b"written by a pod that had not refreshed yet",
    )
    .await;
    assert_eq!(legal_old_row.data_key_id, key_a);
    assert_eq!(stale.snapshot().generation(), stale_generation);

    // Now it reads a row the *other* instance wrote under B. On-demand refresh, then success.
    let new_row = seed(
        &fixture.pool,
        &rotating.active_content_cipher(),
        AadProfile::MemoryRecordContent,
        b"written by a pod that had",
    )
    .await;
    assert_eq!(new_row.data_key_id, key_b);
    assert_eq!(
        read_through_keyring(&fixture.pool, &stale, &new_row)
            .await
            .expect("the stale instance must resolve the new key"),
        new_row.plaintext
    );

    // Exactly one. A miss that reloaded per row, or one that reloaded and then reloaded again
    // on the retry, both leave every assertion above true.
    assert_eq!(
        stale.snapshot().generation(),
        stale_generation + 1,
        "one unknown key must cost exactly one refresh"
    );
    // And its own mid-deploy write is still readable, by both instances.
    for instance in [&stale, &rotating] {
        assert_eq!(
            read_through_keyring(&fixture.pool, instance, &legal_old_row)
                .await
                .expect("the mid-deploy row must stay readable"),
            legal_old_row.plaintext
        );
    }
}

// =======================================================================================
// R2 — master-key rotation. The central promise.
// =======================================================================================

/// **R2 positive.** After `rewrap`, every `master_key_id` and every `wrapped_key` changed, and
/// the SHA-256 of every value in **all five** `*_encrypted` columns is byte-identical.
///
/// This is the non-negotiable of `docs/decision-encryption-at-rest.md` §12, asserted directly
/// rather than argued. The three assertions are of different kinds on purpose:
///
/// * the wrappings **must all change** — otherwise `rewrap` did nothing;
/// * the ciphertext **must not change, at all** — otherwise a master-key rotation is a
///   full re-encryption of every content table, which is the thing envelope encryption exists
///   to avoid;
/// * the row counts must be **non-zero in all five columns** — otherwise "identical" is a
///   statement about two empty sets.
#[tokio::test]
async fn r2_rewrap_changes_every_wrapping_and_not_one_byte_of_ciphertext() {
    let fixture = fixture().await;

    // Step 0: a deployment running on one master key.
    let only_old = env_custody(&[("master-old", 0x11)], "master-old");
    let booted = keyring(&fixture, only_old.clone()).await;
    let key_a = booted.snapshot().active_content_key_id();
    // A second data key, so the rewrap covers more than one row and `retiring` is represented.
    let rotating_admin = admin(&fixture, only_old.clone());
    let key_b = rotating_admin
        .add(DataKeyPurpose::Content)
        .await
        .expect("add")
        .id;
    rotating_admin.promote(key_b).await.expect("promote");
    let refreshed = booted.refresh().await.expect("refresh");

    // Rows in ALL FIVE columns, under both data keys.
    let mut rows = seed_all_five(&fixture.pool, &refreshed.active_content(), "under B").await;
    let cipher_a = refreshed
        .entry(key_a)
        .expect("A is retiring and still loaded")
        .cipher()
        .expect("A opens");
    rows.extend(seed_all_five(&fixture.pool, &cipher_a, "under A").await);

    let before_columns = column_fingerprints(&fixture.pool).await;
    let before_wrappings = wrappings(&fixture.pool).await;
    assert_eq!(
        before_columns.len(),
        AadProfile::ALL.len(),
        "the fingerprint must cover every declared sealed column"
    );
    for (column, (count, _)) in &before_columns {
        assert_eq!(
            *count, 2,
            "{column} holds no rows, so byte-identity across it would assert nothing"
        );
    }
    assert_eq!(before_wrappings.len(), 2);

    // Step 1 of the rotation: both master keys held, ACTIVE_KEY_ID deliberately untouched.
    // This is why `wrap_under` exists — the target is held and is *not* the active key.
    let both = env_custody(&[("master-old", 0x11), ("master-new", 0x22)], "master-old");
    let report = admin(&fixture, both.clone())
        .rewrap("master-new")
        .await
        .expect("rewrap");
    assert_eq!(report.rewrapped.len(), 2);
    assert!(report.left_alone.is_empty());

    let after_columns = column_fingerprints(&fixture.pool).await;
    let after_wrappings = wrappings(&fixture.pool).await;

    // (a) every master_key_id changed, and (b) every wrapped_key changed.
    assert_eq!(after_wrappings.len(), before_wrappings.len());
    for (id, (master, wrapped, backend, algorithm)) in &after_wrappings {
        let (old_master, old_wrapped, _, _) = &before_wrappings[id];
        assert_eq!(old_master, "master-old");
        assert_eq!(master, "master-new", "{id} still names the old master key");
        assert_ne!(wrapped, old_wrapped, "{id}'s wrapped key was not re-sealed");
        assert_eq!(backend, CUSTODY_BACKEND_ENVIRONMENT);
        assert_eq!(algorithm, WRAP_ALGORITHM_AES_256_GCM);
    }

    // (c) THE non-negotiable: the SHA-256 of every value in all five columns is unchanged.
    assert_eq!(
        after_columns, before_columns,
        "a master-key rewrap must not touch one byte of any *_encrypted column"
    );

    // Step 3 of the rotation: promote the new master key and drop the old one entirely. A
    // process holding *only* `master-new` must open the whole keyring and read every row
    // written before the rotation.
    let only_new = env_custody(&[("master-new", 0x22)], "master-new");
    let after_rotation = keyring(&fixture, only_new).await;
    assert_eq!(after_rotation.snapshot().len(), 2);
    for row in &rows {
        assert_eq!(
            read_through_keyring(&fixture.pool, &after_rotation, row)
                .await
                .expect("every pre-rotation row must open under the new master key alone"),
            row.plaintext,
            "{}.{} row {} did not survive the master-key rotation",
            row.profile.table(),
            row.profile.column(),
            row.id
        );
    }
}

/// **R2 negative — the test whose absence is why key rotation bricks systems.**
///
/// The operator skipped step 2: they added the new master key, promoted it, and dropped the
/// old one without ever running `rewrap`. Boot must refuse, and the refusal must be
/// actionable without a second document.
#[tokio::test]
// NOTE ON THE NAME, which is "omitted" rather than the obvious word for what the operator
// did. `scripts/test-log-lib.sh::tl_count_skips` greps the whole test log, case-insensitively,
// for the present participle of "skip", and reds `scripts/gates.sh` and every CI shard when it
// matches — because a skipped database suite reports green, and that has invalidated a round
// of results here before. `cargo test` prints every test's NAME into that log, so a test named
// for the rewrap an operator failed to perform emits that word on every fully green run and is
// reported as "suites did not run". Measured: the first draft of this test put a seventh skip
// line into a log whose other six were the genuine Redis ones.
async fn an_omitted_rewrap_refuses_to_boot_and_names_the_missing_master_key_and_both_remedies() {
    let fixture = fixture().await;
    let only_old = env_custody(&[("master-old", 0x11)], "master-old");
    let booted = keyring(&fixture, only_old).await;
    let key_a = booted.snapshot().active_content_key_id();
    let rows = seed_all_five(
        &fixture.pool,
        &booted.active_content_cipher(),
        "unrewrapped",
    )
    .await;
    drop(booted);

    // No rewrap. Only the new master key is configured.
    let only_new = env_custody(&[("master-new", 0x22)], "master-new");
    let error = ContentKeyring::load(fixture.pool.clone(), only_new, &settings(), metrics())
        .await
        .expect_err("a keyring sealed under a master key this process lacks must abort boot");

    assert!(
        matches!(&error, KeyringError::UnopenableDataKey { data_key_id, .. } if *data_key_id == key_a),
        "{error:?}"
    );
    let text = error.to_string();
    for (needle, why) in [
        (key_a.to_string(), "the data key that cannot be opened"),
        (
            "master-old".to_string(),
            "the master key id that is MISSING",
        ),
        ("master-new".to_string(), "what IS configured"),
        (
            "MOIRA_CONTENT_ENCRYPTION__KEYS".to_string(),
            "the literal variable to put the key in",
        ),
        ("Restore the master key".to_string(), "remedy one"),
        ("<base64 of 32 bytes>".to_string(), "remedy one, actionable"),
        (
            format!("moira keyring abandon {key_a}"),
            "remedy two, naming the key",
        ),
        ("--confirm".to_string(), "remedy two's first guard"),
        ("--reason".to_string(), "remedy two's second guard"),
        (
            "permanently unreadable".to_string(),
            "what remedy two costs",
        ),
    ] {
        assert!(text.contains(&needle), "the refusal omits {why}: {text}");
    }

    // A refusal, not a repair: nothing was minted beside the key it could not open, and not
    // one row was touched.
    let keys: i64 = sqlx::query_scalar("select count(*) from content_data_keys")
        .fetch_one(&fixture.pool)
        .await
        .expect("count");
    assert_eq!(keys, 1);
    for (column, (count, _)) in column_fingerprints(&fixture.pool).await {
        assert_eq!(count, 1, "{column} changed during a refused boot");
    }

    // And the same condition refuses the rewrap itself, with the *ordering* remedy: this is
    // what makes "skipping step 2 fails loudly at step 4" true rather than hoped for.
    //
    // A second key, mintable because `master-new` IS held, is what gives this teeth. With one
    // key, "refuses in full" and "refuses on the first row it cannot open" are the same
    // observation. With two, a rewrap that converted what it could would leave this one under
    // `master-new` and the other under `master-old` — a keyring no single configuration can
    // open, which is strictly worse than not having started.
    let partial_risk = admin(&fixture, env_custody(&[("master-new", 0x22)], "master-new"));
    let mintable = partial_risk
        .add(DataKeyPurpose::Content)
        .await
        .expect("a key can still be minted under the key this process does hold");
    let before_refusal = wrappings(&fixture.pool).await;
    assert_eq!(before_refusal[&mintable.id].0, "master-new");
    assert_eq!(before_refusal[&key_a].0, "master-old");

    let error = partial_risk
        .rewrap("master-new")
        .await
        .expect_err("rewrap must refuse a keyring it cannot fully unwrap");
    assert!(
        matches!(&error, KeyringAdminError::SourceMasterKeyNotHeld { master_key_id, .. }
            if master_key_id == "master-old"),
        "{error:?}"
    );
    assert!(error.to_string().contains("in full"), "{error}");
    assert_eq!(
        wrappings(&fixture.pool).await,
        before_refusal,
        "a refused rewrap must have written nothing at all; a half-converted keyring cannot \
         be opened by any single configuration"
    );

    // Restoring the old key is remedy one, and it works with nothing re-encrypted.
    let restored = env_custody(&[("master-old", 0x11), ("master-new", 0x22)], "master-new");
    let recovered = keyring(&fixture, restored).await;
    for row in &rows {
        assert_eq!(
            read_through_keyring(&fixture.pool, &recovered, row)
                .await
                .expect("remedy one must actually recover the rows"),
            row.plaintext
        );
    }
}

/// `rewrap` verifies the key check value before it re-seals, and refuses on a mismatch.
///
/// The condition is a custody backend that returned 32 valid-looking bytes that are not the
/// right ones — a mis-keyed KMS alias, a master key list edited into the wrong order. Without
/// this check the wrong key is re-sealed under the new master key, the mismatch is *carried
/// forward*, and the keyring now opens perfectly and decrypts nothing. Nothing downstream
/// could then tell that from data corruption.
#[tokio::test]
async fn rewrap_refuses_to_re_seal_a_key_that_fails_its_own_check_value() {
    let fixture = fixture().await;
    let old = env_custody(&[("old", 0x11)], "old");
    let booted = keyring(&fixture, old).await;
    let key_a = booted.snapshot().active_content_key_id();
    drop(booted);

    // The one edit a keyring without a key check value could not notice: the row still
    // unwraps, under a configured master key, to a perfectly valid 32-byte AES key.
    sqlx::query("update content_data_keys set key_check_value = $1 where id = $2")
        .bind(vec![0xffu8; 8])
        .bind(key_a)
        .execute(&fixture.pool)
        .await
        .expect("corrupt the check value");
    let before = wrappings(&fixture.pool).await;

    let both = env_custody(&[("old", 0x11), ("new", 0x22)], "old");
    let error = admin(&fixture, both)
        .rewrap("new")
        .await
        .expect_err("a key that fails its own check value must not be re-sealed");
    assert!(
        matches!(
            &error,
            KeyringAdminError::Keyring(KeyringError::KeyCheckValueMismatch { data_key_id, .. })
                if *data_key_id == key_a
        ),
        "{error:?}"
    );
    assert_eq!(
        wrappings(&fixture.pool).await,
        before,
        "the refusal must have written nothing"
    );
}

// =======================================================================================
// R3 — custody backend swap
// =======================================================================================

/// **R3, the common case.** A structurally different backend serving the same bytes under the
/// same key id: **zero writes, zero re-encryption.**
///
/// This works only because the custody backend name is absent from the wrapped-key AAD. The
/// byte-identity assertion over `wrapped_key` is what turns that from a comment in
/// `wrapped_data_key_aad` into a fact — bind the backend name there and this test goes red.
#[tokio::test]
async fn r3_a_different_backend_serving_the_same_bytes_needs_no_writes_at_all() {
    let fixture = fixture().await;
    let environment = env_custody(&[("shared-2026-08", 0x33)], "shared-2026-08");
    let booted = keyring(&fixture, environment).await;
    let rows = seed_all_five(
        &fixture.pool,
        &booted.active_content_cipher(),
        "custody swap",
    )
    .await;

    let before_wrappings = wrappings(&fixture.pool).await;
    let before_columns = column_fingerprints(&fixture.pool).await;
    drop(booted);

    // The swap: a different implementation, a different backend name, the same 32 bytes under
    // the same id. Nothing is run. Nothing is migrated. The process simply restarts.
    let vault_ish = InMemoryCustody::new(&[("shared-2026-08", 0x33)], "shared-2026-08");
    assert_ne!(vault_ish.backend_name(), CUSTODY_BACKEND_ENVIRONMENT);
    let swapped = keyring(&fixture, vault_ish).await;

    for row in &rows {
        assert_eq!(
            read_through_keyring(&fixture.pool, &swapped, row)
                .await
                .expect("every row must open under the new backend"),
            row.plaintext
        );
    }
    assert_eq!(
        wrappings(&fixture.pool).await,
        before_wrappings,
        "a custody swap serving the same bytes must write nothing to content_data_keys"
    );
    assert_eq!(column_fingerprints(&fixture.pool).await, before_columns);
}

/// **R3, the KMS case.** A backend that will not release key bytes is R2 with a different
/// target: unwrap with the old backend, wrap with the new one.
///
/// The three R2 assertions hold here too, and one more that only this case can make: the new
/// rows carry the KMS backend's *own* `wrap_algorithm`, which the loader then rebuilds the AAD
/// from. Hard-coding AES-256-GCM into the rewrap AAD would produce rows that store cleanly and
/// never open again — silently, until a restart.
#[tokio::test]
async fn r3_rewrapping_onto_a_backend_that_never_releases_key_bytes() {
    let fixture = fixture().await;
    let environment = env_custody(&[("env-2026-08", 0x44)], "env-2026-08");
    let booted = keyring(&fixture, environment.clone()).await;
    let rows = seed_all_five(&fixture.pool, &booted.active_content_cipher(), "to kms").await;

    let before_columns = column_fingerprints(&fixture.pool).await;
    let before_wrappings = wrappings(&fixture.pool).await;
    drop(booted);

    let kms = FakeKmsCustody::new(
        &[("arn:aws:kms:eu-west-1:1:key/abc", 0x55)],
        "arn:aws:kms:eu-west-1:1:key/abc",
    );
    let report = admin(&fixture, environment)
        .rewrap_with(kms.clone(), "arn:aws:kms:eu-west-1:1:key/abc")
        .await
        .expect("rewrap onto the KMS backend");
    assert_eq!(report.rewrapped.len(), 1);
    assert_eq!(report.target_backend, "fake_kms");

    let after_wrappings = wrappings(&fixture.pool).await;
    for (id, (master, wrapped, backend, algorithm)) in &after_wrappings {
        let (old_master, old_wrapped, old_backend, old_algorithm) = &before_wrappings[id];
        assert_ne!(master, old_master);
        assert_ne!(wrapped, old_wrapped);
        assert_eq!(backend, "fake_kms");
        assert_ne!(old_backend, "fake_kms");
        // The row records the KMS algorithm, not the environment backend's.
        assert_eq!(algorithm, FAKE_KMS_ALGORITHM);
        assert_ne!(old_algorithm, FAKE_KMS_ALGORITHM);
    }
    assert_eq!(
        column_fingerprints(&fixture.pool).await,
        before_columns,
        "swapping to a KMS must not touch one byte of any *_encrypted column"
    );

    // The whole point: a process whose only custody is the KMS opens the keyring and reads
    // every row written long before the KMS existed.
    let on_kms = keyring(&fixture, kms.clone()).await;
    for row in &rows {
        assert_eq!(
            read_through_keyring(&fixture.pool, &on_kms, row)
                .await
                .expect("every row must open through the KMS-wrapped keyring"),
            row.plaintext
        );
    }
    // Boot unwrapped once per key and never again. Under environment custody a regression
    // here costs microseconds; under a real KMS it is a network round trip per row.
    assert_eq!(kms.unwraps.load(Ordering::SeqCst), 1);
    assert_eq!(kms.wraps.load(Ordering::SeqCst), 1);
}

// =======================================================================================
// abandon
// =======================================================================================

/// **The confession, end to end.** A master key is lost, boot refuses, `abandon` breaks the
/// deadlock, the service starts, and the damage is confined to the rows that key sealed.
#[tokio::test]
async fn abandoning_a_lost_key_lets_the_process_start_and_confines_the_loss_to_its_own_rows() {
    let fixture = fixture().await;
    // Key A under a master key that survives; key B under one that will be lost.
    let both = env_custody(&[("kept", 0x66), ("lost", 0x77)], "lost");
    let first = keyring(&fixture, both.clone()).await;
    let key_b = first.snapshot().active_content_key_id();
    let doomed = seed_all_five(
        &fixture.pool,
        &first.active_content_cipher(),
        "sealed under B",
    )
    .await;
    drop(first);

    // Rotate the *data* key so that a second key exists, minted under `kept`.
    let kept_active = env_custody(&[("kept", 0x66), ("lost", 0x77)], "kept");
    let rotating = admin(&fixture, kept_active.clone());
    let key_a = rotating.add(DataKeyPurpose::Content).await.expect("add").id;
    rotating.promote(key_a).await.expect("promote");
    let second = keyring(&fixture, kept_active).await;
    let survivors = seed_all_five(
        &fixture.pool,
        &second.active_content_cipher(),
        "sealed under A",
    )
    .await;
    drop(second);

    // The loss. `lost` is gone from the configuration and cannot be recovered.
    let only_kept = env_custody(&[("kept", 0x66)], "kept");
    let error = ContentKeyring::load(
        fixture.pool.clone(),
        only_kept.clone(),
        &settings(),
        metrics(),
    )
    .await
    .expect_err("a lost master key must abort boot");
    assert!(
        matches!(&error, KeyringError::UnopenableDataKey { data_key_id, .. } if *data_key_id == key_b),
        "{error:?}"
    );

    // The two guards, before the act. Reported apart because they are different mistakes.
    let admin = admin(&fixture, only_kept.clone());
    assert!(matches!(
        admin.abandon(key_b, false, "the vault was deleted").await,
        Err(KeyringAdminError::AbandonNotConfirmed { .. })
    ));
    assert!(matches!(
        admin.abandon(key_b, true, "   ").await,
        Err(KeyringAdminError::AbandonReasonMissing { .. })
    ));
    // Neither refusal changed anything.
    assert_eq!(state_of(&fixture.pool, key_b).await, "retiring");

    let abandonment = admin
        .abandon(
            key_b,
            true,
            "master key vault destroyed 2026-08-06, no backup",
        )
        .await
        .expect("abandon");
    assert_eq!(abandonment.active_content_key_remaining, Some(key_a));
    assert_eq!(state_of(&fixture.pool, key_b).await, "abandoned");

    // A status, never a delete. Every byte is still there, in case the key resurfaces.
    let (_, wrapped, _, _) = &wrappings(&fixture.pool).await[&key_b];
    assert!(!wrapped.is_empty());
    for row in &doomed {
        assert!(!stored_envelope(&fixture.pool, row).await.is_empty());
    }

    // Boot succeeds now, and the loss is exactly the rows key B sealed.
    let started = keyring(&fixture, only_kept).await;
    for row in &doomed {
        let error = read_through_keyring(&fixture.pool, &started, row)
            .await
            .expect_err("a row under an abandoned key must refuse");
        assert!(
            matches!(error, KeyringError::AbandonedDataKey { id } if id == key_b),
            "a row under an abandoned key must earn its OWN refusal, not \"unknown key\": {error:?}"
        );
    }
    for row in &survivors {
        assert_eq!(
            read_through_keyring(&fixture.pool, &started, row)
                .await
                .expect("rows under every other key keep working"),
            row.plaintext
        );
    }
    // And the abandoned key cost no custody call at all: it is carried as a marker and never
    // handed to a backend that could only fail on it.
    assert_eq!(started.database_loads(), 1);

    // **The anti-deadlock property.** A rewrap after the abandonment must *leave the
    // abandoned key alone* rather than refuse on it. Refusing would re-brick the deployment
    // for precisely the key the operator has already been forced to give up on — the
    // abandonment would buy one boot and then stop the very next master-key rotation.
    let rotated = env_custody(&[("kept", 0x66), ("rotated", 0xaa)], "kept");
    let report = KeyringAdmin::new(fixture.pool.clone(), rotated)
        .rewrap("rotated")
        .await
        .expect("a rewrap must not be blocked by an abandoned key");
    assert_eq!(report.rewrapped, vec![key_a]);
    assert_eq!(report.left_alone, vec![(key_b, "abandoned".to_string())]);
    // Reported, not silent: an operator about to destroy `kept` has to see what still names
    // a master key this rewrap did not move.
    let after = wrappings(&fixture.pool).await;
    assert_eq!(after[&key_a].0, "rotated");
    assert_eq!(after[&key_b].0, "lost", "the abandoned row was rewritten");

    // Abandoning twice is refused rather than silently re-stamped over the first reason.
    assert!(matches!(
        admin.abandon(key_b, true, "again").await,
        Err(KeyringAdminError::NotAbandonable { .. })
    ));
    let reason: Option<String> =
        sqlx::query_scalar("select abandon_reason from content_data_keys where id = $1")
            .bind(key_b)
            .fetch_one(&fixture.pool)
            .await
            .expect("read the reason");
    assert_eq!(
        reason.as_deref(),
        Some("master key vault destroyed 2026-08-06, no backup"),
        "the second abandon overwrote the original reason"
    );
}

// =======================================================================================
// retire, and the reseal that makes it reachable
// =======================================================================================

/// **Retire refusal, then reseal, then retire** — and a resurrected row fails cleanly.
#[tokio::test]
async fn retire_refuses_while_rows_reference_the_key_and_reseal_is_what_makes_it_reachable() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");
    let instance = keyring(&fixture, custody.clone()).await;
    let key_a = instance.snapshot().active_content_key_id();
    let old_rows = seed_all_five(
        &fixture.pool,
        &instance.active_content_cipher(),
        "to be moved",
    )
    .await;

    let admin = admin(&fixture, custody.clone());
    let key_b = admin.add(DataKeyPurpose::Content).await.expect("add").id;
    admin.promote(key_b).await.expect("promote");

    // Refused while active, whatever the counts say.
    assert!(matches!(
        admin.retire(key_b).await,
        Err(KeyringAdminError::StillActive { .. })
    ));

    // Refused while referenced, and the refusal names the counts an operator has to act on.
    let error = admin
        .retire(key_a)
        .await
        .expect_err("a key with live rows must not be retirable");
    let text = error.to_string();
    assert!(
        matches!(&error, KeyringAdminError::StillReferenced { total, .. } if *total == 5),
        "{error:?}"
    );
    for profile in AadProfile::ALL {
        assert!(
            text.contains(&profile_key(profile)),
            "the refusal does not name {}: {text}",
            profile_key(profile)
        );
    }
    assert!(text.contains("reseal"), "{text}");

    // R4.
    let report = admin
        .reseal(key_a, key_b, ResealOptions::default())
        .await
        .expect("reseal");
    assert_eq!(report.resealed, 5);
    assert_eq!(report.skipped, 0);
    assert_eq!(admin.usage(key_a).await.expect("usage").total(), 0);
    assert_eq!(admin.usage(key_b).await.expect("usage").total(), 5);

    // The rows moved keys and kept their plaintext. Both halves: a reseal that wrote garbage
    // would still zero the count above.
    let refreshed = keyring(&fixture, custody.clone()).await;
    for row in &old_rows {
        let envelope = stored_envelope(&fixture.pool, row).await;
        assert_eq!(
            EnvelopeHeader::parse(&envelope).expect("parse").data_key_id,
            key_b,
            "{} was not moved onto the new key",
            row.profile.table()
        );
        assert_eq!(
            read_through_keyring(&fixture.pool, &refreshed, row)
                .await
                .expect("a resealed row must open"),
            row.plaintext
        );
    }

    admin.retire(key_a).await.expect("retire");
    assert_eq!(state_of(&fixture.pool, key_a).await, "retired");

    // A resurrected row — one that names the retired key after the fact — fails **cleanly**.
    // Never a panic, and never a decrypt: a retired key is not loaded, which is precisely what
    // "a row referencing it fails cleanly rather than lazily" means.
    let orphan = seed(
        &fixture.pool,
        &ContentCipher::new(key_a, &Zeroizing::new([0xaau8; 32])),
        AadProfile::RagChunkText,
        b"a row restored from a backup taken before the reseal",
    )
    .await;
    let after_retirement = keyring(&fixture, custody).await;
    let error = read_through_keyring(&fixture.pool, &after_retirement, &orphan)
        .await
        .expect_err("a row naming a retired key must refuse");
    assert!(
        matches!(error, KeyringError::UnknownDataKey { data_key_id } if data_key_id == key_a),
        "{error:?}"
    );
}

/// **Reseal is resumable, and it converges.**
///
/// The interrupted run is bounded rather than killed, and the run that finishes it is issued
/// by a **freshly constructed [`KeyringAdmin`]** — so there is provably no state carried
/// between them. That is the property the design claims: the selection is
/// `where content_envelope_data_key_id(col) = $from`, so every pass re-derives what is left.
#[tokio::test]
async fn reseal_stopped_half_way_converges_when_it_is_run_again() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");
    let instance = keyring(&fixture, custody.clone()).await;
    let key_a = instance.snapshot().active_content_key_id();

    // Six rows in one column, so a bounded pass genuinely leaves work behind.
    let mut rows = Vec::new();
    for index in 0..6 {
        rows.push(
            seed(
                &fixture.pool,
                &instance.active_content_cipher(),
                AadProfile::ConversationMessageContent,
                format!("row {index}").as_bytes(),
            )
            .await,
        );
    }

    let admin = admin(&fixture, custody.clone());
    let key_b = admin.add(DataKeyPurpose::Content).await.expect("add").id;
    admin.promote(key_b).await.expect("promote");

    let partial = admin
        .reseal(
            key_a,
            key_b,
            ResealOptions {
                batch: 2,
                sleep_ms: 0,
                max_batches: Some(1),
            },
        )
        .await
        .expect("the bounded pass");
    assert_eq!(partial.resealed, 2);
    assert_eq!(partial.batches, 1);
    // Half done, and provably so: the bound must not have quietly run to completion, or the
    // resumption below would be testing nothing.
    assert_eq!(admin.usage(key_a).await.expect("usage").total(), 4);

    // A brand-new admin, holding nothing the interrupted one knew.
    let resumed = KeyringAdmin::new(fixture.pool.clone(), custody.clone());
    let finished = resumed
        .reseal(
            key_a,
            key_b,
            ResealOptions {
                batch: 2,
                sleep_ms: 0,
                max_batches: None,
            },
        )
        .await
        .expect("the resumed pass");
    assert_eq!(
        finished.resealed, 4,
        "the resumed pass redid work or missed some"
    );
    assert_eq!(finished.skipped, 0);
    assert_eq!(resumed.usage(key_a).await.expect("usage").total(), 0);

    // A third run is a no-op — idempotent, not merely resumable.
    let again = resumed
        .reseal(key_a, key_b, ResealOptions::default())
        .await
        .expect("a third run");
    assert_eq!(
        again,
        ResealReport {
            resealed: 0,
            skipped: 0,
            batches: 1
        }
    );

    // And no row was double-sealed into nonsense along the way.
    let refreshed = keyring(&fixture, custody).await;
    for row in &rows {
        assert_eq!(
            read_through_keyring(&fixture.pool, &refreshed, row)
                .await
                .expect("every row must open after an interrupted reseal"),
            row.plaintext
        );
    }
}

/// **The compare-and-swap, under a writer that wins the race.**
///
/// A row rewritten between the reseal's `select` and its `update` must be **skipped**, and the
/// concurrent write must survive. Made deterministic by Postgres rather than by a sleep: the
/// competing transaction takes the row lock *first*, so the reseal's `update` is guaranteed to
/// block on it, and the test waits for that blocked lock to appear in `pg_locks` before
/// committing. Under `READ COMMITTED` the update then re-evaluates its `where` against the
/// row the other transaction wrote, `col = $old` no longer matches, and zero rows are hit.
///
/// Without the `and {col} = $3` clause the reseal would win instead, and the concurrent
/// writer's row would be silently replaced with a re-seal of the *stale* plaintext.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_row_rewritten_mid_reseal_is_skipped_and_the_concurrent_write_survives() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");
    let instance = keyring(&fixture, custody.clone()).await;
    let key_a = instance.snapshot().active_content_key_id();
    let row = seed(
        &fixture.pool,
        &instance.active_content_cipher(),
        AadProfile::ConversationMessageContent,
        b"the original content",
    )
    .await;

    let admin = admin(&fixture, custody.clone());
    let key_b = admin.add(DataKeyPurpose::Content).await.expect("add").id;
    admin.promote(key_b).await.expect("promote");
    let refreshed = instance.refresh().await.expect("refresh");

    // The competing writer: the same row, already moved onto the new key with NEW content —
    // exactly what a live update of that row would produce.
    let competitor = refreshed
        .active_content()
        .seal(&row.identity.borrow(), b"rewritten by live traffic")
        .expect("seal");
    let mut blocking = fixture.pool.begin().await.expect("begin");
    let holder_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *blocking)
        .await
        .expect("the competing writer's backend pid");
    sqlx::query("update conversation_messages set content_encrypted = $1 where id = $2")
        .bind(&competitor)
        .bind(row.id)
        .execute(&mut *blocking)
        .await
        .expect("the competing write takes the row lock");

    // The reseal now reads the *committed* (old) value and blocks on the update.
    let resealing = {
        let admin = KeyringAdmin::new(fixture.pool.clone(), custody.clone());
        tokio::spawn(async move { admin.reseal(key_a, key_b, ResealOptions::default()).await })
    };

    // Wait for the block itself rather than for a duration.
    wait_until_blocked_by(
        &fixture.pool,
        holder_pid,
        1,
        "the reseal never blocked on the competing write's row lock; this test proved nothing \
         about the compare-and-swap",
    )
    .await;

    blocking.commit().await.expect("commit the competing write");
    let report = resealing.await.expect("join").expect("reseal");

    // The CAS fired: nothing was written for that row by the reseal.
    assert_eq!(
        report.skipped, 1,
        "the compare-and-swap did not skip the rewritten row"
    );
    assert_eq!(report.resealed, 0);

    // The concurrent write was not lost. This is the assertion the `and col = $3` clause
    // exists for — without it the reseal overwrites `competitor` with a re-seal of the
    // original content, and the live update disappears.
    assert_eq!(
        stored_envelope(&fixture.pool, &row).await,
        competitor,
        "the reseal clobbered a row a concurrent writer had already rewritten"
    );
    let final_keyring = keyring(&fixture, custody).await;
    assert_eq!(
        read_through_keyring(&fixture.pool, &final_keyring, &row)
            .await
            .expect("the final row must read correctly"),
        b"rewritten by live traffic"
    );
}

// =======================================================================================
// Concurrent rotation
// =======================================================================================

/// **Two pods rotate at once.** Exactly one promotion succeeds; the loser learns it lost from
/// the **database**, not from the application.
///
/// Made deterministic by a third transaction that holds the active key's row until both
/// promotions are queued behind it. Both are then guaranteed to have entered their demote
/// statement before either committed, which is the only interleaving in which the partial
/// unique index is the thing that decides.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_simultaneous_promotions_produce_one_winner_and_one_database_refusal() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");
    let instance = keyring(&fixture, custody.clone()).await;
    let bootstrap = instance.snapshot().active_content_key_id();

    let admin = Arc::new(admin(&fixture, custody.clone()));
    // Both `add`s serialise on the advisory lock and get their own versions. A version
    // collision would be a refusal that teaches an operator nothing.
    let first = admin.add(DataKeyPurpose::Content).await.expect("add");
    let second = admin.add(DataKeyPurpose::Content).await.expect("add");
    assert_ne!(first.key_version, second.key_version);

    // Hold the active row so both promotions queue on it.
    let mut gate = fixture.pool.begin().await.expect("begin");
    let gate_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await
        .expect("the gate's backend pid");
    sqlx::query("select id from content_data_keys where id = $1 for update")
        .bind(bootstrap)
        .fetch_one(&mut *gate)
        .await
        .expect("hold the active key's row");

    let tasks: Vec<_> = [first.id, second.id]
        .into_iter()
        .map(|id| {
            let admin = Arc::clone(&admin);
            tokio::spawn(async move { (id, admin.promote(id).await) })
        })
        .collect();

    wait_until_blocked_by(
        &fixture.pool,
        gate_pid,
        2,
        "both promotions never queued on the active key's row, so they did not race and the \
         partial unique index was never the thing that decided",
    )
    .await;
    gate.commit().await.expect("release the gate");

    let mut winner = None;
    let mut loser = None;
    for task in tasks {
        match task.await.expect("join") {
            (id, Ok(_)) => {
                assert!(winner.replace(id).is_none(), "both promotions succeeded");
            }
            (id, Err(error)) => {
                assert!(loser.replace(id).is_none(), "both promotions failed");
                // The refusal is the DATABASE's. No amount of application-side care is what
                // makes "exactly one active key per purpose" true.
                let sqlx_error = match &error {
                    KeyringAdminError::Database(error) => error,
                    other => panic!("the loser must lose on the unique index, not on {other:?}"),
                };
                assert!(
                    sqlx_error
                        .as_database_error()
                        .is_some_and(sqlx::error::DatabaseError::is_unique_violation),
                    "not a unique violation: {sqlx_error}"
                );
            }
        }
    }
    let winner = winner.expect("one promotion must have succeeded");
    let loser = loser.expect("one promotion must have been refused");

    // The loser observes the winner — through `status`, the verb an operator would run.
    let status = admin.status().await.expect("status");
    let active: Vec<&KeyStatus> = status
        .keys
        .iter()
        .filter(|key| key.state == "active")
        .collect();
    assert_eq!(active.len(), 1, "exactly one key may be active per purpose");
    assert_eq!(active[0].id, winner);
    assert_eq!(state_of(&fixture.pool, loser).await, "pending");
    assert_eq!(state_of(&fixture.pool, bootstrap).await, "retiring");

    // And its retry succeeds, because its own key was left untouched by the failure.
    admin.promote(loser).await.expect("the loser's retry");
    assert_eq!(state_of(&fixture.pool, loser).await, "active");
    assert_eq!(state_of(&fixture.pool, winner).await, "retiring");
}

// =======================================================================================
// status and usage
// =======================================================================================

/// `status` is what an operator reads mid-rotation, so every field it needs is asserted —
/// including the one that is a preview of whether boot will refuse.
#[tokio::test]
async fn status_counts_every_sealed_column_and_says_which_master_keys_are_held() {
    let fixture = fixture().await;
    let sealer = env_custody(&[("gone", 0x88)], "gone");
    let booted = keyring(&fixture, sealer.clone()).await;
    let key_a = booted.snapshot().active_content_key_id();
    seed_all_five(&fixture.pool, &booted.active_content_cipher(), "counted").await;
    // Two rows in one column, so a per-column count of 1 everywhere cannot pass by accident.
    seed(
        &fixture.pool,
        &booted.active_content_cipher(),
        AadProfile::RagChunkText,
        b"a second chunk",
    )
    .await;
    drop(booted);

    let held = env_custody(&[("present", 0x99)], "present");
    let status = admin(&fixture, held).status().await.expect("status");
    assert_eq!(status.backend, CUSTODY_BACKEND_ENVIRONMENT);
    assert_eq!(status.active_master_key_id, "present");
    assert_eq!(status.configured_master_key_ids, ["present"]);
    assert_eq!(status.keys.len(), 1);

    let key = &status.keys[0];
    assert_eq!(key.id, key_a);
    assert_eq!(key.state, "active");
    assert_eq!(key.purpose, "content");
    assert_eq!(key.master_key_id, "gone");
    // The field that turns `status` into a rehearsal of the next restart.
    assert!(
        !key.master_key_held,
        "status must say when this process could not open a key"
    );
    assert_eq!(key.key_check_value.len(), 16, "8 bytes rendered as hex");
    assert_eq!(key.usage.total(), 6);
    assert_eq!(
        key.usage.per_column.len(),
        AadProfile::ALL.len(),
        "a sealed column that status does not count is invisible to the retirement audit"
    );
    assert_eq!(
        key.usage.per_column[&profile_key(AadProfile::RagChunkText)],
        2
    );
    assert_eq!(
        key.usage.per_column[&profile_key(AadProfile::MemoryRecordContent)],
        1
    );
    // `usage` and `status` must agree — they are the same question asked two ways.
    assert_eq!(
        admin(&fixture, env_custody(&[("present", 0x99)], "present"))
            .usage(key_a)
            .await
            .expect("usage"),
        key.usage
    );
}

// =======================================================================================
// The SQL helper and the Rust parser are one format
// =======================================================================================

/// **The property test across the language boundary.**
///
/// `content_envelope_data_key_id()` in migration `0027` is a second implementation of the
/// envelope header parser, written in SQL, and the retirement audit depends on it. Two copies
/// of one mapping is the class of bug `src/security/crypto.rs` already carries a warning
/// about, so the two are checked against each other over a generated corpus rather than over
/// one hand-written example.
///
/// The corpus is deliberately mixed: real envelopes across all five profiles and a wide range
/// of plaintext lengths, plus the near-misses that decide the helper's three guards — a
/// flipped magic byte, a bumped format version, a truncation to one byte under the minimum,
/// and pure noise. A corpus of only valid envelopes would agree trivially, because both
/// implementations would return `Some` for everything.
#[tokio::test]
async fn the_sql_helper_and_the_rust_parser_agree_over_a_thousand_generated_envelopes() {
    let fixture = fixture().await;

    // A fixed seed, so a disagreement is reproducible from the failure message alone.
    const SEED: u64 = 0x5eed_1234_abcd_0027;
    let mut state = SEED;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut corpus: Vec<Vec<u8>> = Vec::with_capacity(1_000);
    let mut expected: Vec<Option<Uuid>> = Vec::with_capacity(1_000);
    let mut valid = 0usize;
    while corpus.len() < 1_000 {
        let draw = next();
        let key_id = Uuid::from_u128(u128::from(next()) << 64 | u128::from(next()));
        let profile = AadProfile::ALL[(draw % 5) as usize];
        let cipher = ContentCipher::new(key_id, &Zeroizing::new([(draw >> 8) as u8; 32]));
        let identity = match profile {
            AadProfile::ConversationMessageContent => OwnedIdentity::ConversationMessage {
                message_id: key_id,
                conversation_id: Uuid::from_u128(u128::from(draw)),
                sequence_number: i64::from(draw as u32),
            },
            AadProfile::ConversationSummaryText => OwnedIdentity::ConversationSummary {
                summary_id: key_id,
                conversation_id: Uuid::from_u128(u128::from(draw)),
                covers_through_sequence: i64::from(draw as u32),
            },
            AadProfile::MemoryRecordContent => OwnedIdentity::MemoryRecord {
                memory_id: key_id,
                application_id: Uuid::from_u128(u128::from(draw)),
                memory_scope: "application".to_string(),
            },
            AadProfile::RagDocumentVersionContent => OwnedIdentity::RagDocumentVersion {
                version_id: key_id,
                document_id: Uuid::from_u128(u128::from(draw)),
                version_number: draw as i32,
            },
            AadProfile::RagChunkText => OwnedIdentity::RagChunk {
                chunk_id: key_id,
                document_version_id: Uuid::from_u128(u128::from(draw)),
                chunk_index: draw as i32,
            },
        };
        // 0 to ~2 KiB of plaintext, so the length field and the body both vary widely.
        let body = vec![(draw >> 16) as u8; (draw % 2_048) as usize];
        let envelope = cipher.seal(&identity.borrow(), &body).expect("seal");

        // One valid envelope, then a near-miss derived from it. Each mutation trips exactly
        // one of the helper's three guards, so a helper that dropped a guard disagrees here.
        match draw % 5 {
            0 => {
                let mut broken = envelope.clone();
                broken[(next() % 4) as usize] ^= 0xff; // magic
                corpus.push(broken);
                expected.push(None);
            }
            1 => {
                let mut broken = envelope.clone();
                broken[4] = broken[4].wrapping_add(1); // format version
                corpus.push(broken);
                expected.push(None);
            }
            2 => {
                corpus.push(envelope[..MIN_ENVELOPE_LEN - 1].to_vec()); // too short
                expected.push(None);
            }
            3 => {
                // Pure noise of a legal length. Overwhelmingly not a v1 envelope.
                let noise: Vec<u8> = (0..MIN_ENVELOPE_LEN + (draw % 32) as usize)
                    .map(|_| next() as u8)
                    .collect();
                let named = envelope_data_key_id(&noise);
                expected.push(named);
                corpus.push(noise);
            }
            _ => {}
        }
        if corpus.len() < 1_000 {
            expected.push(Some(key_id));
            corpus.push(envelope);
            valid += 1;
        }
    }
    corpus.truncate(1_000);
    expected.truncate(1_000);

    // A corpus that is all-valid or all-invalid would make the comparison vacuous.
    let named_count = expected.iter().filter(|it| it.is_some()).count();
    assert!(valid > 400, "too few valid envelopes: {valid}");
    assert!(
        (150..900).contains(&named_count),
        "the corpus is not mixed enough to test both answers: {named_count} of 1000 are named"
    );

    // One round trip for the whole corpus, in order.
    let from_sql: Vec<Option<Uuid>> = sqlx::query_scalar(
        "select content_envelope_data_key_id(e) from unnest($1::bytea[]) with ordinality as t(e, n) order by n",
    )
    .bind(&corpus)
    .fetch_all(&fixture.pool)
    .await
    .expect("call the migration's helper over the corpus");
    assert_eq!(from_sql.len(), 1_000);

    for (index, ((sql, rust), bytes)) in from_sql
        .iter()
        .zip(expected.iter())
        .zip(corpus.iter())
        .enumerate()
    {
        assert_eq!(
            sql,
            rust,
            "the SQL helper and the Rust parser disagree at corpus index {index} \
             (seed {SEED:#x}, {} bytes, first 8: {:02x?}). \
             migrations/0027 and src/security/content_envelope.rs are one format and are \
             changed together or not at all.",
            bytes.len(),
            &bytes[..bytes.len().min(8)],
        );
        // Cross-checked against the public accessor too, so the expectation above is not
        // merely this test agreeing with itself about what it just sealed.
        assert_eq!(*sql, envelope_data_key_id(bytes), "at corpus index {index}");
    }
}

/// `moira_content_keyring_keys{state}` is what makes a rotation observable without a shell on
/// the pod, so the series has to be *correct on the way down* as well as on the way up.
///
/// The zero is the assertion. A gauge nobody rewrites keeps its last value forever, so a
/// keyring that reported only the states it found would leave `pending 1` standing after the
/// promotion that consumed it — and the one series an operator watches during a rotation would
/// be the one that lies.
#[tokio::test]
async fn the_keyring_state_gauge_reports_the_states_it_has_none_of() {
    let fixture = fixture().await;
    let custody = env_custody(&[("k1", 1)], "k1");
    let instance = keyring(&fixture, custody.clone()).await;

    let counts = |snapshot: &KeyringSnapshot| -> BTreeMap<&'static str, usize> {
        snapshot.state_counts().into_iter().collect()
    };

    // Bootstrap: one active, and an explicit zero for every other carried state.
    let at_boot = counts(&instance.snapshot());
    assert_eq!(at_boot["active"], 1);
    assert_eq!(at_boot["pending"], 0);
    assert_eq!(at_boot["retiring"], 0);
    assert_eq!(at_boot["abandoned"], 0);
    assert!(
        !at_boot.contains_key("retired"),
        "a retired key is not loaded, so reporting `retired 0` would invite the reading that \
         none exist"
    );

    let admin = admin(&fixture, custody.clone());
    let added = admin.add(DataKeyPurpose::Content).await.expect("add");
    let after_add = counts(&instance.refresh().await.expect("refresh"));
    assert_eq!(after_add["pending"], 1);
    assert_eq!(after_add["active"], 1);

    admin.promote(added.id).await.expect("promote");
    let after_promote = counts(&instance.refresh().await.expect("refresh"));
    // The whole point: `pending` went back to zero and was WRITTEN as zero.
    assert_eq!(after_promote["pending"], 0);
    assert_eq!(after_promote["active"], 1);
    assert_eq!(after_promote["retiring"], 1);

    // The label domain is closed over the states the snapshot carries — a sixth state could
    // not arrive without `DataKeyState::ALL` growing, which is what keeps this exhaustive.
    assert_eq!(
        after_promote.len(),
        DataKeyState::ALL
            .into_iter()
            .filter(|state| state.is_carried_in_snapshot())
            .count()
    );
}

// =======================================================================================
// The gate itself
// =======================================================================================

/// The gate's own contract, asserted rather than described.
///
/// `rotation_gate_database` returns a `LibTestDatabase`, not an `Option`, so there is no
/// early-return form for a rotation test to be written with — the "skip" this suite must not
/// contain is not merely discouraged here, it is unspellable. If this file ever grows a
/// `let Some(_) = … else { return }`, it came from `test_database` and not from the gate.
#[tokio::test]
async fn the_rotation_gate_hands_out_a_database_rather_than_an_option() {
    let database = rotation_gate_database().await;
    let live: String = sqlx::query_scalar("select current_database()")
        .fetch_one(database.pool())
        .await
        .expect("the gate's database is reachable");
    assert!(live.starts_with("moira_test_"), "{live}");
}
