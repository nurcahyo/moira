// A real TLS certificate for the fixture servers, and a way to trust it that is
// not "turn certificate validation off".
//
// ============================================================================
// WHY TLS AT ALL, FOR A LOCAL FIXTURE
// ============================================================================
//
// Two Moira-side gates make an https fixture the only workable shape, and they
// have DIFFERENT remedies — getting one right does not get the other right:
//
//  (i) `auth_provider_settings` URLs go through `validate_https_url`
//      (`src/application/auth_settings.rs`), which is https-only with NO
//      `allow_http` escape hatch, and performs ONLY a scheme-and-host check —
//      no private-host check. So `https://localhost:PORT` PASSES and
//      `http://localhost:PORT` is a hard `400 auth_provider_url_not_allowed`.
//      Nothing on the Moira side can be configured to accept the http form.
//
// (ii) `POST /api/v1/admin/jwt-issuers`'s `jwks_url` goes somewhere else
//      entirely — `reject_denied_jwks_url` → `validate_jwks_url`
//      (`src/security/ssrf.rs`) — which is a full SSRF check that DOES reject
//      loopback, private and link-local addresses. Its escape hatch is
//      `auth.jwks.allow_insecure_dev_urls`
//      (`MOIRA_AUTH__JWKS__ALLOW_INSECURE_DEV_URLS=true`), and
//      `Settings::validate` HARD-FAILS production when it is set. So a fixture
//      Moira needs that flag, and a production Moira can never have it.
//
// A fixture that terminates TLS satisfies (i) directly and makes (ii) a
// configuration question rather than a redesign.
//
// ============================================================================
// HOW THE FIXTURE CERTIFICATE IS TRUSTED
// ============================================================================
//
// Bun reads `NODE_EXTRA_CA_CERTS` once, at process start. Setting it from a
// `bunfig` preload is too late — verified empirically: the handshake still fails
// with `self signed certificate`. And `bun test` is invoked directly, so there
// is no wrapper script to set it in.
//
// `NODE_TLS_REJECT_UNAUTHORIZED=0` does work when set at runtime, and is what
// most fixtures reach for. It is NOT what this one does, because it disables
// chain validation for every origin in the process — including any real one a
// future test touches.
//
// Instead `trustFixtureCa()` installs a `fetch` wrapper that attaches the
// fixture CA to requests for the fixture's own origins, via Bun's per-request
// `tls.ca` option, and passes everything else through untouched. The result is a
// REAL TLS handshake with REAL chain validation against a known CA. Nothing is
// stubbed: every response in these tests comes off a socket.
//
// The wrapper delegates to `nativeFetch`, NOT to whatever `globalThis.fetch`
// happens to be — happy-dom's registrar has already replaced it by the time any
// test runs, and happy-dom's implementation ignores the `tls` option entirely.
// See `native-globals.ts`.

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { nativeFetch } from "./native-globals";

export interface FixtureTls {
  readonly certPath: string;
  readonly keyPath: string;
  readonly cert: Buffer;
  readonly key: Buffer;
}

let generated: FixtureTls | undefined;

/**
 * Generate (once per process) a self-signed certificate valid for `localhost`
 * and `127.0.0.1`.
 *
 * Shells out to `openssl` rather than using a JS certificate library: Node's
 * `crypto` cannot mint an X.509 certificate, and adding a dependency that can
 * would put a certificate-authority implementation in the console's dependency
 * tree — and therefore in the Trivy scan — to serve a test fixture.
 *
 * REQUIREMENT, stated so it fails legibly: `openssl` must be on PATH. It is on
 * macOS and on `ubuntu-latest`. If a future runner lacks it, this throws with
 * that sentence rather than producing an empty key file.
 */
export function fixtureTls(): FixtureTls {
  if (generated !== undefined) return generated;

  const dir = mkdtempSync(join(tmpdir(), "moira-console-fixture-tls-"));
  const keyPath = join(dir, "key.pem");
  const certPath = join(dir, "cert.pem");

  const result = spawnSync(
    "openssl",
    [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      keyPath,
      "-out",
      certPath,
      "-days",
      "1",
      "-subj",
      "/CN=localhost",
      "-addext",
      "subjectAltName=DNS:localhost,IP:127.0.0.1",
    ],
    { encoding: "utf8" },
  );

  if (result.status !== 0 || !existsSync(certPath) || !existsSync(keyPath)) {
    throw new Error(
      "could not generate the fixture TLS certificate. `openssl` must be on PATH — " +
        "these tests drive a TLS-terminated mock IdP because Moira's provider URL validation " +
        "is https-only with no escape hatch.\n" +
        (result.stderr ?? ""),
    );
  }

  generated = {
    certPath,
    keyPath,
    cert: readFileSync(certPath),
    key: readFileSync(keyPath),
  };
  return generated;
}

/* -------------------------------------------------------------------------- */
/* Scoped trust                                                               */
/* -------------------------------------------------------------------------- */

const trustedOrigins = new Set<string>();
let replacedFetch: typeof fetch | undefined;

function originOf(input: RequestInfo | URL): string | null {
  const url =
    typeof input === "string" ? input : input instanceof URL ? input.href : (input as Request).url;
  try {
    return new URL(url).origin;
  } catch {
    return null;
  }
}

/**
 * Trust the fixture CA for `origin`, and only for `origin`.
 *
 * Idempotent, and safe to call for several origins. `untrustFixtureCa()`
 * restores whatever `fetch` was installed before, so a suite that forgets to
 * call it leaks a wrapper rather than a disabled trust store.
 */
export function trustFixtureCa(origin: string): void {
  trustedOrigins.add(new URL(origin).origin);
  if (replacedFetch !== undefined) return;

  const ca = fixtureTls().cert;
  replacedFetch = globalThis.fetch;
  const previous = replacedFetch;

  globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
    const origin_ = originOf(input);
    if (origin_ === null || !trustedOrigins.has(origin_)) {
      return previous(input as RequestInfo, init);
    }
    // Bun's `RequestInit` accepts `tls`; the cast is the only way to express it
    // against the DOM lib types this project compiles against. `nativeFetch`
    // rather than `previous`, because `previous` is happy-dom's fetch and it
    // drops the option without saying so.
    return nativeFetch(input as RequestInfo, { ...init, tls: { ca } } as RequestInit);
  }) as typeof fetch;
}

/** Restore the process's previous `fetch`. Always call this in `afterAll`. */
export function untrustFixtureCa(): void {
  if (replacedFetch === undefined) return;
  globalThis.fetch = replacedFetch;
  replacedFetch = undefined;
  trustedOrigins.clear();
}
