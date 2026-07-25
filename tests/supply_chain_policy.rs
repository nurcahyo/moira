//! Policy assertions over `deny.toml` (plan 05, finding P0-4).
//!
//! **Why this exists when `cargo deny check` already runs in CI.** `cargo deny check` proves the
//! current dependency graph satisfies whatever policy it is handed. It cannot notice that the
//! policy itself got weaker. Flip `unknown-git` to `"allow"`, delete the `[sources]` section
//! outright, or empty the license allow-list, and `cargo deny check` goes right on passing — with
//! less enforcement behind it. That silent-downgrade path is precisely the shape of P0-4, whose
//! original bug was a supply-chain gate that ran green because it had no configuration at all.
//! These tests pin the policy's *teeth* so removing them is a red build rather than a quiet loss.
//!
//! **Why the assertions are hand-rolled rather than TOML-parsed.** `Cargo.toml` has no
//! `[dev-dependencies]` section, and the plan's Module 1 prefers keeping it that way where a
//! hand-rolled assertion is adequate (a TOML parser would be the first-ever entry). Substring
//! matching over the embedded file is adequate here: every property below is a literal directive,
//! and the file is `include_str!`-embedded so deleting `deny.toml` fails the build outright rather
//! than skipping the checks.

/// Embedded at compile time from the repo root, so this suite cannot silently pass against a
/// missing or unreadable policy file the way a runtime read could.
const DENY_TOML: &str = include_str!("../deny.toml");

/// Strips comment bodies so a directive that only appears inside prose (this file's policy
/// comments quote several verbatim) is never mistaken for a live setting.
fn directives(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find('#') {
            Some(index) => &line[..index],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn deny_toml_parses_and_declares_every_required_section() {
    let live = directives(DENY_TOML);

    // The four sections plan 05 requires. A policy missing any one of them silently stops
    // enforcing that whole class of supply-chain risk.
    for section in ["[advisories]", "[licenses]", "[bans]", "[sources]"] {
        assert!(
            live.contains(section),
            "deny.toml must declare {section}; without it `cargo deny check` still passes but \
             enforces nothing for that class, which is the exact P0-4 failure mode"
        );
    }

    // A populated license allow-list is what makes `[licenses]` more than decoration. Assert on
    // the two licenses that cover most of the tree rather than the whole list, so ordinary
    // additions do not churn this test while an emptied list still fails it.
    assert!(
        live.contains("Apache-2.0") && live.contains("MIT"),
        "the [licenses] allow-list must be populated from the real dependency graph; an empty \
         list accepts every license, defeating the gate"
    );
}

#[test]
fn deny_toml_denies_unknown_registries_and_git_sources() {
    let live = directives(DENY_TOML);

    // These two are the supply-chain integrity core: they are what stops a dependency from being
    // pulled out of an arbitrary registry or a git URL that no one reviewed.
    assert!(
        live.contains(r#"unknown-registry = "deny""#),
        "deny.toml must set `unknown-registry = \"deny\"`; anything weaker lets a dependency be \
         sourced from a registry outside crates.io without review"
    );
    assert!(
        live.contains(r#"unknown-git = "deny""#),
        "deny.toml must set `unknown-git = \"deny\"`; anything weaker lets a dependency be \
         sourced from an arbitrary git URL without review"
    );

    // Wildcard versions would let an unreviewed major bump enter the tree on any `cargo update`.
    assert!(
        live.contains(r#"wildcards = "deny""#),
        "deny.toml must set `wildcards = \"deny\"` so a wildcard version cannot pull in an \
         unreviewed release"
    );
}

/// An undocumented `[advisories].ignore` entry is the other half of P0-4: the gate stays green
/// while an advisory is waived for a reason nobody recorded and nobody can re-evaluate. Every
/// entry must therefore carry an adjacent explanatory comment. An empty `ignore` list passes
/// trivially — the point is to document waivers, not to require one.
#[test]
fn deny_toml_advisories_ignore_entries_are_documented() {
    // Scoped to `[advisories]`: `[licenses.private]` also carries an `ignore` key.
    let advisories: Vec<&str> = DENY_TOML
        .lines()
        .skip_while(|line| line.trim() != "[advisories]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .collect();
    assert!(
        !advisories.is_empty(),
        "deny.toml must declare an [advisories] section with a body"
    );

    let Some(start) = advisories
        .iter()
        .position(|line| line.trim_start().starts_with("ignore"))
    else {
        return; // No `ignore` key at all: nothing to document.
    };

    // `ignore = []` written on one line is the empty list; anything inline is not reviewable.
    if let Some((_, after_open)) = advisories[start].split_once('[')
        && let Some((inline, _)) = after_open.split_once(']')
    {
        assert!(
            inline.trim().is_empty(),
            "an inline [advisories].ignore entry has nowhere to carry its rationale; write the \
             list across multiple lines with a comment above each entry, found: {:?}",
            advisories[start]
        );
        return;
    }

    let mut documented = false;
    for line in advisories[start + 1..]
        .iter()
        .take_while(|l| l.trim() != "]")
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            documented = true;
            continue;
        }
        // Only `{ ... }` tables and bare `"RUSTSEC-…"` strings start an entry; anything else is
        // a continuation of the entry above and must not reset the rationale it already has.
        if !(trimmed.starts_with('{') || trimmed.starts_with('"')) {
            continue;
        }
        assert!(
            documented,
            "every [advisories].ignore entry needs an adjacent explanatory comment; {trimmed:?} \
             has none, and an undocumented waiver is exactly how a supply-chain gate rots"
        );
        documented = false;
    }
}

/// `deny.toml`'s `[licenses.private] ignore = true` only suppresses `error[unlicensed]` for
/// workspace members actually marked unpublishable. Moira declares no `license` field by design
/// (it is a deployed service, not a published crate), so dropping `publish = false` turns the
/// licenses check red — a confusing failure that reads as a dependency problem when it is not.
/// This pins the pairing so the two settings cannot drift apart.
#[test]
fn the_crate_is_marked_unpublishable_so_the_license_gate_stays_green() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        directives(manifest).contains("publish = false"),
        "Cargo.toml must keep `publish = false`: it pairs with `[licenses.private] ignore = true` \
         in deny.toml, and without it `cargo deny check` fails with `error[unlicensed]: moira`"
    );
}
