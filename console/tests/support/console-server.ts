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

export interface ConsoleServer {
  readonly origin: string;
  readonly auth: ReturnType<typeof createConsoleAuth>;
  readonly jwksUrl: string;
  readonly requests: string[];
  stop(): void;
}

/**
 * Start the console on an OS-assigned port.
 *
 * `deps.env.consoleOrigin` must already name the port this will bind to, because
 * Better Auth composes its redirect URI from `baseURL` and the IdP re-checks
 * that URI at the token endpoint. `reserveConsoleOrigin` below exists so a test
 * can learn the port before constructing the environment.
 */
export function startConsoleServer(deps: ConsoleAuthDeps, port: number): ConsoleServer {
  const tls = fixtureTls();
  const auth = createConsoleAuth(deps);
  const requests: string[] = [];

  const server = Bun.serve({
    port,
    hostname: "127.0.0.1",
    tls: { key: tls.key, cert: tls.cert },
    async fetch(request): Promise<Response> {
      const url = new URL(request.url);
      requests.push(`${request.method} ${url.pathname}`);
      if (url.pathname.startsWith(AUTH_BASE_PATH)) {
        // Requires `useNativeWhatwgGlobals()` to be in effect: Better Auth
        // builds its reply with the global `Response` and sets cookies through
        // the global `Headers`, and happy-dom's versions of both are wrong for
        // a server. See `native-globals.ts`.
        //
        // NOT `auth.handler(request)`. That was the shape until finding F25:
        // every wire-level test drove Better Auth directly and never touched
        // `app/api/auth/[...all]/route.ts`, so the handler's own behaviour — the
        // 503 for an unresolvable configuration, and the keyed 403 the token
        // endpoint owes a session outside the allow-list — was exercised by
        // nothing. Only the runtime RESOLUTION is stubbed; every line of policy
        // in the route module is the shipped one.
        return handleAuthRequest(request, async () => ({
          ok: true,
          auth,
          config: deps.config,
        }));
      }
      // The post-sign-in landing page. The flow redirects here, so it has to
      // exist; nothing about it is under test.
      return new Response("console", { status: 200, headers: { "content-type": "text/plain" } });
    },
  });

  return {
    origin: `https://localhost:${server.port}`,
    auth,
    jwksUrl: `https://localhost:${server.port}${AUTH_BASE_PATH}${AUTH_JWKS_PATH}`,
    requests,
    stop: () => {
      server.stop(true);
    },
  };
}

/**
 * Claim a free port, then release it.
 *
 * There is an unavoidable race between releasing and re-binding. It is accepted
 * rather than engineered around: the alternative is to construct the Better Auth
 * instance twice (once to learn the port, once with the right `baseURL`), and a
 * two-phase construction would diverge from how the shipped code builds it.
 */
export function reserveConsolePort(): number {
  const probe = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Response("") });
  const port: number | undefined = probe.port;
  probe.stop(true);
  if (port === undefined) throw new Error("Bun.serve did not report a port");
  return port;
}
