//! F47 — the `get_or_create_*_policy` family must read without writing.
//!
//! # Why "the policy row comes back" is not the property
//!
//! The obvious guard here asserts that `get_or_create_conversation_policy` returns the
//! application's policy. That assertion passes identically against
//! `on conflict … do update set application_id = excluded.application_id` and against
//! `on conflict … do nothing` plus a `select` — the defect was never in the value returned.
//! **The defect is the write**, so every assertion below is on the write, observed three
//! different ways:
//!
//! | observed | what it catches |
//! |---|---|
//! | `xmin` | a new heap tuple — the dead-tuple churn and the WAL record |
//! | `version` | the `ETag` served by `GET …/policy` and demanded back on `If-Match`, bumped by the `<table>_bump_version` trigger |
//! | `pg_notify('moira_runtime_config', …)` | the `<table>_runtime_config_notify` trigger, whose payload makes every replica run `apply_invalidation` — which calls `cache.invalidate_all()`, `runtime_handles.invalidate_all()` and `auth_settings.invalidate_all()` unconditionally |
//!
//! The third is the consequence that made F47 more than a performance note: a
//! conversation-linked turn read two policy rows three times, so it wiped every replica's
//! runtime-config and provider-handle caches three times per turn.
//!
//! # The silence is checked against a control, in the same test, on the same listener
//!
//! An assertion that *no* notification arrived is worthless if notifications cannot arrive —
//! that is the F16 shape, where a mitigation's tests exercised a predicate re-implemented in
//! the test module and deleting the real one left everything green. So
//! [`a_policy_read_is_not_a_write`] proves the channel is live **after** proving the reads
//! were silent, by performing a genuine `put_*_policy` on the same listener and requiring a
//! notification for that table. Silence and liveness are asserted as one fact, not two.
//!
//! # No timeouts and no `sleep` in the notification assertion
//!
//! "Nothing arrived within N milliseconds" is a delay, which `CONVENTIONS.md` §3 rejects, and
//! it is also weaker than it looks. Instead the test emits its own **sentinel** notification
//! after the reads and then blocks on `recv()`: Postgres delivers notifications in commit
//! order on one connection, so if the sentinel is the first thing received, nothing the reads
//! did was announced. That is an acknowledgement gate, and it is exact.
//!
//! # Which variables the fixture's known state collapses — and the test that separates them
//!
//! [`a_policy_read_is_not_a_write`] pins every row to "already exists" so the signatures are
//! exactly comparable. That known state makes *read-then-insert* and **select-only**
//! indistinguishable: a mutation that deleted the insert entirely and returned `NotFound` for
//! a missing policy would satisfy every assertion in it, because no policy is ever missing
//! there. [`a_first_touch_creates_the_row_and_says_so`] is what separates them, and
//! [`concurrent_first_touches_all_succeed`] separates *both* from the read-then-**bare**-insert
//! spelling that `get_or_create_application_execution_policy` actually shipped — which never
//! wrote on the steady-state path and so passes the first test, and raced on the first-touch
//! path and so fails the third.
//!
//! # All five family members, not the two on the conversation path
//!
//! The cheapest edit that breaks this property is to revert **one** member. Every test below
//! therefore drives all five: the four on `PgConversationRepository` and
//! `get_or_create_application_execution_policy` on `PgPublicRepository`.

mod support;

use std::sync::Arc;

use moira::infra::repositories::{
    ConversationRepository, PgConversationRepository, PgPublicRepository, PublicRepository,
};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgListener};
use support::TestDatabase;
use tokio::sync::Barrier;
use uuid::Uuid;

/// Payload emitted by the test itself to close the notification window.
const SENTINEL: &str = "f47-sentinel";

/// The channel `notify_moira_runtime_config_change` writes to.
const CHANNEL: &str = "moira_runtime_config";

/// How many times a "read" is repeated before the row is re-inspected. Three is not arbitrary:
/// it is what one conversation-linked turn issues across `extract_memories`,
/// `summarize_conversation_unscoped` and the memory-policy read.
const READS: usize = 3;

/// Every table in the `get_or_create_*_policy` family, paired with a call that reads it.
///
/// Kept as data so no member can be covered by one test and missed by another, and pinned
/// against the schema by [`the_family_is_these_five_and_no_others`] so a sixth policy table
/// cannot be added with the old spelling and simply go unwatched.
const POLICY_TABLES: [&str; 5] = [
    "application_conversation_policies",
    "application_memory_policies",
    "application_retrieval_policies",
    "application_embedding_policies",
    "application_execution_policies",
];

/// Reads every family member once, so a test can express "do the reads" without repeating the
/// five call sites. Returns nothing: the assertions are all on the database.
async fn read_every_policy(pool: &PgPool, application_id: Uuid) {
    let conversations = PgConversationRepository::new(pool.clone());
    let public = PgPublicRepository::new(pool.clone());
    conversations
        .get_or_create_conversation_policy(application_id)
        .await
        .expect("conversation policy");
    conversations
        .get_or_create_memory_policy(application_id)
        .await
        .expect("memory policy");
    conversations
        .get_or_create_retrieval_policy(application_id)
        .await
        .expect("retrieval policy");
    conversations
        .get_or_create_embedding_policy(application_id)
        .await
        .expect("embedding policy");
    public
        .get_or_create_application_execution_policy(application_id)
        .await
        .expect("execution policy");
}

/// The row's identity as the storage layer sees it.
///
/// `xmin` is the inserting transaction of the *current tuple version*, so it changes on any
/// heap update — including a "no-op" `set application_id = excluded.application_id`, which
/// Postgres does not optimise away. `version` and `updated_at` come from the
/// `<table>_bump_version` trigger and are the caller-visible half of the same write.
#[derive(Debug, PartialEq, Eq)]
struct RowSignature {
    xmin: String,
    version: i64,
    updated_at: String,
}

async fn signature(pool: &PgPool, table: &str, application_id: Uuid) -> RowSignature {
    let row = sqlx::query(&format!(
        "select xmin::text as xmin, version, updated_at::text as updated_at \
         from {table} where application_id = $1"
    ))
    .bind(application_id)
    .fetch_optional(pool)
    .await
    .expect("read the policy row signature")
    .unwrap_or_else(|| panic!("{table} has no row for this application"));
    RowSignature {
        xmin: row.get("xmin"),
        version: row.get("version"),
        updated_at: row.get("updated_at"),
    }
}

async fn row_count(pool: &PgPool, table: &str, application_id: Uuid) -> i64 {
    sqlx::query(&format!(
        "select count(*) as n from {table} where application_id = $1"
    ))
    .bind(application_id)
    .fetch_one(pool)
    .await
    .expect("count policy rows")
    .get("n")
}

async fn seed_application(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    let suffix = id.simple();
    sqlx::query(
        "insert into applications (id, external_application_id, application_slug, \
         display_name, metadata) values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(format!("f47-{suffix}"))
    .bind(format!("f47-{suffix}"))
    .bind("F47 policy read")
    .bind(json!({ "test_fixture": true }))
    .execute(pool)
    .await
    .expect("seed application");
    id
}

/// The suite's own database, or `None` when there is no Postgres to talk to.
///
/// Fail-closed behaviour is not re-implemented here: `database_origin` in
/// `tests/support/mod.rs` already panics under `CI=true` and prints the one skip line
/// `scripts/gates.sh` greps for. A second copy of that check is how the predicate and the
/// wiring drift apart — F16's shape exactly.
async fn database(max_connections: u32) -> Option<TestDatabase> {
    TestDatabase::create_with_max_connections(max_connections).await
}

/// Emits the sentinel and returns every notification that preceded it.
///
/// Blocks until the sentinel is seen, so it can neither hang on an empty channel nor pass by
/// giving up early.
async fn drain_until_sentinel(listener: &mut PgListener, pool: &PgPool) -> Vec<String> {
    sqlx::query("select pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(SENTINEL)
        .execute(pool)
        .await
        .expect("emit the sentinel notification");
    let mut seen = Vec::new();
    loop {
        let notification = listener.recv().await.expect("listener stayed connected");
        let payload = notification.payload().to_string();
        if payload == SENTINEL {
            return seen;
        }
        seen.push(payload);
    }
}

/// The property: reading a policy that already exists changes nothing and announces nothing.
#[tokio::test]
async fn a_policy_read_is_not_a_write() {
    let Some(db) = database(8).await else {
        return;
    };
    let pool = db.pool.clone();
    let application_id = seed_application(&pool).await;

    // First touch, deliberately outside the observed window: creating a policy really is a
    // write, and this test is about the reads that follow.
    read_every_policy(&pool, application_id).await;

    let mut before = Vec::new();
    for table in POLICY_TABLES {
        before.push(signature(&pool, table, application_id).await);
    }

    let mut listener = PgListener::connect_with(&pool)
        .await
        .expect("attach a listener");
    listener.listen(CHANNEL).await.expect("listen");

    for _ in 0..READS {
        read_every_policy(&pool, application_id).await;
    }

    let announced = drain_until_sentinel(&mut listener, &pool).await;
    assert!(
        announced.is_empty(),
        "reading a policy announced a runtime-config change, so every replica dropped its \
         runtime and provider-handle caches: {announced:?}"
    );

    for (table, expected) in POLICY_TABLES.into_iter().zip(before) {
        let actual = signature(&pool, table, application_id).await;
        assert_eq!(
            actual, expected,
            "{READS} reads of {table} rewrote the row; xmin/version/updated_at all move on a \
             heap update, and `version` is the ETag an operator's If-Match depends on"
        );
    }

    // The control, on the same listener: a genuine write must still be announced. Without
    // this the assertion above would hold just as well against a database with the triggers
    // dropped, or a listener attached to the wrong channel.
    sqlx::query(
        "update application_conversation_policies set conversations_enabled = \
         not conversations_enabled where application_id = $1",
    )
    .bind(application_id)
    .execute(&pool)
    .await
    .expect("a real policy write");
    let announced = drain_until_sentinel(&mut listener, &pool).await;
    assert!(
        announced
            .iter()
            .any(|payload| payload.contains("application_conversation_policies")),
        "the notification channel is not live, so the silence asserted above proves nothing; \
         saw {announced:?}"
    );
}

/// The separator for the collapsed variable: a missing policy is still created.
///
/// [`a_policy_read_is_not_a_write`] cannot distinguish read-then-insert from a select with no
/// insert at all, because it never runs against a missing row. This does.
#[tokio::test]
async fn a_first_touch_creates_the_row_and_says_so() {
    let Some(db) = database(8).await else {
        return;
    };
    let pool = db.pool.clone();
    let application_id = seed_application(&pool).await;

    for table in POLICY_TABLES {
        assert_eq!(
            row_count(&pool, table, application_id).await,
            0,
            "{table} must start empty for this test to mean anything"
        );
    }

    let mut listener = PgListener::connect_with(&pool)
        .await
        .expect("attach a listener");
    listener.listen(CHANNEL).await.expect("listen");

    read_every_policy(&pool, application_id).await;

    for table in POLICY_TABLES {
        assert_eq!(
            row_count(&pool, table, application_id).await,
            1,
            "{table} was not auto-provisioned on first touch"
        );
    }

    // A creation is a write and *should* be announced — once per table, not once per read.
    let announced = drain_until_sentinel(&mut listener, &pool).await;
    for table in POLICY_TABLES {
        assert_eq!(
            announced
                .iter()
                .filter(|payload| payload.contains(table))
                .count(),
            1,
            "expected exactly one creation notification for {table}, saw {announced:?}"
        );
    }
}

/// The other half of F47, and the one the write assertions cannot see.
///
/// Removing `do update` removed a row-level lock as well as a heap write, and the two are
/// separable: `select … for update` would restore the serialisation with **no** new tuple
/// version, no `version` bump and no notification, leaving every assertion in
/// [`a_policy_read_is_not_a_write`] green. That is the cheapest edit that breaks this
/// finding's property while satisfying the rest of this file, so it gets its own test.
///
/// A conflicting exclusive lock is held on the row by an uncommitted transaction on another
/// connection. A read that takes no lock finishes immediately; one that takes any lock blocks
/// until the holder resolves, and the bound below turns that into a failure instead of a hang.
/// The bound is not an interleaving delay — the correct implementation never waits at all.
#[tokio::test]
async fn a_policy_read_does_not_wait_for_a_row_lock() {
    let Some(db) = database(8).await else {
        return;
    };
    let pool = db.pool.clone();
    let application_id = seed_application(&pool).await;
    read_every_policy(&pool, application_id).await;

    for table in POLICY_TABLES {
        let mut holder = pool.begin().await.expect("open the lock holder");
        sqlx::query(&format!(
            "select 1 from {table} where application_id = $1 for update"
        ))
        .bind(application_id)
        .fetch_one(&mut *holder)
        .await
        .unwrap_or_else(|error| panic!("hold an exclusive lock on {table}: {error}"));

        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_every_policy(&pool, application_id),
        )
        .await;

        // Released before the assertion so a failure does not leave the fixture wedged.
        holder.rollback().await.expect("release the lock holder");
        assert!(
            read.is_ok(),
            "reading policies blocked on an exclusive row lock held on {table}, so these \
             reads still serialise concurrent turns of the same application"
        );
    }
}

/// The family is defined by the schema, not by the list at the top of this file.
///
/// Every assertion here iterates [`POLICY_TABLES`]. A sixth `application_*_policies` table
/// with a `get_or_create_*_policy` of its own would be covered by nothing and this suite
/// would stay green — the shape `MINIMUM_SUITES_SCANNED` exists to prevent one file over in
/// `tests/test_database_isolation.rs`. So the list is pinned against `information_schema`,
/// which is an independent source: adding the table is what reds this, before anyone has to
/// remember the convention.
#[tokio::test]
async fn the_family_is_these_five_and_no_others() {
    let Some(db) = database(8).await else {
        return;
    };
    let mut actual: Vec<String> = sqlx::query(
        "select table_name from information_schema.tables \
         where table_schema = 'public' and table_name like 'application\\_%\\_policies'",
    )
    .fetch_all(&db.pool)
    .await
    .expect("list policy tables")
    .into_iter()
    .map(|row| row.get::<String, _>("table_name"))
    .collect();
    actual.sort();

    let mut expected: Vec<String> = POLICY_TABLES.iter().map(|t| (*t).to_string()).collect();
    expected.sort();

    assert_eq!(
        actual, expected,
        "the set of application policy tables changed; add the new member to POLICY_TABLES \
         and give it a `get_or_create_*_policy` that reads without writing (F47)"
    );
}

/// The race the row lock used to hide.
///
/// `on conflict do nothing` returns no row on the conflict path, so the row has to be fetched
/// by a second statement — and a concurrent inserter can commit in between. It also removes
/// the row-level lock that serialised these callers. Both directions are exercised here:
/// every task must succeed, and exactly one row must exist afterwards.
///
/// This is also the test that fails against the spelling
/// `get_or_create_application_execution_policy` shipped — a `select` followed by an
/// `insert` with **no** `on conflict` clause, whose loser raised
/// `duplicate key value violates unique constraint`.
#[tokio::test]
async fn concurrent_first_touches_all_succeed() {
    const RACERS: usize = 10;

    let Some(db) = database(16).await else {
        return;
    };
    let pool = db.pool.clone();

    for table in POLICY_TABLES {
        // A fresh application per table, so every racer is a genuine first touch.
        let application_id = seed_application(&pool).await;
        let barrier = Arc::new(Barrier::new(RACERS));
        let mut tasks = Vec::with_capacity(RACERS);

        for _ in 0..RACERS {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                let conversations = PgConversationRepository::new(pool.clone());
                let public = PgPublicRepository::new(pool);
                // An acknowledgement gate, not a delay: `wait` returns only once every task
                // has arrived, so all ten issue their first statement together.
                barrier.wait().await;
                match table {
                    "application_conversation_policies" => conversations
                        .get_or_create_conversation_policy(application_id)
                        .await
                        .map(|policy| policy.application_id),
                    "application_memory_policies" => conversations
                        .get_or_create_memory_policy(application_id)
                        .await
                        .map(|policy| policy.application_id),
                    "application_retrieval_policies" => conversations
                        .get_or_create_retrieval_policy(application_id)
                        .await
                        .map(|policy| policy.application_id),
                    "application_embedding_policies" => conversations
                        .get_or_create_embedding_policy(application_id)
                        .await
                        .map(|policy| policy.application_id),
                    "application_execution_policies" => public
                        .get_or_create_application_execution_policy(application_id)
                        .await
                        .map(|policy| policy.application_id),
                    other => panic!("unhandled policy table {other}"),
                }
            }));
        }

        for task in tasks {
            let returned = task
                .await
                .expect("racer task did not panic")
                .unwrap_or_else(|error| {
                    panic!("a concurrent first touch of {table} failed: {error}")
                });
            assert_eq!(
                returned, application_id,
                "{table} returned another application's policy"
            );
        }

        assert_eq!(
            row_count(&pool, table, application_id).await,
            1,
            "{RACERS} concurrent first touches of {table} left more or fewer than one row"
        );
    }
}
