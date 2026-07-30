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

async function handle(request: Request): Promise<Response> {
  const runtimeState = await consoleRuntime();

  if (!runtimeState.ok) {
    // A configuration problem, not a crash: "no provider is enabled yet" is the
    // normal first-run state. The body carries the i18n KEY only — never
    // English prose, and never a Moira `request_id` or `details`.
    return Response.json(
      {
        error: {
          code: runtimeState.resolution.problem,
          message_key: runtimeState.resolution.messageKey,
        },
      },
      { status: 503, headers: { "cache-control": "no-store" } },
    );
  }

  return runtimeState.auth.handler(request);
}

export const GET = handle;
export const POST = handle;
export const PUT = handle;
export const PATCH = handle;
export const DELETE = handle;
