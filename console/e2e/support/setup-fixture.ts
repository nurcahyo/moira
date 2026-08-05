// The setup-wizard e2e fixture: what it is, and every value both halves share.
//
// ============================================================================
// WHY THIS EXISTS (issue #71, AC4)
// ============================================================================
//
// `/setup` is a public route, so `a11y.e2e.ts`'s walker has always visited it —
// but the main e2e console runs with NO `MOIRA_SYSTEM_KEY` and with
// `MOIRA_API_URL = https://moira.invalid`, so `withSetupWindow` answered
// `404 setup_unavailable` on every run and `SetupWizard` early-returned its
// two-element refusal panel. Every green "no critical or serious axe violations
// on /setup" line audited that panel. None of the wizard's step organisms —
// welcome, the auth-settings form, the sign-in surface, done — had ever been
// rendered by the suite, so no axe evidence existed for any of them.
//
// A second console server fixes that WITHOUT touching the first one. The main
// server keeps its unreachable Moira and its missing system key (every existing
// assertion, including `smoke.e2e.ts`'s "the root route contacts no external
// origin", is about that server and is unchanged). The fixture server is the
// SAME production standalone artifact, pointed at a stub Moira on loopback and
// given the bootstrap key, so `/setup` reaches `kind: "ready"` and the wizard
// actually renders.
//
// ============================================================================
// WHY THE STUB MOIRA IS TLS-TERMINATED
// ============================================================================
//
// The fixture console is `node .next/standalone/server.js`, and Next's
// standalone entrypoint hard-sets `process.env.NODE_ENV = 'production'` before
// any application code runs. `lib/env.ts` then refuses an http `MOIRA_API_URL`
// (and refuses `CONSOLE_ALLOW_INSECURE_URLS` outright in production). So the
// stub has to speak https, exactly as `tests/support/fixture-tls.ts` explains
// for the unit fixtures — and for the same reason the certificate is generated
// with `openssl` rather than shipped: a private key in the repository is a
// private key in the repository, whatever it is for.
//
// Trust is scoped by `NODE_EXTRA_CA_CERTS` on the FIXTURE CONSOLE PROCESS ONLY.
// Not `NODE_TLS_REJECT_UNAUTHORIZED=0`: that would disable chain validation for
// every origin in that process, and the point of the fixture is that the
// console's own request path is unmodified.
//
// ============================================================================
// WHY THE FIXTURE CONSOLE NEEDS A REAL DATABASE
// ============================================================================
//
// The wizard only reaches its sign-in step when `isProvisioningComplete` holds,
// and one of that gate's six conditions is `consoleSecretStored` — which
// `deriveProvisioningState` answers by asking the console's OWN secret store
// (decision D7). In production `lib/env.ts` requires `CONSOLE_DATABASE_URL`, so
// the store is `PostgresConsoleSecretStore` and the answer comes off a real
// socket. There is no in-memory path available to a production-shaped process,
// and inventing one for the fixture would mean testing a store the deployment
// never uses.
//
// So the fixture gets its own database, created and migrated by
// `run-setup-fixture.ts`. It is derived from `CONSOLE_TEST_DATABASE_URL` — the
// variable the console gate already requires and CI's `console` job already
// provides — but is a DIFFERENT DATABASE from the unit suite's, so neither run
// can see the other's rows.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

import { CONSOLE_ROOT } from "./paths";
import { SENTINEL_ENV } from "./secrets";

/* -------------------------------------------------------------------------- */
/* Ports and origins                                                          */
/* -------------------------------------------------------------------------- */

/**
 * Deliberately adjacent to the main e2e port (3210) and to nothing else. Both
 * are overridable so two checkouts can run the suite at once.
 */
export const SETUP_FIXTURE_CONSOLE_PORT = Number(
  process.env["CONSOLE_SETUP_E2E_PORT"] ?? 3211,
);
export const SETUP_FIXTURE_MOIRA_PORT = Number(
  process.env["CONSOLE_SETUP_E2E_MOIRA_PORT"] ?? 3212,
);

/** Where the browser reaches the fixture console. Plain http on loopback. */
export const SETUP_FIXTURE_BASE_URL = `http://127.0.0.1:${SETUP_FIXTURE_CONSOLE_PORT}`;

/**
 * Where the fixture CONSOLE reaches the stub Moira. `localhost`, not
 * `127.0.0.1`, because the generated certificate's SAN carries both and the
 * hostname form is what a reader recognises as "a TLS origin".
 */
export const SETUP_FIXTURE_MOIRA_URL = `https://localhost:${SETUP_FIXTURE_MOIRA_PORT}`;

/**
 * The origin the fixture console ADVERTISES — never the one it binds.
 *
 * Same deliberate mismatch as the main e2e server: a console behind a
 * TLS-terminating ingress publishes an https issuer and JWKS host while
 * listening on plain http on loopback. `.invalid` so nothing can resolve it.
 */
export const SETUP_FIXTURE_PUBLIC_ORIGIN = "https://console-setup.e2e.invalid";

/* -------------------------------------------------------------------------- */
/* The fixture's own credentials                                              */
/* -------------------------------------------------------------------------- */

/**
 * The bootstrap system key the fixture console runs on.
 *
 * Deliberately the SENTINEL value rather than a fresh string: `forbiddenValues()`
 * already treats every `SENTINEL_ENV` entry as a needle, so the leak scans in
 * `setup-wizard.e2e.ts` check the wizard's rendered steps for this exact value
 * without a second registration anywhere. A key minted here would be a key no
 * scan looked for.
 */
export const SETUP_FIXTURE_SYSTEM_KEY = SENTINEL_ENV["MOIRA_E2E_SENTINEL_SYSTEM_KEY"]!;

/**
 * The OAuth client secret the e2e types into the auth-settings form.
 *
 * Same reasoning: it is already a needle, so "the secret the operator typed
 * never comes back to the browser" is checkable by the existing scanner.
 */
export const SETUP_FIXTURE_CLIENT_SECRET =
  SENTINEL_ENV["MOIRA_E2E_SENTINEL_OAUTH_CLIENT_SECRET"]!;

/** Public identifiers the wizard submits. None of these is a credential. */
export const SETUP_FIXTURE_PROVIDER = {
  displayName: "Fixture OIDC",
  clientId: "moira-console-setup-e2e.apps.fixture.invalid",
  discoveryUrl: "https://idp.fixture.invalid/.well-known/openid-configuration",
  allowedDomain: "example.test",
} as const;

/* -------------------------------------------------------------------------- */
/* The fixture database                                                       */
/* -------------------------------------------------------------------------- */

const UNIT_TEST_DSN_DEFAULT = "postgres://postgres:postgres@127.0.0.1:5432/console_auth_test";

/**
 * The fixture console's own database, on the same server the unit suite uses.
 *
 * A DIFFERENT database, not a different schema: `db/migrate.ts` keeps one ledger
 * per database, and two suites sharing one would have to agree about truncation
 * order forever.
 */
export function setupFixtureDatabaseUrl(env: NodeJS.ProcessEnv = process.env): string {
  const base = env["CONSOLE_SETUP_E2E_DATABASE_URL"];
  if (base !== undefined && base.trim() !== "") return base.trim();
  const url = new URL(env["CONSOLE_TEST_DATABASE_URL"] ?? UNIT_TEST_DSN_DEFAULT);
  url.pathname = "/console_setup_e2e";
  return url.toString();
}

/* -------------------------------------------------------------------------- */
/* The fixture certificate                                                    */
/* -------------------------------------------------------------------------- */

/** Regenerated when older than this; the certificate itself is valid for 30 days. */
const CERT_MAX_AGE_MS = 20 * 24 * 60 * 60 * 1000;

export interface SetupFixtureTls {
  readonly dir: string;
  readonly certPath: string;
  readonly keyPath: string;
}

/**
 * Generate (or reuse) a self-signed certificate for `localhost` / `127.0.0.1`.
 *
 * NOT shared with `tests/support/fixture-tls.ts`, and the split is deliberate:
 * that module's other half installs a Bun-specific `fetch` wrapper
 * (`tls: { ca }` is a Bun `RequestInit` extension) and imports happy-dom-aware
 * globals, none of which loads under the Playwright runner's plain Node. What IS
 * shared is the only part that matters — the exact `openssl` invocation, kept
 * identical so a reader comparing them sees one recipe.
 *
 * The output lives under `console/.setup-e2e/` and is reused across runs so the
 * Playwright config (which is re-imported in every worker process) pays for
 * `openssl` at most once.
 *
 * REQUIREMENT, stated so it fails legibly: `openssl` must be on PATH. It is on
 * macOS and on `ubuntu-latest`.
 */
export function setupFixtureTls(): SetupFixtureTls {
  const dir = path.join(CONSOLE_ROOT, ".setup-e2e");
  const certPath = path.join(dir, "cert.pem");
  const keyPath = path.join(dir, "key.pem");

  if (isFresh(certPath) && isFresh(keyPath)) return { dir, certPath, keyPath };

  mkdirSync(dir, { recursive: true });
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
      "30",
      "-subj",
      "/CN=localhost",
      "-addext",
      "subjectAltName=DNS:localhost,IP:127.0.0.1",
    ],
    { encoding: "utf8" },
  );

  if (result.status !== 0 || !existsSync(certPath) || !existsSync(keyPath)) {
    throw new Error(
      "could not generate the setup-fixture TLS certificate. `openssl` must be on PATH — " +
        "the fixture console runs with NODE_ENV=production, where `lib/env.ts` refuses an " +
        "http MOIRA_API_URL and refuses CONSOLE_ALLOW_INSECURE_URLS outright, so the stub " +
        "Moira has to terminate TLS.\n" +
        (result.stderr ?? ""),
    );
  }
  return { dir, certPath, keyPath };
}

function isFresh(file: string): boolean {
  try {
    const stat = statSync(file);
    return stat.size > 0 && Date.now() - stat.mtimeMs < CERT_MAX_AGE_MS;
  } catch {
    return false;
  }
}

/** The certificate bytes, for a server that has to present them. */
export function readSetupFixtureTls(): { readonly cert: Buffer; readonly key: Buffer } {
  const tls = setupFixtureTls();
  return { cert: readFileSync(tls.certPath), key: readFileSync(tls.keyPath) };
}

/* -------------------------------------------------------------------------- */
/* The stub's control surface                                                 */
/* -------------------------------------------------------------------------- */

/**
 * Where the SPEC (not the console) drives the stub's scenario.
 *
 * Namespaced under `/__fixture/` so it can never be mistaken for part of Moira's
 * contract, and served only by the stub — the console has no route that reaches
 * it and no knowledge that it exists.
 */
export const SETUP_FIXTURE_CONTROL_PATH = "/__fixture/state";

/** What the stub should answer for the next requests. */
export interface SetupFixtureScenario {
  /** `GET /api/v1/admin/setup/claim-status`. `true` closes the setup window. */
  readonly claimed: boolean;
}

export const FRESH_DEPLOYMENT: SetupFixtureScenario = { claimed: false };
export const CLAIMED_DEPLOYMENT: SetupFixtureScenario = { claimed: true };
