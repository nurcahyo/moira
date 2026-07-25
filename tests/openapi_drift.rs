//! Plan 05 / finding P1-10a — the OpenAPI drift gate, asserted over a real socket.
//!
//! `docs/openapi.json` is the contract every consumer outside this repository reads:
//! the generated console client, the Scalar reference `/docs` renders, and every later
//! plan that diffs the spec to prove it changed nothing it did not mean to change. The
//! unit-level gate in `src/http/mod.rs` already proves that the committed file matches
//! what `documented_router(...).into_openapi()` *generates*. That is necessary and it is
//! not sufficient, because it never starts a server:
//!
//! * `moira::http::router` hands the generated document to the handler as an
//!   `Extension<Arc<OpenApiDocument>>`, and `openapi_json` (`src/http/openapi.rs`) is
//!   free to transform it before serving — it does exactly that when
//!   `docs.expose_admin` is off, stripping every `/api/v1/admin/` path. A regression
//!   that stripped, reordered, truncated, or re-serialized the document on the way out
//!   would leave the unit gate green and ship a spec no client can use.
//! * A ~640 KB JSON body is large enough that the failure modes are real ones: a body
//!   limit or a response-rewriting layer applied to the operational route group would
//!   truncate it, and `tower::ServiceExt::oneshot` — the in-process call the rest of the
//!   HTTP suites use for convenience — cannot see a framing bug that only exists once
//!   the bytes cross a socket.
//!
//! So both tests here drive `MoiraHttpServer`, i.e. `moira::build_router` over
//! `127.0.0.1:0`, and read the body back through a real `reqwest` client.
//!
//! ## Why two tests
//!
//! `openapi_json` serves two different documents, chosen by `settings.docs.expose_admin`:
//!
//! | `expose_admin` | served document                          | admin auth |
//! |----------------|------------------------------------------|------------|
//! | `true`         | the full document, byte-for-byte frozen  | required   |
//! | `false`        | the same document minus `/api/v1/admin/*`| none       |
//!
//! The committed file is the *full* document (its
//! `committed_openapi_marks_if_match_required_on_execution_policy_put` unit test reads an
//! admin path out of it), so only the first configuration can be compared to it directly.
//! The second is compared to the committed file minus the admin paths, which is the only
//! assertion that pins the redaction to a pure subset operation: a `public_document` that
//! also dropped a schema, a security scheme, or the `info` block would otherwise be
//! invisible until a public client broke on it.
//!
//! ## Skip policy
//!
//! Only the first test needs Postgres, and it needs it for a non-obvious reason: with
//! `expose_admin = true` the handler authenticates the caller before serving, and
//! `AuthService::authenticate_admin` takes a `&PgPool`, so a poolless state answers `503`
//! before it ever reaches the document. It therefore goes through [`LifecycleFixture`],
//! which owns this repository's fail-closed convention (`plans/CONVENTIONS.md` §3): skip
//! when `MOIRA_TEST_DATABASE_URL` is unset, but panic if `CI` is set to `true`, matched
//! on the *value* rather than on mere presence.
//!
//! The second test deliberately takes no database at all, so at least one end-to-end
//! assertion on the served spec runs in every environment — including a developer laptop
//! with no Postgres, where the whole point of a drift gate is that it is not the test
//! that quietly opts out.
//!
//! No `sleep()` anywhere (`plans/CONVENTIONS.md` §3): the server is ready as soon as
//! `MoiraHttpServer::start` has bound its listener, and every await is bounded by
//! [`WAIT`].

mod support;

use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use moira::{app::AppState, config::Settings};
use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use tokio::time::timeout;
use uuid::Uuid;

use support::{LifecycleFixture, MoiraHttpServer};

const WAIT: Duration = Duration::from_secs(20);

/// The committed contract, relative to the crate root.
const COMMITTED_OPENAPI_PATH: &str = "docs/openapi.json";

/// Path prefix `openapi::public_document` strips when `docs.expose_admin` is off.
const ADMIN_PATH_PREFIX: &str = "/api/v1/admin/";

/// The exact command that regenerates the committed contract.
///
/// Kept verbatim in step with `REGENERATE_OPENAPI_COMMAND` in `src/http/mod.rs`, which is
/// the test that actually writes the file. This constant is load-bearing, not decorative:
/// a drift gate whose failure message does not say how to fix it is a gate engineers learn
/// to bypass, and a gate that names a command which does not exist is worse than none.
const REGENERATE_OPENAPI_COMMAND: &str = "UPDATE_SNAPSHOTS=1 cargo test --lib \
     http::tests::committed_openapi_matches_the_generated_document";

/// The drift gate proper: what the server serves is what the repository committed.
///
/// A failure here that is accompanied by a failing
/// `http::tests::committed_openapi_matches_the_generated_document` means only that the
/// spec was not regenerated after a route, DTO, parameter, status code or header change.
/// A failure here while that unit test is *green* is the far more interesting one: it
/// means the document is generated correctly and then damaged on the way out, somewhere
/// between `moira::http::router`'s `Extension` and the bytes on the wire. The failure
/// message says both, because the two have completely different fixes.
#[tokio::test]
async fn served_openapi_document_matches_committed_docs_openapi_json() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };

    // The full document is only reachable with `expose_admin` on; the fixture's own state
    // leaves it at the hardened default, so a second state is built over the same pool.
    let mut settings = Settings::default();
    settings.docs.expose_admin = true;
    let state = AppState::new(settings, Some(fixture.pool.clone())).expect("docs app state");
    let server = MoiraHttpServer::start(state).await;
    let client = Client::new();

    // No credentials are sent: `auth.admin.enabled` is off by default, so
    // `authenticate_admin` falls through to the dev-admin actor. That is the same path
    // every other admin-route test in this repository takes, and it is what makes this
    // assertion about the *document* rather than about authentication.
    let served = fetch_openapi(&client, &server.base_url).await;
    let committed = committed_openapi_value();

    // Semantic comparison, not byte comparison. The handler serializes through
    // `serde_json::to_value` and Axum's `Json`, which emits compact JSON with no trailing
    // newline; the committed file is pretty-printed with one. Comparing bytes would fail
    // on formatting that is not part of the contract and would teach the next engineer to
    // ignore this test. Formatting *is* pinned — by the byte assertion in the unit gate,
    // which owns the file's on-disk shape.
    let mut differences = Vec::new();
    json_differences(&committed, &served, "", &mut differences);
    assert!(
        differences.is_empty(),
        "{}",
        drift_failure_message("GET /openapi.json (docs.expose_admin = true)", &differences)
    );

    // The document must actually be the admin-inclusive one. Without this, a regression
    // that flipped the handler onto the public branch unconditionally would be caught
    // above only by the resulting diff — and if the committed file itself were ever
    // regenerated from a stripped document, both would agree on the wrong thing.
    let admin_paths = admin_path_count(&served);
    assert!(
        admin_paths > 0,
        "docs.expose_admin = true must serve the admin paths; the served document has none, \
         so the handler took the public branch"
    );

    server.shutdown().await;
}

/// The public document must be the committed one minus the admin paths — nothing else.
///
/// `openapi::public_document` is a redaction, and a redaction that removes more than it
/// promises is a silent API break for every unauthenticated consumer: the console renders
/// `/docs` from this document, and a missing schema or security scheme there looks like a
/// removed feature. Asserting "committed minus `/api/v1/admin/*`" rather than a hand-listed
/// set of expected paths means a newly added public route is covered the day it is added.
#[tokio::test]
async fn served_public_openapi_document_is_the_committed_document_without_admin_paths() {
    // No database: with `expose_admin` off the handler never touches `state.pool()`, so
    // this gate runs everywhere rather than skipping wherever Postgres is absent.
    let state = AppState::new(Settings::default(), None).expect("public docs app state");
    let server = MoiraHttpServer::start(state).await;
    let client = Client::new();

    let served = fetch_openapi(&client, &server.base_url).await;

    let mut expected = committed_openapi_value();
    let paths = expected["paths"]
        .as_object_mut()
        .expect("the committed document must carry a `paths` object");
    let before = paths.len();
    paths.retain(|path, _| !path.starts_with(ADMIN_PATH_PREFIX));
    let removed = before - paths.len();
    assert!(
        removed > 0,
        "the committed document contains no {ADMIN_PATH_PREFIX}* path, so this test would \
         pass without exercising the redaction at all"
    );

    let mut differences = Vec::new();
    json_differences(&expected, &served, "", &mut differences);
    assert!(
        differences.is_empty(),
        "{}",
        drift_failure_message(
            "GET /openapi.json (docs.expose_admin = false, expected: the committed document \
             minus every admin path)",
            &differences,
        )
    );

    assert_eq!(
        admin_path_count(&served),
        0,
        "the public document must not leak any {ADMIN_PATH_PREFIX}* path"
    );

    server.shutdown().await;
}

/// Fetches `/openapi.json` over the socket and asserts the transport-level contract.
///
/// The status, content type and correlation header are asserted here rather than in each
/// test because a body that fails to parse gives a far worse failure message than a status
/// assertion does: `503` from a poolless state and `401` from an unauthenticated admin
/// branch both deserialize into "expected value at line 1", which says nothing.
async fn fetch_openapi(client: &Client, base_url: &str) -> Value {
    let request_id = format!("openapi-drift-{}", Uuid::now_v7());
    let response = timeout(
        WAIT,
        client
            .get(format!("{base_url}/openapi.json"))
            .header("x-request-id", &request_id)
            .send(),
    )
    .await
    .expect("GET /openapi.json timed out")
    .expect("GET /openapi.json");

    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let echoed_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let body = timeout(WAIT, response.text())
        .await
        .expect("reading the OpenAPI body timed out")
        .expect("OpenAPI response body");

    assert_eq!(
        status,
        StatusCode::OK,
        "GET /openapi.json must serve the document; got {status} with body {body}"
    );
    assert!(
        content_type.starts_with("application/json"),
        "GET /openapi.json must be served as JSON, got {content_type:?}"
    );
    assert_eq!(
        echoed_request_id, request_id,
        "the correlation id must survive the documentation route like every other route"
    );

    // A truncated body is the failure mode a socket-level test exists to catch, and it
    // presents as a parse error rather than as a diff, so it is called out by name.
    serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!(
            "the served OpenAPI document is not valid JSON: {err}\nThis is not spec drift — a \
             {} byte body that does not parse means it was truncated or rewritten in transit \
             (a body limit or a response-rewriting layer on the operational route group).",
            body.len()
        )
    })
}

fn committed_openapi_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_OPENAPI_PATH)
}

fn committed_openapi_value() -> Value {
    let path = committed_openapi_file();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read {}: {err}\nGenerate it with:\n\n    {REGENERATE_OPENAPI_COMMAND}\n",
            path.display()
        )
    });
    serde_json::from_str(&text).unwrap_or_else(|err| {
        panic!(
            "{} is not valid JSON: {err}\nRegenerate it with:\n\n    \
             {REGENERATE_OPENAPI_COMMAND}\n",
            path.display()
        )
    })
}

fn admin_path_count(document: &Value) -> usize {
    document["paths"]
        .as_object()
        .expect("the served document must carry a `paths` object")
        .keys()
        .filter(|path| path.starts_with(ADMIN_PATH_PREFIX))
        .count()
}

/// Collects JSON pointers at which two documents disagree.
///
/// A whole-document `assert_eq!` on a ~640 KB spec is unreadable, and an unreadable
/// failure is a failure engineers learn to ignore.
///
/// This mirrors the helper of the same name in `src/http/mod.rs`'s test module. The
/// duplication is forced, not sloppy: an integration test links `moira` compiled *without*
/// `cfg(test)`, so nothing under `#[cfg(test)] mod tests` is reachable from here, and
/// promoting a diff utility into the library's public surface to satisfy a test would be
/// the worse trade.
fn json_differences(left: &Value, right: &Value, pointer: &str, differences: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(left_object), Value::Object(right_object)) => {
            let keys: BTreeSet<&String> = left_object.keys().chain(right_object.keys()).collect();
            for key in keys {
                let child = format!("{pointer}/{key}");
                match (left_object.get(key), right_object.get(key)) {
                    (Some(left_value), Some(right_value)) => {
                        json_differences(left_value, right_value, &child, differences);
                    }
                    (Some(_), None) => differences.push(format!("{child}: only in committed")),
                    (None, Some(_)) => differences.push(format!("{child}: only in served")),
                    (None, None) => unreachable!("key came from one of the two maps"),
                }
            }
        }
        (Value::Array(left_values), Value::Array(right_values)) => {
            if left_values.len() != right_values.len() {
                differences.push(format!(
                    "{pointer}: array length {} in committed, {} in served",
                    left_values.len(),
                    right_values.len()
                ));
                return;
            }
            for (index, (left_value, right_value)) in
                left_values.iter().zip(right_values.iter()).enumerate()
            {
                let child = format!("{pointer}/{index}");
                json_differences(left_value, right_value, &child, differences);
            }
        }
        _ => {
            if left != right {
                differences.push(format!(
                    "{pointer}: committed {} != served {}",
                    truncate(left),
                    truncate(right)
                ));
            }
        }
    }
}

/// Renders a scalar for a failure message, capped so one long description cannot bury the
/// other twenty-four differences under it.
fn truncate(value: &Value) -> String {
    const LIMIT: usize = 80;
    let rendered = value.to_string();
    match rendered.char_indices().nth(LIMIT) {
        Some((index, _)) => format!("{}…", &rendered[..index]),
        None => rendered,
    }
}

fn drift_failure_message(what: &str, differences: &[String]) -> String {
    let shown: Vec<&str> = differences.iter().take(25).map(String::as_str).collect();
    let elided = differences.len().saturating_sub(shown.len());
    let mut message = format!(
        "{what} does not match {COMMITTED_OPENAPI_PATH}.\n\n\
         If `cargo test --lib http::tests::committed_openapi_matches_the_generated_document` \
         also fails, the committed spec is simply stale — regenerate and commit it with:\n\n    \
         {REGENERATE_OPENAPI_COMMAND}\n\n\
         If that unit test PASSES and only this one fails, the spec is generated correctly and \
         then damaged on the way out: look at `openapi_json` and `public_document` in \
         `src/http/openapi.rs` and at the `Extension` handed over in `moira::http::router`. \
         Regenerating would freeze the damage — do not.\n\n\
         Never hand-edit {COMMITTED_OPENAPI_PATH}.\n\n\
         {} difference(s):\n",
        differences.len()
    );
    for difference in shown {
        message.push_str("  - ");
        message.push_str(difference);
        message.push('\n');
    }
    if elided > 0 {
        message.push_str(&format!("  … and {elided} more\n"));
    }
    message
}
