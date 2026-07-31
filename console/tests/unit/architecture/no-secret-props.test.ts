// No secret is passed as a prop into a layer that renders client-side.
//
// CONVENTIONS.md §6 rule 5 (`plans/CONVENTIONS.md:159`), specified as a name
// scan at `plans/08:1438`.
//
// ============================================================================
// THIS GUARD IS VACUOUS BY CONSTRUCTION ON THE DAY IT LANDS
// ============================================================================
//
// The tree has seven `*Props` interfaces and zero secret-shaped members, so
// `violations === []` is true whether or not the scanner works. That is not a
// reason to defer it — it is the reason to land it NOW, with a positive control,
// as the TRIPWIRE for the once-only secret modal rather than as a chore
// alongside it. The controls live in `tests/support/secret-props-fixture.tsx`
// and are asserted in this file.
//
// ============================================================================
// THE DESIGN TENSION, RESOLVED RATHER THAN AVOIDED
// ============================================================================
//
// `OnceOnlySecretModal` exists to receive a plaintext token as a prop. The rule
// as written would therefore fail the one component it was written to protect.
// Both halves of the resolution are required:
//
//   (a) a SINGLE-FILE allow-list, with a companion assertion that it holds at
//       most one entry, that the entry names a file that exists, and — the
//       reverse direction — that the entry is still NEEDED. An allow-list entry
//       whose file no longer declares a secret-shaped prop is a stale carve-out
//       hiding behind a passing test (`src/i18n/catalog/mod.rs:989-995`).
//
//   (b) inside the allowed file, a FLOW rule: the secret may not be handed to
//       any imported component as a prop. This is a heuristic — it matches three
//       spellings and a determined author can defeat it by renaming through an
//       intermediate. THE REAL GATE IS THE E2E NEEDLE: a fixed fake token seeded
//       into the Moira stub's once-only response and added to
//       `forbiddenValues()`, which fails if the value reaches the browser by any
//       route at all. This file is the fast, local, always-on half.

import { describe, expect, test } from "bun:test";

import {
  consoleFileExists,
  extractPropsInterfaces,
  readConsoleFile,
  renderingSourceFiles,
  RENDERING_ROOTS,
  SECRET_PROP_PATTERN,
  secretFlowViolations,
  secretPropViolations,
} from "../../support/secret-props-scan";

/**
 * The one file permitted to name a secret in its props.
 *
 * EMPTY until `modules/secrets/OnceOnlySecretModal.tsx` lands. Adding a second
 * entry fails the assertion below; that is deliberate, because "which component
 * may hold the plaintext" is a one-answer question.
 */
const SECRET_PROP_ALLOW_LIST: readonly string[] = [];

const FIXTURE = "tests/support/secret-props-fixture.tsx";

const files = renderingSourceFiles();
const interfaces = files.flatMap((file) => extractPropsInterfaces(file, readConsoleFile(file)));

describe("the scanner is alive", () => {
  test("it found rendering-layer files", () => {
    expect(files.length, `scanned: ${files.join(", ")}`).toBeGreaterThanOrEqual(7);
  });

  test("it discovered at least seven *Props interfaces", () => {
    // Badge, Button, Input, Label, Spinner, FormField, StatusBadgeGroup. A
    // scanner that stopped matching would report zero interfaces and zero
    // violations, and only this floor would notice.
    const names = interfaces.map((declared) => `${declared.file}:${declared.name}`);
    expect(interfaces.length, `found: ${names.join(", ")}`).toBeGreaterThanOrEqual(7);
  });

  test("it read the members of those interfaces, not just their names", () => {
    // An interface with no members parsed is an interface whose members were
    // never checked.
    const empty = interfaces
      .filter((declared) => declared.members.length === 0)
      .map((declared) => `${declared.file}:${declared.name}`);
    expect(empty, "these *Props interfaces parsed with zero members").toEqual([]);
  });

  test("every rendering root contributed at least one file", () => {
    // `modules/` is empty at this commit, so it is excluded from this assertion
    // by name rather than silently: the moment the first organism lands, the
    // exclusion below has to go, and the test says so.
    for (const root of RENDERING_ROOTS.filter((candidate) => candidate !== "modules")) {
      expect(
        files.some((file) => file.startsWith(`${root}/`)),
        `${root}/ contributed no files — the scan of that layer is vacuous`,
      ).toBe(true);
    }
  });

  test("POSITIVE CONTROL — rule (a) flags the known-bad fixture", () => {
    const fixtureInterfaces = extractPropsInterfaces(FIXTURE, readConsoleFile(FIXTURE));
    const violations = secretPropViolations(fixtureInterfaces);
    const members = violations.map((violation) => violation.member).sort();
    expect(
      members,
      `the fixture declares clientSecret and inviteToken; the scanner reported ${JSON.stringify(
        violations,
      )}`,
    ).toEqual(["clientSecret", "inviteToken"]);
  });

  test("NEGATIVE CONTROL — the fixture's clean interface is not flagged", () => {
    const fixtureInterfaces = extractPropsInterfaces(FIXTURE, readConsoleFile(FIXTURE));
    const clean = fixtureInterfaces.find((declared) => declared.name === "FixtureCleanProps");
    expect(clean, "the fixture's clean interface was not parsed at all").toBeDefined();
    expect(secretPropViolations([clean!])).toEqual([]);
    // `expiresAt` and `onDismiss` are exactly the kind of neighbouring prop a
    // too-broad pattern would swallow.
    expect([...clean!.members].sort()).toEqual(["expiresAt", "label", "onDismiss"]);
  });

  test("the pattern matches every name CONVENTIONS §6 rule 5 lists", () => {
    for (const name of [
      "secret",
      "systemKey",
      "privateKey",
      "clientSecret",
      "apiKey",
      "token",
      "password",
    ]) {
      expect(SECRET_PROP_PATTERN.test(name), `${name} is not matched`).toBe(true);
    }
    for (const name of ["label", "expiresAt", "onDismiss", "variant", "tone"]) {
      expect(SECRET_PROP_PATTERN.test(name), `${name} is falsely matched`).toBe(false);
    }
  });
});

describe("rule (a) — no secret-shaped prop on a rendering layer", () => {
  test("components/atoms/**, components/molecules/** and modules/** are clean", () => {
    const violations = secretPropViolations(
      interfaces.filter((declared) => !SECRET_PROP_ALLOW_LIST.includes(declared.file)),
    );
    const report = violations
      .map((violation) => `  - ${violation.file}: ${violation.interfaceName}.${violation.member}`)
      .join("\n");
    expect(
      violations,
      "these render client-side and must never receive a credential as a prop " +
        `(CONVENTIONS §6 rule 5):\n${report}`,
    ).toEqual([]);
  });
});

describe("the allow-list is exactly one answer, and an honest one", () => {
  test("it holds at most one entry", () => {
    expect(
      SECRET_PROP_ALLOW_LIST.length,
      '"which component may hold the plaintext" is a one-answer question',
    ).toBeLessThanOrEqual(1);
  });

  test("every entry names a file that exists", () => {
    const missing = SECRET_PROP_ALLOW_LIST.filter((file) => !consoleFileExists(file));
    expect(
      missing,
      "an allow-list entry for a file that does not exist is a dead carve-out",
    ).toEqual([]);
  });

  test("every entry is still NEEDED", () => {
    // The reverse direction. An entry whose file no longer declares a
    // secret-shaped prop is a permission nobody is using, and it will silently
    // cover the next file that takes that path.
    const unnecessary = SECRET_PROP_ALLOW_LIST.filter((file) => {
      if (!consoleFileExists(file)) return false; // reported by the test above
      const declared = extractPropsInterfaces(file, readConsoleFile(file));
      return secretPropViolations(declared).length === 0;
    });
    expect(unnecessary, "these allow-list entries carve out nothing — remove them").toEqual([]);
  });
});

describe("rule (b) — an allowed file may hold the secret but never forward it", () => {
  for (const file of SECRET_PROP_ALLOW_LIST) {
    test(`${file} does not hand the secret to a child component`, () => {
      const flows = secretFlowViolations(readConsoleFile(file));
      expect(
        flows,
        "plan 09:324: the token appears exactly once and never as a prop on a reusable " +
          `component beyond that modal's own render:\n  ${flows.join("\n  ")}`,
      ).toEqual([]);
    });
  }

  test("POSITIVE CONTROL — all three forwarding spellings are detected", () => {
    const flows = secretFlowViolations(readConsoleFile(FIXTURE));
    expect(flows.length, `detected: ${flows.join(" | ")}`).toBe(3);
  });

  test("POSITIVE CONTROL — each spelling in isolation", () => {
    expect(secretFlowViolations("<Child secret={secret} />")).toHaveLength(1);
    expect(secretFlowViolations("<Child value={secret} />")).toHaveLength(1);
    expect(secretFlowViolations("<Child {...{ secret }} />")).toHaveLength(1);
  });

  test("NEGATIVE CONTROL — rendering the secret as text is permitted", () => {
    expect(secretFlowViolations("<code>{secret}</code>")).toEqual([]);
    expect(secretFlowViolations("<Child>{null}</Child>\nconst x = secret;")).toEqual([]);
  });
});
