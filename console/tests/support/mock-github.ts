// A GitHub-shaped OAuth 2.0 provider, in-process, over TLS.
//
// ============================================================================
// WHY THIS EXISTS ALONGSIDE `mock-idp.ts`
// ============================================================================
//
// `mock-idp.ts` is an OpenID Connect provider: it serves a discovery document,
// signs an `id_token`, and answers a bearer-checked `/userinfo` that returns
// `sub` and `email_verified`. **GitHub has none of those.** Testing the GitHub
// provider against a second instance of the OIDC mock would exercise two OIDC
// providers and call one of them "github", which is the shape of test that
// reports coverage it does not have.
//
// What is actually different, and what this file is here to make real:
//
//   * NO `/.well-known/openid-configuration`. The endpoints are fixed and must
//     be configured explicitly. `migrations/0020` requires `discovery_url is
//     null` on `method = 'github_oauth'` for exactly this reason.
//   * NO `id_token` in the token response. Better Auth's
//     `generic-oauth/routes.mjs::getUserInfo` reads the id token FIRST and only
//     falls back to `userInfoUrl`, so this exercises the fallback branch.
//   * `GET /user` returns a NUMERIC `id`, a `login`, and an `email` that is the
//     account's PUBLIC profile address — which GitHub does not require to be
//     verified. There is no `email_verified` field anywhere in it.
//   * `GET /user/emails` is a SEPARATE endpoint, requires the `user:email`
//     scope, and is the only place `verified` and `primary` are reported.
//
// The console's `githubProfile` (in `lib/auth.ts`) is written against exactly
// this shape, and `unverifiedEmail`/`emailsScopeDenied`/`emailsUnverified`
// below are the switches that let a test drive the failure directions rather
// than only the happy one.
//
// WHAT THIS CANNOT COVER, deferred rather than faked:
//   * github.com's real consent screen, cookie handling and rate limits;
//   * GitHub's actual `id` allocation, SAML-SSO-enforced orgs, and the
//     `X-OAuth-Scopes` response headers a real token carries;
//   * organisation membership checks (`GET /user/memberships/orgs/{org}`),
//     which wave 4 does not ship.
// No GitHub client id or secret exists for this project and none is requested.
// Everything the console does against GitHub is UNVERIFIED against the real
// provider, and nothing here pretends otherwise.

import { fixtureTls } from "./fixture-tls";
// `NativeResponse`, not the global `Response`: happy-dom has replaced the
// global by the time any test runs, and `Bun.serve` rejects a foreign
// `Response` by constructor identity. See `native-globals.ts`.
import { NativeResponse } from "./native-globals";

/* -------------------------------------------------------------------------- */
/* Configuration                                                              */
/* -------------------------------------------------------------------------- */

export interface MockGithubUser {
  /** GitHub's `id` is a NUMBER, and that is the point of typing it as one. */
  readonly id: number;
  readonly login: string;
  readonly name: string;
  /** The address `GET /user/emails` reports. */
  readonly email: string;
}

export interface MockGithubOptions {
  readonly clientId: string;
  readonly clientSecret: string;
  readonly user: MockGithubUser;
  /**
   * Report the primary address as UNVERIFIED.
   *
   * The takeover vector, made drivable: a GitHub account can set its public
   * profile email to anything, so a console that trusted `GET /user`'s `email`
   * would let an attacker claim an address an existing admin already holds — and
   * Better Auth's implicit account linking would link it onto that admin's user
   * row. With this on, the console must refuse the sign-in.
   */
  readonly emailsUnverified?: boolean;
  /**
   * Answer `403` on `/user/emails`, as GitHub does without the `user:email`
   * scope. The console must refuse rather than fall back to `/user`'s address.
   */
  readonly emailsScopeDenied?: boolean;
  /**
   * The address `GET /user` reports in its public profile, when it differs from
   * the verified one. Defaults to the verified address.
   */
  readonly publicProfileEmail?: string;
}

interface PendingAuthorization {
  readonly redirectUri: string;
  readonly codeChallenge: string | null;
  readonly codeChallengeMethod: string | null;
  readonly scope: string;
}

export interface MockGithub {
  readonly origin: string;
  readonly authorizationUrl: string;
  readonly tokenUrl: string;
  /** GitHub's `https://api.github.com/user`. `/emails` hangs off it. */
  readonly userInfoUrl: string;
  /** `"<METHOD> <path>"` for each request, in order. */
  readonly requests: string[];
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
  return { clientId: form.get("client_id"), clientSecret: form.get("client_secret") };
}

/* -------------------------------------------------------------------------- */
/* The server                                                                 */
/* -------------------------------------------------------------------------- */

export async function startMockGithub(options: MockGithubOptions): Promise<MockGithub> {
  const tls = fixtureTls();
  const codes = new Map<string, PendingAuthorization>();
  const accessTokens = new Set<string>();
  const requests: string[] = [];

  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    tls: { key: tls.key, cert: tls.cert },
    async fetch(request): Promise<Response> {
      const url = new URL(request.url);
      requests.push(`${request.method} ${url.pathname}`);

      /* ---- NO discovery document -------------------------------------- */
      // Answered explicitly rather than by falling through to the 404 below, so
      // a test can assert that the console never asks: a `github_oauth` row
      // carrying a `discovery_url` is refused by `migrations/0020`, and a
      // console that quietly probed for one would be building on a shape the
      // schema forbids.
      if (url.pathname === "/.well-known/openid-configuration") {
        return json({ message: "Not Found" }, 404);
      }

      /* ---- authorize --------------------------------------------------- */
      if (url.pathname === "/login/oauth/authorize") {
        const params = url.searchParams;
        if (params.get("client_id") !== options.clientId) {
          return oauthError("unauthorized_client", 401, "unknown client_id");
        }
        const redirectUri = params.get("redirect_uri");
        if (redirectUri === null || redirectUri === "") {
          return oauthError("invalid_request", 400, "redirect_uri is required");
        }

        const code = randomToken("gho_code");
        codes.set(code, {
          redirectUri,
          codeChallenge: params.get("code_challenge"),
          codeChallengeMethod: params.get("code_challenge_method"),
          scope: params.get("scope") ?? "read:user user:email",
        });

        const location = new URL(redirectUri);
        location.searchParams.set("code", code);
        const state = params.get("state");
        if (state !== null) location.searchParams.set("state", state);
        // NO `iss` parameter. GitHub does not implement RFC 9207, and Better
        // Auth only compares `ctx.query.iss` when the provider config declares
        // an `issuer` — which a `github_oauth` row cannot, by `migrations/0020`.
        return new NativeResponse(null, { status: 302, headers: { location: location.toString() } });
      }

      /* ---- token ------------------------------------------------------- */
      if (url.pathname === "/login/oauth/access_token" && request.method === "POST") {
        const form = new URLSearchParams(await request.text());
        const { clientId, clientSecret } = readClientCredentials(request, form);

        if (clientId !== options.clientId) {
          return oauthError("invalid_client", 401, "unknown client_id");
        }
        // THE D7 CHECK, same as the OIDC mock: the console holds this secret in
        // its own store and Moira never sees it.
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

        const accessToken = randomToken("gho");
        accessTokens.add(accessToken);
        // NO `id_token`, and no `expires_in` either — GitHub's OAuth app tokens
        // do not expire. This is the whole reason the console needs a userinfo
        // path for this provider.
        return json({
          access_token: accessToken,
          token_type: "bearer",
          scope: pending.scope,
        });
      }

      /* ---- GET /user --------------------------------------------------- */
      if (url.pathname === "/user" && request.method === "GET") {
        const token = bearer(request);
        if (token === null || !accessTokens.has(token)) {
          return json({ message: "Bad credentials" }, 401);
        }
        return json({
          id: options.user.id, // a NUMBER, deliberately
          login: options.user.login,
          name: options.user.name,
          avatar_url: `${origin}/avatars/${options.user.login}`,
          // The PUBLIC profile address. No `email_verified` anywhere: GitHub
          // does not report verification here, and a console that inferred it
          // from the field's presence would accept an attacker-chosen address.
          email: options.publicProfileEmail ?? options.user.email,
        });
      }

      /* ---- GET /user/emails -------------------------------------------- */
      if (url.pathname === "/user/emails" && request.method === "GET") {
        const token = bearer(request);
        if (token === null || !accessTokens.has(token)) {
          return json({ message: "Bad credentials" }, 401);
        }
        if (options.emailsScopeDenied === true) {
          // GitHub's shape for a token without `user:email`.
          return json(
            { message: "Resource not accessible by personal access token" },
            403,
          );
        }
        return json([
          {
            email: options.user.email,
            primary: true,
            verified: options.emailsUnverified !== true,
            visibility: "private",
          },
          // A second, verified, NON-primary address. Present because "take the
          // first verified one" and "take the primary verified one" only differ
          // when this row exists — and the first is wrong: a GitHub account can
          // add any address it can receive mail at, so a non-primary verified
          // address is not the account's identity.
          {
            email: `secondary+${options.user.login}@users.noreply.github.test`,
            primary: false,
            verified: true,
            visibility: null,
          },
        ]);
      }

      return json({ message: "Not Found" }, 404);
    },
  });

  const origin = `https://localhost:${server.port}`;

  return {
    origin,
    authorizationUrl: `${origin}/login/oauth/authorize`,
    tokenUrl: `${origin}/login/oauth/access_token`,
    userInfoUrl: `${origin}/user`,
    requests,
    routes: () => [...requests],
    stop: () => {
      server.stop(true);
    },
  };
}

function bearer(request: Request): string | null {
  const authorization = request.headers.get("authorization") ?? "";
  // GitHub accepts both spellings; Better Auth sends `Bearer`.
  for (const scheme of ["Bearer ", "token "]) {
    if (authorization.startsWith(scheme)) return authorization.slice(scheme.length);
  }
  return null;
}
