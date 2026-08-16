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
//!
//! # `0036`, and why the second half of this file resolves candidates rather than reading rows
//!
//! Issue #256: `0028` retires the legacy aliases and stops there, but `deprecated` is not a label
//! — `list_model_candidates` joins `provider_models` on `pm.status = 'active'`, so a
//! `routing_policies` row still naming a retired alias resolves to *nothing* the moment `0028`
//! commits, and a route with no other policy stops answering. `0036` repoints those policies onto
//! the successor on the same provider.
//!
//! The repoint tests therefore assert on what
//! `PgRuntimeRepository::list_model_candidates` returns, not on `routing_policies` columns. The
//! defect is invisible in the row — the policy still exists, still says `active`, and still names
//! a model that still exists — and only shows up as an empty candidate list. Reading the column
//! back would have passed against `0028` alone.

mod support;

use moira::infra::repositories::{PgRuntimeRepository, RuntimeRepository};
use serde_json::Value;
use sqlx::Row;
use support::TestDatabase;
use uuid::Uuid;

const MIGRATION_0028: &str = include_str!("../migrations/0028_deepseek_v4_catalog.sql");
const MIGRATION_0036: &str =
    include_str!("../migrations/0036_deepseek_legacy_aliases_do_not_strand_routing_policies.sql");

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

async fn apply_migration_0036(db: &TestDatabase) {
    sqlx::raw_sql(MIGRATION_0036)
        .execute(&db.pool)
        .await
        .expect("apply migration 0036 against the fixture database");
}

/// A route of its own per case, so one test's policies can never resolve into another's.
async fn seed_route(db: &TestDatabase) -> Uuid {
    let route_id = Uuid::now_v7();
    sqlx::query(
        "insert into route_definitions (id, route_key, display_name) \
         values ($1, $2, 'DeepSeek repoint fixture')",
    )
    .bind(route_id)
    .bind(format!("deepseek-{}", Uuid::now_v7().simple()))
    .execute(&db.pool)
    .await
    .expect("seed a route definition");
    route_id
}

async fn seed_policy(
    db: &TestDatabase,
    route_id: Uuid,
    provider_id: Uuid,
    provider_model_id: Uuid,
    status: &str,
) -> Uuid {
    let policy_id = Uuid::now_v7();
    sqlx::query(
        "insert into routing_policies \
         (id, route_id, provider_id, provider_model_id, priority, status) \
         values ($1, $2, $3, $4, 100, $5)",
    )
    .bind(policy_id)
    .bind(route_id)
    .bind(provider_id)
    .bind(provider_model_id)
    .bind(status)
    .execute(&db.pool)
    .await
    .expect("seed a routing policy");
    policy_id
}

async fn model_id(db: &TestDatabase, provider_id: Uuid, model_key: &str) -> Uuid {
    sqlx::query_scalar(
        "select id from provider_models \
         where provider_id = $1 and model_key = $2 and deleted_at is null",
    )
    .bind(provider_id)
    .bind(model_key)
    .fetch_one(&db.pool)
    .await
    .unwrap_or_else(|error| panic!("read back the id of {model_key}: {error}"))
}

/// What the route actually resolves to, through the production query rather than a copy of it.
///
/// `list_model_candidates` is the one `MoiraExecutionService` calls, `pm.status = 'active'`
/// predicate included, so this asks the question an execution asks.
async fn resolvable_model_keys(db: &TestDatabase, route_id: Uuid) -> Vec<String> {
    PgRuntimeRepository::new(db.pool.clone())
        .list_model_candidates(route_id, None, None, 10)
        .await
        .expect("resolve the route's candidates")
        .into_iter()
        .map(|candidate| candidate.model_key)
        .collect()
}

async fn policy_row(db: &TestDatabase, policy_id: Uuid) -> (Uuid, i64, Value) {
    let row = sqlx::query(
        "select provider_model_id, version, metadata from routing_policies where id = $1",
    )
    .bind(policy_id)
    .fetch_one(&db.pool)
    .await
    .expect("read back the routing policy");
    (
        row.get("provider_model_id"),
        row.get("version"),
        row.get("metadata"),
    )
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

/// Issue #256 finding 1 — the whole of it, in the order a deployment lives it.
///
/// The middle assertion is the finding: after `0028` and before `0036`, both routes resolve to
/// **nothing**. Not to a deprecated model, not to a degraded one — the candidate list is empty
/// and the route stops answering, from a migration, with the policy row still sitting there
/// looking correct.
#[tokio::test]
async fn a_route_pointed_at_a_legacy_alias_stops_resolving_under_0028_and_0035_repoints_it() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let provider_id = seed_deepseek_provider(&db).await;
    seed_legacy_models_as_active(&db, provider_id).await;
    let chat_id = model_id(&db, provider_id, "deepseek-chat").await;
    let reasoner_id = model_id(&db, provider_id, "deepseek-reasoner").await;

    let chat_route = seed_route(&db).await;
    let chat_policy = seed_policy(&db, chat_route, provider_id, chat_id, "active").await;
    let reasoner_route = seed_route(&db).await;
    let reasoner_policy =
        seed_policy(&db, reasoner_route, provider_id, reasoner_id, "active").await;

    assert_eq!(
        resolvable_model_keys(&db, chat_route).await,
        vec!["deepseek-chat".to_string()],
        "the fixture must start from a route that actually resolves, or the assertion below \
         proves nothing"
    );

    apply_migration_0028(&db).await;

    assert!(
        resolvable_model_keys(&db, chat_route).await.is_empty()
            && resolvable_model_keys(&db, reasoner_route).await.is_empty(),
        "0028 alone leaves both routes resolving to nothing: every candidate query filters \
         pm.status = 'active', so deprecating the alias removes the only candidate the policy \
         had. This is the break 0036 exists to prevent"
    );

    apply_migration_0036(&db).await;

    assert_eq!(
        resolvable_model_keys(&db, chat_route).await,
        vec!["deepseek-v4-flash".to_string()],
        "0036 must repoint a deepseek-chat policy onto the general-chat successor on the same \
         provider"
    );
    assert_eq!(
        resolvable_model_keys(&db, reasoner_route).await,
        vec!["deepseek-v4-pro".to_string()],
        "0036 must repoint a deepseek-reasoner policy onto the reasoning-tier successor"
    );

    let (repointed_model, version, metadata) = policy_row(&db, chat_policy).await;
    assert_eq!(
        repointed_model,
        model_id(&db, provider_id, "deepseek-v4-flash").await
    );
    assert_eq!(
        version, 2,
        "the routing_policies version trigger must have fired, so a client holding an old \
         If-Match gets a 409 rather than silently overwriting the repoint"
    );
    assert_eq!(
        metadata
            .pointer("/deepseek_v4_repoint/from_model_key")
            .and_then(Value::as_str),
        Some("deepseek-chat"),
        "the substitution must be recorded on the row it changed — 0036 picks which successor \
         replaces which alias, and an operator who disagrees can only reverse it if the migration \
         says what it moved off"
    );

    let (_, _, reasoner_metadata) = policy_row(&db, reasoner_policy).await;
    assert_eq!(
        reasoner_metadata
            .pointer("/deepseek_v4_repoint/from_model_key")
            .and_then(Value::as_str),
        Some("deepseek-reasoner")
    );
}

/// A route that already has a live policy for the successor must not end up with the same
/// candidate twice.
///
/// `routing_policies` has no uniqueness over `(route, application, tenant, provider, model)`, so
/// a blind repoint would be accepted by the database and would put one model in a fallback chain
/// twice — the same upstream tried, failed and retried as though it were a second option.
#[tokio::test]
async fn a_scope_that_already_names_the_successor_is_not_given_a_duplicate_candidate() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let provider_id = seed_deepseek_provider(&db).await;
    seed_legacy_models_as_active(&db, provider_id).await;
    apply_migration_0028(&db).await;

    let route = seed_route(&db).await;
    let legacy_policy = seed_policy(
        &db,
        route,
        provider_id,
        model_id(&db, provider_id, "deepseek-chat").await,
        "active",
    )
    .await;
    seed_policy(
        &db,
        route,
        provider_id,
        model_id(&db, provider_id, "deepseek-v4-flash").await,
        "active",
    )
    .await;

    apply_migration_0036(&db).await;

    assert_eq!(
        resolvable_model_keys(&db, route).await,
        vec!["deepseek-v4-flash".to_string()],
        "the successor must appear exactly once in the route's fallback chain"
    );
    let (still_legacy, version, metadata) = policy_row(&db, legacy_policy).await;
    assert_eq!(
        still_legacy,
        model_id(&db, provider_id, "deepseek-chat").await,
        "the redundant legacy policy is left as it is; nothing is stranded, because the route is \
         already served by the successor policy"
    );
    assert_eq!(
        version, 1,
        "an untouched policy must not have been rewritten"
    );
    assert_eq!(metadata, serde_json::json!({}));
}

/// The documented scope, both edges of it: history is not rewritten, and a policy an operator has
/// merely switched off is still repaired.
///
/// A `disabled` policy resolves to nothing either way today, so leaving it would cost nothing
/// now and would resurrect a dead model reference the day someone re-enables it.
#[tokio::test]
async fn a_soft_deleted_policy_is_left_alone_and_a_disabled_one_is_repointed() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let provider_id = seed_deepseek_provider(&db).await;
    seed_legacy_models_as_active(&db, provider_id).await;
    let chat_id = model_id(&db, provider_id, "deepseek-chat").await;

    let route = seed_route(&db).await;
    let disabled = seed_policy(&db, route, provider_id, chat_id, "disabled").await;
    let deleted = seed_policy(&db, route, provider_id, chat_id, "deleted").await;
    sqlx::query("update routing_policies set deleted_at = now() where id = $1")
        .bind(deleted)
        .execute(&db.pool)
        .await
        .expect("soft-delete a routing policy");

    apply_migration_0028(&db).await;
    apply_migration_0036(&db).await;

    let (disabled_model, _, _) = policy_row(&db, disabled).await;
    assert_eq!(
        disabled_model,
        model_id(&db, provider_id, "deepseek-v4-flash").await,
        "a disabled policy is repaired now so that re-enabling it later does not resurrect a \
         retired model id"
    );

    let (deleted_model, _, deleted_metadata) = policy_row(&db, deleted).await;
    assert_eq!(
        deleted_model, chat_id,
        "a soft-deleted policy is history and is not rewritten"
    );
    assert_eq!(deleted_metadata, serde_json::json!({}));
}

/// Idempotence, and the no-deepseek-provider case in the same run.
///
/// The version column is the sharp end: `0036` is an UPDATE behind a NOTIFY and a version-bump
/// trigger, so a second pass that matched even one row would show up as a bump and as a spurious
/// cross-replica cache invalidation.
#[tokio::test]
async fn reapplying_migration_0035_matches_nothing_the_second_time() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    // A provider that is not DeepSeek, with a model deliberately named after a retired alias.
    // Every statement in 0036 is scoped to `providers.provider_type = 'deepseek'`, and the model
    // key alone must not be enough to drag a row in.
    let other_provider = Uuid::now_v7();
    sqlx::query(
        "insert into providers (id, provider_type, display_name, status) \
         values ($1, 'openai', 'Not DeepSeek', 'active')",
    )
    .bind(other_provider)
    .execute(&db.pool)
    .await
    .expect("seed a non-deepseek provider");
    let other_model = Uuid::now_v7();
    sqlx::query(
        "insert into provider_models (id, provider_id, model_key, display_name, status) \
         values ($1, $2, 'deepseek-chat', 'Confusingly named', 'active')",
    )
    .bind(other_model)
    .bind(other_provider)
    .execute(&db.pool)
    .await
    .expect("seed a non-deepseek model");
    let other_route = seed_route(&db).await;
    let other_policy = seed_policy(&db, other_route, other_provider, other_model, "active").await;

    apply_migration_0036(&db).await;

    assert_eq!(
        policy_row(&db, other_policy).await,
        (other_model, 1, serde_json::json!({})),
        "0036 is scoped to deepseek providers; a same-named model on another provider is not its \
         business, and a fresh install with no deepseek provider must see no writes at all"
    );

    let provider_id = seed_deepseek_provider(&db).await;
    seed_legacy_models_as_active(&db, provider_id).await;
    let route = seed_route(&db).await;
    let policy = seed_policy(
        &db,
        route,
        provider_id,
        model_id(&db, provider_id, "deepseek-chat").await,
        "active",
    )
    .await;

    apply_migration_0028(&db).await;
    apply_migration_0036(&db).await;
    let first_pass = policy_row(&db, policy).await;

    apply_migration_0036(&db).await;
    let second_pass = policy_row(&db, policy).await;

    assert_eq!(
        first_pass, second_pass,
        "the second run must match no rows at all — same model, same version, same metadata"
    );
    assert_eq!(first_pass.1, 2, "exactly one update, from the first run");
}
