//! The three `moira_content_envelope_*` families, asserted through the production seam — #171.
//!
//! # Why this suite is database-backed rather than a unit test on `MetricsRegistry`
//!
//! A test that called `record_content_envelope_seal` directly would pass with the recorders wired
//! to nothing at all, which is precisely the #125 shape the issue is about: a counter that exists,
//! is documented, is tested, and never moves in production. So every assertion here drives
//! `KeyringContentAccess` — the object `AppState::content_access()` hands to every repository —
//! over a **real keyring** loaded from a real database under a real master key, and reads the
//! rendered `/metrics` body afterwards.
//!
//! # The negative half is the assertion that matters
//!
//! `a_refused_open_moves_only_the_failure_counter` asserts that `..._open_total` is **unchanged**
//! across a failing open. A success counter that also ticks on the failure path is worse than no
//! counter, because a dashboard built on it reads as healthy exactly when it is not — and the
//! cheapest way to write that bug is to increment right after the AEAD and let a later refusal
//! fall through. Only the negative assertion catches it.

mod support;

use base64::{Engine, engine::general_purpose::STANDARD};
use moira::{
    config::Settings,
    security::{
        ContentIdentity, ContentOpener, ContentSealer, ENVELOPE_HEADER_LEN, KeyringContentAccess,
    },
};
use support::LifecycleFixture;
use uuid::Uuid;

const SEAL_TOTAL: &str = "moira_content_envelope_seal_total";
const OPEN_TOTAL: &str = "moira_content_envelope_open_total";
const OPEN_FAILED_TOTAL: &str = "moira_content_envelope_open_failed_total";

/// The `AadProfile::label` of the identity every test below uses.
const PROFILE: &str = "conversation_message_content";

/// A named master key rather than the dev sentinel, so the fixture is production-shaped.
const MASTER: [u8; 32] = [0x71; 32];

struct Case {
    fixture: LifecycleFixture,
}

impl Case {
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::with_settings(|settings: &mut Settings| {
            settings.content_encryption.keys = format!("m171:{}", STANDARD.encode(MASTER));
            settings.content_encryption.active_key_id = "m171".to_string();
            settings.content_encryption.allow_insecure_dev_key = false;
        })
        .await?;
        Some(Self { fixture })
    }

    /// The same seam the repositories seal and open with.
    fn access(&self) -> KeyringContentAccess {
        self.fixture.state.content_access()
    }

    /// The key id every successful open in this suite must be labelled with.
    fn active_data_key_id(&self) -> Uuid {
        self.fixture
            .state
            .content_keyring
            .as_ref()
            .expect("the fixture configures content encryption, so a keyring is loaded")
            .snapshot()
            .active_content_key_id()
    }

    /// The registry the keyring writes into is `AppState`'s own — `AppState::new` clones one
    /// handle into `ContentKeyring::load`. Rendering from here therefore reads the same series
    /// the seam just wrote, and a change that gave the content seam a *second* registry would
    /// make every assertion below fail rather than quietly measure nothing.
    fn rendered(&self) -> String {
        self.fixture
            .state
            .metrics
            .render_prometheus("moira-test", false, false)
    }
}

fn identity(message_id: Uuid, conversation_id: Uuid) -> ContentIdentity<'static> {
    ContentIdentity::ConversationMessage {
        message_id,
        conversation_id,
        sequence_number: 1,
    }
}

/// One series' value, located by family name plus every label fragment that must be on the line.
///
/// Matching on fragments rather than on a whole rendered line keeps the assertions independent of
/// label ordering and of the builder-time `service` label, neither of which this suite is about.
fn series(rendered: &str, family: &str, labels: &[String]) -> f64 {
    rendered
        .lines()
        .filter(|line| line.starts_with(&format!("{family}{{")))
        .find(|line| labels.iter().all(|label| line.contains(label.as_str())))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {family} series with {labels:?} in:\n{rendered}"))
}

fn seal_total(rendered: &str) -> f64 {
    series(rendered, SEAL_TOTAL, &[format!("profile=\"{PROFILE}\"")])
}

fn open_total(rendered: &str, data_key_id: Uuid) -> f64 {
    series(
        rendered,
        OPEN_TOTAL,
        &[
            format!("profile=\"{PROFILE}\""),
            format!("data_key_id=\"{data_key_id}\""),
        ],
    )
}

fn open_failed_total(rendered: &str, reason: &str) -> f64 {
    series(
        rendered,
        OPEN_FAILED_TOTAL,
        &[
            format!("profile=\"{PROFILE}\""),
            format!("reason=\"{reason}\""),
        ],
    )
}

/// A seal moves `..._seal_total{profile}` and nothing else.
///
/// **Cheapest edit that breaks the property while leaving a weaker test green:** delete the
/// `record_content_envelope_seal` call in `ContentSealer::seal_content`. A test that only checked
/// "the family is present in the body" would still pass, because the family is seeded at zero for
/// all five profiles — which is why this asserts a *delta* rather than presence.
#[tokio::test]
async fn a_seal_moves_the_seal_counter() {
    let Some(case) = Case::new().await else {
        return;
    };
    let before = seal_total(&case.rendered());

    case.access()
        .seal_content(&identity(Uuid::now_v7(), Uuid::now_v7()), "envelope body")
        .expect("the fixture's keyring has a writable active key");

    let after = seal_total(&case.rendered());
    assert_eq!(
        after - before,
        1.0,
        "one seal must move moira_content_envelope_seal_total by exactly one"
    );
}

/// An open moves `..._open_total{profile,data_key_id}`, labelled with the key that actually
/// protected the row.
///
/// **Cheapest edit that breaks it:** label the counter with `snapshot.active_content_key_id()`
/// instead of `header.data_key_id`. Every assertion that ignored the label value would stay green,
/// and the family would silently stop being able to answer "has anything opened under the *old*
/// key" — the one question #138 built it for. Asserting the exact id is what closes that.
#[tokio::test]
async fn an_open_moves_the_open_counter_under_the_key_that_sealed_it() {
    let Some(case) = Case::new().await else {
        return;
    };
    let access = case.access();
    let identity = identity(Uuid::now_v7(), Uuid::now_v7());
    let sealed = access
        .seal_content(&identity, "envelope body")
        .expect("seal must succeed");
    let data_key_id = case.active_data_key_id();

    // Before the first open the series does not exist yet — it is deliberately unseeded, since
    // seeding would require inventing a key id. That absence is itself part of the contract.
    assert!(
        !case.rendered().contains(&format!("{OPEN_TOTAL}{{")),
        "..._open_total must not be seeded: a seeded series would have to name a fake key"
    );

    let opened = access
        .open_content(&sealed, &identity)
        .expect("the row was sealed by this same keyring");
    assert_eq!(opened, "envelope body");

    assert_eq!(
        open_total(&case.rendered(), data_key_id),
        1.0,
        "the open counter must be labelled with the data key the envelope header named"
    );
}

/// **The assertion this suite exists for.** A refused open moves the failure counter and leaves
/// the success counter exactly where it was.
///
/// Three refusals are driven, one per class the design distinguishes:
///
/// * a corrupted **body** — the AEAD arm, which must collapse to the single `aead_open_failed`;
/// * a corrupted **magic** — a header discriminant, which may name itself;
/// * an **unknown key id** in the header — the arm that must *not* put that id on any label,
///   because the header is attacker-writable and doing so is the cardinality leak the success
///   counter's bound depends on never opening.
///
/// **Cheapest edit that breaks the property:** move
/// `metrics.record_content_envelope_open(identity.profile(), header.data_key_id)` from the tail of
/// `open_with_snapshot` to immediately after `EnvelopeHeader::parse` succeeds. Both positive tests
/// above stay green — a successful open still increments once — and only the `open_before ==
/// open_after` assertions here go red.
#[tokio::test]
async fn a_refused_open_moves_only_the_failure_counter() {
    let Some(case) = Case::new().await else {
        return;
    };
    let access = case.access();
    let identity = identity(Uuid::now_v7(), Uuid::now_v7());
    let sealed = access
        .seal_content(&identity, "envelope body")
        .expect("seal must succeed");
    let data_key_id = case.active_data_key_id();

    // One good open first, so `..._open_total` exists and a later "unchanged" assertion is a real
    // comparison rather than an assertion about an absent series.
    access
        .open_content(&sealed, &identity)
        .expect("the intact envelope must open");
    let open_before = open_total(&case.rendered(), data_key_id);
    assert_eq!(open_before, 1.0);

    // (1) The AEAD arm: flip the last byte of the tag. Framing stays valid, so this reaches the
    // cipher and fails there.
    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    let aead_before = open_failed_total(&case.rendered(), "aead_open_failed");
    access
        .open_content(&tampered, &identity)
        .expect_err("a flipped tag byte must not authenticate");
    let rendered = case.rendered();
    assert_eq!(
        open_failed_total(&rendered, "aead_open_failed") - aead_before,
        1.0,
        "an AEAD refusal must move the failure counter"
    );
    assert_eq!(
        open_total(&rendered, data_key_id),
        open_before,
        "an AEAD refusal must NOT move the success counter"
    );

    // (2) A header discriminant: break the magic.
    let mut bad_magic = sealed.clone();
    bad_magic[0] ^= 0xff;
    let magic_before = open_failed_total(&case.rendered(), "bad_magic");
    access
        .open_content(&bad_magic, &identity)
        .expect_err("bytes that are not a Moira envelope must be refused");
    let rendered = case.rendered();
    assert_eq!(
        open_failed_total(&rendered, "bad_magic") - magic_before,
        1.0,
        "a framing refusal must be counted under its own discriminant"
    );
    assert_eq!(
        open_total(&rendered, data_key_id),
        open_before,
        "a framing refusal must NOT move the success counter"
    );

    // (3) The unknown-key arm. `data_key_id` sits at offset 8 of the header and is inside the
    // AAD, so overwriting it would fail the tag anyway — but it is refused *before* the cipher is
    // reached, which is what this asserts, and it must leave no trace of the id it named.
    let unknown = Uuid::now_v7();
    let mut foreign_key = sealed.clone();
    foreign_key[8..24].copy_from_slice(unknown.as_bytes());
    let unknown_before = open_failed_total(&case.rendered(), "unknown_data_key");
    access
        .open_content(&foreign_key, &identity)
        .expect_err("an envelope naming a key this keyring has never held must be refused");
    let rendered = case.rendered();
    assert_eq!(
        open_failed_total(&rendered, "unknown_data_key") - unknown_before,
        1.0,
        "a key the snapshot does not carry must be counted as unknown_data_key"
    );
    assert_eq!(
        open_total(&rendered, data_key_id),
        open_before,
        "an unknown-key refusal must NOT move the success counter"
    );
    assert!(
        !rendered.contains(&unknown.to_string()),
        "the key id named by a refused envelope must never reach a label value — it is written \
         by whoever wrote the row, so labelling it is an unbounded series set on the scrape \
         path:\n{rendered}"
    );
}

/// Every failure this suite provoked, and every one it could not, stays inside the closed reason
/// domain — and the whole family carries no key id, no row id and no fragment of the body.
///
/// **Cheapest edit that breaks it:** add `"data_key_id" => header.data_key_id.to_string()` to the
/// failure counter. Every other test in this file stays green.
#[tokio::test]
async fn the_failure_family_carries_no_identifier_and_no_ciphertext() {
    let Some(case) = Case::new().await else {
        return;
    };
    let access = case.access();
    let message_id = Uuid::now_v7();
    let conversation_id = Uuid::now_v7();
    let identity = identity(message_id, conversation_id);
    let sealed = access
        .seal_content(&identity, "envelope body")
        .expect("seal must succeed");

    // A blob shorter than the 42-byte header cannot even be parsed.
    access
        .open_content(&sealed[..ENVELOPE_HEADER_LEN - 1], &identity)
        .expect_err("a short blob must be refused");

    let rendered = case.rendered();
    for line in rendered
        .lines()
        .filter(|line| line.starts_with(&format!("{OPEN_FAILED_TOTAL}{{")))
    {
        assert!(
            !line.contains("data_key_id="),
            "the failure family must carry no data key id: {line}"
        );
        assert!(
            !line.contains(&message_id.to_string()) && !line.contains(&conversation_id.to_string()),
            "a row identifier reached a label value: {line}"
        );
    }
    assert_eq!(
        open_failed_total(&rendered, "too_short"),
        1.0,
        "a blob below the length floor is a framing refusal with its own discriminant"
    );
    // The one thing an AEAD refusal may say. Anything finer would make /metrics the oracle the
    // wire format refuses to be, so there must be exactly one AEAD-flavoured reason in the domain.
    let aead_reasons: Vec<&str> = rendered
        .lines()
        .filter(|line| line.starts_with(&format!("{OPEN_FAILED_TOTAL}{{")))
        .filter(|line| line.contains("reason=\"aead"))
        .collect();
    assert_eq!(
        aead_reasons.len(),
        5,
        "exactly one AEAD reason per profile — five profiles, one value — and no second, finer \
         AEAD discriminant:\n{rendered}"
    );
    assert!(
        aead_reasons
            .iter()
            .all(|line| line.contains("reason=\"aead_open_failed\"")),
        "the only admissible AEAD reason is aead_open_failed:\n{rendered}"
    );
}
