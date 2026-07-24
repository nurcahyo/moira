mod support;

use axum::{Json, Router, http::HeaderMap, routing::get};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use moira::{
    application::{AdminService, ConversationService, PublicExecutionService},
    domain::{
        ConsumerKeyCreateRequest, ConversationMessageQuery, ConversationQuery, ExecutionQuery,
        MemoryPatchRequest, MemoryPolicyPutRequest, MemoryQuery, UsageQuery,
    },
    security::{Actor, ActorType},
};
use serde_json::{Value, json};
use support::{LifecycleFixture, public_response_request, request_context};
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

const TEST_KEY_ID: &str = "public-authorization-test-key";
const TEST_RSA_MODULUS: &str = "r686LSRV-46Cn3oh00Zo43hZNDiHY-Oei0JLSjApgCAD1btVtD2ju5zlGxA97OPjzWAGC0Z8ZqYwmfNwFWLyaC8Sr5-R2ejUuBpH32t8aFf4Z6p1MLUlmXWHviBNVutUzeicKMPWzVQ0xnoktJ6jOxDOkox8JMiNPGbTRAuQ-7poobvKH34738OP8fdaCpPIabtTfvz5gI11PYTLDlwrDWje3smeonXuxwj1lChvv5m08J7BsK4Jvb_YaUq0kCuQbpjFApOaTc_cY-xYrWVRcv9aprKEsJQvBm8xdDiAukfybT-GE3vFOMjmrWqVcPd46mYL0cr_VdxScWum5S1rcQ";
const TEST_RSA_PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCvrzotJFX7joKf
eiHTRmjjeFk0OIdj456LQktKMCmAIAPVu1W0PaO7nOUbED3s4+PNYAYLRnxmpjCZ
83AVYvJoLxKvn5HZ6NS4Gkffa3xoV/hnqnUwtSWZdYe+IE1W61TN6Jwow9bNVDTG
eiS0nqM7EM6SjHwkyI08ZtNEC5D7umihu8offjvfw4/x91oKk8hpu1N+/PmAjXU9
hMsOXCsNaN7eyZ6ide7HCPWUKG+/mbTwnsGwrgm9v9hpSrSQK5BumMUCk5pNz9xj
7FitZVFy/1qmsoSwlC8GbzF0OIC6R/JtP4YTe8U4yOatapVw93jqZgvRyv9V3FJx
a6blLWtxAgMBAAECggEAFx+nNp3bu1qMktUOcrKHx7jldNwj5d/l1EqLgl5IeBa+
qnkX1LtwO5dxCFjg7bcpGrUS1pUWdqRVLU4/aHE3msLnYLpOBjKBHSJIZ33MSCec
CHkFJ74QDtzLWxkBVPlwlhGRzEPKmAgHUkBtaGCg93tE1UEsbeL/w/18vS4QjTFJ
bK+3O8vkDqdYQAJInbjURhcv7OQIF848CEwkmI/s5boSfOV3nTCRHd0cnCAuEjGv
/y0gikfzmDdBY+SK/tF41ctFuR+WU1xcR1PoLj87rKS9Nm5GkQeDuzO5JbgGqgIe
kFI41mqVcs1MK2sx63yHj1ngNF6B0PEgspKIpjQ6iQKBgQD3KZ16wzJkF4fakFcS
dFP3eoxXvAVgrODJx3IQpBrLcG+5pJTwFRKwPPdYvd6hqa+MUhEqYlhJNw6G89x2
bWy8Y7Cqjy92Oa95zTFCdJ/Fmd2Fhkx7Dhnnztn66Se8NbUl/LWnTLPspeVT2+XR
9DtSiB+Mugv0BCIBD2uAglQ3rwKBgQC191e1hpVUSzBnd4MtStMyIZegaUhKireJ
YBs/tNmEe7SVYWG3rGzZzOVwKkC3wcF+mYisRqncxjyySu8i6RjwrHgN+W+sF9h0
/4wIU/lOenKtzG0DER0gfwDuzI8fvwPpv4RvOop5+r0kRwB64BUm6+/4K6snlrrs
Em2BeY423wKBgDbKQt6z5rfJf5Qz6xlsMDDsObA5PffwWuRgEikeN9JhWmMM2Pdf
tITc/vftHy03MHMqviNnKasRSWchJ/4Yw8H/V2p3002h/AREOGdC8ygas8ClxM6C
kbuRX0D/7o8KWN3S53HuzvPm0q+ET637NitVgajwlTXCtMcHZA1Y1tKBAoGAbApw
CVffUi1SkBxlxn6m5x0K6jOYuKmkT+zAQRMgE4lfr1IisuuttaPylqZ/xptER+bh
P2i1cmBBqZrUYeYE6OF+Zs2zgHqoCs+wVUGGxRHvBUJbd3ax1JmT9DWAxVik+iS8
fU5E6if2JZQCtPJXnMR5tuA2v0q/sWs/maCS0AECgYEA9DjpdIMB+efwyqWiBGPe
KyAcIS0RU0PHejdULbZoW20yC4qRTwkRdUKVICbK7ubtzB8jy5HLBR2IsGPjQKyh
5JtiEST48mPRj2FLFCb5pW+S1Sxl0+kb2094nmbOZzZU0FmtqOlopBPD3RCv2twO
8PBnvQBPRWjhbQGhwavb5Lw=
-----END PRIVATE KEY-----"#;

const PUBLIC_READ_SCOPES: &[&str] = &[
    "moira:responses:read",
    "moira:executions:read",
    "moira:usage:read",
    "moira:models:read",
    "moira:routes:read",
    "moira:conversations:read",
    "moira:memories:read",
    "moira:memories:write",
    "moira:memories:delete",
];

#[derive(Debug)]
struct SeededResources {
    response_id: Uuid,
    execution_id: Uuid,
    model_id: Uuid,
    route_id: Uuid,
    conversation_id: String,
    message_id: String,
    memory_id: String,
}

struct TestIssuer {
    issuer: String,
    task: JoinHandle<()>,
}

impl TestIssuer {
    async fn start(fixture: &LifecycleFixture) -> Self {
        let jwks = json!({
            "keys": [{
                "kty": "RSA",
                "kid": TEST_KEY_ID,
                "use": "sig",
                "alg": "RS256",
                "n": TEST_RSA_MODULUS,
                "e": "AQAB"
            }]
        });
        let app = Router::new().route(
            "/jwks",
            get({
                let jwks = jwks.clone();
                move || {
                    let jwks = jwks.clone();
                    async move { Json(jwks) }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test JWKS server");
        let address = listener.local_addr().expect("test JWKS address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test JWKS");
        });
        let issuer = format!("https://issuer.test/{}", Uuid::now_v7());
        sqlx::query(
            r#"
            insert into trusted_jwt_issuers (
                id, issuer, jwks_url, expected_audiences, allowed_algorithms,
                subject_claim, application_id_claim, scopes_claim
            )
            values ($1, $2, $3, '{}', array['RS256'], 'sub', 'application_id', 'scope')
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(&issuer)
        .bind(format!("http://{address}/jwks"))
        .execute(&fixture.pool)
        .await
        .expect("insert trusted JWT issuer");
        Self { issuer, task }
    }

    fn token(&self, application_id: Option<&str>, scopes: &[&str]) -> String {
        let mut claims = json!({
            "iss": self.issuer,
            "sub": format!("jwt-user-{}", Uuid::now_v7()),
            "scope": scopes.join(" "),
            "exp": chrono::Utc::now().timestamp() + 3600
        });
        if let Some(application_id) = application_id {
            claims["application_id"] = Value::String(application_id.to_string());
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KEY_ID.to_string());
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY.as_bytes())
                .expect("parse test RSA private key"),
        )
        .expect("sign test JWT")
    }

    fn bearer_headers(&self, application_id: Option<&str>, scopes: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", self.token(application_id, scopes))
                .parse()
                .expect("JWT authorization header"),
        );
        headers
    }
}

impl Drop for TestIssuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn trusted_jwt_requires_a_valid_application_binding_for_public_reads() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = TestIssuer::start(&fixture).await;
    let public = PublicExecutionService::new(&fixture.state).expect("public execution service");

    let unbound = fixture
        .state
        .auth
        .authenticate_caller(
            &fixture.pool,
            &issuer.bearer_headers(
                None,
                &[
                    "moira:responses:create",
                    "moira:responses:stream",
                    "moira:executions:read",
                    "moira:models:read",
                    "moira:routes:read",
                    "moira:capabilities:read",
                    "moira:conversations:read",
                    "moira:memories:read",
                    "moira:memories:write",
                    "moira:memories:delete",
                ],
            ),
        )
        .await
        .expect("authenticate otherwise valid unbound JWT");
    assert_eq!(unbound.actor_type, ActorType::TrustedJwt);
    assert!(
        public
            .list_executions(&unbound, &ExecutionQuery::default())
            .await
            .is_err(),
        "an unbound TrustedJwt must not acquire wildcard public access"
    );
    assert!(
        public
            .create_response(
                &unbound,
                &request_context(),
                public_response_request("unbound-route"),
            )
            .await
            .is_err()
    );
    assert!(
        public
            .stream_response(
                unbound.clone(),
                request_context(),
                public_response_request("unbound-route"),
            )
            .await
            .is_err()
    );
    assert!(public.list_models(&unbound).await.is_err());
    assert!(public.list_routes(&unbound).await.is_err());
    assert!(public.capabilities(&unbound).await.is_err());

    let resources = seed_public_resources(&fixture, fixture.application_id, "unbound-target").await;
    let conversations = ConversationService::new(&fixture.state).expect("conversation service");
    assert!(
        conversations
            .list_conversations(&unbound, &ConversationQuery::default())
            .await
            .is_err()
    );
    assert!(
        conversations
            .list_messages(
                &unbound,
                &resources.conversation_id,
                &ConversationMessageQuery::default(),
            )
            .await
            .is_err()
    );
    assert!(
        conversations
            .get_memory(&unbound, &resources.memory_id)
            .await
            .is_err()
    );
    assert!(
        conversations
            .list_memories(&unbound, &MemoryQuery::default())
            .await
            .is_err()
    );
    assert!(
        conversations
            .patch_memory(
                &unbound,
                &request_context(),
                &resources.memory_id,
                MemoryPatchRequest {
                    content: Some("must not cross application boundaries".to_string()),
                    ..MemoryPatchRequest::default()
                },
            )
            .await
            .is_err()
    );
    assert!(
        conversations
            .delete_memory(&unbound, &request_context(), &resources.memory_id)
            .await
            .is_err()
    );

    let malformed = fixture
        .state
        .auth
        .authenticate_caller(
            &fixture.pool,
            &issuer.bearer_headers(Some("not-a-uuid"), &["moira:executions:read"]),
        )
        .await;
    assert!(
        malformed.is_err(),
        "a configured application UUID claim must reject malformed values"
    );
}

#[tokio::test]
async fn application_bound_actor_cannot_read_another_app_public_resources() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let application_b = create_application(&fixture, "application-b").await;
    let own = seed_public_resources(&fixture, fixture.application_id, "application-a").await;
    let other = seed_public_resources(&fixture, application_b, "application-b").await;
    let actor = application_actor(fixture.application_id, PUBLIC_READ_SCOPES);
    let public = PublicExecutionService::new(&fixture.state).expect("public execution service");
    let conversations = ConversationService::new(&fixture.state).expect("conversation service");
    conversations
        .put_memory_policy(
            &fixture.actor,
            &request_context(),
            fixture.application_id,
            MemoryPolicyPutRequest {
                enabled: Some(true),
                user_can_list: Some(true),
                user_can_edit: Some(true),
                user_can_delete: Some(true),
                ..MemoryPolicyPutRequest::default()
            },
        )
        .await
        .expect("enable memory policy for authorization test");

    assert_eq!(
        public
            .get_response(&actor, &request_context(), own.response_id)
            .await
            .expect("read own response")
            .id,
        format!("resp_{}", own.response_id)
    );
    assert!(
        public
            .get_response(&actor, &request_context(), other.response_id)
            .await
            .is_err()
    );

    assert_eq!(
        public
            .get_execution(&actor, &request_context(), own.execution_id)
            .await
            .expect("read own execution")
            .execution_id,
        format!("exec_{}", own.execution_id)
    );
    assert!(
        public
            .get_execution(&actor, &request_context(), other.execution_id)
            .await
            .is_err()
    );

    let usage = public
        .list_usage(&actor, &request_context(), &UsageQuery::default())
        .await
        .expect("list application usage");
    assert!(
        usage
            .data
            .iter()
            .any(|item| { item.execution_id == format!("exec_{}", own.execution_id) })
    );
    assert!(
        !usage
            .data
            .iter()
            .any(|item| { item.execution_id == format!("exec_{}", other.execution_id) })
    );

    let models = public
        .list_models(&actor)
        .await
        .expect("list application models");
    assert!(models.data.iter().any(|item| item.id == own.model_id));
    assert!(!models.data.iter().any(|item| item.id == other.model_id));

    let routes = public
        .list_routes(&actor)
        .await
        .expect("list application routes");
    assert!(routes.data.iter().any(|item| item.id == own.route_id));
    assert!(!routes.data.iter().any(|item| item.id == other.route_id));

    assert_eq!(
        conversations
            .get_conversation(&actor, &request_context(), &own.conversation_id)
            .await
            .expect("read own conversation")
            .id,
        own.conversation_id
    );
    assert!(
        conversations
            .get_conversation(&actor, &request_context(), &other.conversation_id)
            .await
            .is_err()
    );
    let conversation_list = conversations
        .list_conversations(&actor, &ConversationQuery::default())
        .await
        .expect("list application conversations");
    assert!(
        conversation_list
            .data
            .iter()
            .any(|item| item.id == own.conversation_id)
    );
    assert!(
        !conversation_list
            .data
            .iter()
            .any(|item| item.id == other.conversation_id)
    );
    let own_messages = conversations
        .list_messages(
            &actor,
            &own.conversation_id,
            &ConversationMessageQuery::default(),
        )
        .await
        .expect("list own conversation messages");
    assert!(
        own_messages
            .data
            .iter()
            .any(|item| item.id == own.message_id)
    );
    assert!(
        conversations
            .list_messages(
                &actor,
                &other.conversation_id,
                &ConversationMessageQuery::default(),
            )
            .await
            .is_err()
    );

    assert_eq!(
        conversations
            .get_memory(&actor, &own.memory_id)
            .await
            .expect("read own memory")
            .id,
        own.memory_id
    );
    assert!(
        conversations
            .get_memory(&actor, &other.memory_id)
            .await
            .is_err()
    );
    let memories = conversations
        .list_memories(&actor, &MemoryQuery::default())
        .await
        .expect("list application memories");
    assert!(memories.data.iter().any(|item| item.id == own.memory_id));
    assert!(!memories.data.iter().any(|item| item.id == other.memory_id));
    assert!(
        conversations
            .patch_memory(
                &actor,
                &request_context(),
                &other.memory_id,
                MemoryPatchRequest {
                    content: Some("cross-application overwrite".to_string()),
                    ..MemoryPatchRequest::default()
                },
            )
            .await
            .is_err()
    );
    assert!(
        conversations
            .delete_memory(&actor, &request_context(), &other.memory_id)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from memory_records where public_id = $1",)
            .bind(&other.memory_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("verify other application memory remains active"),
        "active"
    );
}

#[tokio::test]
async fn consumer_key_and_jwt_constraints_intersect_instead_of_widening_access() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let issuer = TestIssuer::start(&fixture).await;
    let key = AdminService::new(&fixture.state)
        .expect("admin service")
        .create_consumer_key(
            &fixture.actor,
            &request_context(),
            ConsumerKeyCreateRequest {
                application_id: fixture.application_id,
                display_name: "JWT intersection test".to_string(),
                scopes: vec![
                    "moira:models:read".to_string(),
                    "moira:routes:read".to_string(),
                ],
                expires_at: None,
            },
        )
        .await
        .expect("create consumer key")
        .secret
        .expect("consumer key plaintext");
    let application_id = fixture.application_id.to_string();
    let mut headers = issuer.bearer_headers(Some(&application_id), &["moira:models:read"]);
    headers.insert("x-consumer-key", key.parse().expect("consumer key header"));

    let combined = fixture
        .state
        .auth
        .authenticate_caller(&fixture.pool, &headers)
        .await
        .expect("authenticate consumer key and JWT");
    assert_eq!(combined.actor_type, ActorType::ConsumerKey);
    assert_eq!(
        combined.internal_application_id,
        Some(fixture.application_id)
    );
    assert_eq!(combined.scopes, vec!["moira:models:read".to_string()]);
    assert!(
        fixture
            .state
            .authz
            .require(&combined, "moira:models:read")
            .is_ok()
    );
    assert!(
        fixture
            .state
            .authz
            .require(&combined, "moira:routes:read")
            .is_err(),
        "the consumer key must not add a scope absent from the JWT"
    );

    let other_application = Uuid::now_v7().to_string();
    let mut conflicting = issuer.bearer_headers(Some(&other_application), &["moira:models:read"]);
    conflicting.insert("x-consumer-key", key.parse().expect("consumer key header"));
    assert!(
        fixture
            .state
            .auth
            .authenticate_caller(&fixture.pool, &conflicting)
            .await
            .is_err(),
        "a JWT for another application must not widen the consumer key binding"
    );
}

fn application_actor(application_id: Uuid, scopes: &[&str]) -> Actor {
    Actor {
        actor_type: ActorType::TrustedJwt,
        application_id: Some(application_id.to_string()),
        external_user_id: None,
        internal_application_id: Some(application_id),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        ..Actor::default()
    }
}

async fn create_application(fixture: &LifecycleFixture, label: &str) -> Uuid {
    let id = Uuid::now_v7();
    let suffix = id.simple();
    sqlx::query(
        r#"
        insert into applications (
            id, external_application_id, application_slug, display_name, metadata
        )
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(format!("authz-{label}-{suffix}"))
    .bind(format!("authz-{label}-{suffix}"))
    .bind(format!("Authorization {label}"))
    .bind(json!({ "test_fixture": true }))
    .execute(&fixture.pool)
    .await
    .expect("insert authorization test application");
    id
}

async fn seed_public_resources(
    fixture: &LifecycleFixture,
    application_id: Uuid,
    label: &str,
) -> SeededResources {
    let provider_id = Uuid::now_v7();
    let model_id = Uuid::now_v7();
    let route_id = Uuid::now_v7();
    let response_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let conversation_uuid = Uuid::now_v7();
    let conversation_id = format!("conv_{conversation_uuid}");
    let message_uuid = Uuid::now_v7();
    let message_id = format!("msg_{message_uuid}");
    let memory_uuid = Uuid::now_v7();
    let memory_id = format!("mem_{memory_uuid}");
    let suffix = Uuid::now_v7().simple().to_string();

    let mut tx = fixture
        .pool
        .begin()
        .await
        .expect("begin authorization seed");
    sqlx::query(
        r#"
        insert into providers (id, provider_type, display_name, metadata)
        values ($1, 'openai_compatible', $2, $3)
        "#,
    )
    .bind(provider_id)
    .bind(format!("Authorization provider {label}"))
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert provider");
    sqlx::query(
        r#"
        insert into provider_models (
            id, provider_id, model_key, display_name, capabilities
        )
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(model_id)
    .bind(provider_id)
    .bind(format!("authz-model-{suffix}"))
    .bind(format!("Authorization model {label}"))
    .bind(json!({ "text": true, "streaming": true }))
    .execute(&mut *tx)
    .await
    .expect("insert provider model");
    sqlx::query(
        r#"
        insert into route_definitions (
            id, route_key, display_name, selection_strategy, metadata
        )
        values ($1, $2, $3, 'default', $4)
        "#,
    )
    .bind(route_id)
    .bind(format!("authz-route-{suffix}"))
    .bind(format!("Authorization route {label}"))
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert route");
    sqlx::query(
        r#"
        insert into routing_policies (
            id, application_id, route_id, provider_id, provider_model_id,
            priority, weight, metadata
        )
        values ($1, $2, $3, $4, $5, 100, 1, $6)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(application_id)
    .bind(route_id)
    .bind(provider_id)
    .bind(model_id)
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert routing policy");
    sqlx::query(
        r#"
        insert into responses (
            id, execution_id, request_id, application_id, status, route_id,
            provider_id, provider_model_id, completed_at, metadata
        )
        values ($1, $2, $3, $4, 'completed', $5, $6, $7, now(), $8)
        "#,
    )
    .bind(response_id)
    .bind(execution_id)
    .bind(format!("authz-request-{suffix}"))
    .bind(application_id)
    .bind(route_id)
    .bind(provider_id)
    .bind(model_id)
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert response");
    sqlx::query(
        r#"
        insert into usage_records (
            id, request_id, execution_id, application_id, provider_id,
            provider_model_id, input_tokens, output_tokens, total_tokens, metadata
        )
        values ($1, $2, $3, $4, $5, $6, 2, 3, 5, $7)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(format!("authz-request-{suffix}"))
    .bind(execution_id)
    .bind(application_id)
    .bind(provider_id)
    .bind(model_id)
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert usage record");
    sqlx::query(
        r#"
        insert into conversations (
            id, public_id, application_id, title, metadata
        )
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(conversation_uuid)
    .bind(&conversation_id)
    .bind(application_id)
    .bind(format!("Authorization conversation {label}"))
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert conversation");
    sqlx::query(
        r#"
        insert into conversation_messages (
            id, public_id, conversation_id, role, message_type, sequence_number,
            content_plain, content_hash, content_size_bytes, metadata
        )
        values ($1, $2, $3, 'user', 'input', 1, $4, $5, $6, $7)
        "#,
    )
    .bind(message_uuid)
    .bind(&message_id)
    .bind(conversation_uuid)
    .bind(format!("Authorization message {label}"))
    .bind(format!("message-hash-{suffix}"))
    .bind(label.len() as i64)
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert conversation message");
    sqlx::query(
        r#"
        insert into memory_records (
            id, public_id, application_id, memory_scope, memory_type,
            content_plain, content_hash, metadata
        )
        values ($1, $2, $3, 'application', 'fact', $4, $5, $6)
        "#,
    )
    .bind(memory_uuid)
    .bind(&memory_id)
    .bind(application_id)
    .bind(format!("Authorization memory {label}"))
    .bind(format!("hash-{suffix}"))
    .bind(json!({ "test_fixture": true }))
    .execute(&mut *tx)
    .await
    .expect("insert memory");
    tx.commit().await.expect("commit authorization seed");

    SeededResources {
        response_id,
        execution_id,
        model_id,
        route_id,
        conversation_id,
        message_id,
        memory_id,
    }
}
