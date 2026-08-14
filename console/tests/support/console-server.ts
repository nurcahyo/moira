// The console's own Better Auth handler, served over TLS on a real socket.
//
// The instance under test is built by `createConsoleAuth` — the shipped factory,
// with the shipped plugin set. The only thing this file adds is a socket, so
// that:
//
//   * the OAuth redirect chain crosses a real origin boundary (the mock IdP
//     redirects to `https://localhost:<console>/api/auth/oauth2/callback/...`),
//     which is what makes cookie `SameSite` and `state` handling meaningful;
//   * the JWKS document is fetched over HTTP by `jose`'s `createRemoteJWKSet`,
//     which is Moira's own verification path — Moira fetches `jwks_url` and
//     resolves the signing key by `kid`. Calling the handler in-process and
//     reading the JSON would test a different thing.

import { handleAuthRequest } from "@/app/api/auth/[...all]/route";
import { createConsoleAuth, type ConsoleAuthDeps } from "@/lib/auth";
import { AUTH_BASE_PATH, AUTH_JWKS_PATH } from "@/lib/env";

import { fixtureTls } from "./fixture-tls";
// `NativeResponse`, not the global `Response`: a reply handed straight back to
// `Bun.serve` is rejected by constructor identity if it is happy-dom's. See
// `native-globals.ts`.
import { NativeResponse } from "./native-globals";

export interface ConsoleServer {
  readonly origin: string;
  readonly auth: ReturnType<typeof createConsoleAuth>;
  readonly jwksUrl: string;
  readonly requests: string[];
  stop(): void;
}

/**
 * A console socket that is BOUND but not yet answering.
 *
 * The two-phase shape exists because of a real ordering constraint: Better Auth
 * composes its redirect URI from `baseURL`, and the IdP re-checks that URI at
 * the token endpoint, so `deps.env.consoleOrigin` has to name the port before
 * `createConsoleAuth` runs — while the port itself is only knowable after
 * binding. Splitting bind from serve satisfies both without ever releasing the
 * socket. @see bindConsoleServer
 */
export interface PendingConsoleServer {
  /** `https://localhost:<the port actually bound>`. */
  readonly origin: string;
  /** The OS-assigned port, for a caller that needs the number itself. */
  readonly port: number;
  /** Install the shipped handler. Callable once. */
  serve(deps: ConsoleAuthDeps): ConsoleServer;
  /** Release the socket without ever having served, for an aborted set-up. */
  stop(): void;
}

/**
 * Bind the console on an OS-assigned port, and KEEP the socket.
 *
 * ============================================================================
 * WHY NOT "PICK A PORT, THEN BIND IT"
 * ============================================================================
 *
 * The shape this replaces reserved a port by binding `port: 0`, reading the
 * number, closing the socket, and re-binding it several `await`s later. Between
 * the close and the re-bind the port belonged to nobody, and its own doc comment
 * conceded the race: "There is an unavoidable race between releasing and
 * re-binding. It is accepted rather than engineered around." Issue #195 is the
 * bill for that: a documentation-only PR went red with `Failed to start server.
 * Is port 34899 in use?` on `the VERIFIED primary address wins over the public
 * profile address`, then passed on a rerun with no code change.
 *
 * WHAT TOOK THE PORT IS NOT KNOWN. It was not a second test file: `bun test`
 * runs files sequentially in one process unless `--concurrent` or `--parallel`
 * is passed, and neither is passed anywhere in this repo (`package.json`'s
 * `test` script, the `Makefile`, and `ci.yml` all invoke a bare `bun test`).
 * The plausible competitor is the harness's own loopback traffic — the kernel
 * can hand a just-released listener port to an outbound `fetch` as its ephemeral
 * source port, and this suite makes many — but that was not proven, and no
 * theory needs to be right for the fix to work.
 *
 * Here the socket is bound once and never released, so there is no window in
 * which the port is unowned and nothing can take it from us, whatever "it" was.
 * The handler is swapped in afterwards: `Bun.serve`'s `fetch` closes over a
 * mutable binding, and nothing can arrive before `serve()` runs anyway, because
 * the caller has not yet told anybody the origin.
 */
// NOT exported. Every caller goes through `withBoundConsole` below, which
// guarantees the socket is released if anything between the bind and the
// `serve()` throws. Exporting this would let the leak — and, by copy-paste, the
// probe-then-rebind shape #195 was about — back into the suite.
function bindConsoleServer(): PendingConsoleServer {
  const tls = fixtureTls();
  const requests: string[] = [];
  let handle: ((request: Request) => Promise<Response>) | null = null;

  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    tls: { key: tls.key, cert: tls.cert },
    fetch(request): Promise<Response> | Response {
      const url = new URL(request.url);
      requests.push(`${request.method} ${url.pathname}`);
      if (handle === null) {
        // Unreachable in a well-formed test: nobody else knows the origin until
        // `serve()` has returned it. Answered rather than thrown so a caller
        // that does race itself sees a diagnosable status rather than a socket
        // reset.
        return new NativeResponse("console harness bound but not yet serving", { status: 503 });
      }
      return handle(request);
    },
  });

  const port: number | undefined = server.port;
  if (port === undefined) throw new Error("Bun.serve did not report a port");
  const origin = `https://localhost:${port}`;

  const stop = (): void => {
    server.stop(true);
  };

  return {
    origin,
    port,
    stop,
    serve(deps: ConsoleAuthDeps): ConsoleServer {
      if (handle !== null) {
        throw new Error("serve() was already called on this console server");
      }
      const auth = createConsoleAuth(deps);

      handle = async (request: Request): Promise<Response> => {
        const url = new URL(request.url);
        if (url.pathname.startsWith(AUTH_BASE_PATH)) {
          // Requires `useNativeWhatwgGlobals()` to be in effect: Better Auth
          // builds its reply with the global `Response` and sets cookies through
          // the global `Headers`, and happy-dom's versions of both are wrong for
          // a server. See `native-globals.ts`.
          //
          // NOT `auth.handler(request)`. That was the shape until finding F25:
          // every wire-level test drove Better Auth directly and never touched
          // `app/api/auth/[...all]/route.ts`, so the handler's own behaviour —
          // the 503 for an unresolvable configuration, and the keyed 403 the
          // token endpoint owes a session outside the allow-list — was exercised
          // by nothing. Only the runtime RESOLUTION is stubbed; every line of
          // policy in the route module is the shipped one.
          return handleAuthRequest(request, async () => ({
            ok: true,
            auth,
            configs: deps.configs,
            // No per-provider problems: the harness is handed already-resolved
            // configurations, so there is nothing that failed to resolve. A test
            // that wants to exercise the drifted-row path resolves through
            // `loadAuthConfigs` instead of through this seam.
            problems: [],
            // The harness resolves a configuration on the spot, so it is never a
            // snapshot that aged out. The staleness path has its own tests in
            // `tests/unit/lib/auth-runtime-refresh.test.ts`.
            stale: false,
          }));
        }
        // The post-sign-in landing page. The flow redirects here, so it has to
        // exist; nothing about it is under test.
        return new Response("console", { status: 200, headers: { "content-type": "text/plain" } });
      };

      return {
        origin,
        auth,
        jwksUrl: `${origin}${AUTH_BASE_PATH}${AUTH_JWKS_PATH}`,
        requests,
        stop,
      };
    },
  };
}

/**
 * Bind a console socket, build the server with it, and release the socket if
 * building throws.
 *
 * Use this rather than calling `bindConsoleServer` directly whenever anything
 * can fail between the bind and the `serve()` — starting a mock IdP, opening a
 * database, resolving a fixture. Holding the socket from bind to serve is what
 * removes the #195 race, but it also means an exception in the middle leaks a
 * listener for the rest of the process, and the caller's own
 * `afterEach`/`finally` cannot help: it has no reference to a server that was
 * never returned, and will typically stop the PREVIOUS test's server instead
 * while `server` still points at it.
 *
 * `build` must return the result of `pending.serve(...)`. On success the socket
 * is owned by the returned `ConsoleServer` and released by its `stop()`.
 */
export async function withBoundConsole(
  build: (pending: PendingConsoleServer) => Promise<ConsoleServer> | ConsoleServer,
): Promise<ConsoleServer> {
  const pending = bindConsoleServer();
  try {
    return await build(pending);
  } catch (error) {
    pending.stop();
    throw error;
  }
}
