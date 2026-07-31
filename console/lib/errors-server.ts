// @server-only
//
// The half of Moira's error envelope that must never cross to the browser.
//
// ============================================================================
// WHY THIS IS A SEPARATE MODULE FROM `lib/errors.ts`
// ============================================================================
//
// `lib/errors.ts` deliberately carries NO `import "server-only"`: `MoiraError`
// is the client-safe union and has to be importable from a client component.
// That is correct, and it is exactly what made keeping `serverDiagnostics()`
// there unsafe.
//
// `serverDiagnostics()` is the only function in that whole file that returns
// UNFILTERED pass-through data. `toMoiraError` builds its result field by field
// so that a field Moira adds to `ErrorDetail` tomorrow does not become a field
// the browser sees today; this one returns `details` verbatim, typed
// `JsonValue`, with no allow-list at all. `details` is server-diagnostic
// material — it is where Moira puts the shape of what went wrong, and that
// routinely means row ids, constraint names, and partially-formatted values.
//
// Until plan 09 wave 3 it was DEAD CODE in every application path — its only
// call sites were two lines of `errors.test.ts` — so nothing was going to notice
// the exposure. Wave 3 adds the first real caller, which is the moment a doc
// comment stops being adequate.
//
// The build guard below is what now enforces it: Next compiles server code with
// the `react-server` export condition (which resolves `server-only` to an empty
// module) and a browser bundle with `default` (a bare `throw`), so an import
// from a client component fails `next build` rather than shipping.
//
// ============================================================================
// WHAT THIS DOES *NOT* COVER — read this before trusting it
// ============================================================================
//
// A SECRET-BEARING RESPONSE NEVER PASSES THROUGH `lib/errors.ts` AT ALL.
// `moira-client.ts` calls `toMoiraError` only under `if (!response.ok)`; a 201
// body is returned raw as `(await response.json()) as T`. There is nothing
// between the JSON parse and a React prop.
//
// So the once-only invite token — `AdminInviteSecretResponse.secret` — is not
// redacted here, is not redacted anywhere in `lib/`, and a reader who assumes
// the error module covers it will be wrong. Its containment is the modal's own
// design (`modules/secrets/OnceOnlySecretModal.tsx`), the `no-secret-props`
// guard, and the e2e needle in `console/e2e/secret-leak.e2e.ts`.
import "server-only";

import { isMoiraErrorResponse } from "./errors";
import type { JsonValue, MoiraErrorResponse } from "./types";

/** `request_id` and `details`. Log these; never return them to a caller. */
export interface MoiraServerDiagnostics {
  readonly requestId: string | null;
  readonly details: JsonValue | null;
}

/**
 * Extract the fields that must stay on the server.
 *
 * The reason `toMoiraError` drops these is that this function exists to consume
 * them — the two are a pair, and the pair is now split across a build boundary
 * on purpose rather than kept together for readability.
 */
export function serverDiagnostics(body: unknown): MoiraServerDiagnostics {
  if (!isMoiraErrorResponse(body)) return { requestId: null, details: null };
  const error = body.error as MoiraErrorResponse["error"] & { details?: JsonValue };
  return {
    requestId: typeof error.request_id === "string" ? error.request_id : null,
    details: error.details ?? null,
  };
}
