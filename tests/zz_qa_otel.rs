//! TEMPORARY QA PROBE — delete after review.
//! Stands up a local OTLP/HTTP collector and inspects exactly what leaves the process.

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::Request;
use moira::{
    app::AppState,
    config::{Settings, telemetry},
};
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_otel_export_payload() {
    let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();

    let app = axum::Router::new().route(
        "/v1/traces",
        axum::routing::post(move |body: Bytes| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().extend_from_slice(&body);
                eprintln!("COLLECTOR received {} bytes", body.len());
                axum::http::StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // ---- 1. shipped default filter (moira=info) -------------------------
    let mut settings = Settings::default();
    settings.telemetry.otel_enabled = true;
    settings.telemetry.otel_endpoint = Some(format!("http://{addr}"));
    settings.telemetry.service_name = "moira-qa-probe".to_string();
    if std::env::var("QA_WIDE_FILTER").is_ok() {
        settings.telemetry.env_filter = "moira=debug,tower_http=debug".to_string();
    }
    eprintln!("env_filter = {:?}", settings.telemetry.env_filter);

    let guard = telemetry::init(&settings.telemetry).expect("telemetry init");
    assert!(guard.otel_enabled());

    let state = AppState::new(Settings::default(), None).unwrap();
    let router = moira::build_router(state).unwrap();

    // Drive traffic carrying secrets in every caller-controllable position.
    for (uri, hdr) in [
        (
            "/api/v1/admin/applications/018f6b1e-0000-7000-8000-0000000000ff?token=QUERYCANARY123",
            "moira_sys_HEADERCANARY456",
        ),
        ("/health/live", "moira_sys_HEADERCANARY456"),
    ] {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-moira-system-key", hdr)
                    .header("authorization", "Bearer BEARERCANARY789")
                    .header("x-request-id", "REQIDCANARY000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        eprintln!("{uri} -> {}", resp.status());
    }

    tracing::info!("qa-probe-info-event");
    tracing::debug!("qa-probe-debug-event");

    let outcome = tokio::task::spawn_blocking(move || guard.shutdown())
        .await
        .unwrap();
    eprintln!("shutdown outcome = {outcome:?}");

    let bytes = captured.lock().unwrap().clone();
    eprintln!("TOTAL CAPTURED OTLP BYTES = {}", bytes.len());
    let text = String::from_utf8_lossy(&bytes);
    // Printable runs of >=4 chars, like `strings`.
    let mut runs = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_graphic() || ch == ' ' {
            cur.push(ch);
        } else {
            if cur.len() >= 4 {
                runs.push(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.len() >= 4 {
        runs.push(cur);
    }
    eprintln!("--- OTLP PAYLOAD STRINGS ---");
    for r in &runs {
        eprintln!("  {r}");
    }
    for canary in [
        "HEADERCANARY456",
        "BEARERCANARY789",
        "QUERYCANARY123",
        "REQIDCANARY000",
    ] {
        eprintln!(
            "canary {canary}: present_in_otlp={}",
            text.contains(canary)
        );
    }
}
