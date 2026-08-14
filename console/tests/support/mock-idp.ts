// A real OpenID Connect provider, in-process, over TLS.
//
// ============================================================================
// WHAT THIS IS AND WHY IT IS NOT A STUB
// ============================================================================
//
// No Google client id or secret exists for this project, and none is requested.
// The whole OAuth path is therefore built and tested against this provider —
// but as a REAL HTTP server, not as a patched `fetch`:
//
//   * TLS-terminated (see `fixture-tls.ts` for why https specifically);
//   * a real discovery document at `/.well-known/openid-configuration`;
//   * a real JWKS document, and real ES256-signed ID tokens whose signature
//     verifies against it;
//   * a real authorization endpoint that issues codes and 302s back;
//   * a real token endpoint that checks `client_id`, `client_secret`,
//     `redirect_uri` and the PKCE `code_verifier`;
//   * a real userinfo endpoint that checks the bearer access token.
//
// The difference matters. A stubbed `fetch` proves the console can parse a JSON
// shape someone wrote by hand. This proves the console can complete an
// authorization-code exchange against something that will refuse it when it is
// wrong — which is the only way the D7 secret custody design gets tested at all:
// hand the console the wrong client secret and `/token` answers
// `401 invalid_client`, exactly as Google would.
//
// A NOTE ON WHAT THE JWKS IS ACTUALLY FOR HERE. The ID token this provider
// signs is real and its signature verifies against the published JWKS — but
// Better Auth's generic-oauth provider does NOT verify it. It calls `decodeJwt`
// and reads the claims. That is standards-sanctioned rather than a bug: OIDC
// Core §3.1.3.7 permits skipping ID Token signature validation when the token
// came over a direct TLS connection to the token endpoint with client
// authentication, which is exactly this flow (TLS + client_secret + PKCE). So
// the trust anchor is the CHANNEL, not the signature. The JWKS is served
// because a conformant provider serves one and because a future verifying
// client would need it — not because this path exercises it. The verification
// path that IS exercised is the console's own JWKS, on the Moira-bound token.
//
// WHAT THIS CANNOT COVER, and is deferred rather than faked:
//   * Google's actual consent screen and its cookie/redirect behaviour;
//   * Google-issued tokens, their exact claim set, and their key rotation
//     cadence;
//   * Google-specific `authorizationUrlParams` (`access_type`, `hd`, `prompt`)
//     and how Google reacts to them.
// Those need a live credential. Nothing here pretends otherwise.

import { exportJWK, generateKeyPair, SignJWT, type JWK, type KeyObject } from "jose";

import { fixtureTls } from "./fixture-tls";
// `NativeResponse`, not the global `Response`: happy-dom has replaced the
// global by the time any test runs, and `Bun.serve` rejects a foreign
// `Response` by constructor identity. See `native-globals.ts`.
import { NativeResponse } from "./native-globals";

/* -------------------------------------------------------------------------- */
/* Configuration                                                              */
/* -------------------------------------------------------------------------- */

export interface MockIdpUser {
  readonly sub: string;
  readonly email: string;
  readonly emailVerified: boolean;
  readonly name: string;
}

export interface MockIdpOptions {
  readonly clientId: string;
  readonly clientSecret: string;
  readonly user: MockIdpUser;
  /**
   * Serve on a FIXED origin instead of an OS-assigned port on `localhost`.
   *
   * The suite wants an ephemeral port — a hard-coded one cannot run twice
   * concurrently. A human driving the setup wizard by hand wants the opposite:
   * the issuer is typed into a form, stored in Moira, and must still be the
   * issuer after the process restarts.
   *
   * It cannot be solved with a TLS proxy in front. `iss` is signed into the ID
   * token and echoed on the callback, so rewriting it downstream invalidates
   * the signature — Better Auth refuses with `issuer_mismatch`, naming the
   * upstream port. The provider itself has to know its public origin, which is
   * what this supplies.
   *
   * `cert` and `key` are PEM bytes. Supply a certificate the consumer already
   * trusts (mkcert issues one) when a real browser has to follow the redirect;
   * the default self-signed fixture cert is fine for Playwright, which is
   * launched with `ignoreHTTPSErrors`.
   */
  readonly publicOrigin?: {
    readonly host: string;
    readonly port: number;
    readonly cert?: string | Buffer;
    readonly key?: string | Buffer;
  };
  /**
   * Leave `email` out of the ID token.
   *
   * Better Auth's generic-oauth provider uses the ID token's claims when it
   * carries BOTH `sub` and `email`, and only then falls back to the userinfo
   * endpoint. Omitting `email` is therefore the switch that forces the userinfo
   * branch, and it is the only way to cover that branch at all.
   */
  readonly idTokenOmitsEmail?: boolean;
  /**
   * Leave `sub` out of everywhere — ID token and userinfo alike.
   *
   * Proves the console refuses a sign-in it cannot key a grant to, rather than
   * inventing a subject of its own.
   */
  readonly omitSubject?: boolean;
}

/** An authorization code, and everything the token endpoint must re-check. */
interface PendingAuthorization {
  readonly redirectUri: string;
  readonly codeChallenge: string | null;
  readonly codeChallengeMethod: string | null;
  readonly scope: string;
}

export interface MockIdp {
  readonly origin: string;
  readonly issuer: string;
  readonly discoveryUrl: string;
  readonly jwksUrl: string;
  readonly authorizationUrl: string;
  readonly tokenUrl: string;
  readonly userInfoUrl: string;
  /** Every request path the provider served, in order. */
  readonly requests: string[];
  /** `"<METHOD> <path>"` for each request. */
  routes(): string[];
  stop(): void;
}

/* -------------------------------------------------------------------------- */
/* Helpers                                                                    */
/* -------------------------------------------------------------------------- */

function json(body: unknown, status = 200): Response {
  return new NativeResponse(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function oauthError(code: string, status: number, description: string): Response {
  return json({ error: code, error_description: description }, status);
}

function randomToken(prefix: string): string {
  return `${prefix}_${crypto.randomUUID().replaceAll("-", "")}`;
}

/** RFC 7636 S256: BASE64URL(SHA256(ASCII(code_verifier))). */
async function s256(verifier: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  return Buffer.from(new Uint8Array(digest)).toString("base64url");
}

/**
 * Read the client credentials from either place the spec allows.
 *
 * Better Auth defaults to `post` (credentials in the form body) but supports
 * `basic`. Accepting both means a change to `authentication` in the console's
 * provider config does not silently start failing against a fixture that only
 * understood one of them.
 */
function readClientCredentials(
  request: Request,
  form: URLSearchParams,
): { clientId: string | null; clientSecret: string | null } {
  const authorization = request.headers.get("authorization");
  if (authorization !== null && authorization.startsWith("Basic ")) {
    const decoded = Buffer.from(authorization.slice("Basic ".length), "base64").toString("utf8");
    const separator = decoded.indexOf(":");
    if (separator > 0) {
      return {
        clientId: decodeURIComponent(decoded.slice(0, separator)),
        clientSecret: decodeURIComponent(decoded.slice(separator + 1)),
      };
    }
  }
  return {
    clientId: form.get("client_id"),
    clientSecret: form.get("client_secret"),
  };
}

/* -------------------------------------------------------------------------- */
/* The server                                                                 */
/* -------------------------------------------------------------------------- */

export async function startMockIdp(options: MockIdpOptions): Promise<MockIdp> {
  const tls = fixtureTls();
  const { publicKey, privateKey } = await generateKeyPair("ES256", { extractable: true });
  const kid = randomToken("mockidp-key");
  const publicJwk: JWK = {
    ...(await exportJWK(publicKey as KeyObject)),
    kid,
    alg: "ES256",
    use: "sig",
  };

  const codes = new Map<string, PendingAuthorization>();
  const accessTokens = new Map<string, MockIdpUser>();
  const requests: string[] = [];

  // `port: 0` — the OS picks. A hard-coded port makes a suite that cannot run
  // twice concurrently and fails opaquely when something else holds it.
  const server = Bun.serve({
    port: options.publicOrigin?.port ?? 0,
    hostname: "127.0.0.1",
    tls: {
      key: options.publicOrigin?.key ?? tls.key,
      cert: options.publicOrigin?.cert ?? tls.cert,
    },
    async fetch(request): Promise<Response> {
      const url = new URL(request.url);
      requests.push(`${request.method} ${url.pathname}`);

      /* ---- discovery -------------------------------------------------- */
      if (url.pathname === "/.well-known/openid-configuration") {
        return json({
          issuer,
          authorization_endpoint: `${origin}/authorize`,
          token_endpoint: `${origin}/token`,
          userinfo_endpoint: `${origin}/userinfo`,
          jwks_uri: `${origin}/jwks`,
          response_types_supported: ["code"],
          subject_types_supported: ["public"],
          id_token_signing_alg_values_supported: ["ES256"],
          scopes_supported: ["openid", "email", "profile"],
          token_endpoint_auth_methods_supported: ["client_secret_post", "client_secret_basic"],
          code_challenge_methods_supported: ["S256"],
          claims_supported: ["sub", "email", "email_verified", "name"],
        });
      }

      /* ---- jwks ------------------------------------------------------- */
      if (url.pathname === "/jwks") {
        return json({ keys: [publicJwk] });
      }

      /* ---- authorize -------------------------------------------------- */
      if (url.pathname === "/authorize") {
        const params = url.searchParams;
        if (params.get("response_type") !== "code") {
          return oauthError("unsupported_response_type", 400, "only `code` is supported");
        }
        if (params.get("client_id") !== options.clientId) {
          return oauthError("unauthorized_client", 401, "unknown client_id");
        }
        const redirectUri = params.get("redirect_uri");
        if (redirectUri === null || redirectUri === "") {
          return oauthError("invalid_request", 400, "redirect_uri is required");
        }

        const code = randomToken("code");
        codes.set(code, {
          redirectUri,
          codeChallenge: params.get("code_challenge"),
          codeChallengeMethod: params.get("code_challenge_method"),
          scope: params.get("scope") ?? "openid email profile",
        });

        // A real provider renders a consent screen here. There is nothing
        // meaningful to fake about that, so this redirects straight back — and
        // that omission is recorded above rather than glossed.
        const location = new URL(redirectUri);
        location.searchParams.set("code", code);
        const state = params.get("state");
        if (state !== null) location.searchParams.set("state", state);
        // OAuth 2.0 `iss` (RFC 9207). Better Auth compares it against the
        // provider's expected issuer when present, so sending it exercises that
        // check rather than skipping it.
        location.searchParams.set("iss", issuer);
        return new NativeResponse(null, {
          status: 302,
          headers: { location: location.toString() },
        });
      }

      /* ---- token ------------------------------------------------------ */
      if (url.pathname === "/token" && request.method === "POST") {
        const form = new URLSearchParams(await request.text());
        const { clientId, clientSecret } = readClientCredentials(request, form);

        if (clientId !== options.clientId) {
          return oauthError("invalid_client", 401, "unknown client_id");
        }
        // THE D7 CHECK. The console holds this secret in its own store; Moira
        // never sees it. If the console's store and Moira's `client_id` have
        // drifted, or the secret was never stored, this is where it surfaces.
        if (clientSecret !== options.clientSecret) {
          return oauthError("invalid_client", 401, "client authentication failed");
        }
        if (form.get("grant_type") !== "authorization_code") {
          return oauthError("unsupported_grant_type", 400, "only authorization_code is supported");
        }

        const code = form.get("code") ?? "";
        const pending = codes.get(code);
        if (pending === undefined) {
          return oauthError("invalid_grant", 400, "unknown or already-redeemed code");
        }
        // Single use. A replayed code is a replayed code.
        codes.delete(code);

        if (form.get("redirect_uri") !== pending.redirectUri) {
          return oauthError("invalid_grant", 400, "redirect_uri does not match the authorization");
        }

        if (pending.codeChallenge !== null) {
          const verifier = form.get("code_verifier");
          if (verifier === null || verifier === "") {
            return oauthError("invalid_grant", 400, "code_verifier is required");
          }
          const expected = pending.codeChallengeMethod === "S256" ? await s256(verifier) : verifier;
          if (expected !== pending.codeChallenge) {
            return oauthError("invalid_grant", 400, "PKCE verification failed");
          }
        }

        const accessToken = randomToken("at");
        accessTokens.set(accessToken, options.user);

        const idTokenBuilder = new SignJWT({
          ...(options.idTokenOmitsEmail === true
            ? {}
            : { email: options.user.email, email_verified: options.user.emailVerified }),
          name: options.user.name,
        })
          .setProtectedHeader({ alg: "ES256", kid, typ: "JWT" })
          .setIssuer(issuer)
          .setAudience(options.clientId)
          .setIssuedAt()
          .setExpirationTime("5m");
        if (options.omitSubject !== true) idTokenBuilder.setSubject(options.user.sub);
        const idToken = await idTokenBuilder.sign(privateKey);

        return json({
          access_token: accessToken,
          token_type: "Bearer",
          expires_in: 3600,
          id_token: idToken,
          scope: pending.scope,
        });
      }

      /* ---- userinfo --------------------------------------------------- */
      if (url.pathname === "/userinfo") {
        const authorization = request.headers.get("authorization") ?? "";
        const token = authorization.startsWith("Bearer ")
          ? authorization.slice("Bearer ".length)
          : "";
        const user = accessTokens.get(token);
        if (user === undefined) {
          return new NativeResponse(null, {
            status: 401,
            headers: { "www-authenticate": 'Bearer error="invalid_token"' },
          });
        }
        return json({
          ...(options.omitSubject === true ? {} : { sub: user.sub }),
          email: user.email,
          email_verified: user.emailVerified,
          name: user.name,
        });
      }

      return json({ error: "not_found" }, 404);
    },
  });

  // Every advertised URL and the signed `iss` come from this one expression, so
  // an override cannot leave discovery and the token disagreeing.
  const origin =
    options.publicOrigin === undefined
      ? `https://localhost:${server.port}`
      : `https://${options.publicOrigin.host}:${options.publicOrigin.port}`;
  const issuer = origin;

  return {
    origin,
    issuer,
    discoveryUrl: `${origin}/.well-known/openid-configuration`,
    jwksUrl: `${origin}/jwks`,
    authorizationUrl: `${origin}/authorize`,
    tokenUrl: `${origin}/token`,
    userInfoUrl: `${origin}/userinfo`,
    requests,
    routes: () => [...requests],
    stop: () => {
      server.stop(true);
    },
  };
}
