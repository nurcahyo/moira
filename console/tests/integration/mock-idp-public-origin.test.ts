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

import { reserveConsolePort } from "../support/console-server";
import { trustFixtureCa, untrustFixtureCa } from "../support/fixture-tls";
import { startMockIdp, type MockIdp } from "../support/mock-idp";
import { restoreDomWhatwgGlobals, useNativeWhatwgGlobals } from "../support/native-globals";

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

    const port = reserveConsolePort();
    const host = "127.0.0.1";
    const origin = `https://${host}:${port}`;

    idp = await startMockIdp({
      clientId: "moira-console.apps.mock-idp.test",
      clientSecret: "mock-idp-client-secret-do-not-reuse",
      user: {
        sub: "mock-idp-subject-public-origin",
        email: "operator@example.com",
        emailVerified: true,
        name: "Console Operator",
      },
      publicOrigin: { host, port },
    });
    trustFixtureCa(origin);

    // The returned handle: not derived from the OS-assigned port, but from the
    // fixed one this test chose.
    expect(idp.origin).toBe(origin);
    expect(idp.issuer).toBe(origin);
    expect(idp.discoveryUrl).toBe(`${origin}/.well-known/openid-configuration`);
    expect(idp.jwksUrl).toBe(`${origin}/jwks`);
    expect(idp.authorizationUrl).toBe(`${origin}/authorize`);
    expect(idp.tokenUrl).toBe(`${origin}/token`);
    expect(idp.userInfoUrl).toBe(`${origin}/userinfo`);

    // The discovery document itself — fetched over the real socket, not
    // asserted against the handle's own fields — has to say the same thing.
    // This is the check a rewriting proxy cannot satisfy: it would leave the
    // signed `iss` on the eventual ID token pointing at the upstream origin
    // while this document (or the reverse) pointed at the public one.
    const response = await fetch(idp.discoveryUrl);
    expect(response.status).toBe(200);
    const discovery = (await response.json()) as Record<string, unknown>;
    expect(discovery["issuer"]).toBe(origin);
    expect(discovery["authorization_endpoint"]).toBe(`${origin}/authorize`);
    expect(discovery["token_endpoint"]).toBe(`${origin}/token`);
    expect(discovery["userinfo_endpoint"]).toBe(`${origin}/userinfo`);
    expect(discovery["jwks_uri"]).toBe(`${origin}/jwks`);
  });
});
