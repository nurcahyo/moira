// A guard nobody has seen fail is an assumption. A guard nothing CALLS is worse.
//
// ============================================================================
// FINDING F25 — WHAT THIS FILE EXISTS FOR
// ============================================================================
//
// `checkSession` (`lib/moira-session.ts`) is the console's session-boundary gate
// and the only caller of `isEmailDomainAllowed`. It shipped in wave 3 with
// eleven green unit assertions in `tests/unit/lib/moira-session.test.ts` — and
// **no shipped caller at all**. Every reference outside its own definition was a
// test, or an i18n catalog *description*. It was dead code that read as coverage.
//
// The divergence is the point: `moira-session.test.ts` was green before the
// wiring existed and is green after, because it constructs its own inputs. No
// assertion in it can tell whether the function it exercises is ever reached.
// That is exactly the gap this file closes, and the reason the rule is about
// REACHABILITY rather than about behaviour.
//
// ============================================================================
// THE TWO HALVES, AND WHY NEITHER IS SUFFICIENT
// ============================================================================
//
//   1. Each guarded symbol is CALLED from at least one shipped file that is not
//      the one defining it. Deleting the call site turns this red.
//   2. The defining module is REACHABLE from `app/**` through value imports.
//      Without this, a call site inside an orphaned module would satisfy (1)
//      while nothing on any request path ever ran it.
//
// ============================================================================
// THE FLOOR — copied from `layer-dependencies.test.ts`, deliberately
// ============================================================================
//
// `listSourceFiles` returns `[]` for a directory it cannot read, so every rule
// here would pass vacuously against an empty scan. That file documents the same
// defect about itself ("a rule over an empty directory is worse than no rule,
// because it reads as coverage") and backs its rules with per-layer counts. The
// floors below are the same device: empty the scan set and the floor fails
// before any rule gets a chance to pass on nothing.

import { describe, expect, test } from "bun:test";

import {
  allSourceFiles,
  directImports,
  type SourceFile,
} from "../../support/server-only-derivation";

const files = allSourceFiles();

/* -------------------------------------------------------------------------- */
/* The guards, and where each is defined                                      */
/* -------------------------------------------------------------------------- */

interface Guard {
  /** The exported function that must be reached. */
  readonly symbol: string;
  /** The module that defines it. References here do not count as call sites. */
  readonly definedIn: string;
  /** Why this one is load-bearing, quoted in the failure message. */
  readonly why: string;
}

const GUARDS: readonly Guard[] = [
  {
    symbol: "checkSession",
    definedIn: "lib/moira-session.ts",
    why: "F25: the console's email-domain and email-verification gate. Unreached, an operator outside allowed_email_domains passes console sign-in and meets a bare 403 from Moira several screens later.",
  },
  {
    symbol: "assertAdmissibleSession",
    definedIn: "lib/moira-session.ts",
    why: "F25: the throwing form checkSession is wired into jwt.getSubject with. It is the single function every minted Moira-bound token passes through.",
  },
  {
    symbol: "consoleSessionCheck",
    definedIn: "lib/auth.ts",
    why: "F25/G12: the request-level form. Its call sites are the token route (which owes the caller a keyed 403 rather than a 500) and the console layout. Both must exist; a layout-only wiring redirects the browser while GET /api/auth/token keeps minting for the same cookie.",
  },
];

// `readIdpSubject` is deliberately NOT in this list, even though it is equally
// load-bearing. It is defined and called inside `lib/auth.ts` alone, so the rule
// below — "called from a file that is not the defining one" — cannot see it, and
// a version relaxed enough to include it would pass on a second intra-module
// call after the first was deleted. A rule that cannot fail for a member is
// worse than that member's absence: it reads as coverage. The property is
// carried instead by `tests/integration/oauth-flow.test.ts`, which mints through
// the real plugin and asserts the `sub`.

/* -------------------------------------------------------------------------- */
/* Reachability                                                               */
/* -------------------------------------------------------------------------- */

/** Every module reachable from `app/**` through value imports, plus `app/**` itself. */
function reachableFromApp(scan: readonly SourceFile[]): Set<string> {
  const reached = new Set<string>(
    scan.filter((file) => file.path.startsWith("app/")).map((file) => file.path),
  );
  // Bounded rather than `while (true)`, matching `deriveCredentialModules`: a
  // cycle plus a bug would otherwise spin forever inside a test.
  for (let pass = 0; pass < scan.length + 1; pass += 1) {
    let grew = false;
    for (const file of scan) {
      if (!reached.has(file.path)) continue;
      for (const target of directImports(scan, file, { includeTypeOnly: false })) {
        if (!reached.has(target)) {
          reached.add(target);
          grew = true;
        }
      }
    }
    if (!grew) break;
  }
  return reached;
}

/**
 * Files that CALL `symbol`, excluding the module that defines it.
 *
 * Matched as `symbol(` on comment-stripped source, so a mention in a doc comment
 * — which is exactly what F25 found instead of a call — does not count. The
 * import statement alone does not count either: `import { checkSession }` has no
 * following `(`.
 */
function callSites(scan: readonly SourceFile[], guard: Guard): string[] {
  const call = new RegExp(`\\b${guard.symbol}\\s*\\(`);
  return scan
    .filter((file) => file.path !== guard.definedIn && call.test(file.code))
    .map((file) => file.path);
}

/* -------------------------------------------------------------------------- */

describe("the scan is alive", () => {
  test("floors — an empty or truncated walk fails here, before any rule passes on nothing", () => {
    expect(files.length, "the shipped source walk returned almost nothing").toBeGreaterThanOrEqual(
      25,
    );
    expect(
      files.filter((file) => file.path.startsWith("app/")).length,
      "no app/ entry points were scanned; every reachability rule below is vacuous",
    ).toBeGreaterThanOrEqual(4);
    expect(
      files.filter((file) => file.path.startsWith("lib/")).length,
      "lib/",
    ).toBeGreaterThanOrEqual(10);
    expect(
      reachableFromApp(files).size,
      "nothing is reachable from app/ — the import graph did not resolve, so 'reachable' " +
        "would be trivially false and 'unreachable' trivially true",
    ).toBeGreaterThanOrEqual(8);
  });

  test("POSITIVE CONTROL — a symbol nothing calls has no call sites", () => {
    expect(
      callSites(files, {
        symbol: "aGuardNothingCalls",
        definedIn: "lib/moira-session.ts",
        why: "fixture",
      }),
    ).toEqual([]);
  });

  test("POSITIVE CONTROL — a mention in a comment is not a call site", () => {
    const commented: SourceFile = {
      path: "lib/fixture-comment.ts",
      source: "// checkSession(session, config) would go here\nexport const x = 1;",
      code: "\nexport const x = 1;",
    };
    expect(
      callSites([commented], {
        symbol: "checkSession",
        definedIn: "lib/moira-session.ts",
        why: "fixture",
      }),
      "F25 found exactly this: references that were comments and catalog descriptions",
    ).toEqual([]);
  });

  test("NEGATIVE CONTROL — a real call is a call site", () => {
    const caller: SourceFile = {
      path: "lib/fixture-caller.ts",
      source: 'import { checkSession } from "./moira-session";\nexport const c = checkSession(null, x);',
      code: 'import { checkSession } from "./moira-session";\nexport const c = checkSession(null, x);',
    };
    expect(
      callSites([caller], {
        symbol: "checkSession",
        definedIn: "lib/moira-session.ts",
        why: "fixture",
      }),
    ).toEqual(["lib/fixture-caller.ts"]);
  });
});

describe("every named guard is reachable from shipped code", () => {
  const reached = reachableFromApp(files);

  for (const guard of GUARDS) {
    test(`${guard.symbol} is defined in a module app/** reaches`, () => {
      expect(
        files.map((file) => file.path),
        `${guard.definedIn} is not in the scan set — the guard below would pass on a typo`,
      ).toContain(guard.definedIn);
      expect(reached, `${guard.definedIn} is unreachable from app/**. ${guard.why}`).toContain(
        guard.definedIn,
      );
    });

    test(`${guard.symbol} has at least one shipped call site`, () => {
      expect(
        callSites(files, guard),
        `nothing outside ${guard.definedIn} calls ${guard.symbol}(). ${guard.why}\n` +
          "Its unit tests will stay green either way — they construct their own inputs and " +
          "cannot observe whether the function is ever reached. That is finding F25.",
      ).not.toEqual([]);
    });
  }

  test("checkSession is wired at the CREDENTIAL boundary, not only at a page", () => {
    // The distinction G12 exists for. `app/(console)/layout.tsx` gating the shell
    // is the UX half; it redirects a browser while `GET /api/auth/token` keeps
    // minting for the same cookie. `lib/auth.ts` is where `jwt.getSubject` lives,
    // and that is the function every minted token passes through — including
    // every server-side `mintMoiraToken`, which no page layout can intercept.
    const sites = callSites(files, GUARDS[0]!).concat(callSites(files, GUARDS[1]!));
    expect(
      sites,
      "the session check reaches no module under lib/ — a page-level-only wiring leaves the " +
        "token endpoint open to exactly the sessions the page refuses",
    ).toContain("lib/auth.ts");
  });
});
