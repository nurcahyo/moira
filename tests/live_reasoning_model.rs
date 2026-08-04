//! Finding F57, re-measured against a real reasoning model — **opt-in, skipped by default**.
//!
//! # Why this exists as a test rather than as a paragraph in the ledger
//!
//! F57's decision — announce an inline reasoning block, never remove it — rests entirely on two
//! measurements taken against a live `Qwen/Qwen3-4B`. This repository has repeatedly been bitten
//! by acting on a measurement taken days earlier on another machine (`HANDOFF.md` §2.5), and the
//! remedy that works is making the measurement cost one command instead of one afternoon.
//!
//! # It cannot become a gate, and is built so it cannot accidentally become one
//!
//! CI has no route to the endpoint, and a gate that depends on someone's LAN is a gate that lies.
//! Every case here returns early unless `MOIRA_LIVE_REASONING_BASE_URL` is set, so the default is
//! a passing no-op. That is the same shape `LifecycleFixture::new()` uses for the database.
//!
//! ```text
//! MOIRA_LIVE_REASONING_BASE_URL=https://local-llm.motrait.com \
//! MOIRA_LIVE_REASONING_MODEL=Qwen/Qwen3-4B \
//!   cargo test --test live_reasoning_model -- --nocapture
//! ```
//!
//! # What was measured, so a re-run can be compared rather than merely repeated
//!
//! Against that endpoint on 2026-08-04, with this module's real prompt and temperature 0:
//!
//! | case | bytes | `<think>` | `</think>` | reasoning share |
//! |---|---|---|---|---|
//! | ordinary transcript | 2 419 | 1 | 1 | 53.7 % |
//! | transcript that *discusses* reasoning tags | 2 544 | 1 | **10** | 5.9 % by the first close |
//! | `max_tokens: 120` | 613 | 1 | **0** (`finish_reason: length`) | 100 % |
//! | `chat_template_kwargs: {"enable_thinking": false}` | 830 | 0 | 0 | 0 % |
//!
//! Row 2 is the one that ended the stripping option: the real terminator was the fifth of ten, so
//! cutting at the first left 985 bytes of chain-of-thought and cutting at the last destroyed 2 173
//! bytes of summary. Row 3 is the one a well-formed-block rule cannot reach at all. Row 4 is the
//! operator's actual fix living on the *server*, which is why the warning names it.

use moira::{
    application::{SUMMARY_TRANSCRIPT_LABEL, parse_summary, summarization_messages},
    domain::{DomainMessageContent, DomainMessageRole},
};
use serde_json::{Value, json};

const TRANSCRIPT: &str = "\
user: We need to ship the invoicing rewrite before the March board meeting.
assistant: Understood. What is currently blocking it?
user: The reconciliation job still double-counts refunds issued in a prior period, and Priya is \
out until the 12th.
assistant: Two options: patch the refund window in the existing job, or finish the ledger \
migration first. The patch is about a day; the migration is closer to three weeks.
user: Take the patch. We can do the migration in Q2.";

/// The endpoint under test, or `None` when the suite is to skip.
fn base_url() -> Option<String> {
    std::env::var("MOIRA_LIVE_REASONING_BASE_URL")
        .ok()
        .map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

fn model() -> String {
    std::env::var("MOIRA_LIVE_REASONING_MODEL").unwrap_or_else(|_| "Qwen/Qwen3-4B".to_string())
}

/// Moira's own summarization messages, rendered onto the OpenAI chat wire shape.
///
/// Built from [`summarization_messages`] rather than from a hand-written literal, so a change to
/// the instruction or to either label is measured rather than silently bypassed — the same reason
/// `tests/conversation_summarization.rs` asserts against the body the mock received.
fn wire_messages() -> Vec<Value> {
    summarization_messages(None, TRANSCRIPT, 1000)
        .into_iter()
        .map(|message| {
            let role = match message.role {
                DomainMessageRole::System => "system",
                DomainMessageRole::User => "user",
                DomainMessageRole::Assistant => "assistant",
                DomainMessageRole::Tool => "tool",
            };
            let content = message
                .content
                .iter()
                .filter_map(|part| match part {
                    DomainMessageContent::Text { text } => Some(text.as_str()),
                    DomainMessageContent::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            json!({ "role": role, "content": content })
        })
        .collect()
}

/// One completion against the live endpoint, returning `(content, finish_reason)`.
async fn complete(extra: Value) -> (String, String) {
    let url = base_url().expect("caller checked");
    let mut body = json!({
        "model": model(),
        "messages": wire_messages(),
        // Moira pins temperature 0 on this path so two runs over one backlog agree.
        "temperature": 0.0,
    });
    if let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .expect("live completion request")
        .error_for_status()
        .expect("live completion status");
    let payload: Value = response.json().await.expect("live completion body");
    let choice = &payload["choices"][0];
    (
        choice["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        choice["finish_reason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// The live model inlines its chain of thought, and `parse_summary` flags it without touching it.
///
/// The assertion that matters is the second one. "It returned a `<think>` block" is the finding;
/// "and the stored text is still every byte the model sent" is the *decision*, and it is the half
/// that a well-meaning future edit would break.
#[tokio::test]
async fn the_live_model_inlines_reasoning_and_moira_flags_it_without_editing_it() {
    if base_url().is_none() {
        return;
    }
    let (raw, finish_reason) = complete(json!({})).await;
    assert_eq!(finish_reason, "stop", "reply was cut short: {raw}");

    let parsed = parse_summary(&raw).expect("a non-empty, bounded reply");
    assert!(
        parsed.inline_reasoning,
        "the endpoint no longer inlines reasoning — F57's premise has changed and the decision \
         recorded at parse_summary should be re-read before anything is built on it. Reply:\n{raw}"
    );
    assert_eq!(
        parsed.text,
        raw.trim(),
        "F57 reports and never removes; the stored text must be the model's own bytes"
    );

    // The number that made this a defect rather than a curiosity: how little of the stored
    // summary is the summary. Printed rather than asserted — the exact share is model- and
    // prompt-dependent, and pinning it would make this suite fail on an unrelated model update.
    let close = raw.find("</think>").map(|at| at + "</think>".len());
    println!(
        "F57 live: {} bytes stored, reasoning through byte {:?}, {} closing tags",
        raw.len(),
        close,
        raw.matches("</think>").count()
    );
    assert!(
        !parsed.text.contains(SUMMARY_TRANSCRIPT_LABEL),
        "Moira's own transcript label must not be echoed into the stored summary:\n{raw}"
    );
}

/// A reply truncated inside the block has **no terminator**, and is flagged anyway.
///
/// This is the case that decides the shape of the fix rather than merely illustrating it: an
/// unterminated block is 100 % chain-of-thought, and a rule that removes only a *well-formed*
/// block leaves it entirely intact. Detection anchored at offset 0 does not care.
#[tokio::test]
async fn a_truncated_reply_is_all_reasoning_and_is_still_flagged() {
    if base_url().is_none() {
        return;
    }
    let (raw, finish_reason) = complete(json!({ "max_tokens": 120 })).await;
    assert_eq!(
        finish_reason, "length",
        "the cap did not truncate, so this case measured nothing: {raw}"
    );

    let parsed = parse_summary(&raw).expect("a non-empty, bounded reply");
    assert!(
        parsed.inline_reasoning,
        "a truncated reasoning block must still be announced:\n{raw}"
    );
    assert!(
        !raw.contains("</think>"),
        "premise check: this case is about the *unterminated* block. A terminator here means the \
         cap no longer lands inside the reasoning, and the case should be re-tuned rather than \
         trusted:\n{raw}"
    );
}
