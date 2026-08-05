// EVERY fixed TCP port the console's Playwright harness binds, declared once.
//
// ============================================================================
// WHY A REGISTRY AND NOT A CONSTANT NEXT TO EACH SERVER
// ============================================================================
//
// Playwright starts every `webServer` entry in ONE run, on ONE machine. Two
// harnesses that each pick "a port next to 3210" therefore do not merely
// conflict in review — they bind the same socket at the same moment, and the
// second one dies with `EADDRINUSE` in a log nobody reads, some minutes into a
// build.
//
// That is not hypothetical. `e2e/support/authenticated-stack.ts` (issue #75)
// first claimed 3211/3212/3213, and the unmerged setup-wizard harness (issue
// #71) claims 3211/3212 for its own fixture console and stub Moira. Both suites
// were green on their own branches, because neither branch could see the other's
// numbers and nothing in either one asked whether the port was free.
//
// So the numbers live here, together, where a duplicate is visible by reading
// one file — and `assertFixedPortsDisjoint()` below turns "visible" into "the
// config refuses to load". A harness that adds a fixed port and does NOT declare
// it here still gets caught, one layer later, by `assertFixedPortsAreFree()`.
//
// ============================================================================
// ADDING A HARNESS: WHAT TO DO HERE
// ============================================================================
//
//   1. Add its ports to `FIXED_PORTS` with the name of the thing that binds
//      them and why the port cannot be OS-assigned.
//   2. Derive them from `E2E_BASE_PORT` so a whole run can be moved with one
//      environment variable.
//   3. Call `assertFixedPortsAreFree()` in the process that binds them, before
//      it binds anything.
//
// A port that CAN be OS-assigned must be: the mock Moira, the mock IdP and the
// mock OpenAI-compatible endpoint in the authenticated stack all bind port 0 and
// report themselves through the control surface, which is why this list has four
// entries instead of seven.

/**
 * Base for every fixed port in the console's e2e harness.
 *
 * Deliberately not 3000 (a dev `next dev` would clash) and not 3100 (commonly
 * taken by local Docker port mappings). Moving a whole run out of the way is one
 * variable: `CONSOLE_E2E_PORT`.
 */
export const E2E_BASE_PORT = Number(process.env["CONSOLE_E2E_PORT"] ?? 3210);

/** The scaffold console — the `webServer` entry that owns `bun run build`. */
export const SCAFFOLD_CONSOLE_PORT = E2E_BASE_PORT;

/**
 * ============================================================================
 * THE AUTHENTICATED STACK STARTS AT +10, AND THE GAP IS DELIBERATE
 * ============================================================================
 *
 * `E2E_BASE_PORT + 1 .. + 9` is left unclaimed on purpose. The setup-wizard
 * harness on the unmerged `feat/console-setup-wizard-71` binds +1 and +2
 * (`CONSOLE_SETUP_E2E_PORT` = 3211, `CONSOLE_SETUP_E2E_MOIRA_PORT` = 3212), and
 * that branch is awaiting review rather than dead: when it lands, its two
 * entries belong in `FIXED_PORTS` below and must not have to renumber anything
 * to get there.
 *
 * Adjacency was never worth anything here — nothing reads these as a range — and
 * it cost a head-on collision between two suites that could not both run.
 */

/** The browser-facing origin: a TLS terminator in front of the standalone server. */
export const AUTH_CONSOLE_PORT = E2E_BASE_PORT + 10;

/** Where a spec asks the harness what it recorded, and resets it. Plain http, loopback. */
export const AUTH_CONTROL_PORT = E2E_BASE_PORT + 11;

/** The standalone Next server itself. Loopback only; the browser never sees it. */
export const AUTH_CONSOLE_INTERNAL_PORT = E2E_BASE_PORT + 12;

/** One fixed port, and the record of who binds it. */
export interface FixedPortClaim {
  /** The process or server that binds it, named as a reader would look for it. */
  readonly boundBy: string;
  readonly port: number;
  /** Why it cannot be OS-assigned. A claim with no answer here does not belong. */
  readonly why: string;
}

/**
 * Every fixed port in the harness.
 *
 * When the setup-wizard harness lands, its two claims go here:
 *
 *   { boundBy: "the setup-fixture console (setup-fixture.ts)", port: …, why: … }
 *   { boundBy: "the setup-fixture stub Moira (setup-fixture.ts)", port: …, why: … }
 */
export const FIXED_PORTS: readonly FixedPortClaim[] = [
  {
    boundBy: "the scaffold console (playwright.config.ts, webServer entry 0)",
    port: SCAFFOLD_CONSOLE_PORT,
    why: "`use.baseURL` and `webServer.url` have to name it before anything starts",
  },
  {
    boundBy: "the authenticated stack's TLS terminator (authenticated-stack.ts)",
    port: AUTH_CONSOLE_PORT,
    why: "AUTH_CONSOLE_ORIGIN is the console's advertised issuer and JWKS host, and the config declares it before the stack runs",
  },
  {
    boundBy: "the authenticated stack's control surface (authenticated-stack.ts)",
    port: AUTH_CONTROL_PORT,
    why: "a spec has to reach the harness without the harness having told it anything first",
  },
  {
    boundBy: "the authenticated stack's standalone Next server (authenticated-stack.ts)",
    port: AUTH_CONSOLE_INTERNAL_PORT,
    why: "the terminator in front of it is configured with the address before the child is spawned",
  },
];

/**
 * Two claims on one port is a load-time error.
 *
 * Called at import, so every consumer of this module — `playwright.config.ts`
 * included — pays for it. The failure a reviewer would otherwise get is
 * `EADDRINUSE` from whichever server happened to lose the race.
 */
export function assertFixedPortsDisjoint(claims: readonly FixedPortClaim[] = FIXED_PORTS): void {
  const byPort = new Map<number, FixedPortClaim>();
  for (const claim of claims) {
    const existing = byPort.get(claim.port);
    if (existing !== undefined) {
      throw new Error(
        `two e2e harnesses claim port ${claim.port}: "${existing.boundBy}" and ` +
          `"${claim.boundBy}". Playwright starts every webServer entry in one run on one ` +
          "machine, so both would bind the same socket and the loser dies with EADDRINUSE " +
          "some minutes into the build. Move one of them to a free offset from " +
          "CONSOLE_E2E_PORT in e2e/support/fixed-ports.ts.",
      );
    }
    byPort.set(claim.port, claim);
  }
}

assertFixedPortsDisjoint();

/**
 * The ports are actually FREE, checked by binding them.
 *
 * The layer `assertFixedPortsDisjoint()` cannot reach: a harness that took a
 * fixed port without declaring it here, an orphaned stack from a killed run, or
 * anything else on the machine. Binding is the only honest test — a port that
 * answers nothing is not the same as a port that is free — and the socket is
 * released immediately, so this is a diagnostic rather than a reservation.
 */
export async function assertFixedPortsAreFree(claims: readonly FixedPortClaim[]): Promise<void> {
  for (const claim of claims) {
    let probe: { stop: (closeActiveConnections?: boolean) => void };
    try {
      probe = Bun.serve({
        port: claim.port,
        hostname: "127.0.0.1",
        fetch: () => new Response(null, { status: 204 }),
      });
    } catch (error) {
      throw new Error(
        `port ${claim.port} is already bound, and ${claim.boundBy} needs it (${claim.why}). ` +
          "Something else in this Playwright run, or left over from a previous one, is on it — " +
          "the setup-wizard fixture (CONSOLE_SETUP_E2E_PORT / CONSOLE_SETUP_E2E_MOIRA_PORT) and " +
          "an orphaned authenticated stack are the two candidates worth checking first. Every " +
          "fixed port belongs in FIXED_PORTS in e2e/support/fixed-ports.ts; move a run out of " +
          `the way with CONSOLE_E2E_PORT. Underlying error: ${(error as Error).message}`,
      );
    }
    probe.stop(true);
  }
}
