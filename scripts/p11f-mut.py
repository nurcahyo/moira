#!/usr/bin/env python3
"""Apply one named mutation probe for plan 11 Sub-Phase F, or exit non-zero.

**Exits 2 when the anchor does not match.** That is the whole reason this is a file rather
than a heredoc: the first version of this harness embedded the anchors in `<<'PY'` heredocs,
one anchor's escaping was wrong, the `assert` raised, the driver had no `set -e`, and the
probe then ran against *unmutated* code and reported `SURVIVED`. A mutation harness that
reports a false survivor is the same failure it exists to detect, aimed at itself.
"""

import sys

CONV = "src/application/conversation.rs"
EXTR = "src/application/memory_extraction.rs"
REPO = "src/infra/repositories/conversation.rs"

CONSENT_MATCH = """    match mode {
        MemoryConsentMode::Disabled => None,
        MemoryConsentMode::ExplicitOnly => Some(MemoryStatus::Candidate),
        MemoryConsentMode::AutomaticWithUserControls | MemoryConsentMode::ApplicationManaged => {
            Some(MemoryStatus::Active)
        }
    }"""

NEAR_DUP = """pub fn is_near_duplicate(distance: f64, threshold: f64) -> bool {
    distance <= threshold
}"""

PROBES = {
    # name: (file, anchor, replacement)
    "M1": (
        EXTR,
        CONSENT_MATCH,
        "    let _ = mode;\n    Some(MemoryStatus::Active)",
    ),
    "M2": (
        EXTR,
        "        status_for_consent_mode(conversation_mode),\n"
        "        status_for_consent_mode(memory_mode),",
        "        status_for_consent_mode(memory_mode),\n"
        "        status_for_consent_mode(memory_mode),",
    ),
    "M3": (
        EXTR,
        NEAR_DUP,
        "pub fn is_near_duplicate(distance: f64, threshold: f64) -> bool {\n"
        "    let _ = (distance, threshold);\n    false\n}",
    ),
    "M4": (
        EXTR,
        NEAR_DUP,
        "pub fn is_near_duplicate(distance: f64, threshold: f64) -> bool {\n"
        "    distance < threshold\n}",
    ),
    "M5": (
        REPO,
        """const DEDUPE_STATUSES: &str = "('active', 'candidate')";""",
        """const DEDUPE_STATUSES: &str = "('active')";""",
    ),
    "M6": (
        REPO,
        "              m.application_id = $2\n              and (",
        "              ($2 is not null or $2 is null)\n              and (",
    ),
    "M7": (
        EXTR,
        "    if candidate.confidence < policy.minimum_extraction_confidence {",
        "    if false {",
    ),
    # The transcript moves into Moira's own instruction slot. Written with an explicit
    # backslash-n so no quoting layer can mangle it — the escaping is exactly what broke the
    # first harness.
    "M8": (
        EXTR,
        'DomainMessage::user(format!("{EXTRACTION_SOURCE_LABEL}\\n{}", transcript.trim_end()))',
        'DomainMessage::system(format!("{EXTRACTION_SOURCE_LABEL}\\n{}", transcript.trim_end()))',
    ),
    "M9": (
        CONV,
        "                contradicts = Some(existing);\n                contradictions += 1;",
        "                let _ = existing;",
    ),
    "M10": (
        REPO,
        "              and m.content_hash = $1\n",
        "              and m.content_hash = $1 and false\n",
    ),
    # The secret-shaped-content refusal is removed.
    "M11": (
        EXTR,
        "    if contains_secret_like_text(content) {",
        "    if false {",
    ),
    # Every candidate is written regardless of the run cap.
    "M12": (
        EXTR,
        "pub const MAXIMUM_CANDIDATES_PER_RUN: usize = 16;",
        "pub const MAXIMUM_CANDIDATES_PER_RUN: usize = 100_000;",
    ),
}


def main() -> int:
    name = sys.argv[1]
    if name not in PROBES:
        print(f"unknown probe {name}", file=sys.stderr)
        return 2
    path, anchor, replacement = PROBES[name]
    source = open(path).read()
    if anchor not in source:
        print(f"{name}: ANCHOR MISSING in {path}", file=sys.stderr)
        return 2
    if source.count(anchor) != 1:
        print(
            f"{name}: anchor matches {source.count(anchor)} times in {path}; "
            "a probe that mutates more than one site proves nothing about either",
            file=sys.stderr,
        )
        return 2
    open(path, "w").write(source.replace(anchor, replacement, 1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
