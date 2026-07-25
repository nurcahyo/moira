//! Prompt/content-leak snapshot suite (plan 05, P1-10b).
//!
//! The companion to `tests/secret_leak_snapshots.rs`: that suite guards
//! credential material, this one guards **caller content** — conversation
//! message bodies, memory content, and RAG document text. All three are data the
//! operator's users wrote, and the ways they escape are quieter than a leaked
//! key: an audit-log `metadata` blob that helpfully records "what changed", an
//! error message that echoes the row it failed on, a list endpoint that joins one
//! table too many.
//!
//! Each canary is a single distinctive string created by this run, carrying a
//! per-run `Uuid`, so no assertion here can be satisfied — or falsified — by a
//! row an earlier run left behind.
//!
//! Every "the canary is absent from X" claim is paired with a **positive
//! control** proving the canary was really stored and really is retrievable from
//! the surface that owns it. Without that pairing a suite like this goes green
//! the moment the fixture silently stops writing anything.

mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use support::{CapturedLogs, LifecycleFixture, install_log_capture};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct Driven {
    label: String,
    method: String,
    uri: String,
    status: StatusCode,
    text: String,
    body: Value,
}

async fn drive(
    router: &Router,
    label: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Driven {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", format!("content-leak-{}", Uuid::now_v7()));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(match &body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .expect("build request");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("HTTP response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);

    Driven {
        label: label.to_string(),
        method: method.to_string(),
        uri: uri.to_string(),
        status,
        text,
        body,
    }
}

#[derive(Debug, Clone)]
struct Canary {
    label: String,
    value: String,
}

struct ContentScenario {
    fixture: LifecycleFixture,
    router: Router,
    logs: CapturedLogs,
    run_id: String,
    canaries: Vec<Canary>,
    /// Surfaces that must never carry a canary.
    unrelated: Vec<Driven>,
    /// Errors raised for resources that are *not* the canary-bearing ones.
    foreign_errors: Vec<Driven>,
    /// The endpoints that legitimately own each canary, kept as positive controls.
    owning: Vec<Driven>,
    served_openapi: Value,
    other_conversation_id: String,
    collection_id: String,
}

impl ContentScenario {
    async fn build() -> Option<Self> {
        let logs = install_log_capture();
        let fixture = LifecycleFixture::new().await?;
        let router = moira::build_router(fixture.state.clone()).expect("build the Moira router");
        let run_id = Uuid::now_v7().simple().to_string();
        let application_id = fixture.application_id;

        // ---- policies (the conversation-policy PUT is also the audited mutation)
        let conversation_policy = drive(
            &router,
            "put conversation policy",
            "PUT",
            &format!("/api/v1/admin/applications/{application_id}/conversation-policy"),
            &[],
            Some(json!({
                "conversations_enabled": true,
                "caller_can_create_conversations": true,
                "memory_enabled": true,
            })),
        )
        .await;
        assert_eq!(
            conversation_policy.status,
            StatusCode::OK,
            "conversation policy PUT failed: {}",
            conversation_policy.text
        );

        let memory_policy = drive(
            &router,
            "put memory policy",
            "PUT",
            &format!("/api/v1/admin/applications/{application_id}/memory-policy"),
            &[],
            Some(json!({
                "enabled": true,
                "manual_memory_enabled": true,
                "consent_mode": "application_managed",
                "user_can_list": true,
            })),
        )
        .await;
        assert_eq!(
            memory_policy.status,
            StatusCode::OK,
            "memory policy PUT failed: {}",
            memory_policy.text
        );

        // ---- a caller key with exactly the scopes the canary flows need -------
        let consumer_key = drive(
            &router,
            "create consumer key",
            "POST",
            "/api/v1/admin/consumer-keys",
            &[],
            Some(json!({
                "application_id": application_id,
                "display_name": format!("content-leak-{run_id}"),
                "scopes": [
                    "moira:conversations:create",
                    "moira:conversations:read",
                    "moira:conversations:write",
                    "moira:memories:create",
                    "moira:memories:read",
                ],
                "expires_at": null,
            })),
        )
        .await;
        assert_eq!(
            consumer_key.status,
            StatusCode::CREATED,
            "consumer key creation failed: {}",
            consumer_key.text
        );
        let caller_key = consumer_key.body["secret"]
            .as_str()
            .expect("consumer key secret")
            .to_string();
        let caller: &[(&str, &str)] = &[("x-consumer-key", caller_key.as_str())];

        // ---- the canaries ----------------------------------------------------
        let message_canary =
            format!("CANARY-MESSAGE-{run_id}-patient-reports-severe-chest-pain-at-0300");
        let memory_canary =
            format!("CANARY-MEMORY-{run_id}-user-prefers-the-hebridean-cottage-address");
        let rag_canary =
            format!("CANARY-RAGDOC-{run_id}-internal-margin-on-the-atlas-contract-is-4-percent");
        let canaries = vec![
            Canary {
                label: "conversation message body".to_string(),
                value: message_canary.clone(),
            },
            Canary {
                label: "memory content".to_string(),
                value: memory_canary.clone(),
            },
            Canary {
                label: "RAG document content".to_string(),
                value: rag_canary.clone(),
            },
        ];

        // ---- conversation + canary message -----------------------------------
        let conversation = drive(
            &router,
            "create conversation",
            "POST",
            "/api/v1/conversations",
            caller,
            Some(json!({
                "title": format!("Content leak suite {run_id}"),
                "metadata": { "suite": "content_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            conversation.status,
            StatusCode::CREATED,
            "conversation creation failed: {}",
            conversation.text
        );
        let conversation_id = conversation.body["id"]
            .as_str()
            .expect("conversation id")
            .to_string();

        let message = drive(
            &router,
            "create canary message",
            "POST",
            &format!("/api/v1/conversations/{conversation_id}/messages"),
            caller,
            Some(json!({
                "role": "user",
                "content": message_canary,
                "metadata": { "suite": "content_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            message.status,
            StatusCode::CREATED,
            "message creation failed: {}",
            message.text
        );

        // A second, canary-free conversation: the "different conversation" whose
        // error responses must not reach into the first one's content.
        let other_conversation = drive(
            &router,
            "create unrelated conversation",
            "POST",
            "/api/v1/conversations",
            caller,
            Some(json!({
                "title": format!("Unrelated {run_id}"),
                "metadata": { "suite": "content_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(other_conversation.status, StatusCode::CREATED);
        let other_conversation_id = other_conversation.body["id"]
            .as_str()
            .expect("unrelated conversation id")
            .to_string();

        // ---- memory ----------------------------------------------------------
        let memory = drive(
            &router,
            "create canary memory",
            "POST",
            "/api/v1/memories",
            caller,
            Some(json!({
                "type": "fact",
                "content": memory_canary,
                "metadata": { "suite": "content_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            memory.status,
            StatusCode::CREATED,
            "memory creation failed: {}",
            memory.text
        );
        let memory_id = memory.body["id"].as_str().expect("memory id").to_string();

        // ---- RAG collection + canary document --------------------------------
        let collection = drive(
            &router,
            "create rag collection",
            "POST",
            "/api/v1/admin/rag-collections",
            &[],
            Some(json!({
                "application_id": application_id,
                "external_tenant_id": null,
                "collection_key": format!("content-leak-{run_id}"),
                "display_name": format!("Content leak {run_id}"),
                "description": null,
                "visibility": "application",
                "metadata": { "suite": "content_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            collection.status,
            StatusCode::CREATED,
            "rag collection creation failed: {}",
            collection.text
        );
        let collection_id = collection.body["id"]
            .as_str()
            .expect("collection id")
            .to_string();

        let document = drive(
            &router,
            "create canary rag document",
            "POST",
            &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
            &[],
            Some(json!({
                "external_document_id": null,
                "title": format!("Content leak document {run_id}"),
                "source_type": "direct_text",
                "source_uri": null,
                "mime_type": "text/plain",
                "content": rag_canary,
                "metadata": { "suite": "content_leak_snapshots" },
            })),
        )
        .await;
        assert_eq!(
            document.status,
            StatusCode::CREATED,
            "rag document creation failed: {}",
            document.text
        );
        let document_id = document.body["id"]
            .as_str()
            .expect("document id")
            .to_string();

        // ---- positive controls: the canaries really are stored and readable ---
        let owning = vec![
            drive(
                &router,
                "list canary conversation messages",
                "GET",
                &format!("/api/v1/conversations/{conversation_id}/messages?limit=25"),
                caller,
                None,
            )
            .await,
            drive(
                &router,
                "get canary memory",
                "GET",
                &format!("/api/v1/memories/{memory_id}"),
                caller,
                None,
            )
            .await,
        ];

        // ---- an audited policy mutation, after the canaries exist -------------
        let audited_mutation = drive(
            &router,
            "mutate conversation policy again",
            "PUT",
            &format!("/api/v1/admin/applications/{application_id}/conversation-policy"),
            &[],
            Some(json!({
                "conversations_enabled": true,
                "caller_can_create_conversations": true,
                "summarization_enabled": true,
                "maximum_recent_messages": 7,
            })),
        )
        .await;
        assert_eq!(
            audited_mutation.status,
            StatusCode::OK,
            "the audited conversation-policy mutation failed: {}",
            audited_mutation.text
        );

        // ---- unrelated surfaces ----------------------------------------------
        let mut unrelated = vec![
            conversation_policy,
            memory_policy,
            audited_mutation,
            drive(
                &router,
                "get canary rag document",
                "GET",
                &format!("/api/v1/admin/rag-documents/{document_id}"),
                &[],
                None,
            )
            .await,
            drive(
                &router,
                "list rag documents",
                "GET",
                &format!("/api/v1/admin/rag-collections/{collection_id}/documents?limit=25"),
                &[],
                None,
            )
            .await,
        ];
        for (label, uri) in [
            ("list applications", "/api/v1/admin/applications?limit=25"),
            ("list providers", "/api/v1/admin/providers?limit=25"),
            ("list routes", "/api/v1/admin/routes?limit=25"),
            (
                "list rag collections",
                "/api/v1/admin/rag-collections?limit=25",
            ),
            ("list consumer keys", "/api/v1/admin/consumer-keys?limit=25"),
            ("list system keys", "/api/v1/admin/system-keys?limit=25"),
            ("list audit events", "/api/v1/admin/audit-events?limit=200"),
            ("admin setup status", "/api/v1/admin/setup/status"),
        ] {
            unrelated.push(drive(&router, label, "GET", uri, &[], None).await);
        }
        for (label, uri) in [
            ("public conversation list", "/api/v1/conversations?limit=25"),
            ("public executions", "/api/v1/executions?limit=25"),
            ("public usage", "/api/v1/usage?limit=25"),
            ("public models", "/api/v1/models?limit=25"),
        ] {
            unrelated.push(drive(&router, label, "GET", uri, caller, None).await);
        }

        // `moira:retrieval:diagnose` is a declared scope with **no route** — no
        // retrieval-diagnostics surface exists yet, so the `docs/todo.md:114`
        // item is covered by absence rather than by assertion. This request
        // pins that: the day the surface appears, this 404 flips and whoever
        // adds it has to decide, deliberately, what it may return.
        let retrieval_diagnostics = drive(
            &router,
            "retrieval diagnostics (expected absent)",
            "POST",
            "/api/v1/admin/retrieval/diagnose",
            &[],
            Some(json!({ "application_id": application_id, "query": "canary" })),
        )
        .await;
        unrelated.push(retrieval_diagnostics);

        // ---- errors raised for a *different* conversation ---------------------
        let missing_conversation = format!("conv_{}", Uuid::now_v7());
        let foreign_errors = vec![
            drive(
                &router,
                "404 on an unknown conversation",
                "GET",
                &format!("/api/v1/conversations/{missing_conversation}"),
                caller,
                None,
            )
            .await,
            drive(
                &router,
                "404 on an unknown conversation's messages",
                "GET",
                &format!("/api/v1/conversations/{missing_conversation}/messages?limit=5"),
                caller,
                None,
            )
            .await,
            drive(
                &router,
                "messages of the unrelated conversation",
                "GET",
                &format!("/api/v1/conversations/{other_conversation_id}/messages?limit=25"),
                caller,
                None,
            )
            .await,
            drive(
                &router,
                "400 on a tampered cursor for the unrelated conversation",
                "GET",
                &format!(
                    "/api/v1/conversations/{other_conversation_id}/messages?limit=1&cursor=not-a-cursor-{run_id}"
                ),
                caller,
                None,
            )
            .await,
            drive(
                &router,
                "404 on an unknown memory",
                "GET",
                &format!("/api/v1/memories/mem_{}", Uuid::now_v7()),
                caller,
                None,
            )
            .await,
        ];

        let openapi = drive(
            &router,
            "openapi document",
            "GET",
            "/openapi.json",
            &[],
            None,
        )
        .await;
        assert_eq!(openapi.status, StatusCode::OK);
        let served_openapi = openapi.body.clone();
        unrelated.push(openapi);

        logs.emit_probe(&format!("content-leak-suite-{run_id}"));

        Some(Self {
            fixture,
            router,
            logs,
            run_id,
            canaries,
            unrelated,
            foreign_errors,
            owning,
            served_openapi,
            other_conversation_id,
            collection_id,
        })
    }

    async fn audit_metadata_rows(&self) -> Vec<String> {
        sqlx::query_scalar::<_, String>("select metadata::text from audit_logs")
            .fetch_all(&self.fixture.pool)
            .await
            .expect("read audit_logs.metadata")
    }

    async fn cleanup(self) {
        let _ = sqlx::query("delete from rag_collections where public_id = $1")
            .bind(&self.collection_id)
            .execute(&self.fixture.pool)
            .await;
        drop(self.router);
    }
}

fn assert_canaries_absent(haystack: &str, where_: &str, canaries: &[Canary]) {
    for canary in canaries {
        assert!(
            !haystack.contains(&canary.value),
            "CONTENT LEAK: {where_} contains the {} canary.\n  canary: {}",
            canary.label,
            canary.value
        );
    }
}

#[tokio::test]
async fn conversation_message_canary_never_appears_in_audit_metadata() {
    let Some(scenario) = ContentScenario::build().await else {
        return;
    };

    // Positive control first: the message really is stored and really is
    // readable from the endpoint that owns it.
    let messages = scenario
        .owning
        .iter()
        .find(|driven| driven.label == "list canary conversation messages")
        .expect("the message listing was driven");
    assert_eq!(messages.status, StatusCode::OK, "{}", messages.text);
    let message_canary = &scenario.canaries[0];
    assert!(
        messages.text.contains(&message_canary.value),
        "the canary message is not retrievable from its own conversation — every \
         absence assertion below would be vacuous: {}",
        messages.text
    );

    let rows = scenario.audit_metadata_rows().await;
    assert!(
        rows.len() >= 5,
        "the driven flows must have written audit rows, found {}",
        rows.len()
    );
    // Conversation and message creation are audited, so the rows this test
    // scans definitely include the ones most likely to over-share.
    let combined = rows.join("\n");
    assert_canaries_absent(&combined, "audit_logs.metadata", &scenario.canaries);

    // Pushed into the database as well, so a row this process never read is
    // still covered.
    for canary in &scenario.canaries {
        let hits: i64 = sqlx::query_scalar(
            "select count(*) from audit_logs where metadata::text like '%' || $1 || '%'",
        )
        .bind(&canary.value)
        .fetch_one(&scenario.fixture.pool)
        .await
        .expect("scan audit_logs.metadata");
        assert_eq!(
            hits, 0,
            "CONTENT LEAK: {hits} audit_logs rows carry the {} canary",
            canary.label
        );
    }

    scenario.cleanup().await;
}

#[tokio::test]
async fn memory_and_rag_document_canaries_never_appear_in_unrelated_list_responses() {
    let Some(scenario) = ContentScenario::build().await else {
        return;
    };

    // Positive controls: the memory is readable where it should be, and the RAG
    // content really landed in `rag_document_versions.content_plain` (the RAG
    // read model deliberately does not return document text over HTTP, so the
    // control has to be the row itself).
    let memory = scenario
        .owning
        .iter()
        .find(|driven| driven.label == "get canary memory")
        .expect("the memory read was driven");
    assert_eq!(memory.status, StatusCode::OK, "{}", memory.text);
    assert!(
        memory.text.contains(&scenario.canaries[1].value),
        "the canary memory is not retrievable from its own endpoint: {}",
        memory.text
    );

    let stored_rag: i64 =
        sqlx::query_scalar("select count(*) from rag_document_versions where content_plain = $1")
            .bind(&scenario.canaries[2].value)
            .fetch_one(&scenario.fixture.pool)
            .await
            .expect("read rag_document_versions");
    assert_eq!(
        stored_rag, 1,
        "the canary RAG document content was not stored — the absence assertions \
         below would be vacuous"
    );

    assert!(
        scenario.unrelated.len() >= 18,
        "expected a broad unrelated surface, drove {}",
        scenario.unrelated.len()
    );
    for driven in &scenario.unrelated {
        assert_canaries_absent(
            &driven.text,
            &format!(
                "the response body of `{}` ({} {} -> {})",
                driven.label, driven.method, driven.uri, driven.status
            ),
            &scenario.canaries,
        );
    }

    // The RAG read model in particular: it must describe the document without
    // ever carrying its text.
    let document = scenario
        .unrelated
        .iter()
        .find(|driven| driven.label == "get canary rag document")
        .expect("the rag document read was driven");
    assert_eq!(document.status, StatusCode::OK, "{}", document.text);
    assert!(
        document.body["title"].is_string() && document.body["mime_type"].is_string(),
        "the RAG read must still describe the document: {}",
        document.text
    );
    assert!(
        document.body.get("content").is_none(),
        "the RAG document read model must not carry a content field at all: {}",
        document.text
    );

    scenario.cleanup().await;
}

#[tokio::test]
async fn error_responses_for_a_different_conversation_never_echo_canary_content() {
    let Some(scenario) = ContentScenario::build().await else {
        return;
    };

    assert!(
        scenario.foreign_errors.len() >= 5,
        "expected the full foreign-resource probe set"
    );

    let mut error_statuses = 0;
    for driven in &scenario.foreign_errors {
        assert_canaries_absent(
            &driven.text,
            &format!(
                "the response body of `{}` ({} {} -> {})",
                driven.label, driven.method, driven.uri, driven.status
            ),
            &scenario.canaries,
        );
        if driven.status.is_client_error() {
            error_statuses += 1;
            // The envelope must still be a real, keyed envelope — an error that
            // degrades to a raw string is the usual way internal detail escapes.
            let key = driven.body["error"]["message_key"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "`{}` returned no keyed error envelope: {}",
                        driven.label, driven.text
                    )
                });
            assert!(
                moira::i18n::is_known_key(key),
                "`{}`: message_key {key:?} has no catalog entry",
                driven.label
            );
        }
    }
    assert!(
        error_statuses >= 3,
        "the foreign-resource probes must actually produce errors, saw {error_statuses}"
    );

    // The unrelated conversation's own message listing must be empty — proof
    // that "no canary here" is a property of the data, not of an endpoint that
    // returns nothing to anybody.
    let unrelated_messages = scenario
        .foreign_errors
        .iter()
        .find(|driven| driven.label == "messages of the unrelated conversation")
        .expect("the unrelated conversation listing was driven");
    assert_eq!(unrelated_messages.status, StatusCode::OK);
    assert_eq!(
        unrelated_messages.body["items"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        0,
        "the unrelated conversation {} must have no messages: {}",
        scenario.other_conversation_id,
        unrelated_messages.text
    );

    scenario.cleanup().await;
}

#[tokio::test]
async fn generated_openapi_document_contains_no_prompt_or_message_content() {
    let Some(scenario) = ContentScenario::build().await else {
        return;
    };

    let serialized =
        serde_json::to_string(&scenario.served_openapi).expect("serialise the served document");
    assert!(
        serialized.len() > 10_000,
        "the served OpenAPI document is implausibly small ({} bytes)",
        serialized.len()
    );
    assert!(
        scenario.served_openapi["paths"]["/api/v1/conversations/{id}/messages"].is_object(),
        "the served document must actually describe the conversation-message routes"
    );

    assert_canaries_absent(
        &serialized,
        "the served OpenAPI document",
        &scenario.canaries,
    );
    // A schema description or example must not carry caller text either; the
    // per-run marker is the cheapest way to say "nothing from this run".
    assert!(
        !serialized.contains(&scenario.run_id),
        "CONTENT LEAK: the served OpenAPI document carries this run's marker"
    );

    scenario.cleanup().await;
}

#[tokio::test]
async fn captured_log_output_never_contains_prompt_or_message_content() {
    let Some(scenario) = ContentScenario::build().await else {
        return;
    };

    let captured = scenario.logs.contents();
    assert!(
        captured.contains(&format!("content-leak-suite-{}", scenario.run_id)),
        "the tracing capture is not live — the suite's own probe event is missing \
         from a {}-byte buffer",
        captured.len()
    );

    assert_canaries_absent(&captured, "captured tracing output", &scenario.canaries);

    scenario.cleanup().await;
}

#[tokio::test]
async fn retrieval_diagnostics_do_not_surface_raw_document_text() {
    let Some(scenario) = ContentScenario::build().await else {
        return;
    };

    let probe = scenario
        .unrelated
        .iter()
        .find(|driven| driven.label == "retrieval diagnostics (expected absent)")
        .expect("the retrieval-diagnostics probe was driven");

    // Covered-by-absence, stated explicitly rather than silently dropped: the
    // `moira:retrieval:diagnose` scope exists in `src/security/authz.rs` but no
    // route is registered for it, so there is no retrieval-diagnostics surface
    // to leak from. If this ever stops being a 404/405, the new surface must be
    // brought under the assertion below deliberately.
    assert!(
        probe.status == StatusCode::NOT_FOUND || probe.status == StatusCode::METHOD_NOT_ALLOWED,
        "a retrieval-diagnostics surface now exists ({}); extend this test to assert \
         it never returns raw document text instead of relying on its absence",
        probe.status
    );
    assert_canaries_absent(
        &probe.text,
        "the retrieval-diagnostics probe",
        &scenario.canaries,
    );

    scenario.cleanup().await;
}
