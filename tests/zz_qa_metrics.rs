//! TEMPORARY QA PROBE — delete after review.
use axum::body::{Body, to_bytes};
use axum::http::Request;
use moira::{app::AppState, config::Settings};
use tower::ServiceExt;

async fn scrape(router: &axum::Router) -> String {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let st = resp.status();
    let b = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = String::from_utf8_lossy(&b).into_owned();
    eprintln!("/metrics -> {st} ({} bytes)", s.len());
    s
}

#[tokio::test]
async fn probe_metrics_cardinality() {
    let mut settings = Settings::default();
    settings.telemetry.prometheus_enabled = true;
    let state = AppState::new(settings, None).unwrap();
    let router = moira::build_router(state).unwrap();

    let base = scrape(&router).await;
    eprintln!("baseline lines = {}", base.lines().count());

    // Adversary 1: unauthenticated requests to every documented route template,
    // with every method, to blow up route x method x status_class.
    let committed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/openapi.json")).unwrap(),
    )
    .unwrap();
    let paths: Vec<String> = committed["paths"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    eprintln!("driving {} path templates", paths.len());

    let mut sent = 0usize;
    for p in &paths {
        // substitute params with attacker-chosen values
        let concrete = p
            .replace("{id}", "018f6b1e-0000-7000-8000-000000000111")
            .replace("{application_id}", "018f6b1e-0000-7000-8000-000000000222")
            .replace("{provider_id}", "018f6b1e-0000-7000-8000-000000000333")
            .replace("{collection_id}", "018f6b1e-0000-7000-8000-000000000444")
            .replace(
                "{conversation_id}",
                "018f6b1e-0000-7000-8000-000000000555",
            )
            .replace("{message_id}", "018f6b1e-0000-7000-8000-000000000666")
            .replace("{memory_id}", "018f6b1e-0000-7000-8000-000000000777")
            .replace("{document_id}", "018f6b1e-0000-7000-8000-000000000888");
        for m in [
            "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
        ] {
            let r = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(m)
                        .uri(&concrete)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let _ = r.status();
            sent += 1;
        }
    }
    eprintln!("sent {sent} requests");

    let after = scrape(&router).await;
    eprintln!("after lines = {}", after.lines().count());
    eprintln!(
        "growth: {} -> {} bytes ({}x)",
        base.len(),
        after.len(),
        after.len() as f64 / base.len().max(1) as f64
    );

    // distinct route label values
    let mut routes = std::collections::BTreeSet::new();
    for line in after.lines() {
        if let Some(i) = line.find("route=\"") {
            let rest = &line[i + 7..];
            if let Some(j) = rest.find('"') {
                routes.insert(rest[..j].to_string());
            }
        }
    }
    eprintln!("distinct route label values = {}", routes.len());

    // Any attacker-chosen UUID reach a label?
    for id in ["000000000111", "000000000222", "000000000333"] {
        eprintln!("uuid fragment {id} in body: {}", after.contains(id));
    }

    // Adversary 2: unmatched paths with unique junk.
    for i in 0..200 {
        let _ = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/nope/{i}/{}", uuid::Uuid::now_v7()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    let after2 = scrape(&router).await;
    eprintln!(
        "after unmatched flood: {} bytes, lines {}",
        after2.len(),
        after2.lines().count()
    );

    // Adversary 3: extension methods.
    for i in 0..50 {
        let _ = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(
                        axum::http::Method::from_bytes(format!("XCANARY{i}").as_bytes()).unwrap(),
                    )
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    let after3 = scrape(&router).await;
    eprintln!("XCANARY in body: {}", after3.contains("XCANARY"));

    // Disabled contract
    let mut off = Settings::default();
    off.telemetry.prometheus_enabled = false;
    let off_router = moira::build_router(AppState::new(off, None).unwrap()).unwrap();
    let resp = off_router
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    eprintln!("disabled /metrics -> {}", resp.status());
    let b = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    eprintln!("disabled body = {}", String::from_utf8_lossy(&b));
}
