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
// does: the handler's source must contain a call to `withConsoleSession(`, and
// the exemption list is checked in both directions so an entry that stops being
// justified fails.

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";

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
];

const EXEMPT_PATHS = EXEMPT.map((entry) => entry.path);

interface Handler {
  readonly path: string;
  readonly source: string;
}

function routeHandlers(): Handler[] {
  const found: Handler[] = [];
  const walk = (absolute: string): void => {
    let entries: string[];
    try {
      entries = readdirSync(absolute);
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = join(absolute, entry);
      if (statSync(full).isDirectory()) {
        walk(full);
        continue;
      }
      if (extname(entry) !== ".ts" && extname(entry) !== ".tsx") continue;
      if (!/^route\.tsx?$/.test(entry)) continue;
      found.push({
        path: relative(CONSOLE_ROOT, full).split("/").join("/"),
        source: readFileSync(full, "utf8"),
      });
    }
  };
  walk(API_ROOT);
  return found.sort((a, b) => a.path.localeCompare(b.path));
}

/** Comments stripped: a mention of the guard in prose is not a call. */
function callsGuard(source: string): boolean {
  const code = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
  return new RegExp(`\\b${SESSION_GUARD}\\s*\\(`).test(code);
}

const handlers = routeHandlers();

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
  test("no handler under app/api/** is unguarded and unexplained", () => {
    const unguarded = handlers
      .filter((handler) => !EXEMPT_PATHS.includes(handler.path))
      .filter((handler) => !callsGuard(handler.source))
      .map((handler) => handler.path);
    expect(
      unguarded,
      "these route handlers sit OUTSIDE the (console) session gate — app/api/** is outside " +
        `every route group — and none of them calls ${SESSION_GUARD}(). Either guard them or ` +
        "add them to EXEMPT with a reason.",
    ).toEqual([]);
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
      expect(callsGuard(handler!.source), `${path} does not call ${SESSION_GUARD}()`).toBe(true);
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
