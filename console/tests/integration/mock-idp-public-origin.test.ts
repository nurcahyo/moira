// `MockIdpOptions.publicOrigin` exists for exactly one reason: a human driving
// the setup wizard types an issuer into a form, and that issuer has to survive
// a restart and be reachable by a real browser that follows the `/authorize`
// redirect. A TLS proxy in front cannot substitute for it — `iss` is signed
// into the ID token and echoed on the callback, so rewriting it downstream
// invalidates the signature. See the option's doc comment in `mock-idp.ts` for
// the verbatim `issuer_mismatch` this was measured against.
//
// What a proxy structurally cannot provide, and what this test asserts
// instead, is that the DISCOVERY document, the RETURNED `issuer`, and every
// advertised URL all agree on the one fixed origin the caller asked for —
// there is no seam where they could drift apart.

import { afterAll, describe, expect, test } from "bun:test";

import { trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { startMockIdp, type MockIdp } from "../support/mock-idp";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";
import { withFreePort } from "../support/ports";

describe("startMockIdp with a fixed publicOrigin", () => {
  let idp: MockIdp | undefined;

  afterAll(() => {
    idp?.stop();
    untrustFixtureCa();
    restoreDomWhatwgGlobals();
  });

  test("discovery, the returned issuer, and every advertised URL all equal the fixed origin", async () => {
    // Server-side test: happy-dom's Headers/fetch would otherwise get in the
    // way of a real TLS handshake. See `native-globals.ts`.
    useNativeWhatwgGlobals();

    const host = "127.0.0.1";
    // The one place in this suite where the port is an INPUT rather than an
    // output: the whole point of `publicOrigin` is that the issuer is decided
    // before the socket exists, so `port: 0` cannot be used here the way every
    // other fixture uses it. `withFreePort` retries on `EADDRINUSE` with a fresh
    // number instead — see `tests/support/ports.ts` and issue #195.
    const started = await withFreePort(async (port) => ({
      idp: await startMockIdp({
        clientId: "moira-console.apps.mock-idp.test",
        clientSecret: "mock-idp-client-secret-do-not-reuse",
        user: {
          sub: "mock-idp-subject-public-origin",
          email: "operator@example.com",
          emailVerified: true,
          name: "Console Operator",
        },
        publicOrigin: { host, port },
      }),
      origin: `https://${host}:${port}`,
    }));
    // Held for `afterAll` as well as used below.
    idp = started.idp;
    const origin = started.origin;
    trustFixtureCa(origin);

    // The returned handle: not derived from the OS-assigned port, but from the
    // fixed one this test chose.
    expect(started.idp.origin).toBe(origin);
    expect(started.idp.issuer).toBe(origin);
    expect(started.idp.discoveryUrl).toBe(`${origin}/.well-known/openid-configuration`);
    expect(started.idp.jwksUrl).toBe(`${origin}/jwks`);
    expect(started.idp.authorizationUrl).toBe(`${origin}/authorize`);
    expect(started.idp.tokenUrl).toBe(`${origin}/token`);
    expect(started.idp.userInfoUrl).toBe(`${origin}/userinfo`);

    // The discovery document itself — fetched over the real socket, not
    // asserted against the handle's own fields — has to say the same thing.
    // This is the check a rewriting proxy cannot satisfy: it would leave the
    // signed `iss` on the eventual ID token pointing at the upstream origin
    // while this document (or the reverse) pointed at the public one.
    const response = await fetch(started.idp.discoveryUrl);
    expect(response.status).toBe(200);
    const discovery = (await response.json()) as Record<string, unknown>;
    expect(discovery["issuer"]).toBe(origin);
    expect(discovery["authorization_endpoint"]).toBe(`${origin}/authorize`);
    expect(discovery["token_endpoint"]).toBe(`${origin}/token`);
    expect(discovery["userinfo_endpoint"]).toBe(`${origin}/userinfo`);
    expect(discovery["jwks_uri"]).toBe(`${origin}/jwks`);
  });
});
