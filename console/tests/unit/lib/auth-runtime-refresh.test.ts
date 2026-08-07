// The snapshot's lifetime — issue #152.
//
// ============================================================================
// WHAT THIS FILE IS FOR
// ============================================================================
//
// Until #152 `lib/auth-runtime.ts` read its auth configuration once per process
// and served it forever. `refreshAuthConfig` had exactly one caller and ran only
// when no snapshot existed yet, so an operator who re-pointed the provider — in
// the wizard, or through Moira's admin API — kept being served the OLD one until
// somebody restarted the console. Nothing said so: sign-in failed with
// `ECONNREFUSED` against an endpoint that no longer existed, which reads as "the
// identity provider is down".
//
// The issue's own words: "a cache that is refreshed only by a code path no test
// exercises is the same defect wearing a different hat". So every property below
// is written to go RED if the corresponding line is removed, and each names the
// edit it is defending against.
//
// ============================================================================
// WHY `globalThis.fetch` IS SUBSTITUTED RATHER THAN A CLIENT INJECTED
// ============================================================================
//
// `consoleRuntime` builds its own Moira client through `moiraClientForSetup`,
// and that is the behaviour under test: a seam that let a test hand in a client
// would be a seam that let the shipped path skip the refresh entirely while
// these tests stayed green. `MoiraClient` captures `globalThis.fetch` at
// construction, so replacing the global is what reaches the real call path.
//
// The clock is substituted, through `resetConsoleRuntime({ now })`. A TTL test
// that cannot move time either sleeps for the real interval or asserts nothing.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import {
  AUTH_CONFIG_REFRESH_RETRY_MS,
  AUTH_CONFIG_SNAPSHOT_TTL_MS,
  consoleRuntime,
  invalidateAuthConfig,
  refreshAuthConfig,
  resetConsoleRuntime,
} from "@/lib/auth-runtime";
import { InMemoryConsoleSecretStore } from "@/lib/console-secrets";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "@/lib/env";
import { MoiraClient } from "@/lib/moira-client";
import type { AuthProviderSettingsRecord, TrustedJwtIssuerRecord } from "@/lib/types";

import { createMoiraStub, MOIRA_STUB_BASE_URL, type MoiraStub } from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const CONSOLE_ORIGIN = "https://console.example.com";
const PROVIDER_ID = "11111111-1111-4111-8111-111111111111";
const ISSUER_ID = "22222222-2222-4222-8222-222222222222";
const CLIENT_ID = "console.apps.idp.test";
const CLIENT_SECRET = "the-client-secret-fixture";
const SECRET_KEY = Buffer.alloc(32, 0x2a);

/** The endpoint the FIRST configuration names. */
const FIRST_DISCOVERY = "https://idp-one.example.com/.well-known/openid-configuration";
/** The endpoint an operator re-points the provider at. */
const SECOND_DISCOVERY = "https://idp-two.example.com/.well-known/openid-configuration";

const PROVIDER_LIST_ROUTE = "GET /api/v1/admin/auth/providers";
const ISSUER_LIST_ROUTE = "GET /api/v1/admin/jwt-issuers";

const BASE_ENV: EnvSource = {
  NODE_ENV: "test",
  MOIRA_API_URL: MOIRA_STUB_BASE_URL,
  CONSOLE_PUBLIC_ORIGIN: CONSOLE_ORIGIN,
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin-audience",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: SECRET_KEY.toString("base64"),
  MOIRA_SYSTEM_KEY: "sk_test_bootstrap",
};

function envWith(overrides: EnvSource = {}): ConsoleEnv {
  return readConsoleEnv({ ...BASE_ENV, ...overrides });
}

function providerRow(
  overrides: Partial<AuthProviderSettingsRecord> = {},
): AuthProviderSettingsRecord {
  return {
    id: PROVIDER_ID,
    method: "generic_oidc",
    display_name: "Corporate IdP",
    enabled: true,
    requested_scopes: ["openid", "email"],
    allowed_email_domains: ["example.com"],
    allowed_algorithms: ["ES256"],
    expected_audiences: [],
    redirect_uris: [],
    metadata: null,
    status: "active",
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 4,
    discovery_url: FIRST_DISCOVERY,
    client_id: CLIENT_ID,
    trusted_jwt_issuer_id: ISSUER_ID,
    ...overrides,
  };
}

function issuerRow(): TrustedJwtIssuerRecord {
  return {
    id: ISSUER_ID,
    // The INCUMBENT: bound to `env.bffIssuerUrl`, which defaults to the console
    // origin. `consoleProviderIdFor` derives `moira-console-idp` from it.
    issuer: CONSOLE_ORIGIN,
    jwks_url: `${CONSOLE_ORIGIN}/api/auth/.well-known/jwks.json`,
    expected_audiences: ["moira-admin-audience"],
    allowed_algorithms: ["ES256"],
    subject_claim: "sub",
    clock_skew_seconds: 60,
    allow_delegation: false,
    status: "active",
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
  };
}

/* -------------------------------------------------------------------------- */
/* The harness                                                                */
/* -------------------------------------------------------------------------- */

/** What Moira currently says. Mutated mid-test to model an operator's write. */
let rows: AuthProviderSettingsRecord[];
/** Set to make the next Moira read fail at transport level. */
let moiraDown: boolean;
let stub: MoiraStub;
let clockMs: number;
let originalFetch: typeof fetch;

/** How many times the configuration has actually been read from Moira. */
function reads(): number {
  return stub.requestsFor(PROVIDER_LIST_ROUTE).length;
}

function page(data: unknown[]) {
  return { status: 200, body: { data, pagination: { has_more: false, next_cursor: null } } };
}

beforeEach(async () => {
  rows = [providerRow()];
  moiraDown = false;
  clockMs = 1_000_000;
  stub = createMoiraStub({
    [PROVIDER_LIST_ROUTE]: () => {
      // A 503 rather than a thrown socket error: `MoiraClient` maps both onto a
      // `MoiraRequestError`, and a status is what a stub can produce honestly.
      if (moiraDown) return { status: 503, body: { error: { code: "service_unavailable" } } };
      return page(rows);
    },
    [ISSUER_LIST_ROUTE]: () => (moiraDown ? { status: 503 } : page([issuerRow()])),
  });

  const store = new InMemoryConsoleSecretStore(SECRET_KEY);
  await store.put(PROVIDER_ID, CLIENT_ID, CLIENT_SECRET);
  resetConsoleRuntime({ store, now: () => clockMs });

  originalFetch = globalThis.fetch;
  globalThis.fetch = stub.fetch;
});

afterEach(() => {
  globalThis.fetch = originalFetch;
  resetConsoleRuntime();
});

/** The discovery URL the process is currently serving sign-in from. */
async function servedDiscovery(env: ConsoleEnv = envWith()): Promise<string | null> {
  const state = await consoleRuntime(env);
  if (!state.ok) throw new Error(`expected a resolvable runtime, got ${state.resolution.problem}`);
  return state.configs[0]?.discoveryUrl ?? null;
}

/* -------------------------------------------------------------------------- */
/* The bound itself                                                           */
/* -------------------------------------------------------------------------- */

describe("the staleness bound is a bound", () => {
  // WITHOUT THIS TEST the cheapest way to defeat every other test in this file
  // is to raise `AUTH_CONFIG_SNAPSHOT_TTL_MS` to a day. Each of them advances the
  // clock BY the constant, so they would all stay green while the console went
  // back to "restart it". The number, not just the mechanism, is the property.
  test("the TTL is short enough that an operator does not experience it as forever", () => {
    expect(AUTH_CONFIG_SNAPSHOT_TTL_MS).toBeGreaterThan(0);
    expect(
      AUTH_CONFIG_SNAPSHOT_TTL_MS,
      "this is the only thing that observes a write made through Moira's admin API; five " +
        "minutes is already Moira's own backstop TTL, and that one sits behind a NOTIFY " +
        "listener that sees every write",
    ).toBeLessThanOrEqual(5 * 60_000);
  });

  test("the retry floor cannot outlast the TTL it backs off from", () => {
    // A retry floor longer than the TTL makes the TTL a lie: the snapshot would
    // be eligible for refresh and forbidden from refreshing at the same time,
    // for longer than the bound this file claims.
    expect(AUTH_CONFIG_REFRESH_RETRY_MS).toBeGreaterThan(0);
    expect(AUTH_CONFIG_REFRESH_RETRY_MS).toBeLessThanOrEqual(AUTH_CONFIG_SNAPSHOT_TTL_MS);
  });
});

/* -------------------------------------------------------------------------- */
/* The TTL                                                                    */
/* -------------------------------------------------------------------------- */

describe("a fresh snapshot is served from memory", () => {
  test("a second resolve inside the TTL makes no Moira call", async () => {
    // The control for everything below, and the reason the fix is a TTL rather
    // than "read it every time": `consoleRuntime()` runs on every authenticated
    // request and every page render, so a per-request read is a Moira round trip
    // on the path whose failure mode is "the console is down".
    await servedDiscovery();
    const afterFirst = reads();
    clockMs += AUTH_CONFIG_SNAPSHOT_TTL_MS - 1;
    await servedDiscovery();
    expect(reads()).toBe(afterFirst);
  });
});

describe("a provider changed in Moira takes effect without a restart", () => {
  test("past the TTL the configuration is re-read, and the NEW endpoint is served", async () => {
    // #152's headline. Before the fix this assertion returned FIRST_DISCOVERY
    // for the lifetime of the process, and the sign-in that followed dialled an
    // endpoint that had been decommissioned.
    expect(await servedDiscovery()).toBe(FIRST_DISCOVERY);

    // The operator re-points the provider — through the admin API, another
    // replica, or `moirad` itself. The console is told nothing.
    rows = [providerRow({ discovery_url: SECOND_DISCOVERY, version: 5 })];
    expect(await servedDiscovery()).toBe(FIRST_DISCOVERY); // still inside the TTL

    clockMs += AUTH_CONFIG_SNAPSHOT_TTL_MS;
    expect(
      await servedDiscovery(),
      "the snapshot aged out and the console kept serving the superseded configuration",
    ).toBe(SECOND_DISCOVERY);
  });

  test("a provider DISABLED in Moira stops being offered", async () => {
    // The other direction, and the one a "keep the old snapshot on any refusal"
    // implementation gets wrong: Moira answered, and the answer is that this
    // deployment has no sign-in. Serving the disabled provider anyway is the
    // same defect — a change that is saved, acknowledged, and not in effect.
    expect(await servedDiscovery()).toBe(FIRST_DISCOVERY);

    rows = [providerRow({ enabled: false, version: 5 })];
    clockMs += AUTH_CONFIG_SNAPSHOT_TTL_MS;

    const state = await consoleRuntime(envWith());
    expect(state.ok).toBe(false);
    if (state.ok) return;
    expect(state.resolution.problem).toBe("no_enabled_provider");
  });
});

/* -------------------------------------------------------------------------- */
/* Explicit invalidation                                                      */
/* -------------------------------------------------------------------------- */

describe("invalidateAuthConfig does not wait for the TTL", () => {
  test("the next resolve re-reads, with the clock untouched", async () => {
    expect(await servedDiscovery()).toBe(FIRST_DISCOVERY);
    const afterFirst = reads();

    rows = [providerRow({ discovery_url: SECOND_DISCOVERY, version: 5 })];
    invalidateAuthConfig();

    expect(await servedDiscovery()).toBe(SECOND_DISCOVERY);
    expect(reads()).toBeGreaterThan(afterFirst);
  });

  test("it does not drop the secret store, which a shipped call site must never do", async () => {
    // `resetConsoleRuntime()` would also be an invalidation — and would discard
    // the secret store and the injected database with it, rebuilding the pool
    // mid-request. `invalidateAuthConfig` is narrow for that reason, and this is
    // what notices if it is ever "simplified" into the wider one.
    await servedDiscovery();
    invalidateAuthConfig();
    // Resolving still finds the sealed client secret, so the store survived. A
    // dropped store would resolve against an empty in-memory one and refuse with
    // `console_secret_unavailable`.
    expect(await servedDiscovery()).toBe(FIRST_DISCOVERY);
  });
});

/* -------------------------------------------------------------------------- */
/* When it cannot be refreshed, it says so                                    */
/* -------------------------------------------------------------------------- */

describe("an unrefreshable snapshot is announced, not passed off as current", () => {
  test("a Moira outage keeps sign-in up and marks the configuration stale", async () => {
    await servedDiscovery();
    moiraDown = true;
    clockMs += AUTH_CONFIG_SNAPSHOT_TTL_MS;

    const state = await consoleRuntime(envWith());
    // Still serving: taking sign-in down for the duration of a backend blip
    // would be a worse failure than serving what we have.
    expect(state.ok).toBe(true);
    if (!state.ok) return;
    expect(state.configs[0]?.discoveryUrl).toBe(FIRST_DISCOVERY);
    // ...but SAYING SO. `/login` renders `console.error.auth_config_stale` from
    // this flag. Silence is the half of #152 that a TTL alone does not fix.
    expect(state.stale).toBe(true);
  });

  test("a failed refresh is not retried on every request", async () => {
    await servedDiscovery();
    moiraDown = true;
    clockMs += AUTH_CONFIG_SNAPSHOT_TTL_MS;
    await consoleRuntime(envWith());
    const afterFailure = reads();

    // Without the retry floor, every request arriving at an expired snapshot is
    // another doomed round trip — the console hammering a backend that is
    // already down, on the path it most needs to stay cheap.
    clockMs += AUTH_CONFIG_REFRESH_RETRY_MS - 1;
    await consoleRuntime(envWith());
    expect(reads()).toBe(afterFailure);

    // And it does come back on its own once the floor has passed.
    moiraDown = false;
    clockMs += AUTH_CONFIG_REFRESH_RETRY_MS;
    const recovered = await consoleRuntime(envWith());
    expect(recovered.ok).toBe(true);
    if (!recovered.ok) return;
    expect(recovered.stale).toBe(false);
    expect(reads()).toBeGreaterThan(afterFailure);
  });

  test("with no bootstrap credential the snapshot is served, stale, and nothing is dialled", async () => {
    // The bootstrap deadlock's second half. A console whose operator removed
    // `MOIRA_SYSTEM_KEY` — which is what an operator is told to do with a
    // bootstrap credential — cannot re-read the configuration at all. It still
    // serves it, because the old configuration is the only one anybody could
    // sign in with, and it still says so.
    const credentialled = envWith();
    const noKey = envWith({ MOIRA_SYSTEM_KEY: undefined });

    // Planted the way the header describes it being planted on that deployment:
    // a request that DOES carry a credential.
    await refreshAuthConfig(
      new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: "sk_test_bootstrap" }),
      credentialled,
    );
    const planted = reads();

    clockMs += AUTH_CONFIG_SNAPSHOT_TTL_MS * 10;
    const state = await consoleRuntime(noKey);
    expect(state.ok).toBe(true);
    if (!state.ok) return;
    expect(state.stale).toBe(true);
    // No credential means no read was even attempted — the alternative is a
    // guaranteed 401 against Moira on every page load.
    expect(reads()).toBe(planted);
  });

  test("a fresh snapshot is never announced as stale", async () => {
    // The negative half. A `stale` that were always true would render the notice
    // on every console and stop meaning anything — which is how an operator
    // learns to ignore the one time it matters.
    const state = await consoleRuntime(envWith());
    expect(state.ok).toBe(true);
    if (!state.ok) return;
    expect(state.stale).toBe(false);
  });
});
