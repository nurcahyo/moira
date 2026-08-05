//! Issue #89 — caller-supplied image URLs go through the hardened SSRF guard.
//!
//! The guard's own rules are proved as unit tests next to the code
//! (`src/security/ssrf.rs` for the address-space decisions, `src/application/public.rs`
//! for the request-level ones), against a scripted resolver so that multi-address answers
//! and timing are deterministic.
//!
//! What *cannot* be proved there, and is the only thing proved here, is **wiring**: that
//! the public request path actually consults the guard. Every unit test in this change
//! would still pass if `validate_content` stopped calling `validate_image_urls`
//! altogether, because they call the validator directly. These do not.
//!
//! The cases below use address *literals* rather than hostnames on purpose: no DNS, so
//! the result does not depend on the build host's resolver, its `/etc/hosts`, or whether
//! the machine has a network at all.

mod support;

use axum::http::StatusCode;
use moira::{
    application::PublicExecutionService,
    domain::{PublicContentPart, PublicInputMessage, PublicMessageRole, PublicResponseRequest},
    error::AppError,
    security::{Actor, ActorType},
};
use support::{LifecycleFixture, public_response_request, request_context};

/// An actor allowed to create responses, bound to the fixture's application.
fn creator(fixture: &LifecycleFixture) -> Actor {
    Actor {
        actor_type: ActorType::ConsumerKey,
        subject: Some("image-ssrf-tester".to_string()),
        application_id: Some(fixture.application_id.to_string()),
        external_user_id: Some("image-ssrf-tester".to_string()),
        internal_application_id: Some(fixture.application_id),
        scopes: vec!["moira:responses:create".to_string()],
        ..Actor::default()
    }
}

/// A request whose only content part is the image under test.
///
/// `route` is deliberately `None`. Naming a route is a *route override*, which is
/// authorised earlier in `validate_request` than the content checks are, so a request
/// carrying one is refused with `route_override_forbidden` before the image URL is ever
/// looked at — and every assertion below would then be passing on the wrong error.
fn request_with_image(image_url: &str) -> PublicResponseRequest {
    PublicResponseRequest {
        input: vec![PublicInputMessage {
            role: PublicMessageRole::User,
            content: vec![PublicContentPart::InputImage {
                image_url: image_url.to_string(),
            }],
        }],
        route: None,
        ..public_response_request("unused")
    }
}

/// The error a refused image URL must produce, whatever the reason.
fn assert_image_url_refused(result: Result<impl std::fmt::Debug, AppError>, label: &str) {
    let error = match result {
        Err(error) => error,
        Ok(value) => panic!("{label} must not be accepted, got {value:?}"),
    };
    let response = error.error_response(Some("test-request".to_string()));
    assert_eq!(
        error.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "{label} must be refused as unprocessable: {response:?}"
    );
    assert_eq!(
        response.error.code, "image_url_not_allowed",
        "{label} must carry the catalogued code: {response:?}"
    );
}

/// The literal spellings a three-string check misses. `2130706433` and `0x7f.0.0.1` are
/// loopback; neither contains the text `127.0.0.1`. `169.254.169.254` is the cloud
/// metadata endpoint. This is the defect issue #89 names, driven through the real
/// request path rather than through the validator in isolation.
#[tokio::test]
async fn the_public_request_path_refuses_image_urls_in_denied_address_space() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let public = PublicExecutionService::new(&fixture.state).expect("public execution service");
    let actor = creator(&fixture);

    for literal in [
        "https://127.0.0.1/a.png",
        "https://2130706433/a.png",
        "https://0x7f.0.0.1/a.png",
        "https://169.254.169.254/latest/meta-data/",
        "https://10.0.0.5/a.png",
        "https://192.168.1.1/a.png",
        "https://172.16.0.1/a.png",
        "https://[::1]/a.png",
        "https://[::ffff:169.254.169.254]/a.png",
        "https://[fd00::1]/a.png",
    ] {
        let result = public
            .create_response(&actor, &request_context(), request_with_image(literal))
            .await;
        assert_image_url_refused(result, literal);
    }
}

/// Scheme and embedded credentials, also through the real path.
#[tokio::test]
async fn the_public_request_path_refuses_non_https_and_credentialed_image_urls() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let public = PublicExecutionService::new(&fixture.state).expect("public execution service");
    let actor = creator(&fixture);

    for literal in [
        "http://images.example.com/a.png",
        "https://user:secret@images.example.com/a.png",
        "not-a-url-at-all",
    ] {
        let result = public
            .create_response(&actor, &request_context(), request_with_image(literal))
            .await;
        assert_image_url_refused(result, literal);
    }
}

/// A refusal must not hand the caller anything about the deployment's network, and must
/// not echo the URL it was given either — an echoed URL is a reflection primitive.
#[tokio::test]
async fn a_refusal_tells_the_caller_nothing_about_the_internal_network() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let public = PublicExecutionService::new(&fixture.state).expect("public execution service");

    let error = public
        .create_response(
            &creator(&fixture),
            &request_context(),
            request_with_image("https://169.254.169.254/latest/meta-data/"),
        )
        .await
        .expect_err("the metadata endpoint must be refused");
    let response = error.error_response(Some("test-request".to_string()));
    // Pin the error to the guard's own refusal first. Without this the assertions below
    // are satisfied by *any* later failure — a routing error names no address either, so
    // an unwired guard would leave this test green.
    assert_eq!(
        response.error.code, "image_url_not_allowed",
        "the refusal must come from the image guard: {response:?}"
    );
    let body = format!("{response:?}");

    assert!(
        !body.contains("169.254.169.254"),
        "the address must not be reflected to the caller: {body}"
    );
    assert!(
        !body.contains("meta-data"),
        "the path must not be reflected to the caller: {body}"
    );
    assert!(
        !body.contains("ip_range"),
        "the internal denial reason must not reach the caller: {body}"
    );
}

/// The guard must not have been bought by refusing every image. A public address literal
/// still passes validation — the request goes on to fail later, on routing or execution,
/// which is a *different* error. If this ever starts returning `image_url_not_allowed`
/// the guard has become a blanket ban and the tests above would still all pass.
#[tokio::test]
async fn a_public_image_url_is_not_refused_by_the_guard() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let public = PublicExecutionService::new(&fixture.state).expect("public execution service");

    let result = public
        .create_response(
            &creator(&fixture),
            &request_context(),
            request_with_image("https://93.184.216.34/a.png"),
        )
        .await;

    if let Err(error) = result {
        let response = error.error_response(Some("test-request".to_string()));
        assert_ne!(
            response.error.code, "image_url_not_allowed",
            "a public address must clear the SSRF guard; it may still fail further \
             down the request, but not here: {response:?}"
        );
    }
}
