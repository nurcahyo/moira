//! Issue #209 — migration `0028_deepseek_v4_catalog.sql` seeds/updates the DeepSeek
//! `provider_models` catalog: `deepseek-v4-flash` / `deepseek-v4-pro` register active, and the
//! legacy `deepseek-chat` / `deepseek-reasoner` aliases retire to `deprecated`.
//!
//! # Why this drives the migration file directly rather than a fixture assertion
//!
//! `tests/support/mod.rs` clones its fixture databases from a template that has already run
//! every migration through `0028` once — against an empty catalog, where every statement in the
//! file is a no-op (there is no `deepseek` provider yet for any of its `where p.provider_type =
//! 'deepseek'` clauses to match). That path alone would never exercise the insert-and-deprecate
//! logic at all. So each test here seeds a `deepseek` provider first and then re-applies
//! `0028`'s raw SQL (`include_str!`, executed with `sqlx::raw_sql` exactly as
//! `tests/support/mod.rs` applies migrations elsewhere) against the fixture pool, which is the
//! real shape a production deployment takes: `0028` ships and runs while the install has no
//! DeepSeek provider configured yet, and the catalog only gets the v4 ids and the deprecation
//! once an operator (or an earlier admin session) has actually registered one.
//!
//! # No `runtime_factory.rs` coverage here, on purpose
//!
//! This is catalog data only. `provider_models.model_key` reaches `rig_core` unvalidated
//! (`Client::completion_model(model_key)` in `src/orchestration/runtime_factory.rs`), so there is
//! no Rust code path this migration changes — the migration's own SQL is the entire behavior
//! under test.

mod support;

use sqlx::Row;
use support::TestDatabase;
use uuid::Uuid;

const MIGRATION_0028: &str = include_str!("../migrations/0028_deepseek_v4_catalog.sql");

/// Every `(model_key, status)` pair currently registered for one provider, ordered so the
/// assertions below are exact rather than set-based.
async fn deepseek_catalog_rows(db: &TestDatabase, provider_id: Uuid) -> Vec<(String, String)> {
    let rows = sqlx::query(
        "select model_key, status from provider_models \
         where provider_id = $1 and deleted_at is null \
         order by model_key",
    )
    .bind(provider_id)
    .fetch_all(&db.pool)
    .await
    .expect("read back the deepseek provider_models catalog");

    rows.into_iter()
        .map(|row| {
            (
                row.get::<String, _>("model_key"),
                row.get::<String, _>("status"),
            )
        })
        .collect()
}

async fn seed_deepseek_provider(db: &TestDatabase) -> Uuid {
    let provider_id = Uuid::now_v7();
    sqlx::query(
        "insert into providers (id, provider_type, display_name, status) \
         values ($1, 'deepseek', 'DeepSeek', 'active')",
    )
    .bind(provider_id)
    .execute(&db.pool)
    .await
    .expect("seed a deepseek provider");
    provider_id
}

/// The path a real deployment took before `0028` shipped: an admin registered the DeepSeek
/// provider and pointed it at the two model ids DeepSeek offered at the time, both `active`.
async fn seed_legacy_models_as_active(db: &TestDatabase, provider_id: Uuid) {
    sqlx::query(
        "insert into provider_models (id, provider_id, model_key, display_name, status) values \
         ($1, $3, 'deepseek-chat', 'DeepSeek Chat', 'active'), \
         ($2, $3, 'deepseek-reasoner', 'DeepSeek Reasoner', 'active')",
    )
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(provider_id)
    .execute(&db.pool)
    .await
    .expect("seed the pre-0028 legacy model rows as an admin would have created them");
}

async fn apply_migration_0028(db: &TestDatabase) {
    sqlx::raw_sql(MIGRATION_0028)
        .execute(&db.pool)
        .await
        .expect("apply migration 0028 against the fixture database");
}

#[tokio::test]
async fn v4_models_register_active_and_legacy_aliases_deprecate_for_an_existing_deepseek_provider()
{
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let provider_id = seed_deepseek_provider(&db).await;
    seed_legacy_models_as_active(&db, provider_id).await;

    apply_migration_0028(&db).await;

    assert_eq!(
        deepseek_catalog_rows(&db, provider_id).await,
        vec![
            ("deepseek-chat".to_string(), "deprecated".to_string()),
            ("deepseek-reasoner".to_string(), "deprecated".to_string()),
            ("deepseek-v4-flash".to_string(), "active".to_string()),
            ("deepseek-v4-pro".to_string(), "active".to_string()),
        ],
        "0028 must add the v4 ids as active and flip the pre-existing legacy aliases to \
         deprecated"
    );
}

/// A tenant that never registered the legacy aliases at all — 0028 still records them,
/// deprecated from the start, rather than leaving discovery silent about their retirement.
#[tokio::test]
async fn legacy_aliases_are_inserted_already_deprecated_when_a_provider_never_had_them() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let provider_id = seed_deepseek_provider(&db).await;

    apply_migration_0028(&db).await;

    assert_eq!(
        deepseek_catalog_rows(&db, provider_id).await,
        vec![
            ("deepseek-chat".to_string(), "deprecated".to_string()),
            ("deepseek-reasoner".to_string(), "deprecated".to_string()),
            ("deepseek-v4-flash".to_string(), "active".to_string()),
            ("deepseek-v4-pro".to_string(), "active".to_string()),
        ],
        "0028 must insert the legacy aliases as deprecated when a deepseek provider never had \
         them, not merely update rows that happen to already exist"
    );
}

/// Idempotency: 0028 is written with `on conflict do nothing` inserts and a status-guarded
/// update (`pm.status <> 'deprecated'`), so re-running it changes nothing further.
#[tokio::test]
async fn reapplying_migration_0028_is_a_no_op_the_second_time() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let provider_id = seed_deepseek_provider(&db).await;
    seed_legacy_models_as_active(&db, provider_id).await;

    apply_migration_0028(&db).await;
    let first_pass = deepseek_catalog_rows(&db, provider_id).await;

    apply_migration_0028(&db).await;
    let second_pass = deepseek_catalog_rows(&db, provider_id).await;

    assert_eq!(
        first_pass, second_pass,
        "re-applying 0028 must not change rows, timestamps aside, once the catalog already \
         reflects it"
    );
}

/// The shape every install actually starts in: 0028 ships while no `deepseek` provider is
/// configured yet. Every statement in the migration is scoped to
/// `providers.provider_type = 'deepseek'`, so this must insert and update nothing at all.
#[tokio::test]
async fn migration_0028_is_a_no_op_with_no_deepseek_provider_configured() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let before: i64 = sqlx::query_scalar("select count(*) from provider_models")
        .fetch_one(&db.pool)
        .await
        .expect("count provider_models before re-applying 0028");

    apply_migration_0028(&db).await;

    let after: i64 = sqlx::query_scalar("select count(*) from provider_models")
        .fetch_one(&db.pool)
        .await
        .expect("count provider_models after re-applying 0028");

    assert_eq!(
        before, after,
        "0028 must not touch provider_models when no deepseek provider exists"
    );
}
