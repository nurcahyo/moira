// Every route handler under `app/api/**` re-checks the session itself.
//
// ============================================================================
// WHY THIS RULE NEEDS A GUARD AND NOT A CONVENTION
// ============================================================================
//
// Route groups contribute no layout to `app/api/**`. `(console)/layout.tsx`
// wraps the PAGES inside its group and nothing else, so a route handler inherits
// no session check at all — and that is by construction, not by oversight:
// `app/api/**` sits outside every group because Next.js puts it there.
//
// Decision W5-D5 chose route handlers over server actions for every mutation in
// this wave, and named the cost in the same sentence: *"each one must therefore
// re-check the session itself, which is explicit rather than inherited."*
//
// An explicit rule holds until the day somebody adds the next handler. This file
// is what makes that day fail loudly instead of shipping an unauthenticated
// mutation endpoint.
//
// ============================================================================
// FINDING F25 IS THE REASON THE RULE IS ABOUT A CALL SITE
// ============================================================================
//
// `checkSession` shipped in wave 3 with eleven green unit assertions and NO
// SHIPPED CALLER: every reference outside its own definition was a test or an
// i18n catalog description. Its unit tests were green before the wiring existed
// and green after, because they construct their own inputs — no assertion in
// them can observe whether the function is ever reached.
//
// So this asserts REACHABILITY, in the same shape `guard-reachability.test.ts`
// does: the handler must contain a call to `withConsoleSession(`, and the
// exemption list is checked in both directions so an entry that stops being
// justified fails.
//
// ============================================================================
// PER **EXPORTED METHOD**, NOT PER FILE — AND THE MUTATION IS WHY
// ============================================================================
//
// The first version of this file scanned each `route.ts` as a whole: "does this
// source contain `withConsoleSession(`". It was mutation-tested by deleting the
// guard from `DELETE` in `app/api/admins/identities/[id]/route.ts` — which
// exports BOTH `PATCH` and `DELETE` — and it stayed **GREEN**, because the
// `PATCH` handler still contained the call.
//
// That is the defect this project has now hit twice (see the ledger's
// "THE JUDGED DESIGN PANEL SPECIFIED A TOOTHLESS GUARD" entry): a guard whose
// granularity cannot represent the state it is named for. A file-level scan
// cannot express "every handler", only "some handler", and the second exported
// method in a file is exactly where an unguarded mutation endpoint would go.
//
// So the body of each exported HTTP handler is extracted by brace matching and
// checked on its own. Re-mutated afterwards: deleting the guard from `DELETE`
// alone now fails, naming the method.

// The EXTRACTOR ITSELF NOW LIVES IN `tests/support/route-exports.ts`. It grew a
// second caller — `tests/unit/api/llm-routes.test.ts`, which needs the same
// answer to decide whether its own handler list is the whole surface — and a
// second transcription of a brace matcher this fiddly is how the two suites
// start disagreeing about what a handler is. The controls below still run here,
// against the shared implementation.

import { describe, expect, test } from "bun:test";
import { join, resolve } from "node:path";

import { exportedHandlers, routeModules, stripComments } from "../../support/route-exports";

const CONSOLE_ROOT = resolve(import.meta.dir, "../../..");
const API_ROOT = join(CONSOLE_ROOT, "app", "api");

/** The guard every handler must reach. */
const SESSION_GUARD = "withConsoleSession";

/**
 * Handlers that legitimately do NOT call `withConsoleSession`, with the reason.
 *
 * Checked in reverse: an entry naming a file that does not exist, or one that
 * DOES call the guard, fails. An exemption list that has stopped carving
 * anything out is a stale carve-out hiding behind a passing test.
 */
const EXEMPT: ReadonlyArray<{ readonly path: string; readonly why: string }> = [
  {
    path: "app/api/auth/[...all]/route.ts",
    why:
      "the Better Auth mount point. It IS the sign-in surface, so requiring a session here would " +
      "make signing in impossible. It runs `consoleSessionCheck` itself on the token path — the " +
      "credential boundary — which is finding F25's actual fix.",
  },
  {
    path: "app/api/health/route.ts",
    why:
      "a liveness probe. It reads nothing, returns no deployment state, and is called by a " +
      "kubelet that has no session and never will.",
  },
  {
    path: "app/api/setup/route.ts",
    why:
      "first-run setup, which by definition runs BEFORE the first admin exists. There is no " +
      "`admin_identities` grant to check against, no operator to be signed in as, and this " +
      "handler's `claim` action is the request that CREATES the first grant — so requiring a " +
      "session here would require the outcome as its own precondition. It runs `withSetupWindow` " +
      "instead, which is narrower rather than weaker: it refuses 404 without the bootstrap " +
      "system key, and 409 once Moira's claim-status reports the deployment claimed — re-read " +
      "from Moira on every request rather than cached. THE THIRD ENTRY, and the ceiling " +
      "assertion below is now binding: a fourth means the shared behaviour belongs behind a " +
      "guarded wrapper.",
  },
];

const EXEMPT_PATHS = EXEMPT.map((entry) => entry.path);

function callsGuard(source: string): boolean {
  const code = stripComments(source);
  return new RegExp(`\\b${SESSION_GUARD}\\s*\\(`).test(code);
}

const handlers = routeModules(API_ROOT, CONSOLE_ROOT);

describe("the scan is alive", () => {
  test("route handlers were found at all", () => {
    // `readdirSync` on a missing directory returns nothing, and every rule below
    // would then pass over an empty set. The floor is the whole reason this test
    // exists before the rules.
    expect(
      handlers.map((handler) => handler.path),
      "no route handlers found under app/api/** — every rule below is vacuous",
    ).not.toEqual([]);
    expect(handlers.length).toBeGreaterThanOrEqual(5);
  });

  test("POSITIVE CONTROL — a handler with no guard call is detected", () => {
    expect(callsGuard('export async function POST() { return Response.json({}); }')).toBe(false);
  });

  test("POSITIVE CONTROL — a mention in a comment is not a call", () => {
    expect(callsGuard("// withConsoleSession(request, handler) would go here\nexport const x = 1;")).toBe(
      false,
    );
  });

  test("NEGATIVE CONTROL — a real call is detected", () => {
    expect(callsGuard("return withConsoleSession(request, async () => new Response());")).toBe(true);
  });
});

describe("every mutation handler re-checks the session", () => {
  test("EVERY exported method is guarded, not merely one per file", () => {
    // Per method. A file-level scan passed while `DELETE` was unguarded and
    // `PATCH` beside it was not — see the header.
    const unguarded = handlers
      .filter((handler) => !EXEMPT_PATHS.includes(handler.path))
      .flatMap((handler) => exportedHandlers(handler.path, handler.source))
      .filter((exported) => !callsGuard(exported.body))
      .map((exported) => `${exported.method} ${exported.file}`);
    expect(
      unguarded,
      "these exported handlers sit OUTSIDE the (console) session gate — app/api/** is outside " +
        `every route group — and do not call ${SESSION_GUARD}(). Either guard them or add the ` +
        "file to EXEMPT with a reason.",
    ).toEqual([]);
  });

  test("the extractor found every exported method, so the rule above is not scanning air", () => {
    // The floor for the new granularity. An extractor that stopped matching
    // would report zero handlers and the rule above would pass on an empty list.
    const extracted = handlers.flatMap((handler) =>
      exportedHandlers(handler.path, handler.source).map(
        (exported) => `${exported.method} ${exported.file}`,
      ),
    );
    // Ten: six of this wave's own handlers plus the four alias exports on the
    // Better Auth mount (`export const GET = handle;`), which are recorded
    // rather than skipped — see `exportedHandlers`.
    expect(extracted.length, `extracted: ${extracted.join(", ")}`).toBeGreaterThanOrEqual(10);
    // The file that produced the finding: two methods, both must be seen.
    expect(extracted).toContain("PATCH app/api/admins/identities/[id]/route.ts");
    expect(extracted).toContain("DELETE app/api/admins/identities/[id]/route.ts");
  });

  test("POSITIVE CONTROL — an unguarded second method in a guarded file is caught", () => {
    const fixture = [
      "export async function PATCH(request: Request) {",
      "  return withConsoleSession(request, async () => new Response());",
      "}",
      "export async function DELETE(request: Request) {",
      "  return Response.json({ ok: true });",
      "}",
    ].join("\n");
    const unguarded = exportedHandlers("fixture/route.ts", fixture)
      .filter((exported) => !callsGuard(exported.body))
      .map((exported) => exported.method);
    expect(unguarded, "the exact mutation a file-level scan missed").toEqual(["DELETE"]);
  });

  test("POSITIVE CONTROL — an alias export is recorded as unguarded, not skipped", () => {
    // `export const DELETE = someHandler;` has no body here. Treating it as
    // "nothing to check" would make the alias form a hole in the rule.
    const extracted = exportedHandlers("fixture/route.ts", "export const DELETE = handle;");
    expect(extracted.map((exported) => exported.method)).toEqual(["DELETE"]);
    expect(callsGuard(extracted[0]!.body)).toBe(false);
  });

  test("NEGATIVE CONTROL — the arrow-function export form is matched too", () => {
    const fixture =
      "export const POST = async (request: Request) => { return withConsoleSession(request, async () => new Response()); };";
    const extracted = exportedHandlers("fixture/route.ts", fixture);
    expect(extracted.map((exported) => exported.method)).toEqual(["POST"]);
    expect(callsGuard(extracted[0]!.body)).toBe(true);
  });

  test("the wave-5 mutation endpoints are all present and all guarded", () => {
    // Named rather than counted. A count survives one handler being deleted
    // while another is added, and these four are the whole mutation surface:
    // create an invitation, withdraw one, transfer ownership, revoke a grant —
    // plus the invitee's own redemption.
    for (const path of [
      "app/api/admins/invites/route.ts",
      "app/api/admins/invites/[id]/revoke/route.ts",
      "app/api/admins/identities/[id]/route.ts",
      "app/api/invite/[token]/redeem/route.ts",
    ]) {
      const handler = handlers.find((candidate) => candidate.path === path);
      expect(handler, `${path} is missing from app/api/**`).toBeDefined();
      const exported = exportedHandlers(path, handler!.source);
      expect(exported.length, `${path} exports no HTTP handler`).toBeGreaterThanOrEqual(1);
      for (const method of exported) {
        expect(
          callsGuard(method.body),
          `${method.method} ${path} does not call ${SESSION_GUARD}()`,
        ).toBe(true);
      }
    }
  });
});

describe("the exemption list is honest in both directions", () => {
  for (const entry of EXEMPT) {
    test(`${entry.path} exists and still needs its exemption`, () => {
      const handler = handlers.find((candidate) => candidate.path === entry.path);
      expect(handler, `${entry.path} is exempt but does not exist — a dead carve-out`).toBeDefined();
      expect(
        callsGuard(handler!.source),
        `${entry.path} now calls ${SESSION_GUARD}(), so its exemption carves out nothing. ` +
          `Reason on record: ${entry.why}`,
      ).toBe(false);
    });
  }

  test("the list is short enough to still be reviewable", () => {
    expect(
      EXEMPT.length,
      "a growing list of unauthenticated API handlers is the thing this guard exists to make " +
        "visible; at four, stop and put the shared behaviour behind a guarded wrapper",
    ).toBeLessThanOrEqual(3);
  });
});
