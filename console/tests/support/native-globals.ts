// Bun test preload, step 1 of 4 (see bunfig.toml `[test].preload`). MUST be
// first — before `dom-register.ts`.
//
// ============================================================================
// WHY
// ============================================================================
//
// `@happy-dom/global-registrator`'s `register()` replaces the WHATWG globals —
// `fetch`, `Response`, `Request`, `Headers` — with happy-dom's own, so component
// tests see browser-shaped ones. Two consequences bite the integration suite,
// and both present as something other than what they are:
//
//  1. happy-dom's `fetch` is a `node:http` wrapper. It ignores Bun's
//     per-request `tls` option entirely, so the TLS fixtures fail with
//     `DEPTH_ZERO_SELF_SIGNED_CERT` — which reads as a certificate problem when
//     it is a fetch-implementation problem.
//
//  2. Better Auth constructs its replies with `new Response(...)`, i.e. with the
//     global. Handing one of those to `Bun.serve` fails with
//     `Expected a Response object, but received 'Response { ... }'` and a
//     several-hundred-line dump of the happy-dom window — which reads as a
//     Better Auth bug when it is a global-object-identity problem.
//
//  3. THE ONE THAT COST THE MOST TO FIND. happy-dom's `Headers` implements the
//     BROWSER contract, where `Set-Cookie` is a forbidden response header:
//     `append("set-cookie", …)` is silently discarded. Better Auth sets the
//     OAuth `state`/PKCE cookie exactly that way, so under happy-dom the
//     sign-in reply comes back with NO `Set-Cookie` at all and the callback
//     then fails `state_security_mismatch / State not persisted correctly`.
//     That presents as a CSRF bug, or as a cookie-jar bug in the test agent,
//     or as a `nextCookies()` misconfiguration — three wrong diagnoses before
//     the right one. Proof: the same handler run under plain `bun run`, with no
//     preloads, returns
//     `set-cookie: __Secure-better-auth.state=…; Max-Age=300; …`.
//
// Hence `useNativeWhatwgGlobals()`: any suite that exercises SERVER behaviour
// installs Bun's own globals for its duration and puts happy-dom's back
// afterwards. A server-side suite needs no DOM, and a DOM that models browser
// restrictions is actively wrong for it.
//
// This module is evaluated as a preload, before happy-dom registers, so every
// binding below is Bun's own. Test files importing this path get the same module
// instance from Bun's registry, so the capture is shared.
//
// Nothing in `lib/` or `app/` may import this file. It exists to undo a test
// harness's interference with itself.

/** Bun's `fetch`, captured before any DOM shim replaces it. */
export const nativeFetch: typeof fetch = globalThis.fetch;

/** Bun's `Response`, for handing back to `Bun.serve`. */
export const NativeResponse: typeof Response = globalThis.Response;

/** Bun's `Headers` — the one that can carry `Set-Cookie`. */
export const NativeHeaders: typeof Headers = globalThis.Headers;

/** Bun's `Request`. */
export const NativeRequest: typeof Request = globalThis.Request;

/** The globals swapped by `useNativeWhatwgGlobals`, in one place. */
const SWAPPED = ["fetch", "Response", "Headers", "Request"] as const;

type SwappableName = (typeof SWAPPED)[number];

const natives: Record<SwappableName, unknown> = {
  fetch: nativeFetch,
  Response: NativeResponse,
  Headers: NativeHeaders,
  Request: NativeRequest,
};

let displaced: Partial<Record<SwappableName, unknown>> | undefined;

/**
 * Install Bun's own WHATWG globals for the duration of a server-side suite.
 *
 * Call in `beforeAll`; pair with `restoreDomWhatwgGlobals()` in `afterAll` so
 * component suites that run later in the same process still see happy-dom's.
 *
 * Idempotent: a second call while already installed does nothing, so the
 * displaced set can never be overwritten with the natives themselves (which
 * would make the restore a no-op and break every later component test).
 */
export function useNativeWhatwgGlobals(): void {
  if (displaced !== undefined) return;
  displaced = {};
  const target = globalThis as unknown as Record<string, unknown>;
  for (const name of SWAPPED) {
    displaced[name] = target[name];
    target[name] = natives[name];
  }
}

/** Put happy-dom's globals back. */
export function restoreDomWhatwgGlobals(): void {
  if (displaced === undefined) return;
  const target = globalThis as unknown as Record<string, unknown>;
  for (const name of SWAPPED) {
    target[name] = displaced[name];
  }
  displaced = undefined;
}
