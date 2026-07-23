use moira::{config::DatabaseSettings, infra::db};

fn migration_test_database_url() -> Option<String> {
    match std::env::var("MOIRA_TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => {
            panic!(
                "MOIRA_TEST_DATABASE_URL must be set when CI=true; \
                 refusing to skip the migration contract"
            );
        }
        _ => {
            eprintln!("skipping migration contract: set MOIRA_TEST_DATABASE_URL to run it locally");
            None
        }
    }
}

#[tokio::test]
async fn security_foundation_migration_creates_contract_tables_when_configured() {
    let Some(url) = migration_test_database_url() else {
        return;
    };
    let settings = DatabaseSettings {
        url: Some(url),
        max_connections: 2,
        min_connections: 1,
        connect_timeout_seconds: 5,
        require: true,
    };
    let pool = db::connect(&settings)
        .await
        .expect("connect test database")
        .expect("database required");
    db::migrate(&pool).await.expect("run migrations");

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
    .fetch_all(&pool)
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
    .fetch_all(&pool)
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
    ])
    .fetch_all(&pool)
    .await
    .expect("list response columns");

    assert_eq!(response_columns.len(), 5);
}
