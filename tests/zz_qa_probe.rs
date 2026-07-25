//! TEMPORARY QA PROBE — delete after review.
use axum::body::{Body, to_bytes};
use axum::http::Request;
use moira::{app::AppState, config::Settings};
use serde_json::Value;
use tower::ServiceExt;

fn diffs(a: &Value, b: &Value, p: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let c = format!("{p}/{k}");
                match (x.get(k), y.get(k)) {
                    (Some(l), Some(r)) => diffs(l, r, &c, out),
                    (Some(_), None) => out.push(format!("{c}: only in COMMITTED")),
                    (None, Some(_)) => out.push(format!("{c}: only in SERVED")),
                    _ => {}
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!("{p}: len {} vs {}", x.len(), y.len()));
                return;
            }
            for (i, (l, r)) in x.iter().zip(y).enumerate() {
                diffs(l, r, &format!("{p}/{i}"), out);
            }
        }
        _ => {
            if a != b {
                out.push(format!("{p}: {a} != {b}"));
            }
        }
    }
}

#[tokio::test]
async fn probe_committed_vs_served_openapi() {
    let mut settings = Settings::default();
    settings.docs.expose_admin = false;
    let state = AppState::new(settings, None).unwrap();
    let router = moira::build_router(state).unwrap();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    println!("STATUS {}", resp.status());
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let served: Value = serde_json::from_slice(&bytes).unwrap();
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/openapi.json")).unwrap(),
    )
    .unwrap();

    let sp = served["paths"].as_object().map(|m| m.len()).unwrap_or(0);
    let cp = committed["paths"].as_object().map(|m| m.len()).unwrap_or(0);
    println!("served paths={sp} committed paths={cp}");

    let mut committed_public = committed.clone();
    committed_public["paths"]
        .as_object_mut()
        .unwrap()
        .retain(|k, _| !k.starts_with("/api/v1/admin/"));
    let mut out = Vec::new();
    diffs(&committed_public, &served, "", &mut out);
    println!("DIFF COUNT (public-filtered) = {}", out.len());
    for d in out.iter().take(60) {
        println!("  {d}");
    }

    // Does the committed doc carry the finalize_document X-Request-Id parameter?
    let op = &committed["paths"]["/health/live"]["get"];
    println!("health/live get params = {}", op["parameters"]);
    println!(
        "health/live 200 headers = {}",
        op["responses"]["200"]["headers"]
    );
}
