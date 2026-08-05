use std::{future::Future, time::Duration};

use moira::{config::DatabaseSettings, infra::db};
use sqlx::PgPool;
use tokio::time::timeout;
use url::Url;
use uuid::Uuid;

const DATABASE_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const MIGRATION_CONTRACT_TIMEOUT: Duration = Duration::from_secs(30);

/// The one deliberately-named way to run with no database. Same spelling and semantics as
/// `tests/support/mod.rs::ALLOW_NO_DATABASE`; kept as a literal here rather than imported
/// because this suite deliberately does not pull in the fixture harness (see
/// `tests/test_database_isolation.rs` — it is one of exactly two files allowed to resolve
/// the database URL itself).
const ALLOW_NO_DATABASE: &str = "MOIRA_TEST_ALLOW_NO_DATABASE";

/// A database this suite may migrate **from nothing** and mutate, without needing
/// `CREATEDB`.
///
/// # Why a second variable rather than reusing the first
///
/// The contract this suite asserts is the migration set itself, so it cannot clone an
/// already-migrated template — it has to start from an empty database. Until now the only
/// way to get one was `CREATE DATABASE`, which made the whole gate unavailable to any CI
/// whose PostgreSQL role is not an administrator (issue #77, `docs/todo.md` #75). A
/// restricted runner can instead provision one empty database out of band — the same shape
/// GitHub Actions' `POSTGRES_DB` already gives this repository's `rust-migrations` job —
/// and point this variable at it.
///
/// It is a **separate** variable on purpose. `MOIRA_TEST_DATABASE_URL` is used by every
/// other suite as a *cluster* to create private databases on, and its database component is
/// never written to; this one names a database that will be migrated and then deliberately
/// damaged (`alter table responses drop column updated_at`). Overloading one variable with
/// both meanings is how someone eventually points the destructive one at a database that
/// matters. [`in_place_migration_database`] refuses to run against anything non-empty for
/// the same reason.
const IN_PLACE_MIGRATION_DATABASE_URL: &str = "MOIRA_TEST_MIGRATION_DATABASE_URL";

/// Whether the no-database opt-out is in force. `CI=true` overrides it: a CI run that
/// cannot reach a database has a broken service container.
fn no_database_opt_out_is_set() -> bool {
    if std::env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) {
        return false;
    }
    std::env::var(ALLOW_NO_DATABASE)
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"))
}

/// The cluster on which this suite creates its isolated migration database.
///
/// **Fail-closed by default, not only under `CI=true`.** It used to print one skip line and
/// return `None`, and the migration contract — the gate that proves Moira's schema can be
/// built from `0001` — then reported success while asserting nothing. That is issue #77
/// problem 1: `scripts/gates.sh` greps for the skip line, but a guarantee that lives in a
/// downstream shell script is not a guarantee the suite makes.
fn migration_test_database_url() -> Option<String> {
    match std::env::var("MOIRA_TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if no_database_opt_out_is_set() => {
            // Written to the process's stderr, not through `eprintln!`: `libtest` captures
            // a passing test's output and prints it nowhere, so an `eprintln!` skip line is
            // invisible to `scripts/gates.sh`'s zero-skip assertion. Measured on this
            // branch — a redirected `cargo test` capture held zero occurrences of
            // `skipping` while every test reported `ok`.
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "skipping migration contract: MOIRA_TEST_DATABASE_URL is not set and \
                 {ALLOW_NO_DATABASE} is in force"
            );
            None
        }
        _ => panic!(
            "MOIRA_TEST_DATABASE_URL is not set (or is empty).\n\n\
             The migration contract refuses to skip: it is the gate that proves Moira's \
             schema can be built from 0001, and a skipped gate reports success while \
             asserting nothing.\n\n\
             Either set MOIRA_TEST_DATABASE_URL to a cluster whose role has CREATEDB:\n\n    \
             export MOIRA_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/moira\n\n\
             or, on a runner without create-database rights, provision one empty, disposable \
             database and set {IN_PLACE_MIGRATION_DATABASE_URL} to it — this suite will \
             migrate and mutate that database in place.\n\n\
             If you genuinely mean to run with no database at all, set {ALLOW_NO_DATABASE}=1. \
             It is ignored when CI=true."
        ),
    }
}

/// The restricted-CI path: a pre-provisioned empty database to migrate in place.
///
/// Returns `None` when the variable is unset, which puts the suite back on the
/// create-and-drop path. Panics when the variable is set but the database is **not** empty:
/// this suite drops a column and re-applies a migration, so running it against a populated
/// database would destroy data and, worse, would still pass.
async fn in_place_migration_database() -> Option<String> {
    let url = std::env::var(IN_PLACE_MIGRATION_DATABASE_URL)
        .ok()
        .filter(|url| !url.trim().is_empty())?;
    let pool = connect_test_database(&url).await;
    let existing: i64 = timeout(
        DATABASE_OPERATION_TIMEOUT,
        sqlx::query_scalar(
            "select count(*) from information_schema.tables where table_schema = 'public'",
        )
        .fetch_one(&pool),
    )
    .await
    .expect("inspect the in-place migration database timed out")
    .expect("inspect the in-place migration database");
    pool.close().await;
    assert_eq!(
        existing, 0,
        "{IN_PLACE_MIGRATION_DATABASE_URL} names a database whose public schema already \
         holds {existing} tables. This suite migrates from nothing and then deliberately \
         damages the schema, so it must be given an empty, disposable database — provision \
         a fresh one per run (a container's POSTGRES_DB is the intended shape)."
    );
    Some(url)
}

#[tokio::test]
async fn security_foundation_migration_creates_contract_tables_when_configured() {
    // The restricted-CI path first: when an empty database has been provisioned out of
    // band there is nothing to create and nothing to drop, and the contract is asserted
    // over the identical `run_migration_contract` code path.
    if let Some(url) = in_place_migration_database().await {
        run_migration_contract(url).await;
        return;
    }
    let Some(admin_url) = migration_test_database_url() else {
        return;
    };
    let admin_pool = connect_test_database(&admin_url).await;
    let database_name = format!("moira_migration_{}", Uuid::now_v7().simple());
    let isolated_url = database_url_with_name(&admin_url, &database_name);
    run_database_operation(
        sqlx::query(&format!(r#"create database "{database_name}""#)).execute(&admin_pool),
        "create isolated migration contract database",
    )
    .await;

    let mut contract_task = tokio::spawn(async move { run_migration_contract(isolated_url).await });
    let contract_result = match timeout(MIGRATION_CONTRACT_TIMEOUT, &mut contract_task).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("migration contract task failed: {error}")),
        Err(_) => {
            contract_task.abort();
            let _ = timeout(DATABASE_OPERATION_TIMEOUT, contract_task).await;
            Err("migration contract task timed out".to_string())
        }
    };

    let cleanup_result = timeout(
        DATABASE_OPERATION_TIMEOUT,
        sqlx::query(&format!(
            r#"drop database if exists "{database_name}" with (force)"#
        ))
        .execute(&admin_pool),
    )
    .await;
    let close_result = timeout(DATABASE_OPERATION_TIMEOUT, admin_pool.close()).await;

    match cleanup_result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => panic!(
            "drop isolated migration contract database {database_name}: {error}; \
             contract task result: {contract_result:?}"
        ),
        Err(_) => panic!(
            "timed out dropping isolated migration contract database {database_name}; \
             contract task result: {contract_result:?}"
        ),
    }
    assert!(
        close_result.is_ok(),
        "timed out closing migration contract administrative pool"
    );
    contract_result.unwrap_or_else(|error| panic!("{error}"));
}

async fn run_migration_contract(url: String) {
    let pool = connect_test_database(&url).await;
    db::migrate(&pool).await.expect("run migrations");

    verify_migration_contract(&pool).await;
    pool.close().await;
}

async fn verify_migration_contract(pool: &PgPool) {
    let rows: Vec<(String,)> = sqlx::query_as(
        r#"
        select table_name
        from information_schema.tables
        where table_schema = 'public'
          and table_name = any($1)
        order by table_name
        "#,
    )
    .bind(vec![
        "applications",
        "providers",
        "provider_models",
        "provider_credentials",
        "trusted_jwt_issuers",
        "system_api_keys",
        "consumer_api_keys",
        "audit_logs",
        "idempotency_records",
        "route_definitions",
        "routing_policies",
        "agent_profiles",
        "provider_runtime_policies",
        "execution_attempts",
        "usage_records",
        "provider_health_snapshots",
        "application_execution_policies",
        "responses",
        "application_conversation_policies",
        "application_memory_policies",
        "application_retrieval_policies",
        "application_embedding_policies",
        "conversations",
        "conversation_messages",
        "conversation_summaries",
        "memory_records",
        "memory_embeddings",
        "memory_extraction_runs",
        "rag_collections",
        "rag_documents",
        "rag_document_versions",
        "rag_chunks",
        "rag_chunk_embeddings",
        "rag_ingestion_runs",
        "context_plans",
        "retrieval_runs",
    ])
    .fetch_all(pool)
    .await
    .expect("list tables");

    assert_eq!(rows.len(), 36);

    let versioned_columns: Vec<(String,)> = sqlx::query_as(
        r#"
        select table_name
        from information_schema.columns
        where table_schema = 'public'
          and column_name = 'version'
          and table_name = any($1)
        order by table_name
        "#,
    )
    .bind(vec![
        "applications",
        "providers",
        "provider_models",
        "provider_credentials",
        "trusted_jwt_issuers",
        "route_definitions",
        "routing_policies",
        "agent_profiles",
        "provider_runtime_policies",
        "application_execution_policies",
        "responses",
        "application_conversation_policies",
        "application_memory_policies",
        "application_retrieval_policies",
        "application_embedding_policies",
        "conversations",
        "memory_records",
        "rag_collections",
        "rag_documents",
    ])
    .fetch_all(pool)
    .await
    .expect("list version columns");

    assert_eq!(versioned_columns.len(), 19);

    let response_columns: Vec<(String,)> = sqlx::query_as(
        r#"
        select column_name
        from information_schema.columns
        where table_schema = 'public'
          and table_name = 'responses'
          and column_name = any($1)
        order by column_name
        "#,
    )
    .bind(vec![
        "execution_id",
        "output_persisted",
        "output_summary",
        "usage_summary",
        "conversation_id",
        "updated_at",
    ])
    .fetch_all(pool)
    .await
    .expect("list response columns");

    assert_eq!(response_columns.len(), 6);

    sqlx::query("alter table responses drop column updated_at")
        .execute(pool)
        .await
        .expect("simulate pre-0008 responses schema");
    let response_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let initial_version = sqlx::query_scalar::<_, i64>(
        r#"
        insert into responses (id, execution_id, request_id, status)
        values ($1, $2, $3, 'in_progress')
        returning version
        "#,
    )
    .bind(response_id)
    .bind(execution_id)
    .bind(format!("migration-contract-{execution_id}"))
    .fetch_one(pool)
    .await
    .expect("insert response migration contract row");
    sqlx::query(include_str!("../migrations/0008_response_updated_at.sql"))
        .execute(pool)
        .await
        .expect("apply response updated_at upgrade migration");
    let updated_at_backfilled =
        sqlx::query_scalar::<_, bool>("select updated_at is not null from responses where id = $1")
            .bind(response_id)
            .fetch_one(pool)
            .await
            .expect("verify response updated_at backfill");
    let terminal_version = sqlx::query_scalar::<_, i64>(
        r#"
        update responses
        set status = 'cancelled', cancelled_at = now()
        where id = $1
        returning version
        "#,
    )
    .bind(response_id)
    .fetch_one(pool)
    .await
    .expect("terminalize response migration contract row");

    assert_eq!(initial_version, 1);
    assert!(updated_at_backfilled);
    assert_eq!(terminal_version, 2);
}

async fn connect_test_database(url: &str) -> PgPool {
    let settings = DatabaseSettings {
        url: Some(url.to_string()),
        max_connections: 2,
        min_connections: 1,
        connect_timeout_seconds: 5,
        require: true,
        migrate_on_startup: false,
    };
    timeout(DATABASE_OPERATION_TIMEOUT, db::connect(&settings))
        .await
        .expect("connect test database timed out")
        .expect("connect test database")
        .expect("database required")
}

fn database_url_with_name(admin_url: &str, database_name: &str) -> String {
    let mut url = Url::parse(admin_url).expect("MOIRA_TEST_DATABASE_URL must be a valid URL");
    url.set_path(&format!("/{database_name}"));
    url.to_string()
}

async fn run_database_operation<'a>(
    operation: impl Future<Output = Result<sqlx::postgres::PgQueryResult, sqlx::Error>> + 'a,
    description: &str,
) {
    timeout(DATABASE_OPERATION_TIMEOUT, operation)
        .await
        .unwrap_or_else(|_| panic!("{description} timed out"))
        .unwrap_or_else(|error| panic!("{description}: {error}"));
}
