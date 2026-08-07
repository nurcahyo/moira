// The Better Auth mount point.
//
// Everything under `/api/auth/*` is served from here: `sign-in/oauth2`, the
// `oauth2/callback/:providerId` the IdP redirects to, `get-session`, `token`,
// `sign-out`, and the JWKS document at `/api/auth/.well-known/jwks.json`.
//
// WHY A HAND-WRITTEN HANDLER RATHER THAN `toNextJsHandler(auth)`. That helper
// takes a ready-made `auth` object at module scope. This console does not have
// one: its provider configuration lives in Moira's DB-backed settings and is
// resolved per request (see `lib/auth-runtime.ts`), so the instance has to be
// obtained inside the handler. The helper's only other contribution is mapping
// the five HTTP verbs onto `auth.handler`, which is the four lines below.
//
// The handler's own `Response` carries `Set-Cookie` directly. `nextCookies()` is
// deliberately absent from the plugin list — see the note in `lib/auth.ts`.

import { consoleSessionCheck } from "@/lib/auth";
import { consoleRuntime } from "@/lib/auth-runtime";
import { AUTH_BASE_PATH } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

/**
 * Node runtime, not Edge.
 *
 * The console signs with `node:crypto` through `jose`, seals the OAuth client
 * secret with AES-256-GCM, and (in a durable deployment) talks to Postgres.
 * None of that runs on the Edge runtime.
 */
export const runtime = "nodejs";

/** Session state and OAuth callbacks are request-scoped; nothing here is static. */
export const dynamic = "force-dynamic";

/**
 * A 503 whose body is an i18n KEY and nothing else.
 *
 * Never English prose, and never a Moira `request_id` or `details` — those are
 * server-diagnostic material and `lib/errors.ts` is the only module allowed to
 * read them.
 */
function unavailable(code: string, messageKey: string): Response {
  return Response.json(
    { error: { code, message_key: messageKey } },
    { status: 503, headers: { "cache-control": "no-store" } },
  );
}

/**
 * The jwt plugin's token endpoint.
 *
 * `GET`, not `POST` — `better-auth/plugins/jwt`'s `getToken` is
 * `createAuthEndpoint("/token", { method: "GET", ... })`. Matched on the path alone anyway,
 * so a future verb change cannot slip the gate.
 */
const TOKEN_PATH = `${AUTH_BASE_PATH}/token`;

/**
 * A refusal whose body carries an i18n KEY and, deliberately, **no `token` field**.
 *
 * `401` for "there is no session" and `403` for "there is a session and it may not act" —
 * the ordinary distinction, and the one a client needs to decide between "sign in" and
 * "ask your operator to add your domain".
 */
function refused(rejection: string, messageKey: string): Response {
  const status = rejection === "no_session" ? 401 : 403;
  return Response.json(
    { error: { code: rejection, message_key: messageKey } },
    { status, headers: { "cache-control": "no-store" } },
  );
}

/* -------------------------------------------------------------------------- */
/* "The configuration names an endpoint nothing answers on" (issue #152)      */
/* -------------------------------------------------------------------------- */

/**
 * Codes that mean **no HTTP response happened at all**.
 *
 * ============================================================================
 * TWO RUNTIMES, TWO SPELLINGS, AND BOTH ARE LOAD-BEARING
 * ============================================================================
 *
 * The console SHIPS on Node — `package.json` pins `node >=24 <25` and the image
 * runs `next start` — where fetch is undici and a refused connection arrives as
 * `TypeError: fetch failed` with an `errno` code on the cause chain, wrapped in
 * an `AggregateError` when a hostname resolved to several addresses and every
 * one was refused. That is verbatim what #152's reproduction showed.
 *
 * The console is TESTED on Bun (`bun test`), whose fetch throws its own error
 * with `code: "ConnectionRefused"` and `errno: 0` — no `TypeError`, no errno
 * string. Verified, not assumed: `tests/unit/api/auth-route.test.ts` dials a
 * closed port for real, and this set was written from what actually arrived.
 *
 * So both spellings are here deliberately. Keeping only Node's would leave the
 * production behaviour untestable; keeping only Bun's would leave it unreachable
 * in production, which is #125's `accept_legacy_hashes` shape — a mechanism
 * wired to nothing that reads it.
 *
 * The TLS codes belong here too. "The certificate expired" is not a transport
 * blip and not a console bug; it is the same operator-facing fact as a refused
 * connection — the endpoint in the configuration cannot be used as configured.
 */
const UNREACHABLE_CODES: ReadonlySet<string> = new Set([
  // Node / undici.
  "ECONNREFUSED",
  "ECONNRESET",
  "ENOTFOUND",
  "EAI_AGAIN",
  "EHOSTUNREACH",
  "ENETUNREACH",
  "ETIMEDOUT",
  "EPIPE",
  "UND_ERR_CONNECT_TIMEOUT",
  "UND_ERR_SOCKET",
  // Bun.
  "ConnectionRefused",
  "ConnectionClosed",
  "FailedToOpenSocket",
  // TLS, on either.
  "CERT_HAS_EXPIRED",
  "DEPTH_ZERO_SELF_SIGNED_CERT",
  "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
  "SELF_SIGNED_CERT_IN_CHAIN",
]);

/** How far down `cause`/`errors` to look before giving up. */
const CAUSE_DEPTH = 5;

/**
 * Did this failure happen because the console could not REACH the identity
 * provider its configuration names?
 *
 * ============================================================================
 * WHY THIS IS NARROW, AND MUST STAY NARROW
 * ============================================================================
 *
 * A predicate that answered `true` for any thrown `Error` would convert every
 * genuine console bug inside Better Auth's handler into a soothing "check your
 * provider's endpoints" — which is #152's defect with the arrow reversed: an
 * operator sent to look at configuration that is fine. So this matches only the
 * two signatures that mean the network never carried a request: undici's exact
 * `TypeError: fetch failed`, and a recognised `errno` on the cause chain.
 *
 * NOTHING FROM THE ERROR REACHES THE RESPONSE. Not the message, not the URL, not
 * the code — the same rule `toTransportError` states in `lib/errors.ts` and
 * `SignInPanel` restates: a thrown fetch error can carry a URL with credentials
 * in it. The return value is a boolean for exactly that reason.
 */
export function isProviderUnreachable(error: unknown, depth: number = CAUSE_DEPTH): boolean {
  if (depth < 0 || error === null || typeof error !== "object") return false;

  const code = (error as { code?: unknown }).code;
  if (typeof code === "string" && UNREACHABLE_CODES.has(code)) return true;

  // undici's own wrapper. Its `cause` usually carries the code as well, but not
  // on every platform and not for every failure mode, so the message is matched
  // too — exactly, never as a substring of arbitrary prose.
  if (error instanceof TypeError && error.message === "fetch failed") return true;

  // `AggregateError` first: one hostname, several addresses, each refused. Its
  // `errors` is where the codes are and its own `cause` is usually empty.
  const aggregated = (error as { errors?: unknown }).errors;
  if (Array.isArray(aggregated)) {
    if (aggregated.some((inner) => isProviderUnreachable(inner, depth - 1))) return true;
  }

  return isProviderUnreachable((error as { cause?: unknown }).cause, depth - 1);
}

/**
 * How the handler obtains the current configuration.
 *
 * Injectable for one reason: `tests/support/console-server.ts` used to bind
 * `auth.handler` to a socket directly, so every wire-level test in
 * `tests/integration/oauth-flow.test.ts` bypassed this file entirely — including
 * the token-endpoint refusal below, which is precisely what those tests exist to
 * exercise. A harness that reimplements the handler proves the harness works.
 *
 * The seam is the runtime resolution and nothing else: the harness supplies an
 * already-resolved `{ auth, config }`, and every line of policy below is the
 * shipped one.
 */
type ConsoleRuntimeState = Awaited<ReturnType<typeof consoleRuntime>>;

// Declared as a call-signature interface rather than as `type R = () => Promise<S>`.
// `tests/support/copy-scan.ts` looks for `>text<` and would read the `>` of the fat arrow
// followed by ` Promise` followed by `<` as a JSX text node — "Promise" is capitalised and
// long enough to satisfy `looksLikeCopy`, and none of the keywords in `CODE_TOKENS` appears
// between the brackets. Worked around here rather than by adding `Promise` to that list:
// widening a copy gate so that new code fits through it is how a gate stops holding.
export interface ConsoleRuntimeResolver {
  (): Promise<ConsoleRuntimeState>;
}

export async function handleAuthRequest(
  request: Request,
  resolveRuntime: ConsoleRuntimeResolver = consoleRuntime,
): Promise<Response> {
  let runtimeState: ConsoleRuntimeState;
  try {
    runtimeState = await resolveRuntime();
  } catch (error) {
    // Resolving the configuration means calling Moira, so a Moira outage lands
    // here. Without this catch it escapes as an unhandled rejection and Next
    // renders a 500 with a stack — which reads as a console bug rather than as
    // "the backend is down", and buries the one fact an operator needs.
    if (isMoiraRequestError(error)) {
      return unavailable("moira_unreachable", error.moiraError.text.messageKey);
    }
    throw error;
  }

  if (!runtimeState.ok) {
    // A configuration problem, not a crash: "no provider is enabled yet" is the
    // normal first-run state, and the whole of the setup wizard's reason to
    // exist.
    return unavailable(runtimeState.resolution.problem, runtimeState.resolution.messageKey);
  }

  // ------------------------------------------------------------------------
  // FINDING F25 — the token endpoint is a credential boundary, so it gets the
  // console's own allow-list before Better Auth ever reaches the signer.
  // ------------------------------------------------------------------------
  //
  // `jwt.getSubject` (`lib/auth.ts`) enforces the same rule and is the backstop
  // that covers every OTHER route to a token, including server-side
  // `mintMoiraToken`. But `getSubject` can only signal by throwing, and an
  // uncaught throw inside a Better Auth endpoint renders as a 500 with no code
  // — an operator outside the allow-list would be told the console is broken.
  //
  // So the check runs here as well, before delegating, to produce a NAMED
  // refusal. The two are not redundant: delete this and the endpoint still
  // refuses, but opaquely; delete `getSubject`'s and every other minting path
  // is open.
  //
  // From wave 4B the check resolves the AUTHENTICATING provider from the session
  // itself and applies that provider's allow-list. Passing "the" configuration
  // is no longer possible: there are N, each with its own trusted issuer row and
  // therefore its own `admission_policy` lookup in Moira, and enforcing the
  // wrong one here would disagree with the server that decides the claim.
  if (new URL(request.url).pathname === TOKEN_PATH) {
    const check = await consoleSessionCheck(
      runtimeState.auth,
      runtimeState.configs,
      request.headers,
    );
    if (!check.ok) return refused(check.rejection, check.messageKey);
  }

  try {
    return await runtimeState.auth.handler(request);
  } catch (error) {
    // ----------------------------------------------------------------------
    // ISSUE #152's THIRD ACCEPTANCE CRITERION.
    // ----------------------------------------------------------------------
    //
    // `POST /api/auth/sign-in/oauth2` dials the discovery, authorization, token
    // or userinfo URL out of the resolved configuration. When that configuration
    // has been superseded in Moira, the endpoint it names may no longer exist —
    // and what escaped this line was a bare `TypeError: fetch failed` with an
    // `ECONNREFUSED` inside it, which Next rendered as a 500 with a stack.
    //
    // Nothing in that names configuration. It reads as "the identity provider is
    // down", which sent the operator to check an IdP that was fine while the
    // actual remedy — the console is serving a configuration that no longer
    // matches Moira — went unmentioned. The TTL and `invalidateAuthConfig` are
    // what stop the console GETTING here; this is what it says if it does, and
    // it stays useful for the ordinary case of a genuinely mistyped endpoint.
    //
    // 503, matching `unavailable` above: the deployment cannot serve sign-in
    // right now, and the caller must not be signed out over it.
    if (isProviderUnreachable(error)) {
      return unavailable(
        "auth_provider_unreachable",
        CONSOLE_MESSAGE_KEYS.auth_provider_unreachable,
      );
    }
    throw error;
  }
}

const handle = (request: Request): Promise<Response> => handleAuthRequest(request);

export const GET = handle;
export const POST = handle;
export const PUT = handle;
export const PATCH = handle;
export const DELETE = handle;
