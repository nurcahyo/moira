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
import type { SessionCheck } from "@/lib/moira-session";
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

const SIGNED_IN: SessionCheck = {
  ok: true,
  identity: { email: "ops@example.com", emailVerified: true, idpSubject: "sub-abc" },
  // The console issuer of the configuration that AUTHENTICATED this session.
  // Required from issue #71: `claim` compares the namespace its `slug` resolves
  // to against this, so a session fixture without it would be a session that
  // could claim in any namespace.
  consoleIssuer: CONSOLE_ISSUER,
};

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
  /** A deployment whose console issuer already owns an enabled bound provider. */
  function installReSavable(): void {
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
    install({
      session: { ok: false, rejection: "no_session", messageKey: "console.error.session_required" },
    });
    const response = await POST(post({ action: "claim" }));
    expect(response.status).toBe(401);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("no_session");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.session_required);
    // Refused before the derivation spent anything on Moira.
    expect(stub.routes()).toEqual([CLAIM_STATUS_ROUTE]);
  });

  test("a session that may not act is a 403, not a 401", async () => {
    install({
      session: {
        ok: false,
        rejection: "email_domain_not_allowed",
        messageKey: CONSOLE_MESSAGE_KEYS.email_domain_not_allowed,
      },
    });
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
