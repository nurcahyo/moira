// No English is rendered from a component; it all comes from the catalog.
//
// ============================================================================
// WHY THIS FILE CARRIES A POSITIVE CONTROL
// ============================================================================
//
// A regex that stops matching finds zero violations and the suite goes green.
// That failure is indistinguishable from success by every assertion that only
// says "violations === []", and this project has already shipped one of them (a
// metrics assertion that matched nothing because of a global label).
//
// So there are two counter-measures, and the guard is worthless without both:
//
//   (a) FLOORS — the scan must have found at least seven component files and at
//       least one JSX text node overall. A walk that returned nothing fails.
//   (b) A POSITIVE CONTROL — `tests/support/hardcoded-copy-fixture.tsx` contains
//       one instance of each violation the scanner claims to detect, and the
//       scanner must report all of them. Without this, the suite proves only
//       that the parser ran.
//
// The allowlist is checked in REVERSE as well: an entry that permits nothing is
// a stale carve-out hiding behind a passing test
// (`src/i18n/catalog/mod.rs:989-995`).

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  ALLOWED_GLYPHS,
  CONSOLE_ROOT,
  scanComponentTree,
  scanSourceForCopy,
} from "../../support/copy-scan";

const result = scanComponentTree();

const FIXTURE = "tests/support/hardcoded-copy-fixture.tsx";

describe("the scanner is alive", () => {
  test("it found component files to scan", () => {
    expect(
      result.filesScanned.length,
      `scanned ${result.filesScanned.length} files: ${result.filesScanned.join(", ")}`,
    ).toBeGreaterThanOrEqual(7);
  });

  test("it saw JSX text nodes at all", () => {
    // Zero here means the JSX_TEXT regex stopped matching, at which point
    // "no violations" says nothing.
    expect(result.jsxTextNodesSeen).toBeGreaterThan(0);
  });

  test("POSITIVE CONTROL — the known-bad fixture is fully reported", () => {
    const source = readFileSync(join(CONSOLE_ROOT, FIXTURE), "utf8");
    const violations = scanSourceForCopy(FIXTURE, source);
    const kinds = new Set(violations.map((violation) => violation.kind));

    expect(
      [...kinds].sort(),
      `the fixture contains one of each violation kind; the scanner reported ${JSON.stringify(
        violations,
      )}`,
    ).toEqual(["a11y-prop", "default-parameter", "jsx-text"]);

    const texts = violations.map((violation) => violation.text);
    expect(texts).toContain("Contact your administrator before retrying this operation.");
    expect(texts).toContain('aria-label="Dismiss this banner"');
    expect(texts).toContain('title="Failure diagram"');
    expect(texts).toContain('heading = "Something went wrong"');
  });

  test("POSITIVE CONTROL — the fixture's `alt` literal is caught too", () => {
    const source = readFileSync(join(CONSOLE_ROOT, FIXTURE), "utf8");
    const texts = scanSourceForCopy(FIXTURE, source).map((violation) => violation.text);
    expect(texts).toContain('alt="A diagram of the failure"');
  });

  test("the fixture is NOT inside the real scan set", () => {
    // If it ever were, this suite would fail on its own control, which reads as
    // a broken guard rather than as a misplaced file.
    expect(result.filesScanned).not.toContain(FIXTURE);
  });
});

describe("the allowlist is honest in both directions", () => {
  test("every allowlist entry still permits something", () => {
    // An entry matching nothing is a carve-out for a case that no longer exists.
    // `*` is the Label atom's required glyph — the one real case today.
    const dead = ALLOWED_GLYPHS.filter((glyph) => (result.allowlistHits[glyph] ?? 0) === 0);
    expect(
      dead,
      "these allowlist glyphs suppress nothing in the tree — remove the carve-outs. " +
        `Hits: ${JSON.stringify(result.allowlistHits)}`,
    ).toEqual([]);
  });

  test("a permitted glyph is genuinely not reported", () => {
    const violations = scanSourceForCopy("fixture.tsx", "<span> * </span>");
    expect(violations).toEqual([]);
  });

  test("the allowlist does not swallow a real word", () => {
    const violations = scanSourceForCopy("fixture.tsx", "<span>Retry</span>");
    expect(violations.map((violation) => violation.kind)).toEqual(["jsx-text"]);
  });
});

describe("components render no hardcoded copy", () => {
  test("components/**, modules/** and app/** are clean", () => {
    const report = result.violations
      .map((violation) => `  - ${violation.file} [${violation.kind}]: ${violation.text}`)
      .join("\n");
    expect(
      result.violations,
      "these render English directly instead of resolving it through lib/i18n. Add a key to " +
        `lib/i18n/keys.ts and an entry to lib/i18n/catalog.en.ts:\n${report}`,
    ).toEqual([]);
  });
});

describe("expression containers are not mistaken for text", () => {
  test("`{t(KEY)}` is not reported", () => {
    expect(
      scanSourceForCopy("fixture.tsx", "<p>{t(CONSOLE_MESSAGE_KEYS.page_home_body)}</p>"),
    ).toEqual([]);
  });

  test("a className is not reported", () => {
    expect(scanSourceForCopy("fixture.tsx", '<p className="body text-large">{x}</p>')).toEqual([]);
  });

  test("a prose comment is not reported", () => {
    expect(
      scanSourceForCopy(
        "fixture.tsx",
        "// Contact your administrator before retrying.\nconst a = 1;",
      ),
    ).toEqual([]);
  });
});
