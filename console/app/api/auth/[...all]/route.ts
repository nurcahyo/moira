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

async function handle(request: Request): Promise<Response> {
  let runtimeState: Awaited<ReturnType<typeof consoleRuntime>>;
  try {
    runtimeState = await consoleRuntime();
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
  if (new URL(request.url).pathname === TOKEN_PATH) {
    const check = await consoleSessionCheck(
      runtimeState.auth,
      runtimeState.config,
      request.headers,
    );
    if (!check.ok) return refused(check.rejection, check.messageKey);
  }

  return runtimeState.auth.handler(request);
}

export const GET = handle;
export const POST = handle;
export const PUT = handle;
export const PATCH = handle;
export const DELETE = handle;
