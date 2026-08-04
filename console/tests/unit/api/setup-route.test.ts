// The BFF setup door, driven as a route handler rather than as a helper.
//
// ============================================================================
// WHY THIS EXERCISES `GET`/`POST` AND NOT AN EXTRACTED FUNCTION
// ============================================================================
//
// `tests/unit/lib/setup-flow.test.ts` already proves the ORDER of the Moira
// writes against a stub. What it cannot observe is whether anything calls
// `runSetupProvisioning` at all — that is finding F25's shape, and every piece
// of this route's value is in the wiring: the guard in front of it, the closure
// that carries the OAuth client secret past `setup-flow.ts` without that module
// ever naming it, and the mapping from a partial write to a resumable response.
//
// So the tests below import `app/api/setup/route.ts` and call its exported
// handlers. The process-wide wiring — environment, Moira client, secret store,
// session — is substituted through `setSetupWindowDependenciesForTests`, the
// same seam `resetConsoleRuntime` provides for the store, so no test here needs
// an environment, a database, or a network.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { GET, POST } from "@/app/api/setup/route";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "@/lib/env";
import { CONSOLE_CATALOG, t } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import { checkSession, type SessionCheck } from "@/lib/moira-session";
import type { SealedClientSecret, ConsoleSecretStore } from "@/lib/console-secrets";
import { setSetupWindowDependenciesForTests } from "@/lib/setup-window";
import {
  MOIRA_STUB_BASE_URL,
  createMoiraStub,
  errorEnvelope,
  type MoiraStub,
  type StubHandler,
} from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const CONSOLE_ORIGIN = "https://console.example.com";
const CONSOLE_ISSUER = CONSOLE_ORIGIN;
const IDP_ISSUER = "https://accounts.google.com";
const ISSUER_ID = "11111111-1111-4111-8111-111111111111";
const PROVIDER_ID = "22222222-2222-4222-8222-222222222222";
const CLIENT_ID = "client-123.apps.googleusercontent.com";

/**
 * Deliberately unmistakable, and asserted absent from every Moira request body.
 *
 * A short value would collide with ordinary payload text; this one can only
 * appear in a body if the route put it there.
 */
const CLIENT_SECRET = "oauth-client-secret-fixture-4f0c81ba27de95";

const BASE_ENV: EnvSource = {
  NODE_ENV: "test",
  MOIRA_API_URL: MOIRA_STUB_BASE_URL,
  CONSOLE_PUBLIC_ORIGIN: CONSOLE_ORIGIN,
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin-audience",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
  MOIRA_SYSTEM_KEY: "sk_test_bootstrap",
};

function envWith(overrides: EnvSource = {}): ConsoleEnv {
  return readConsoleEnv({ ...BASE_ENV, ...overrides });
}

/** A `ConsoleSecretStore` that records `put()` and reports sealed PRESENCE. */
class RecordingSecretStore implements ConsoleSecretStore {
  readonly puts: Array<{ providerId: string; clientId: string; plaintext: string }> = [];
  /** Provider ids the store claims to hold a sealed secret for. */
  readonly sealedIds = new Set<string>();
  /** Set to make `put` fail, so the store step of the sequence can be exercised. */
  failure: Error | null = null;

  async put(providerId: string, clientId: string, plaintext: string): Promise<void> {
    if (this.failure !== null) throw this.failure;
    this.puts.push({ providerId, clientId, plaintext });
    this.sealedIds.add(providerId);
  }
  async read(providerId: string): Promise<SealedClientSecret | null> {
    if (!this.sealedIds.has(providerId)) return null;
    // Presence is all the route may consult; the members are inert fixtures.
    return {
      version: 1,
      iv: "AAAA",
      ciphertext: "AAAA",
      clientId: CLIENT_ID,
      updatedAt: "2026-08-04T00:00:00Z",
    };
  }
  async reveal(): Promise<string | null> {
    return null;
  }
  async remove(): Promise<void> {}
  async newestUpdatedAt(): Promise<string | null> {
    return null;
  }
}

function issuerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: ISSUER_ID,
    issuer: CONSOLE_ISSUER,
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
    scopes_claim: null,
    ...overrides,
  };
}

function providerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: PROVIDER_ID,
    method: "google_oauth",
    display_name: "Google Workspace",
    enabled: false,
    requested_scopes: [],
    allowed_email_domains: ["example.com"],
    allowed_algorithms: [],
    expected_audiences: [],
    redirect_uris: [],
    metadata: {},
    status: "active",
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    issuer: IDP_ISSUER,
    client_id: CLIENT_ID,
    trusted_jwt_issuer_id: ISSUER_ID,
    ...overrides,
  };
}

const CLAIM_STATUS_ROUTE = "GET /api/v1/admin/setup/claim-status";
const ISSUER_LIST_ROUTE = "GET /api/v1/admin/jwt-issuers";
const ISSUER_CREATE_ROUTE = "POST /api/v1/admin/jwt-issuers";
const PROVIDER_LIST_ROUTE = "GET /api/v1/admin/auth/providers";
const PROVIDER_CREATE_ROUTE = "POST /api/v1/admin/auth/providers";
const PROVIDER_GET_ROUTE = `GET /api/v1/admin/auth/providers/${PROVIDER_ID}`;
const PROVIDER_PATCH_ROUTE = `PATCH /api/v1/admin/auth/providers/${PROVIDER_ID}`;
const PROVIDER_ENABLE_ROUTE = `POST /api/v1/admin/auth/providers/${PROVIDER_ID}/enable`;
const AUTH_METHODS_ROUTE = "GET /api/v1/admin/setup/auth-methods";
const CLAIM_ROUTE = "POST /api/v1/admin/setup/claim";

const emptyIssuerList: StubHandler = () => ({
  status: 200,
  body: { data: [], pagination: { has_more: false, next_cursor: null } },
});

const populatedIssuerList: StubHandler = () => ({
  status: 200,
  body: { data: [issuerRecord()], pagination: { has_more: false, next_cursor: null } },
});

const emptyProviderList: StubHandler = () => ({
  status: 200,
  body: { data: [], pagination: { has_more: false, next_cursor: null } },
});

const enabledProviderList: StubHandler = () => ({
  status: 200,
  body: {
    data: [providerRecord({ enabled: true, version: 2 })],
    pagination: { has_more: false, next_cursor: null },
  },
});

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [CLAIM_STATUS_ROUTE]: () => ({ status: 200, body: { claimed: false } }),
    [ISSUER_LIST_ROUTE]: emptyIssuerList,
    [ISSUER_CREATE_ROUTE]: () => ({ status: 201, body: issuerRecord() }),
    [PROVIDER_LIST_ROUTE]: emptyProviderList,
    [PROVIDER_CREATE_ROUTE]: () => ({ status: 201, body: providerRecord() }),
    [PROVIDER_ENABLE_ROUTE]: () => ({
      status: 200,
      body: providerRecord({ enabled: true, version: 2 }),
    }),
    [AUTH_METHODS_ROUTE]: () => ({
      status: 200,
      body: {
        methods: [
          {
            id: PROVIDER_ID,
            method: "google_oauth",
            display_name: "Google Workspace",
            requested_scopes: ["openid", "email"],
            allowed_email_domains: ["example.com", "ops.example.com"],
            client_id: CLIENT_ID,
            discovery_url: "https://accounts.google.com/.well-known/openid-configuration",
            issuer: IDP_ISSUER,
            jwks_url: "https://www.googleapis.com/oauth2/v3/certs",
          },
        ],
      },
    }),
    [CLAIM_ROUTE]: () => ({
      status: 201,
      body: {
        id: "33333333-3333-4333-8333-333333333333",
        issuer: CONSOLE_ISSUER,
        subject: "sub-abc",
        email: "ops@example.com",
        email_verified: true,
        granted_scopes: ["moira:admin"],
        status: "active",
        created_at: "2026-08-04T00:00:00Z",
        version: 1,
        is_primary: true,
        notice: {
          message_key: "moira.notice.admin_identity_claimed",
          message: "Admin identity claimed.",
        },
      },
    }),
    ...overrides,
  };
}

/* -------------------------------------------------------------------------- */
/* Sessions: HAND-BUILT VERDICTS ARE NOT ALLOWED HERE                         */
/* -------------------------------------------------------------------------- */
//
// `install` substitutes `readSession` wholesale, so no test in this file
// exercises the real session resolution. That is deliberate — the route's job is
// what it DOES with a verdict — but it is only safe while the verdicts handed in
// are ones the real decider can actually produce.
//
// A hand-written literal is not that. The refusal an operator whose domain is
// missing from the allow-list really holds is `email_domain_not_allowed` WITH
// the row that resolved them named on it, and a literal that omitted the row
// would let the route's remedy path pass a test no operator can reproduce. So
// every fixture below is built by calling `checkSession` — the same function
// `consoleSessionCheck` calls on the shipped path — against the configuration
// that would have resolved it. If `checkSession` stops naming the row, or
// changes which conditions it refuses, these fixtures change with it and the
// route tests fail rather than drifting.

/** The configuration a session resolves against, as `checkSession` reads it. */
function authConfig(overrides: {
  readonly moiraProviderId?: string;
  readonly consoleIssuer?: string;
  readonly allowedEmailDomains?: readonly string[];
} = {}) {
  return {
    allowedEmailDomains: overrides.allowedEmailDomains ?? ["example.com"],
    // The console issuer of the configuration that AUTHENTICATES the session.
    // Required from issue #71: `claim` compares the namespace its `slug`
    // resolves to against this, so a session without it could claim in any
    // namespace.
    consoleIssuer: overrides.consoleIssuer ?? CONSOLE_ISSUER,
    // ...and the Moira ROW that configuration was resolved from. `provision`
    // compares the derived row against this before it re-saves an ENABLED
    // provider, so a session without it could re-point any live sign-in
    // provider.
    moiraProviderId: overrides.moiraProviderId ?? PROVIDER_ID,
  };
}

const OTHER_PROVIDER_ID = "44444444-4444-4444-8444-444444444444";

const SIGNED_IN: SessionCheck = checkSession(
  { email: "ops@example.com", emailVerified: true, idpSubject: "sub-abc" },
  authConfig(),
);

/** No session at all — the true state of an anonymous caller in the window. */
const NO_SESSION: SessionCheck = checkSession(null, authConfig());

/** A real, allow-listed session established through a DIFFERENT provider row. */
const SIGNED_IN_ELSEWHERE: SessionCheck = checkSession(
  { email: "ops@other.example", emailVerified: true, idpSubject: "sub-other" },
  authConfig({
    moiraProviderId: OTHER_PROVIDER_ID,
    consoleIssuer: "https://console.example.com/idp/other",
    allowedEmailDomains: ["other.example"],
  }),
);

/**
 * THE OPERATOR THE PATCH PATH EXISTS FOR.
 *
 * Signed in through the deployment's own provider row, and refused by the
 * console's copy of the very allow-list they have come back to widen. Not a
 * contrived shape: it is what `checkSession` returns for the exact sequence the
 * wizard walks an operator through — sign in, claim, `403
 * admin_claim_domain_not_allowed`, "Edit auth settings".
 */
const OPERATOR_OUTSIDE_THE_ALLOW_LIST: SessionCheck = checkSession(
  { email: "ops@newdomain.example", emailVerified: true, idpSubject: "sub-abc" },
  authConfig(),
);

/** The same refusal, but resolved through a row that is NOT the derived one. */
const OUTSIDER_ELSEWHERE: SessionCheck = checkSession(
  { email: "ops@newdomain.example", emailVerified: true, idpSubject: "sub-other" },
  authConfig({ moiraProviderId: OTHER_PROVIDER_ID }),
);

/** Authenticated through the derived row, but the IdP never verified them. */
const UNVERIFIED_HERE: SessionCheck = checkSession(
  { email: "ops@example.com", emailVerified: false, idpSubject: "sub-abc" },
  authConfig(),
);

let stub: MoiraStub;
let store: RecordingSecretStore;

/** Wire the route to a stub Moira, a recording store, and a fixed session. */
function install(
  options: {
    readonly handlers?: Record<string, StubHandler>;
    readonly env?: ConsoleEnv;
    readonly session?: SessionCheck;
  } = {},
): void {
  stub = createMoiraStub(options.handlers ?? handlers());
  store = new RecordingSecretStore();
  const env = options.env ?? envWith();
  setSetupWindowDependenciesForTests({
    env,
    client: new MoiraClient({
      baseUrl: MOIRA_STUB_BASE_URL,
      systemKey: env.moiraSystemKey,
      fetch: stub.fetch,
    }),
    store,
    storageMode: "ephemeral",
    readSession: async () => options.session ?? SIGNED_IN,
  });
}

/**
 * Wire the route to a FULLY PROVISIONED deployment: the trusted issuer and the
 * enabled bound provider exist in Moira's records, and the console holds a
 * sealed secret for the row. This is what the claim action derives its state
 * from — the browser no longer sends one.
 */
function installProvisioned(
  options: {
    readonly handlers?: Record<string, StubHandler>;
    readonly session?: SessionCheck;
    readonly secretSealed?: boolean;
  } = {},
): void {
  install({
    handlers: handlers({
      [ISSUER_LIST_ROUTE]: populatedIssuerList,
      [PROVIDER_LIST_ROUTE]: enabledProviderList,
      ...options.handlers,
    }),
    ...(options.session === undefined ? {} : { session: options.session }),
  });
  if (options.secretSealed !== false) store.sealedIds.add(PROVIDER_ID);
}

function post(body: unknown): Request {
  return new Request("https://console.example.com/api/setup", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

const PROVISION_BODY = {
  action: "provision",
  method: "google_oauth",
  display_name: "Google Workspace",
  issuer: IDP_ISSUER,
  discovery_url: "https://accounts.google.com/.well-known/openid-configuration",
  client_id: CLIENT_ID,
  client_secret: CLIENT_SECRET,
  allowed_email_domains: ["example.com"],
  submission_id: "submission-0001",
} as const;

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

function errorOf(body: Record<string, unknown>): Record<string, unknown> {
  return body["error"] as Record<string, unknown>;
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setSetupWindowDependenciesForTests(null);
});

/* -------------------------------------------------------------------------- */
/* AC3 — the window                                                           */
/* -------------------------------------------------------------------------- */

describe("the setup window is the gate, and it is checked before anything else", () => {
  test("no bootstrap system key: every method answers 404 and makes no Moira call", async () => {
    install({ env: envWith({ MOIRA_SYSTEM_KEY: undefined }) });

    for (const response of [await GET(), await POST(post(PROVISION_BODY))]) {
      expect(response.status).toBe(404);
      const body = errorOf(await json(response));
      expect(body["code"]).toBe("setup_unavailable");
      expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.setup_system_key_absent);
    }
    // THE POINT: not merely refused — refused without asking Moira anything. The
    // console cannot even build a client without the key.
    expect(stub.routes()).toEqual([]);
  });

  test("Moira reports the deployment claimed: every method answers 409", async () => {
    install({
      handlers: handlers({
        [CLAIM_STATUS_ROUTE]: () => ({ status: 200, body: { claimed: true } }),
      }),
    });

    const responses = [
      await GET(),
      await POST(post(PROVISION_BODY)),
      await POST(post({ action: "claim" })),
    ];
    for (const response of responses) {
      expect(response.status).toBe(409);
      const body = errorOf(await json(response));
      expect(body["code"]).toBe("setup_already_claimed");
      expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.setup_already_claimed);
    }

    // Claim-status was re-read on each request rather than cached, and nothing
    // beyond it ran.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, CLAIM_STATUS_ROUTE, CLAIM_STATUS_ROUTE]);
  });

  test("the claim-status read itself failing is a keyed Moira failure, not a 500", async () => {
    install({
      handlers: handlers({
        [CLAIM_STATUS_ROUTE]: () => ({ status: 503, body: errorEnvelope("database_unavailable") }),
      }),
    });
    const response = await GET();
    expect(response.status).toBe(503);
    const error = errorOf(await json(response)) as Record<string, unknown>;
    expect(error["remedy"]).toBe("wait_for_backend");
    // The boundary rule: Moira's `request_id` and `details` do not cross.
    expect(JSON.stringify(error)).not.toContain("req_stub_0001");
    expect(JSON.stringify(error)).not.toContain("must not cross the boundary");
  });

  test("a response body is never cacheable", async () => {
    expect((await GET()).headers.get("cache-control")).toBe("no-store");
    expect((await POST(post({ action: "nope" }))).headers.get("cache-control")).toBe("no-store");
  });
});

/* -------------------------------------------------------------------------- */
/* GET — the display-safe aggregation (D4)                                    */
/* -------------------------------------------------------------------------- */

describe("GET aggregates server-side and publishes a narrowed view", () => {
  test("it reads auth-methods with the system key and returns a projection", async () => {
    const response = await GET();
    expect(response.status).toBe(200);
    const body = await json(response);

    // Claim-status (the guard), the aggregation, then the rehydration lookup —
    // which stops at the issuer list when no issuer is registered yet.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, AUTH_METHODS_ROUTE, ISSUER_LIST_ROUTE]);
    expect(stub.requestsFor(AUTH_METHODS_ROUTE)[0]?.headers["X-Moira-System-Key"]).toBe(
      "sk_test_bootstrap",
    );

    expect(body["claimed"]).toBe(false);
    expect(body["console_issuer"]).toBe(CONSOLE_ISSUER);
    expect(body["audience"]).toBe("moira-admin-audience");
    expect(body["methods"]).toEqual([
      {
        id: PROVIDER_ID,
        method: "google_oauth",
        display_name: "Google Workspace",
        interactive: true,
        has_client_id: true,
        has_discovery_url: true,
        allowed_email_domain_count: 2,
      },
    ]);
    // Nothing provisioned yet: the derived state is empty, and the wizard will
    // start from the auth-settings step.
    expect(body["state"]).toMatchObject({ providerId: null, trustedJwtIssuerId: null });
  });

  test("a provisioned deployment REHYDRATES the display-safe state — the OAuth round trip depends on it", async () => {
    // Sign-in navigates the operator away from /setup and back; the fresh
    // document knows only what this response tells it. Without these fields the
    // claim step is permanently unreachable after the very sign-in it requires.
    installProvisioned();
    const body = await json(await GET());
    expect(body["state"]).toEqual({
      trustedJwtIssuerId: ISSUER_ID,
      trustedJwtIssuerVersion: 1,
      providerId: PROVIDER_ID,
      providerVersion: 2,
      providerTrustedJwtIssuerId: ISSUER_ID,
      providerEnabled: true,
      allowedEmailDomainCount: 1,
      consoleSecretStored: true,
    });
    expect(body["provider_id"]).toBeString();
  });

  test("the raw auth-methods response does not cross to the browser", async () => {
    // Decision D4, asserted rather than described. The setup window is reachable
    // without a session, so `allowed_email_domains` — the deny-by-default
    // admin-claim policy — must not be published here, and neither must the IdP
    // endpoint set. Moira withholds the same field from its ANONYMOUS projection
    // for exactly this reason. Run against a PROVISIONED deployment on purpose:
    // the rehydrated `state` reads the raw provider rows, and it too must
    // publish a domain COUNT, never a domain.
    installProvisioned();
    const serialised = JSON.stringify(await json(await GET()));
    expect(serialised).not.toContain("ops.example.com");
    expect(serialised).not.toContain("allowed_email_domains");
    expect(serialised).not.toContain("googleapis.com");
    expect(serialised).not.toContain(CLIENT_ID);
  });

  test("`?slug=` scopes the rehydration to THAT namespace, and is echoed back", async () => {
    // The escape hatch's other half. An operator whose enabled provider is
    // broken provisions a replacement under a new slug; the OAuth round trip and
    // every reload come back through this handler as a plain GET, and the query
    // is the only thing that survives them. A rehydration that always answered
    // for the incumbent would land them on the broken row's state with the wrong
    // `provider_id` behind the sign-in button.
    installProvisioned();
    const body = await json(await GET(new Request("http://console.local/api/setup?slug=recovery")));

    // The echo is what the wizard puts back on the provision body, on the claim
    // body and on the callback URL.
    expect(body["slug"]).toBe("recovery");
    // Nothing is provisioned under `recovery` yet — the fixture's row is bound
    // to the INCUMBENT issuer — so the derivation for this namespace is empty.
    // That is the point: this deployment's incumbent row must not be handed to a
    // wizard run that is deliberately not addressing it.
    expect(body["state"]).toMatchObject({ providerId: null, trustedJwtIssuerId: null });
  });

  test("an absent or empty `?slug=` is the incumbent, not a refusal", async () => {
    installProvisioned();
    for (const url of ["http://console.local/api/setup", "http://console.local/api/setup?slug="]) {
      const body = await json(await GET(new Request(url)));
      expect(body["slug"]).toBeNull();
      expect(body["state"]).toMatchObject({ providerId: PROVIDER_ID });
    }
  });

  test("a malformed `?slug=` is the same keyed 400 the provision body gets", async () => {
    // One spelling of the rule: the slug becomes a URL path segment and part of
    // the issuer string Moira pins tokens to, and `consoleIssuerForSlug` throws a
    // developer diagnostic on a bad one. Caught here rather than surfacing as a
    // 500.
    const response = await GET(new Request("http://console.local/api/setup?slug=Not%20A%20Slug"));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_provider_slug_invalid,
    );
    // Refused before the guard spent anything on Moira.
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* AC1 — provisioning runs the sequence and stores the secret console-side     */
/* -------------------------------------------------------------------------- */

describe("POST provision runs issuer -> provider -> secret -> enable", () => {
  test("the four writes happen in exactly that order", async () => {
    const response = await POST(post(PROVISION_BODY));
    expect(response.status).toBe(201);

    expect(stub.routes()).toEqual([
      CLAIM_STATUS_ROUTE,
      // The DERIVATION, before anything is written: which trusted issuer this
      // console owns, and therefore which provider row — if any — a write here
      // is allowed to touch. It stops at the issuer list on a fresh deployment
      // because there is no issuer yet, so there can be no bound row either.
      ISSUER_LIST_ROUTE,
      ISSUER_LIST_ROUTE,
      ISSUER_CREATE_ROUTE,
      PROVIDER_CREATE_ROUTE,
      PROVIDER_ENABLE_ROUTE,
    ]);

    const body = await json(response);
    expect(body["console_issuer"]).toBe(CONSOLE_ISSUER);
    expect(body["submission_id"]).toBe("submission-0001");
    expect(body["state"]).toMatchObject({
      trustedJwtIssuerId: ISSUER_ID,
      providerId: PROVIDER_ID,
      providerTrustedJwtIssuerId: ISSUER_ID,
      providerEnabled: true,
      consoleSecretStored: true,
    });
    expect((body["trace"] as unknown[]).length).toBe(4);
  });

  test("`consoleSecretStore().put()` is genuinely reached — this route is its first caller", () => {
    // The assertion the whole item exists for. `put()` shipped with a unit suite
    // and no production caller; a spy on the STORE is the only thing that can
    // observe whether the wiring reaches it.
    return POST(post(PROVISION_BODY)).then(() => {
      expect(store.puts).toEqual([
        { providerId: PROVIDER_ID, clientId: CLIENT_ID, plaintext: CLIENT_SECRET },
      ]);
    });
  });

  test("the client secret appears in NO Moira request body, on any step", async () => {
    await POST(post(PROVISION_BODY));
    const onTheWire = JSON.stringify(stub.requests);
    expect(onTheWire).not.toContain(CLIENT_SECRET);
    // Nor in the response the browser gets back.
    const echoed = JSON.stringify(await json(await POST(post(PROVISION_BODY))));
    expect(echoed).not.toContain(CLIENT_SECRET);
  });

  test("the provider create body carries the issuer id and never `enabled`", async () => {
    await POST(post(PROVISION_BODY));
    const created = stub.bodyOf(PROVIDER_CREATE_ROUTE) as Record<string, unknown>;
    expect(created["trusted_jwt_issuer_id"]).toBe(ISSUER_ID);
    expect("enabled" in created).toBe(false);
    // The row's `issuer` is the IdP's, never the console's — see setup-flow.ts.
    expect(created["issuer"]).toBe(IDP_ISSUER);
    expect(created["allowed_email_domains"]).toEqual(["example.com"]);
  });

  test("the two Idempotency-Keys are derived from the submission id and differ", async () => {
    await POST(post(PROVISION_BODY));
    const issuerKey = stub.requestsFor(ISSUER_CREATE_ROUTE)[0]?.headers["Idempotency-Key"];
    const providerKey = stub.requestsFor(PROVIDER_CREATE_ROUTE)[0]?.headers["Idempotency-Key"];
    expect(issuerKey).toBeString();
    expect(providerKey).toBeString();
    // One string across two operations is a conflict waiting for the first retry.
    expect(issuerKey).not.toBe(providerKey);

    // Derived, so the same submission replays the same keys.
    install();
    await POST(post(PROVISION_BODY));
    expect(stub.requestsFor(ISSUER_CREATE_ROUTE)[0]?.headers["Idempotency-Key"]).toBe(
      issuerKey as string,
    );
  });

  test("a provider row Moira returns without a client_id fails the secret step", async () => {
    install({
      handlers: handlers({
        [PROVIDER_CREATE_ROUTE]: () => ({ status: 201, body: providerRecord({ client_id: null }) }),
      }),
    });
    const response = await POST(post(PROVISION_BODY));
    const error = errorOf(await json(response));
    expect(error["step"]).toBe("store_console_secret");
    // Nothing was sealed, and the provider was left disabled.
    expect(store.puts).toEqual([]);
    expect(stub.routes()).not.toContain(PROVIDER_ENABLE_ROUTE);
  });
});

/* -------------------------------------------------------------------------- */
/* AC2 — every step's failure names its own remedy, and resume resumes         */
/* -------------------------------------------------------------------------- */

describe("a partial write comes back resumable, with the remedy for its step", () => {
  const cases = [
    {
      step: "ensure_trusted_jwt_issuer",
      remedy: "retry",
      overrides: {
        [ISSUER_CREATE_ROUTE]: () => ({ status: 400, body: errorEnvelope("jwks_url_rejected") }),
      },
    },
    {
      step: "create_auth_provider",
      remedy: "retry_reuses_trusted_jwt_issuer",
      overrides: {
        [PROVIDER_CREATE_ROUTE]: () => ({
          status: 400,
          body: errorEnvelope("auth_provider_url_not_allowed"),
        }),
      },
    },
    {
      step: "enable_auth_provider",
      remedy: "retry_enable_no_secret_re_entry",
      overrides: {
        [PROVIDER_ENABLE_ROUTE]: () => ({
          status: 409,
          body: errorEnvelope("duplicate_enabled_provider_for_issuer"),
        }),
      },
    },
  ] as const;

  for (const scenario of cases) {
    test(`${scenario.step} -> ${scenario.remedy}`, async () => {
      install({ handlers: handlers({ ...scenario.overrides }) });
      const response = await POST(post(PROVISION_BODY));
      const error = errorOf(await json(response));

      expect(error["code"]).toBe("setup_provisioning_failed");
      expect(error["step"]).toBe(scenario.step);
      expect(error["remedy"]).toBe(scenario.remedy);
      expect(error["message_key"]).toBeString();
      expect(t(error["message_key"] as string)).not.toBe(error["message_key"]);
      expect(error["state"]).toBeDefined();
      expect(error["submission_id"]).toBe("submission-0001");
      expect(JSON.stringify(error)).not.toContain(CLIENT_SECRET);
    });
  }

  test("store_console_secret -> retry_or_discard_provider", async () => {
    // The one step whose failure is the CONSOLE's, so it is provoked at the
    // store rather than at the stub.
    install();
    store.failure = new Error("the console database refused the write");
    const response = await POST(post(PROVISION_BODY));
    const error = errorOf(await json(response));
    expect(error["step"]).toBe("store_console_secret");
    expect(error["remedy"]).toBe("retry_or_discard_provider");
    expect(error["requires_client_secret_re_entry"]).toBe(true);
    expect(error["moira"]).toBeNull();
    // The provider was created and deliberately left disabled.
    expect(stub.routes()).not.toContain(PROVIDER_ENABLE_ROUTE);
  });

  test("the enable step does NOT ask for the secret again — it is already stored", async () => {
    install({
      handlers: handlers({
        [PROVIDER_ENABLE_ROUTE]: () => ({
          status: 409,
          body: errorEnvelope("duplicate_enabled_provider_for_issuer"),
        }),
      }),
    });
    const error = errorOf(await json(await POST(post(PROVISION_BODY))));
    expect(error["remedy"]).toBe("retry_enable_no_secret_re_entry");
    // Derived from the STATE, not from the remedy string: by this point the
    // console holds the secret, so re-entry would be asking for a value it has.
    expect(error["requires_client_secret_re_entry"]).toBe(false);
    expect((error["state"] as Record<string, unknown>)["consoleSecretStored"]).toBe(true);
    expect(store.puts.length).toBe(1);
  });

  test("a retry with `resume` does NOT re-POST the trusted JWT issuer", async () => {
    // The §0 partial state. A naive retry re-POSTs the issuer into
    // `trusted_jwt_issuers_issuer_active_unique`, which Moira does not map to a
    // 409 — the operator would see an opaque 500. Reuse-first is what avoids it,
    // and this asserts the route actually feeds the recorded state back in.
    install({
      handlers: handlers({
        [PROVIDER_CREATE_ROUTE]: () => ({
          status: 400,
          body: errorEnvelope("auth_provider_url_not_allowed"),
        }),
      }),
    });
    const failure = errorOf(await json(await POST(post(PROVISION_BODY))));
    const state = failure["state"];
    expect((state as Record<string, unknown>)["trustedJwtIssuerId"]).toBe(ISSUER_ID);

    // Second attempt: the issuer row now exists, so the list finds it.
    install({ handlers: handlers({ [ISSUER_LIST_ROUTE]: populatedIssuerList }) });
    const retry = await POST(post({ ...PROVISION_BODY, resume: state }));
    expect(retry.status).toBe(201);
    expect(stub.routes()).toEqual([
      CLAIM_STATUS_ROUTE,
      // Derivation: the issuer now exists, so the bound-provider lookup runs
      // too and finds none — the first attempt died before the create.
      ISSUER_LIST_ROUTE,
      PROVIDER_LIST_ROUTE,
      // Provisioning: reuse-first, so the issuer is adopted rather than
      // re-POSTed, and the provider is created for the first time.
      ISSUER_LIST_ROUTE,
      PROVIDER_CREATE_ROUTE,
      PROVIDER_ENABLE_ROUTE,
    ]);
    expect(stub.routes()).not.toContain(ISSUER_CREATE_ROUTE);
  });

  test("a resume payload the console cannot read is refused, not silently restarted", async () => {
    const response = await POST(post({ ...PROVISION_BODY, resume: { providerId: 7 } }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_resume_state_invalid,
    );
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE]);
  });

  test("an empty client_secret WITHOUT a sealed one on record is still refused", async () => {
    // The relaxation is exactly one shape wide, and the fact it turns on is the
    // console's own: a secret sealed for the row the SERVER derived. A resume
    // that says `consoleSecretStored: true` cannot manufacture that fact — here
    // the deployment is unprovisioned, so the derived answer is `false` and the
    // secret is still required.
    const resume = {
      trustedJwtIssuerId: ISSUER_ID,
      trustedJwtIssuerVersion: 1,
      providerId: PROVIDER_ID,
      providerVersion: 2,
      providerTrustedJwtIssuerId: ISSUER_ID,
      providerEnabled: false,
      allowedEmailDomainCount: 1,
      consoleSecretStored: true,
    };
    const response = await POST(post({ ...PROVISION_BODY, client_secret: "", resume }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_client_secret_required,
    );
    // The derivation ran (it is what answered the question) and nothing else.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE]);
    expect(store.puts).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* THE BLOCKER: which row a privileged write may touch is DERIVED, not sent    */
/* -------------------------------------------------------------------------- */
//
// The setup window runs on the bootstrap system key with no session in front of
// it, so every body field that selects a Moira row selects what an anonymous
// caller can rewrite with that key while the window is open. `resume` was
// exactly that field: shape-checked only, its `providerId` steering
// `getAuthProvider` + `patchAuthProvider`, its `consoleSecretStored` standing
// in for proof that the console holds the row's OAuth client secret. `GET
// /api/setup` publishes the row ids.
//
// The group below is what replaced the old "a resume that names a provider row
// PATCHes it" test. That test asserted the unsafe behaviour as the contract:
// it named a row in the body and expected the PATCH. The contract now is that
// the row comes from `deriveProvisioningState`, that a disagreeing hint is
// refused, and that the legitimate remedies still work.

describe("the re-save target is server-derived, and `resume` is only a hint", () => {
  /**
   * A deployment whose console issuer already owns an ENABLED bound provider.
   *
   * The session is passed EXPLICITLY rather than left to `install`'s default,
   * because from this wave on it is load-bearing here: an enabled row is a live
   * authenticator and may only be re-saved by a caller signed in through it, so
   * a test that got its session by accident would be a test that stopped
   * describing why it passes. `SIGNED_IN.moiraProviderId === PROVIDER_ID` — the
   * row these handlers serve.
   */
  function installReSavable(session: SessionCheck = SIGNED_IN): void {
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        [PROVIDER_LIST_ROUTE]: enabledProviderList,
        [PROVIDER_GET_ROUTE]: () => ({
          status: 200,
          body: providerRecord({ enabled: true, version: 2 }),
        }),
        [PROVIDER_PATCH_ROUTE]: () => ({
          status: 200,
          body: providerRecord({
            enabled: true,
            version: 3,
            allowed_email_domains: ["example.com", "gmail.com"],
          }),
        }),
      }),
      session,
    });
    store.sealedIds.add(PROVIDER_ID);
  }

  const AGREEING_HINT = {
    trustedJwtIssuerId: ISSUER_ID,
    trustedJwtIssuerVersion: 1,
    providerId: PROVIDER_ID,
    providerVersion: 2,
    providerTrustedJwtIssuerId: ISSUER_ID,
    providerEnabled: true,
    allowedEmailDomainCount: 1,
    consoleSecretStored: true,
  };

  /** The domain-refusal remedy's body: add the domain, no secret re-entry. */
  const RE_SAVE_BODY = {
    ...PROVISION_BODY,
    client_secret: "",
    allowed_email_domains: ["example.com", "gmail.com"],
  };

  const DERIVE_THEN_PATCH = [
    CLAIM_STATUS_ROUTE,
    // The authority: this console's trusted issuer, then the row bound to it.
    ISSUER_LIST_ROUTE,
    PROVIDER_LIST_ROUTE,
    // Provisioning: adopt the issuer, read the row for a fresh If-Match, patch.
    ISSUER_LIST_ROUTE,
    PROVIDER_GET_ROUTE,
    PROVIDER_PATCH_ROUTE,
  ];

  test("the domain-refusal re-save PATCHes the DERIVED row and needs no secret re-entry", async () => {
    // The remedy the `console.setup.domain_not_allowed.*` copy prescribes must
    // still be followable: "add {domain} below, save the provider again".
    // Replaying the create against the same submission id with a changed body
    // would be `409 idempotency_conflict`; a fresh create would mint a second
    // row the partial unique index refuses at enable. So the PATCH stays — it
    // is the TARGET that stopped coming from the request.
    installReSavable();
    const response = await POST(post({ ...RE_SAVE_BODY, resume: AGREEING_HINT }));
    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual(DERIVE_THEN_PATCH);

    const patched = stub.bodyOf(PROVIDER_PATCH_ROUTE) as Record<string, unknown>;
    expect(patched["allowed_email_domains"]).toEqual(["example.com", "gmail.com"]);
    expect("enabled" in patched).toBe(false);
    // The sealed secret still stands; nothing was re-written and nothing asked.
    expect(store.puts).toEqual([]);
    expect((await json(response))["state"]).toMatchObject({
      providerEnabled: true,
      allowedEmailDomainCount: 2,
      consoleSecretStored: true,
    });
  });

  test("the same re-save works with NO resume at all — the hint was never what chose the row", async () => {
    // The reload case, and the proof that the hint is not load-bearing: a fresh
    // document (a back-navigation after a reload, say) sends no `resume`, and
    // the console still finds the row it owns, still patches it, and still does
    // not ask for a secret it already holds.
    //
    // What it does NOT do is admit an ANONYMOUS caller by the same door — the
    // row is enabled, so the operator's session is still required. That is the
    // group below; here the operator has one.
    installReSavable();
    const response = await POST(post(RE_SAVE_BODY));
    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual(DERIVE_THEN_PATCH);
    expect(store.puts).toEqual([]);
  });

  test("a resume naming a row this console does NOT own is refused, with nothing written", async () => {
    // The attack. The deployment is unclaimed and the system key is present, so
    // the window is open to an anonymous caller. Moira holds an incumbent
    // provider row — enabled, bound to somebody else's trusted issuer, its id
    // handed out by `GET /api/setup` — and the caller names it, intending to
    // rewrite its allow-list, client id and endpoint URLs and then sign in as
    // an administrator.
    const INCUMBENT_ID = "55555555-5555-4555-8555-555555555555";
    const INCUMBENT_GET = `GET /api/v1/admin/auth/providers/${INCUMBENT_ID}`;
    const INCUMBENT_PATCH = `PATCH /api/v1/admin/auth/providers/${INCUMBENT_ID}`;
    install({
      handlers: handlers({
        [INCUMBENT_GET]: () => ({
          status: 200,
          body: providerRecord({
            id: INCUMBENT_ID,
            enabled: true,
            version: 9,
            trusted_jwt_issuer_id: "99999999-9999-4999-8999-999999999999",
          }),
        }),
        [INCUMBENT_PATCH]: () => {
          throw new Error("the console must never PATCH a row it does not own");
        },
      }),
    });

    const response = await POST(
      post({
        ...PROVISION_BODY,
        allowed_email_domains: ["attacker.example"],
        client_id: "attacker-client-id",
        resume: {
          trustedJwtIssuerId: "99999999-9999-4999-8999-999999999999",
          trustedJwtIssuerVersion: 1,
          providerId: INCUMBENT_ID,
          providerVersion: 9,
          providerTrustedJwtIssuerId: "99999999-9999-4999-8999-999999999999",
          providerEnabled: true,
          allowedEmailDomainCount: 1,
          consoleSecretStored: true,
        },
      }),
    );

    expect(response.status).toBe(409);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("setup_resume_state_conflict");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.setup_resume_state_conflict);

    // NOTHING left the console beyond the guard's claim-status read and the
    // derivation itself. The incumbent row was never even read, let alone
    // written, and no trusted issuer or provider was created on the way.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE]);
    expect(stub.routes()).not.toContain(INCUMBENT_GET);
    expect(stub.routes()).not.toContain(INCUMBENT_PATCH);
    expect(stub.routes()).not.toContain(ISSUER_CREATE_ROUTE);
    expect(stub.routes()).not.toContain(PROVIDER_CREATE_ROUTE);
    expect(store.puts).toEqual([]);
  });

  test("a resume that claims a stored secret the console does not hold is refused", async () => {
    // The second half of the same defect: `consoleSecretStored` was a boolean
    // the caller sent, standing in for proof of ownership. Here the row IS this
    // console's, but nothing is sealed for it — so the hint disagrees and the
    // request is refused rather than admitted with an empty secret.
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        [PROVIDER_LIST_ROUTE]: enabledProviderList,
      }),
    });
    const response = await POST(
      post({ ...PROVISION_BODY, resume: { ...AGREEING_HINT, consoleSecretStored: true } }),
    );
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["code"]).toBe("setup_resume_state_conflict");
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE, PROVIDER_LIST_ROUTE]);
    expect(stub.routes()).not.toContain(PROVIDER_PATCH_ROUTE);
  });

  test("a stale hint that has not caught up with the derived row is refused, not resolved", async () => {
    // A browser that still believes nothing is provisioned, against a console
    // that now owns a row. The console cannot tell this apart from the attack
    // above and does not try: neither writes, and a reload re-derives the truth.
    installReSavable();
    const response = await POST(
      post({ ...RE_SAVE_BODY, resume: { ...AGREEING_HINT, providerId: null } }),
    );
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_resume_state_conflict,
    );
    expect(stub.routes()).not.toContain(PROVIDER_PATCH_ROUTE);
  });

  test("a version or domain-count that drifted is NOT a conflict — the remedy stays followable", async () => {
    // Only the three members that carry authority are compared. Versions and
    // the domain count drift for ordinary reasons (an enable bumped one, the
    // operator just edited the other), and refusing on those would put the
    // domain-refusal remedy straight back into the dead end it came from.
    installReSavable();
    const response = await POST(
      post({
        ...RE_SAVE_BODY,
        resume: { ...AGREEING_HINT, providerVersion: 1, allowedEmailDomainCount: 7 },
      }),
    );
    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual(DERIVE_THEN_PATCH);
  });

  test("a row Moira reports bound elsewhere is refused BEFORE the patch, not after", async () => {
    // Defence in depth, driven through the route: even with the derived id, the
    // runner re-proves the binding on the row it reads back. A row that changed
    // hands between the derivation and the read is refused with the write
    // unmade — the read-back check further down would only have noticed after.
    installReSavable();
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        [PROVIDER_LIST_ROUTE]: enabledProviderList,
        [PROVIDER_GET_ROUTE]: () => ({
          status: 200,
          body: providerRecord({
            enabled: true,
            version: 2,
            trusted_jwt_issuer_id: "99999999-9999-4999-8999-999999999999",
          }),
        }),
        [PROVIDER_PATCH_ROUTE]: () => {
          throw new Error("the console must never PATCH a row it does not own");
        },
      }),
    });
    store.sealedIds.add(PROVIDER_ID);

    const response = await POST(post(RE_SAVE_BODY));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_ordering_violated,
    );
    expect(stub.routes()).not.toContain(PROVIDER_PATCH_ROUTE);
    expect(store.puts).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* THE RESIDUAL: an ENABLED provider may only be re-saved by its own operator  */
/* -------------------------------------------------------------------------- */
//
// Deriving the row server-side settled WHICH row a privileged write may touch.
// It did not settle WHO may ask for that write, and for one shape of row that
// is the whole risk. The setup window is open whenever a system key is present
// and nobody has claimed. Inside it, an ANONYMOUS caller needs no `resume` at
// all: `deriveProvisioningState` finds the console's OWN provider row for them,
// and the update path PATCHes it — client id, issuer, discovery, authorization,
// token, userinfo and jwks URLs, and `allowed_email_domains`. When that row is
// already ENABLED, the allow-list is the last thing between an unclaimed
// deployment and an attacker: re-point the endpoints at an IdP they control,
// widen the allow-list to a domain they own, sign in, claim admin.
//
// The rule now: an enabled row is re-savable only by a caller carrying a valid
// session established THROUGH THAT ROW. Absent and disabled rows are untouched
// by it — an absent row is the first-run create, and a disabled row
// authenticates nobody — and both are asserted below as controls, because the
// cheapest way to "fix" the attack tests is to require a session for every
// provision, which would make first-run setup impossible.

describe("an ENABLED provider is not re-pointed by a caller who cannot prove they are the operator", () => {
  const RE_POINT_BODY = {
    ...PROVISION_BODY,
    // The whole point of the write: another IdP, and a domain the caller owns.
    issuer: "https://idp.attacker.example",
    discovery_url: "https://idp.attacker.example/.well-known/openid-configuration",
    client_id: "attacker-client-id",
    client_secret: "attacker-client-secret",
    allowed_email_domains: ["attacker.example"],
  };

  /**
   * The enabled-row deployment, with every write route booby-trapped.
   *
   * `PROVIDER_PATCH_ROUTE` throwing is not belt and braces: `stub.routes()`
   * proves no PATCH was RECORDED, and this proves none was ATTEMPTED even by a
   * path that swallowed its own failure.
   */
  function installEnabledRow(session: SessionCheck): void {
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        [PROVIDER_LIST_ROUTE]: enabledProviderList,
        [PROVIDER_GET_ROUTE]: () => ({
          status: 200,
          body: providerRecord({ enabled: true, version: 2 }),
        }),
        [PROVIDER_PATCH_ROUTE]: () => {
          throw new Error("an enabled provider was PATCHed without an operator session");
        },
        [PROVIDER_CREATE_ROUTE]: () => {
          throw new Error("a refused re-save must not fall back to creating a second row");
        },
      }),
      session,
    });
    store.sealedIds.add(PROVIDER_ID);
  }

  /** The same deployment, but with the PATCH allowed to succeed. */
  function installEnabledRowPatchable(session: SessionCheck): void {
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        [PROVIDER_LIST_ROUTE]: enabledProviderList,
        [PROVIDER_GET_ROUTE]: () => ({
          status: 200,
          body: providerRecord({ enabled: true, version: 2 }),
        }),
        [PROVIDER_PATCH_ROUTE]: () => ({
          status: 200,
          body: providerRecord({
            enabled: true,
            version: 3,
            allowed_email_domains: ["example.com", "gmail.com"],
          }),
        }),
      }),
      session,
    });
    store.sealedIds.add(PROVIDER_ID);
  }

  /** Nothing was written, by any route, on any store. */
  function expectNothingWasWritten(): void {
    expect(stub.routes()).not.toContain(PROVIDER_PATCH_ROUTE);
    expect(stub.routes()).not.toContain(PROVIDER_CREATE_ROUTE);
    expect(stub.routes()).not.toContain(PROVIDER_ENABLE_ROUTE);
    expect(stub.routes()).not.toContain(ISSUER_CREATE_ROUTE);
    // Belt: no write verb reached the stub at all, whatever its path.
    expect(stub.routes().filter((route) => !route.startsWith("GET "))).toEqual([]);
    expect(store.puts).toEqual([]);
  }

  test("THE ATTACK: no session at all — refused, and no write left the console", async () => {
    installEnabledRow(NO_SESSION);
    const response = await POST(post(RE_POINT_BODY));

    expect(response.status).toBe(401);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("setup_enabled_provider_requires_session");
    expect(error["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_enabled_provider_requires_session,
    );

    // The refusal is not merely a status: the stub records the whole wire, and
    // the only calls on it are the guard's claim-status read and the derivation
    // that answered "this row is enabled". Nothing was read from the row, and
    // nothing at all was written.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE, PROVIDER_LIST_ROUTE]);
    expectNothingWasWritten();
  });

  test("THE VARIANT: a valid session through a DIFFERENT provider — refused the same way", async () => {
    // Not a stranger: an allow-listed, verified, real session. It simply was not
    // established through the row being rewritten. Without this case the gate
    // could be satisfied by "some session exists", and on an unclaimed
    // deployment any enabled provider can mint one.
    installEnabledRow(SIGNED_IN_ELSEWHERE);
    const response = await POST(post(RE_POINT_BODY));

    expect(response.status).toBe(403);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("setup_enabled_provider_session_mismatch");
    expect(error["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
    );
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE, PROVIDER_LIST_ROUTE]);
    expectNothingWasWritten();
  });

  test("a session refused for a reason signing in cannot fix keeps ITS OWN key", async () => {
    // The two statuses are not decoration: 401 says "there is nobody here",
    // 403 says "you are here and may not". Only the first is fixed by signing
    // in, and the wizard renders the difference.
    //
    // ...and the KEY is not decoration either. This session authenticated
    // through the derived row; what disqualifies it is an address the IdP never
    // verified. Answering that with `setup_enabled_provider_requires_session`
    // ("Sign in through it first, then save your changes") would send them to
    // repeat the one action that cannot change the answer.
    installEnabledRow(UNVERIFIED_HERE);
    const response = await POST(post(RE_POINT_BODY));
    expect(response.status).toBe(403);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("email_not_verified");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.email_not_verified);
    expectNothingWasWritten();
  });

  test("THE REMEDY: the operator the ALLOW-LIST refused re-saves the row that refused them", async () => {
    // The legitimate path this PATCH exists for, and the reason the gate is a
    // session rather than a blanket refusal.
    //
    // WAS A REGRESSION, and this test used to pin it. The operator arrives here
    // by the route the wizard itself walks them down: sign in, claim, Moira
    // answers `403 admin_claim_domain_not_allowed`, take the "Edit auth
    // settings" way back, widen the allow-list, save. At that moment the
    // console's OWN `checkSession` has already refused their session on the
    // same list — `email_domain_not_allowed`, which is what
    // `OPERATOR_OUTSIDE_THE_ALLOW_LIST` is, built by `checkSession` itself
    // rather than asserted. A gate that demanded `SessionCheck.ok` therefore
    // refused the one caller it exists to serve, and the widen-and-retry
    // instruction was unfollowable.
    //
    // The proof that it is the ROW and not the mere refusal that admits them is
    // the next test.
    installEnabledRowPatchable(OPERATOR_OUTSIDE_THE_ALLOW_LIST);
    const response = await POST(
      post({
        ...PROVISION_BODY,
        client_secret: "",
        allowed_email_domains: ["example.com", "newdomain.example"],
      }),
    );

    expect(response.status).toBe(201);
    expect(stub.routes()).toContain(PROVIDER_PATCH_ROUTE);
    const patched = stub.bodyOf(PROVIDER_PATCH_ROUTE) as Record<string, unknown>;
    expect(patched["allowed_email_domains"]).toEqual(["example.com", "newdomain.example"]);
  });

  test("THE LIMIT: the same allow-list refusal from ANOTHER row is still refused", async () => {
    // Without this, "refused on the allow-list" alone would be enough, and on an
    // unclaimed multi-provider deployment ANY enabled provider can mint a
    // session — so a stranger who can authenticate anywhere could re-point the
    // row here. What admits the caller above is that the row which resolved
    // their cookie IS the row being written; this is the same rejection with
    // that one fact removed.
    installEnabledRow(OUTSIDER_ELSEWHERE);
    const response = await POST(post(RE_POINT_BODY));

    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["code"]).toBe("email_domain_not_allowed");
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE, PROVIDER_LIST_ROUTE]);
    expectNothingWasWritten();
  });

  test("THE REMEDY still works for an allow-listed operator too", async () => {
    // The other half of the same remedy: an operator whose address WAS on the
    // list all along (they are fixing a URL, not a domain) holds an ordinary
    // `ok` session, and admitting the refused one must not have cost them
    // anything.
    installEnabledRowPatchable(SIGNED_IN);
    const response = await POST(
      post({
        ...PROVISION_BODY,
        client_secret: "",
        allowed_email_domains: ["example.com", "gmail.com"],
      }),
    );

    expect(response.status).toBe(201);
    expect(stub.routes()).toContain(PROVIDER_PATCH_ROUTE);
    const patched = stub.bodyOf(PROVIDER_PATCH_ROUTE) as Record<string, unknown>;
    expect(patched["allowed_email_domains"]).toEqual(["example.com", "gmail.com"]);
    expect((await json(response))["state"]).toMatchObject({ allowedEmailDomainCount: 2 });
  });

  test("the row handed to the IN-MODULE lock is the CALLER's, not the derived one", async () => {
    // The pre-check and the in-module lock look redundant while the derivation
    // and the fresh read agree. They do not agree in exactly one case — the row
    // was DISABLED when derived and is ENABLED by the time it is read back — and
    // that case is the only place the two possible sources for
    // `sessionProviderId` differ:
    //
    //   operator.sessionProviderId   null (the pre-check never asked: the
    //                                derived row was disabled)
    //   derived.providerId           the row id — which the lock would then
    //                                compare against ITSELF and pass
    //
    // Substituting the second for the first leaves every other test in this
    // file green while reopening the hole the lock exists to close. This is
    // what fails when someone does.
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        // The derivation sees a disabled row, so the pre-check stands down...
        [PROVIDER_LIST_ROUTE]: () => ({
          status: 200,
          body: {
            data: [providerRecord({ enabled: false })],
            pagination: { has_more: false, next_cursor: null },
          },
        }),
        // ...and the fresh read the PATCH is preconditioned on sees it enabled.
        [PROVIDER_GET_ROUTE]: () => ({
          status: 200,
          body: providerRecord({ enabled: true, version: 2 }),
        }),
        [PROVIDER_PATCH_ROUTE]: () => {
          throw new Error("a row that became enabled mid-flight was PATCHed anonymously");
        },
      }),
      session: NO_SESSION,
    });
    store.sealedIds.add(PROVIDER_ID);

    const response = await POST(post(RE_POINT_BODY));
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["code"]).toBe("setup_enabled_provider_requires_session");
    expect(stub.routes()).not.toContain(PROVIDER_PATCH_ROUTE);
    expect(store.puts).toEqual([]);
  });

  test("CONTROL: an UNPROVISIONED deployment still provisions with no session", async () => {
    // The first run, which is the whole reason the setup window exists: there
    // is no provider to sign in through, so demanding a session here would make
    // the deployment unclaimable. Requiring one for every provision is the
    // cheapest way to turn the two attack tests green, and this is what refuses
    // it.
    install({ session: NO_SESSION });
    const response = await POST(post(PROVISION_BODY));

    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual([
      CLAIM_STATUS_ROUTE,
      ISSUER_LIST_ROUTE,
      ISSUER_LIST_ROUTE,
      ISSUER_CREATE_ROUTE,
      PROVIDER_CREATE_ROUTE,
      PROVIDER_ENABLE_ROUTE,
    ]);
    expect(store.puts).toEqual([
      { providerId: PROVIDER_ID, clientId: CLIENT_ID, plaintext: CLIENT_SECRET },
    ]);
  });

  test("CONTROL: a DISABLED row is still re-saved and enabled with no session", async () => {
    // The interrupted first run being finished. A disabled row authenticates
    // nobody, so rewriting it escalates nothing — and the operator cannot have
    // a session through it, because it cannot issue one. Gating this would make
    // a partial setup unresumable.
    install({
      handlers: handlers({
        [ISSUER_LIST_ROUTE]: populatedIssuerList,
        [PROVIDER_LIST_ROUTE]: () => ({
          status: 200,
          body: {
            data: [providerRecord({ enabled: false })],
            pagination: { has_more: false, next_cursor: null },
          },
        }),
        [PROVIDER_GET_ROUTE]: () => ({
          status: 200,
          body: providerRecord({ enabled: false, version: 2 }),
        }),
        [PROVIDER_PATCH_ROUTE]: () => ({
          status: 200,
          body: providerRecord({ enabled: false, version: 3 }),
        }),
      }),
      session: NO_SESSION,
    });

    const response = await POST(post(PROVISION_BODY));
    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual([
      CLAIM_STATUS_ROUTE,
      ISSUER_LIST_ROUTE,
      PROVIDER_LIST_ROUTE,
      ISSUER_LIST_ROUTE,
      PROVIDER_GET_ROUTE,
      PROVIDER_PATCH_ROUTE,
      PROVIDER_ENABLE_ROUTE,
    ]);
    expect((await json(response))["state"]).toMatchObject({ providerEnabled: true });
  });

  test("the session is not resolved at all unless the derived row is enabled", async () => {
    // Resolving a session costs a Moira read of the auth configuration, and on
    // a fresh deployment there is no enabled provider to resolve it against —
    // the answer would be "no session" after paying for it. So the read is
    // lazy, and this pins that: a `readSession` that throws must never be
    // reached on the create path.
    stub = createMoiraStub(handlers());
    store = new RecordingSecretStore();
    const env = envWith();
    setSetupWindowDependenciesForTests({
      env,
      client: new MoiraClient({
        baseUrl: MOIRA_STUB_BASE_URL,
        systemKey: env.moiraSystemKey,
        fetch: stub.fetch,
      }),
      store,
      storageMode: "ephemeral",
      readSession: async () => {
        throw new Error("provisioning resolved a session it did not need");
      },
    });

    expect((await POST(post(PROVISION_BODY))).status).toBe(201);
  });
});

/* -------------------------------------------------------------------------- */
/* Input the console refuses before any request leaves the process            */
/* -------------------------------------------------------------------------- */

describe("provision input is validated before the first Moira write", () => {
  const refusals: ReadonlyArray<{
    readonly what: string;
    readonly patch: Record<string, unknown>;
    readonly key: string;
    /**
     * The reads this refusal is allowed to have made. Almost every one is pure
     * shape and makes none beyond the guard's; the missing-secret refusal is
     * the exception, because whether an empty secret is admissible is a fact
     * about the console's own store and can only be answered by deriving it.
     */
    readonly reads?: readonly string[];
  }> = [
    {
      what: "an empty allow-list, which would deny the operator's own claim",
      patch: { allowed_email_domains: [] },
      key: CONSOLE_MESSAGE_KEYS.setup_allowed_email_domains_required,
    },
    {
      what: "a missing client secret",
      patch: { client_secret: "" },
      key: CONSOLE_MESSAGE_KEYS.setup_client_secret_required,
      reads: [CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE],
    },
    {
      what: "a missing client id",
      patch: { client_id: "  " },
      key: CONSOLE_MESSAGE_KEYS.setup_client_id_required,
    },
    {
      what: "a missing display name",
      patch: { display_name: "" },
      key: CONSOLE_MESSAGE_KEYS.setup_display_name_required,
    },
    {
      what: "a non-interactive method",
      patch: { method: "jwks" },
      key: CONSOLE_MESSAGE_KEYS.setup_method_unsupported,
    },
    {
      what: "neither discovery nor a complete manual endpoint set",
      patch: { discovery_url: "", token_url: "" },
      key: CONSOLE_MESSAGE_KEYS.setup_issuer_or_discovery_required,
    },
    {
      what: "a slug that cannot survive being a URL path segment",
      patch: { slug: "Not A Slug" },
      key: CONSOLE_MESSAGE_KEYS.setup_provider_slug_invalid,
    },
  ];

  for (const refusal of refusals) {
    test(`${refusal.what} is a keyed 400 with nothing written`, async () => {
      install();
      const response = await POST(post({ ...PROVISION_BODY, ...refusal.patch }));
      expect(response.status).toBe(400);
      expect(errorOf(await json(response))["message_key"]).toBe(refusal.key);
      // The claim-status read is the guard's; nothing beyond the declared reads
      // went out, and in particular nothing was WRITTEN.
      expect(stub.routes()).toEqual([...(refusal.reads ?? [CLAIM_STATUS_ROUTE])]);
      expect(store.puts).toEqual([]);
    });
  }

  test("an unreadable body and an unknown action are refused before the guard runs", async () => {
    const malformed = new Request("https://console.example.com/api/setup", {
      method: "POST",
      body: "not json",
    });
    expect((await POST(malformed)).status).toBe(400);
    expect(
      errorOf(await json(await POST(post({ action: "delete-everything" }))))["message_key"],
    ).toBe(CONSOLE_MESSAGE_KEYS.setup_action_unknown);
    expect(stub.routes()).toEqual([]);
  });

  test("a manual endpoint set without discovery is accepted", async () => {
    install();
    const response = await POST(
      post({
        ...PROVISION_BODY,
        method: "generic_oidc",
        discovery_url: "",
        authorization_url: "https://idp.example.com/authorize",
        token_url: "https://idp.example.com/token",
      }),
    );
    expect(response.status).toBe(201);
    const created = stub.bodyOf(PROVIDER_CREATE_ROUTE) as Record<string, unknown>;
    expect(created["discovery_url"]).toBeNull();
    expect(created["token_url"]).toBe("https://idp.example.com/token");
  });
});

/* -------------------------------------------------------------------------- */
/* claim                                                                      */
/* -------------------------------------------------------------------------- */

describe("POST claim derives the provisioning state SERVER-SIDE", () => {
  // The browser sends `{action: "claim"}` and nothing else. Its own memory of
  // the provisioning state did not survive the OAuth round trip — sign-in is a
  // full navigation away from /setup and back — so the gate is computed from
  // Moira's records plus the console's secret store, on every claim.

  test("claims with the CONSOLE's issuer and the session's own subject", async () => {
    installProvisioned();
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(201);

    // The derivation went to the source of truth before the claim went out.
    expect(stub.routes()).toEqual([
      CLAIM_STATUS_ROUTE,
      ISSUER_LIST_ROUTE,
      PROVIDER_LIST_ROUTE,
      CLAIM_ROUTE,
    ]);

    const body = stub.bodyOf(CLAIM_ROUTE) as Record<string, unknown>;
    expect(body).toEqual({
      issuer: CONSOLE_ISSUER,
      subject: "sub-abc",
      email: "ops@example.com",
      email_verified: true,
    });
    // `scopes: []` would create a permanent no-op admin; `setup_token` is a hard
    // 400. Both must be ABSENT rather than empty.
    expect("scopes" in body).toBe(false);
    expect("setup_token" in body).toBe(false);

    const request = stub.requestsFor(CLAIM_ROUTE)[0];
    expect(request?.headers["Idempotency-Key"]).toBeString();
    // System key only: a bearer token is refused on this operation even if it
    // verifies.
    expect(request?.headers["X-Moira-System-Key"]).toBe("sk_test_bootstrap");
    expect(request?.headers["Authorization"]).toBeUndefined();

    expect((await json(response))["identity"]).toMatchObject({ email: "ops@example.com" });
  });

  test("a slug naming a namespace the session did not authenticate against is refused", async () => {
    // ========================================================================
    // THE CLAIM NAMESPACE IS BOUND TO THE PROVIDER THAT AUTHENTICATED (#71)
    // ========================================================================
    //
    // `slug` selects the console-issuer namespace the `admin_identities` grant
    // is written into. It is bounded by `PROVIDER_SLUG_PATTERN` and therefore
    // well-formed — and well-formed was all it had to be: the session below is
    // a REAL, allow-listed, verified session, established through the incumbent
    // provider (`consoleIssuer === CONSOLE_ISSUER`), and it used to be able to
    // claim under `.../idp/other` simply by naming it. That namespace's own
    // allow-list was never applied to this identity, because the allow-list
    // `checkSession` ran is the AUTHENTICATING configuration's.
    //
    // Now the two derived issuers are compared and the request is refused with
    // nothing written — asserted by the absence of `CLAIM_ROUTE` below.
    installProvisioned();
    const response = await POST(post({ action: "claim", slug: "other" }));

    expect(response.status).toBe(403);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("setup_claim_issuer_mismatch");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.setup_claim_issuer_mismatch);
    expect(
      stub.routes(),
      "the claim reached Moira despite naming a namespace this session never authenticated " +
        "against",
    ).not.toContain(CLAIM_ROUTE);
  });

  test("AC4 — an unverified address is refused BEFORE the request leaves", async () => {
    // `SetupOrderingError` from `claimAdminIdentity`, which is the defence in
    // depth in front of Moira's own `admin_claim_email_not_verified`. The
    // session check normally refuses first; this asserts the second gate holds
    // when it does not. The deployment IS provisioned, so the refusal cannot be
    // the unreachable-step one.
    installProvisioned({
      session: {
        ok: true,
        identity: { email: "ops@example.com", emailVerified: false, idpSubject: "sub-abc" },
        consoleIssuer: CONSOLE_ISSUER,
        moiraProviderId: PROVIDER_ID,
      },
    });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_email_not_verified,
    );
    expect(stub.routes()).not.toContain(CLAIM_ROUTE);
  });

  test("a provider Moira reports DISABLED makes the claim step unreachable — whatever the browser believes", async () => {
    installProvisioned({
      handlers: {
        [PROVIDER_LIST_ROUTE]: () => ({
          status: 200,
          body: {
            data: [providerRecord({ enabled: false })],
            pagination: { has_more: false, next_cursor: null },
          },
        }),
      },
    });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_claim_step_unreachable,
    );
    expect(stub.routes()).not.toContain(CLAIM_ROUTE);
  });

  test("a missing console secret makes the claim step unreachable", async () => {
    installProvisioned({ secretSealed: false });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_claim_step_unreachable,
    );
    expect(stub.routes()).not.toContain(CLAIM_ROUTE);
  });

  test("an UNPROVISIONED deployment refuses the claim as unreachable, from its own records", async () => {
    // Nothing registered in Moira at all: the derived state is empty and the
    // claim step is not reachable. Previously this was a 400 about an
    // unreadable body-state; the body no longer carries one to misread.
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.setup_claim_step_unreachable,
    );
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE, ISSUER_LIST_ROUTE]);
  });

  test("no session is a 401 carrying the session check's own key", async () => {
    install({ session: NO_SESSION });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(401);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("no_session");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.session_required);
    // Refused before the derivation spent anything on Moira.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE]);
  });

  test("a session that may not act is a 403, not a 401", async () => {
    // The CLAIM path is unchanged by the provisioning gate's allow-list
    // admission: a claim by an identity outside the allow-list is exactly what
    // Moira would refuse, so the console refuses it here rather than spending a
    // round trip on it. The provisioning path admits the same session, because
    // there the allow-list is the thing being edited — see the enabled-provider
    // describe block below.
    install({ session: OPERATOR_OUTSIDE_THE_ALLOW_LIST });
    expect((await POST(post({ action: "claim" }))).status).toBe(403);
  });

  test("403 admin_claim_domain_not_allowed is re-keyed WITH the offending domain", async () => {
    installProvisioned({
      handlers: {
        [CLAIM_ROUTE]: () => ({
          status: 403,
          body: errorEnvelope("admin_claim_domain_not_allowed"),
        }),
      },
    });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(403);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("admin_claim_domain_not_allowed");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.setup_claim_domain_not_allowed);
    // Named, not generic. Moira's own envelope does not carry the domain, and
    // this is the last screen on which the allow-list can still be changed.
    expect(error["message_args"]).toEqual({ domain: "example.com" });
    expect(
      t(CONSOLE_MESSAGE_KEYS.setup_claim_domain_not_allowed, { domain: "example.com" }),
    ).toContain("example.com");
  });

  test("any other Moira refusal is passed through as the client-safe union", async () => {
    installProvisioned({
      handlers: {
        [CLAIM_ROUTE]: () => ({
          status: 409,
          body: errorEnvelope("admin_identity_already_claimed"),
        }),
      },
    });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(409);
    const error = errorOf(await json(response));
    expect(error["remedy"]).toBe("already_complete");
    expect(JSON.stringify(error)).not.toContain("req_stub_0001");
  });

  test("a fabricated client state cannot open the gate — the body's state is ignored", async () => {
    // The old contract let the browser ECHO a state; a fabricated COMPLETE one
    // must not reach further than an honest empty one now that the server
    // derives its own.
    const fabricated = {
      trustedJwtIssuerId: ISSUER_ID,
      trustedJwtIssuerVersion: 1,
      providerId: PROVIDER_ID,
      providerVersion: 2,
      providerTrustedJwtIssuerId: ISSUER_ID,
      providerEnabled: true,
      allowedEmailDomainCount: 1,
      consoleSecretStored: true,
    };
    const response = await POST(post({ action: "claim", state: fabricated }));
    expect(response.status).toBe(409);
    expect(stub.routes()).not.toContain(CLAIM_ROUTE);
  });
});

/* -------------------------------------------------------------------------- */
/* i18n coverage for the keys this route owns                                 */
/* -------------------------------------------------------------------------- */

describe("every key this route emits is real English", () => {
  const OWNED = [
    CONSOLE_MESSAGE_KEYS.setup_system_key_absent,
    CONSOLE_MESSAGE_KEYS.setup_already_claimed,
    CONSOLE_MESSAGE_KEYS.setup_request_body_invalid,
    CONSOLE_MESSAGE_KEYS.setup_action_unknown,
    CONSOLE_MESSAGE_KEYS.setup_method_unsupported,
    CONSOLE_MESSAGE_KEYS.setup_display_name_required,
    CONSOLE_MESSAGE_KEYS.setup_client_id_required,
    CONSOLE_MESSAGE_KEYS.setup_client_secret_required,
    CONSOLE_MESSAGE_KEYS.setup_issuer_or_discovery_required,
    CONSOLE_MESSAGE_KEYS.setup_allowed_email_domains_required,
    CONSOLE_MESSAGE_KEYS.setup_provider_slug_invalid,
    CONSOLE_MESSAGE_KEYS.setup_resume_state_invalid,
    CONSOLE_MESSAGE_KEYS.setup_resume_state_conflict,
    CONSOLE_MESSAGE_KEYS.setup_ordering_violated,
    CONSOLE_MESSAGE_KEYS.setup_claim_step_unreachable,
    CONSOLE_MESSAGE_KEYS.setup_email_not_verified,
    CONSOLE_MESSAGE_KEYS.setup_claim_domain_not_allowed,
    CONSOLE_MESSAGE_KEYS.setup_enabled_provider_requires_session,
    CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
  ] as const;

  test("each resolves to English rather than to its own name", () => {
    for (const key of OWNED) {
      expect(t(key), `${key} renders as a bare key`).not.toBe(key);
      expect(CONSOLE_CATALOG[key].description.trim()).not.toBe("");
    }
  });

  test("no message names an environment variable or a URL", () => {
    // The same negative space `i18n-catalog-coverage.test.ts` asserts globally,
    // restated for this group because the obvious copy for a missing system key
    // is to name the variable.
    for (const key of OWNED) {
      const message = CONSOLE_CATALOG[key].message;
      expect(/MOIRA_[A-Z_]+|CONSOLE_[A-Z_]+/.test(message), key).toBe(false);
      expect(/https?:\/\//.test(message), key).toBe(false);
    }
  });
});
