// @server-only
//
// The console's own mutation transport: route handlers under `app/api/**`.
//
// ============================================================================
// WHY ROUTE HANDLERS AND NOT SERVER ACTIONS (plan 09 decision W5-D5)
// ============================================================================
//
// Every mutation this wave adds — create invitation, revoke invitation, transfer
// ownership, revoke a grant, redeem an invitation — is triggered from a
// `"use client"` organism, and `layer-dependencies.test.ts` rule 5 forbids a
// client module from importing any credential-carrying module. `lib/moira-client.ts`
// is in that set by name. So the mutation needs a server-side transport, and
// there were two candidates:
//
//   SERVER ACTIONS — the plan's assumption. There is not one `actions.ts` in
//   this repository, `nextCookies()` is deliberately absent from the Better Auth
//   plugin list (with it installed, the sign-in reply came back with no
//   `Set-Cookie` at all and the callback then failed `state_security_mismatch`),
//   and there is no module-scope `auth` object to import. Adopting them means
//   changing the auth instance, in a wave that should not be touching it.
//
//   ROUTE HANDLERS — the shipped precedent. `SignInPanel` already posts to
//   `POST /api/auth/sign-in/oauth2` with `fetch`, and its header says why.
//
// ============================================================================
// `app/api/**` IS OUTSIDE THE `(console)` SESSION GATE — BY CONSTRUCTION
// ============================================================================
//
// Route groups contribute no URL segment and no layout to `app/api/**`: the
// `(console)` group's `layout.tsx` wraps the pages inside that group and nothing
// else. So a route handler inherits NO session check, and every one of them must
// perform its own.
//
// That is explicit rather than inherited, which is the right direction — but it
// is also exactly the kind of rule that holds until the day somebody adds the
// twelfth handler. `tests/unit/architecture/route-handler-session.test.ts` scans
// `app/api/**` and fails on any handler that neither calls `withConsoleSession`
// nor appears on a named, justified exemption list.
//
// ============================================================================
// WHAT THE CHECK IS AND IS NOT
// ============================================================================
//
// `consoleSessionCheck` runs the same `checkSession` that `jwt.getSubject` runs
// before minting, from the same resolved configuration. **It is not the security
// control.** Moira is: `evaluate_claim_policy` and `admission_policy` apply the
// deployment's `allowed_email_domains` server-side on every claim and every
// redemption, and `require_primary_actor` decides ownership from row state that
// the console cannot see. Deleting this check loses the *explanation* an operator
// gets — a named refusal instead of a bare 403 several screens later — and the
// defence in depth. It does not open anything Moira was relying on it to close.
//
// Finding F25 is the reason that paragraph is written down: `checkSession`
// shipped with eleven green unit assertions and no caller at all, and a reader
// arriving here would reasonably assume the session boundary was already wired.

import "server-only";

import { consoleSessionCheck } from "./auth";
import { consoleRuntime, type ConsoleRuntime } from "./auth-runtime";
import { consoleEnv, type ConsoleEnv } from "./env";
import { isMoiraRequestError, type MoiraError } from "./errors";
import type { MoiraClient } from "./moira-client";
import { moiraClientForSession, type ConsoleSessionIdentity } from "./moira-session";

/** What a guarded handler is handed. Never the raw request headers. */
export interface ConsoleApiContext {
  /** The verified session, as `checkSession` resolved it. */
  readonly identity: ConsoleSessionIdentity;
  /**
   * A Moira client authenticating AS THE SIGNED-IN OPERATOR.
   *
   * Built by `moiraClientForSession`, which deliberately passes no `systemKey`:
   * `MoiraClient` prefers the system key over the bearer token when both are
   * present, so including it would authenticate every admin call as the
   * bootstrap credential instead of as the human — defeating the audit trail,
   * and, on the redemption path, throwing outright (`bearer_only`).
   */
  readonly client: MoiraClient;
  readonly env: ConsoleEnv;
}

export type ConsoleApiHandler = (context: ConsoleApiContext) => Promise<Response>;

/* -------------------------------------------------------------------------- */
/* Test seam                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The three process-wide things a route handler cannot be tested without.
 *
 * ============================================================================
 * WHAT THIS DELIBERATELY DOES **NOT** SUBSTITUTE
 * ============================================================================
 *
 * `consoleSessionCheck`. That is the gate, and a seam that replaced it would
 * turn every "the handler refuses without a session" test into an assertion
 * about the stub — green whether or not the handler calls `withConsoleSession`
 * at all, which is finding F25's exact shape.
 *
 * So a test supplies a real `ConsoleAuth` (the same `createConsoleAuth` the
 * process builds) plus real resolved configs, and the check runs for real
 * against the request's own headers. A request with no session cookie is a 401
 * because the shipped code decided so; a request carrying one minted by the
 * shipped sign-in flow is admitted for the same reason.
 *
 * The seam exists because the other three are unreachable in a unit test:
 * `consoleRuntime()` calls Moira to resolve the configuration, `consoleEnv()`
 * reads `process.env` and refuses to boot without a database in production, and
 * `moiraClientForSession` mints a JWT against a live signing key.
 *
 * Modelled on `setSetupWindowDependenciesForTests` — same shape, same
 * `null`-restores-production contract.
 */
export interface ConsoleApiDependencies {
  readonly runtime: () => Promise<ConsoleRuntime>;
  readonly env: () => ConsoleEnv;
  readonly clientFor: (
    env: ConsoleEnv,
    runtime: Extract<ConsoleRuntime, { ok: true }>,
    headers: Headers,
  ) => MoiraClient;
}

let dependencies: ConsoleApiDependencies | null = null;

/** Install substitutes, or pass `null` to restore the shipped wiring. */
export function setConsoleApiDependenciesForTests(overrides: ConsoleApiDependencies | null): void {
  dependencies = overrides;
}

const NO_STORE = { "cache-control": "no-store" } as const;

/** A keyed JSON body. Never English prose, never a Moira `request_id`. */
function keyed(status: number, code: string, messageKey: string): Response {
  return Response.json({ error: { code, message_key: messageKey } }, { status, headers: NO_STORE });
}

/**
 * A client-safe rendering of a Moira failure.
 *
 * `MoiraError` is already the narrowed, client-safe union — `lib/errors.ts` is
 * the only module that reads `request_id` and `details`, and it does not put
 * either into this shape. Serialising it whole is therefore safe AND is the
 * reason the organisms can render a remedy: the mapping from `(status, code)` to
 * `remedy` happens once, on the server, rather than being re-derived in three
 * components.
 */
export function moiraErrorBody(error: MoiraError): { readonly error: MoiraError } {
  return { error };
}

/** Status for a `MoiraError`, defaulting to 502 for transport-level failures. */
function statusFor(error: MoiraError): number {
  return error.kind === "api" ? error.status : 502;
}

/**
 * Run `handler` only for a session that exists and may act.
 *
 * The three refusal shapes, all keyed and all distinguishable by a client:
 *
 *   503  the deployment has no resolvable auth configuration, or Moira is
 *        unreachable. Not a session problem, and specifically not a 401 — the
 *        caller must not be signed out because the backend is down.
 *   401  no session.
 *   403  a session that may not act: unverified email, a domain outside the
 *        allow-list, no IdP subject, or a provider this configuration does not
 *        contain.
 */
export async function withConsoleSession(
  request: Request,
  handler: ConsoleApiHandler,
): Promise<Response> {
  const wiring = dependencies;
  let runtimeState: ConsoleRuntime;
  try {
    runtimeState = await (wiring === null ? consoleRuntime() : wiring.runtime());
  } catch (error) {
    // A Moira outage lands here, because resolving the configuration means
    // calling Moira. Without this catch it escapes as an unhandled rejection and
    // Next renders a 500 with a stack, which reads as a console bug.
    if (isMoiraRequestError(error)) {
      return keyed(503, "moira_unreachable", error.moiraError.text.messageKey);
    }
    throw error;
  }

  if (!runtimeState.ok) {
    return keyed(503, runtimeState.resolution.problem, runtimeState.resolution.messageKey);
  }

  // THE SESSION CHECK ITSELF IS NEVER SUBSTITUTED. `setConsoleApiDependenciesForTests`
  // replaces the environment, the runtime and the Moira transport — the three
  // things a test cannot supply — and deliberately not this line, so a test that
  // exercises a handler exercises the REAL gate. See that function's header.
  const check = await consoleSessionCheck(runtimeState.auth, runtimeState.configs, request.headers);
  if (!check.ok) {
    const status = check.rejection === "no_session" ? 401 : 403;
    return keyed(status, check.rejection, check.messageKey);
  }

  const env = wiring === null ? consoleEnv() : wiring.env();
  const client =
    wiring === null
      ? moiraClientForSession(env, runtimeState.auth, request.headers)
      : wiring.clientFor(env, runtimeState, request.headers);

  try {
    return await handler({ identity: check.identity, client, env });
  } catch (error) {
    if (isMoiraRequestError(error)) {
      return Response.json(moiraErrorBody(error.moiraError), {
        status: statusFor(error.moiraError),
        headers: NO_STORE,
      });
    }
    throw error;
  }
}

/**
 * Read a JSON body, or `null` when it is absent or unparseable.
 *
 * Returning `null` rather than throwing keeps a malformed body a 400 the handler
 * writes with its own key, instead of a 500 with a stack.
 */
export async function readJsonBody(request: Request): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await request.json();
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** A 400 for a body the console itself rejected, keyed like every other refusal. */
export function badRequest(messageKey: string): Response {
  return keyed(400, "invalid_request", messageKey);
}

/**
 * A 404 the CONSOLE decided, for a nested resource that does not belong to the
 * parent named in the path.
 *
 * Distinct from Moira's own 404 on purpose. "This model id exists, but not under
 * this provider" is a refusal only the console can make — Moira's flat
 * `POST /provider-models/{id}/disable` would happily act on it — and it is the
 * check that stops a request body choosing which row a privileged call touches.
 */
export function notFound(messageKey: string): Response {
  return keyed(404, "not_found", messageKey);
}
