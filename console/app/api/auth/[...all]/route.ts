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

import { consoleRuntime } from "@/lib/auth-runtime";
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

  return runtimeState.auth.handler(request);
}

export const GET = handle;
export const POST = handle;
export const PUT = handle;
export const PATCH = handle;
export const DELETE = handle;
