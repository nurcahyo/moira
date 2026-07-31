// THE B1 REGRESSION SUITE.
//
// Plan 08's wizard, as originally written, could never produce a successful
// claim: it wrote the auth-provider row for the IdP, claimed with the console's
// issuer, and never set `trusted_jwt_issuer_id`, so the admission-policy lookup
// matched neither stage and every claim was 403 admin_claim_domain_not_allowed.
//
// Two things had to change, and BOTH are asserted here, because fixing only the
// first fixes nothing:
//
//   1. `POST /api/v1/admin/jwt-issuers` runs FIRST, before the provider create.
//   2. The provider create body CARRIES that issuer's id as
//      `trusted_jwt_issuer_id`.
//
// `flow_writes_happen_in_exactly_this_order` and
// `provider_create_body_carries_the_trusted_jwt_issuer_id_from_step_one` are the
// two that fail if someone reorders the steps back.

import { beforeEach, describe, expect, test } from "bun:test";

import { MoiraClient } from "@/lib/moira-client";
import {
  EMPTY_PROVISIONING_STATE,
  SetupOrderingError,
  SetupProvisioningError,
  assertB1Invariant,
  buildProviderCreateBody,
  claimAdminIdentity,
  claimIdempotencyKey,
  consoleIssuerConfigFor,
  isProvisioningComplete,
  provisioningDeficiencies,
  reachableSetupStep,
  runSetupProvisioning,
  type AuthProviderConfig,
  type ConsoleIssuerConfig,
  type SetupProvisioningRequest,
  type SetupProvisioningState,
  type SetupWizardState,
} from "@/lib/setup-flow";
import {
  MOIRA_STUB_BASE_URL,
  createMoiraStub,
  errorEnvelope,
  type StubHandler,
} from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const CONSOLE_ISSUER = "https://console.example.com";
const IDP_ISSUER = "https://accounts.google.com";
const ISSUER_ID = "11111111-1111-4111-8111-111111111111";
const PROVIDER_ID = "22222222-2222-4222-8222-222222222222";

const consoleConfig: ConsoleIssuerConfig = {
  issuer: CONSOLE_ISSUER,
  jwksUrl: "https://console.example.com/api/auth/jwks",
  audience: "https://moira.test/api/v1/admin",
};

const providerConfig: AuthProviderConfig = {
  method: "google_oauth",
  displayName: "Google Workspace",
  issuer: IDP_ISSUER,
  discoveryUrl: "https://accounts.google.com/.well-known/openid-configuration",
  clientId: "client-123.apps.googleusercontent.com",
  allowedEmailDomains: ["example.com"],
};

function issuerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: ISSUER_ID,
    issuer: CONSOLE_ISSUER,
    jwks_url: consoleConfig.jwksUrl,
    expected_audiences: [consoleConfig.audience],
    allowed_algorithms: ["ES256"],
    subject_claim: "sub",
    clock_skew_seconds: 60,
    allow_delegation: false,
    status: "active",
    created_at: "2026-07-29T00:00:00Z",
    updated_at: "2026-07-29T00:00:00Z",
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
    created_at: "2026-07-29T00:00:00Z",
    updated_at: "2026-07-29T00:00:00Z",
    version: 1,
    issuer: IDP_ISSUER,
    client_id: providerConfig.clientId,
    trusted_jwt_issuer_id: ISSUER_ID,
    ...overrides,
  };
}

const emptyIssuerList: StubHandler = () => ({
  status: 200,
  body: { data: [], pagination: { has_more: false, next_cursor: null } },
});

function happyPathHandlers(overrides: Record<string, StubHandler> = {}) {
  return {
    "GET /api/v1/admin/jwt-issuers": emptyIssuerList,
    "POST /api/v1/admin/jwt-issuers": () => ({ status: 201, body: issuerRecord() }),
    "POST /api/v1/admin/auth/providers": () => ({ status: 201, body: providerRecord() }),
    "POST /api/v1/admin/auth/providers/22222222-2222-4222-8222-222222222222/enable": () => ({
      status: 200,
      body: providerRecord({ enabled: true, version: 2 }),
    }),
    "POST /api/v1/admin/setup/claim": () => ({
      status: 201,
      body: {
        id: "33333333-3333-4333-8333-333333333333",
        issuer: CONSOLE_ISSUER,
        subject: "sub-abc",
        email: "ops@example.com",
        email_verified: true,
        granted_scopes: ["moira:admin"],
        status: "active",
        created_at: "2026-07-29T00:00:00Z",
        version: 1,
        notice: {
          message_key: "moira.notice.admin_identity_claimed",
          message: "Admin identity claimed.",
        },
      },
    }),
    ...overrides,
  };
}

let secretWrites: Array<{ providerId: string; clientId: string | null }> = [];

function provisioningRequest(
  overrides: Partial<SetupProvisioningRequest> = {},
): SetupProvisioningRequest {
  return {
    console: consoleConfig,
    provider: providerConfig,
    storeClientSecret: async (providerId, clientId) => {
      secretWrites.push({ providerId, clientId });
    },
    idempotencyKeys: {
      trustedJwtIssuer: "idem-issuer-0001",
      authProvider: "idem-provider-0001",
    },
    ...overrides,
  };
}

function clientFor(stub: { fetch: typeof fetch }): MoiraClient {
  return new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "sk_test_bootstrap",
    fetch: stub.fetch,
  });
}

beforeEach(() => {
  secretWrites = [];
});

/* -------------------------------------------------------------------------- */
/* B1 — ordering                                                              */
/* -------------------------------------------------------------------------- */

describe("B1: the jwt issuer is registered first and the provider is bound to it", () => {
  test("flow_writes_happen_in_exactly_this_order", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    const result = await runSetupProvisioning(clientFor(stub), provisioningRequest());

    // The exact wire order. If a future edit moves POST /jwt-issuers below
    // POST /auth/providers, this assertion is what fails.
    expect(stub.routes()).toEqual([
      "GET /api/v1/admin/jwt-issuers",
      "POST /api/v1/admin/jwt-issuers",
      "POST /api/v1/admin/auth/providers",
      "POST /api/v1/admin/auth/providers/22222222-2222-4222-8222-222222222222/enable",
    ]);

    expect(result.trace.map((entry) => entry.step)).toEqual([
      "ensure_trusted_jwt_issuer",
      "create_auth_provider",
      "store_console_secret",
      "enable_auth_provider",
    ]);
  });

  test("jwt_issuer_create_strictly_precedes_provider_create", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await runSetupProvisioning(clientFor(stub), provisioningRequest());

    const routes = stub.routes();
    const issuerIndex = routes.indexOf("POST /api/v1/admin/jwt-issuers");
    const providerIndex = routes.indexOf("POST /api/v1/admin/auth/providers");
    expect(issuerIndex).toBeGreaterThanOrEqual(0);
    expect(providerIndex).toBeGreaterThanOrEqual(0);
    expect(issuerIndex).toBeLessThan(providerIndex);
  });

  test("provider_create_body_carries_the_trusted_jwt_issuer_id_from_step_one", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await runSetupProvisioning(clientFor(stub), provisioningRequest());

    const body = stub.bodyOf("POST /api/v1/admin/auth/providers") as Record<string, unknown>;
    // The id must be the one the ISSUER step returned, not a guess or a copy of
    // the provider id. Reordering without this is the defect B1 describes.
    expect(body["trusted_jwt_issuer_id"]).toBe(ISSUER_ID);
  });

  test("provider_create_body_keeps_issuer_as_the_idp_not_the_console", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await runSetupProvisioning(clientFor(stub), provisioningRequest());

    const body = stub.bodyOf("POST /api/v1/admin/auth/providers") as Record<string, unknown>;
    expect(body["issuer"]).toBe(IDP_ISSUER);
    expect(body["issuer"]).not.toBe(CONSOLE_ISSUER);
  });

  test("reordering_alone_is_refused_in_process", () => {
    // The literal shape of the regression: a caller that built the provider body
    // before registering the issuer. It never reaches the network.
    expect(() => buildProviderCreateBody(providerConfig, null)).toThrow(SetupOrderingError);
    expect(() => assertB1Invariant(null)).toThrow(SetupOrderingError);
    expect(() => assertB1Invariant("")).toThrow(SetupOrderingError);
  });

  test("a_provider_row_returned_without_the_binding_is_never_enabled", async () => {
    const stub = createMoiraStub(
      happyPathHandlers({
        "POST /api/v1/admin/auth/providers": () => ({
          status: 201,
          body: providerRecord({ trusted_jwt_issuer_id: null }),
        }),
      }),
    );

    const error = (await runSetupProvisioning(clientFor(stub), provisioningRequest()).catch(
      (caught: unknown) => caught,
    )) as SetupProvisioningError;

    expect(error).toBeInstanceOf(SetupProvisioningError);
    expect(error.step).toBe("create_auth_provider");
    // Crucially: no enable call. An enabled row without the binding is the
    // permanent-403 state the whole ordering exists to prevent.
    expect(stub.routes()).not.toContain(
      "POST /api/v1/admin/auth/providers/22222222-2222-4222-8222-222222222222/enable",
    );
  });
});

/* -------------------------------------------------------------------------- */
/* B4 — the console never sends `enabled`                                     */
/* -------------------------------------------------------------------------- */

describe("enable is the commit point, enforced client-side", () => {
  test("provider_create_body_never_contains_enabled", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await runSetupProvisioning(clientFor(stub), provisioningRequest());

    const body = stub.bodyOf("POST /api/v1/admin/auth/providers") as Record<string, unknown>;
    // Absent, not `false`. `enabled` is a plain writable boolean in Moira, so
    // sending `true` would land an enabled provider with no console secret.
    expect(Object.keys(body)).not.toContain("enabled");
    expect(JSON.stringify(body)).not.toContain('"enabled"');
  });

  test("enable_carries_if_match_and_no_idempotency_key", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await runSetupProvisioning(clientFor(stub), provisioningRequest());

    const enable = stub.requestsFor(
      "POST /api/v1/admin/auth/providers/22222222-2222-4222-8222-222222222222/enable",
    )[0];
    expect(enable).toBeDefined();
    expect(enable?.headers["If-Match"]).toBe("1");
    // The spec declares no Idempotency-Key on enable; retry safety is If-Match
    // plus natural idempotence.
    expect(Object.keys(enable?.headers ?? {})).not.toContain("Idempotency-Key");
  });

  test("the_console_secret_is_stored_before_the_row_is_enabled", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    const result = await runSetupProvisioning(clientFor(stub), provisioningRequest());

    const steps = result.trace.map((entry) => entry.step);
    expect(steps.indexOf("store_console_secret")).toBeLessThan(
      steps.indexOf("enable_auth_provider"),
    );
    expect(secretWrites).toEqual([
      { providerId: PROVIDER_ID, clientId: providerConfig.clientId ?? null },
    ]);
  });
});

/* -------------------------------------------------------------------------- */
/* Partial states                                                             */
/* -------------------------------------------------------------------------- */

describe("partial states are resumable, never restarted", () => {
  test("orphan_trusted_issuer_is_reused_not_recreated_on_retry", async () => {
    // First attempt: the issuer lands, the provider create fails. That leaves an
    // orphan trusted issuer — inert, but a naive retry that re-POSTs it hits
    // `trusted_jwt_issuers_issuer_active_unique`, which Moira does NOT map to a
    // 409: the operator would see an opaque 500 database_error.
    const firstStub = createMoiraStub(
      happyPathHandlers({
        "POST /api/v1/admin/auth/providers": () => ({
          status: 400,
          body: errorEnvelope("auth_provider_url_not_allowed"),
        }),
      }),
    );

    const failure = (await runSetupProvisioning(clientFor(firstStub), provisioningRequest()).catch(
      (caught: unknown) => caught,
    )) as SetupProvisioningError;

    expect(failure.step).toBe("create_auth_provider");
    expect(failure.remedy).toBe("retry_reuses_trusted_jwt_issuer");
    expect(failure.state.trustedJwtIssuerId).toBe(ISSUER_ID);

    // Retry: the issuer already exists, so the list finds it and the flow adopts
    // it. No second POST.
    const retryStub = createMoiraStub(
      happyPathHandlers({
        "GET /api/v1/admin/jwt-issuers": () => ({
          status: 200,
          body: { data: [issuerRecord()], pagination: { has_more: false, next_cursor: null } },
        }),
        "POST /api/v1/admin/jwt-issuers": () => {
          throw new Error("retry must not re-register the trusted JWT issuer");
        },
      }),
    );

    const result = await runSetupProvisioning(
      clientFor(retryStub),
      provisioningRequest({ resume: failure.state }),
    );

    expect(retryStub.requestsFor("POST /api/v1/admin/jwt-issuers")).toHaveLength(0);
    expect(result.trace[0]).toEqual({
      step: "ensure_trusted_jwt_issuer",
      operation: "listTrustedJwtIssuers",
      outcome: "reused",
    });
    expect(isProvisioningComplete(result.state)).toBe(true);
  });

  test("an_existing_but_disabled_issuer_is_enabled_rather_than_adopted_broken", async () => {
    const stub = createMoiraStub(
      happyPathHandlers({
        "GET /api/v1/admin/jwt-issuers": () => ({
          status: 200,
          body: {
            data: [issuerRecord({ status: "disabled" })],
            pagination: { has_more: false, next_cursor: null },
          },
        }),
        "POST /api/v1/admin/jwt-issuers/11111111-1111-4111-8111-111111111111/enable": () => ({
          status: 200,
          body: issuerRecord({ version: 2 }),
        }),
      }),
    );

    const result = await runSetupProvisioning(clientFor(stub), provisioningRequest());
    // A disabled issuer fails resolve_active_issuer with 400
    // unregistered_trusted_issuer at claim time — the failure would otherwise be
    // deferred to the very last step.
    expect(stub.routes()).toContain(
      "POST /api/v1/admin/jwt-issuers/11111111-1111-4111-8111-111111111111/enable",
    );
    expect(result.trace[0]?.outcome).toBe("enabled");
  });

  test("an_existing_issuer_that_asserts_scopes_is_refused_before_the_provider_write", async () => {
    const stub = createMoiraStub(
      happyPathHandlers({
        "GET /api/v1/admin/jwt-issuers": () => ({
          status: 200,
          body: {
            data: [issuerRecord({ scopes_claim: "scp" })],
            pagination: { has_more: false, next_cursor: null },
          },
        }),
      }),
    );

    const error = (await runSetupProvisioning(clientFor(stub), provisioningRequest()).catch(
      (caught: unknown) => caught,
    )) as SetupProvisioningError;

    expect(error.step).toBe("ensure_trusted_jwt_issuer");
    expect(stub.requestsFor("POST /api/v1/admin/auth/providers")).toHaveLength(0);
  });

  test("a_failed_console_secret_write_leaves_the_provider_disabled", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    const error = (await runSetupProvisioning(
      clientFor(stub),
      provisioningRequest({
        storeClientSecret: async () => {
          throw new Error("console database unavailable");
        },
      }),
    ).catch((caught: unknown) => caught)) as SetupProvisioningError;

    expect(error.step).toBe("store_console_secret");
    expect(error.remedy).toBe("retry_or_discard_provider");
    expect(error.messageKey).toBe("console.error.auth_provider_secret_write_failed");
    expect(stub.routes()).not.toContain(
      "POST /api/v1/admin/auth/providers/22222222-2222-4222-8222-222222222222/enable",
    );
    expect(error.state.providerEnabled).toBe(false);
  });

  test("a_failed_enable_asks_for_a_retry_without_re_entering_the_secret", async () => {
    const stub = createMoiraStub(
      happyPathHandlers({
        "POST /api/v1/admin/auth/providers/22222222-2222-4222-8222-222222222222/enable": () => ({
          status: 400,
          body: errorEnvelope("auth_provider_method_config_incomplete"),
        }),
      }),
    );

    const error = (await runSetupProvisioning(clientFor(stub), provisioningRequest()).catch(
      (caught: unknown) => caught,
    )) as SetupProvisioningError;

    expect(error.step).toBe("enable_auth_provider");
    expect(error.remedy).toBe("retry_enable_no_secret_re_entry");
    expect(error.state.consoleSecretStored).toBe(true);
    expect(error.moiraError?.kind).toBe("api");
  });
});

/* -------------------------------------------------------------------------- */
/* Gates                                                                      */
/* -------------------------------------------------------------------------- */

describe("the claim step is navigation state, not advice", () => {
  const committed: SetupProvisioningState = {
    trustedJwtIssuerId: ISSUER_ID,
    trustedJwtIssuerVersion: 1,
    providerId: PROVIDER_ID,
    providerVersion: 2,
    providerTrustedJwtIssuerId: ISSUER_ID,
    providerEnabled: true,
    allowedEmailDomains: ["example.com"],
    consoleSecretStored: true,
  };

  function wizard(overrides: Partial<SetupWizardState> = {}): SetupWizardState {
    return {
      claimed: false,
      provisioning: committed,
      signedInWithAllowedIdentity: true,
      claimSucceeded: false,
      ...overrides,
    };
  }

  test("a_fresh_deployment_starts_at_the_auth_settings_step", () => {
    expect(reachableSetupStep(wizard({ provisioning: EMPTY_PROVISIONING_STATE }))).toBe(
      "auth_settings",
    );
  });

  test("a_provider_without_the_binding_never_opens_the_claim_step", () => {
    const unbound = { ...committed, providerTrustedJwtIssuerId: null };
    expect(provisioningDeficiencies(unbound)).toEqual(["provider_not_bound_to_trusted_jwt_issuer"]);
    expect(reachableSetupStep(wizard({ provisioning: unbound }))).toBe("auth_settings");
  });

  test("a_disabled_provider_never_opens_the_claim_step", () => {
    const disabled = { ...committed, providerEnabled: false };
    expect(provisioningDeficiencies(disabled)).toEqual(["provider_not_enabled"]);
    expect(reachableSetupStep(wizard({ provisioning: disabled }))).toBe("auth_settings");
  });

  test("an_empty_allow_list_never_opens_the_claim_step", () => {
    const denyAll = { ...committed, allowedEmailDomains: [] };
    expect(provisioningDeficiencies(denyAll)).toEqual(["allowed_email_domains_empty"]);
    expect(reachableSetupStep(wizard({ provisioning: denyAll }))).toBe("auth_settings");
  });

  test("a_missing_console_secret_never_opens_the_claim_step", () => {
    const noSecret = { ...committed, consoleSecretStored: false };
    expect(provisioningDeficiencies(noSecret)).toEqual(["console_secret_not_stored"]);
  });

  test("sign_in_precedes_the_claim", () => {
    expect(reachableSetupStep(wizard({ signedInWithAllowedIdentity: false }))).toBe("sign_in");
    expect(reachableSetupStep(wizard())).toBe("claim");
  });

  test("an_already_claimed_deployment_goes_straight_to_done", () => {
    expect(reachableSetupStep(wizard({ claimed: true }))).toBe("done");
  });

  test("claimAdminIdentity_refuses_to_run_when_the_provider_is_not_committed", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await expect(
      claimAdminIdentity(clientFor(stub), wizard({ provisioning: EMPTY_PROVISIONING_STATE }), {
        consoleIssuer: CONSOLE_ISSUER,
        subject: "sub-abc",
        email: "ops@example.com",
        emailVerified: true,
      }),
    ).rejects.toThrow(SetupOrderingError);
    // Nothing was sent. A deep-link into the claim cannot produce a request that
    // is guaranteed to 403.
    expect(stub.requests).toHaveLength(0);
  });
});

/* -------------------------------------------------------------------------- */
/* The claim body                                                             */
/* -------------------------------------------------------------------------- */

describe("the claim body", () => {
  const committedWizard: SetupWizardState = {
    claimed: false,
    provisioning: {
      trustedJwtIssuerId: ISSUER_ID,
      trustedJwtIssuerVersion: 1,
      providerId: PROVIDER_ID,
      providerVersion: 2,
      providerTrustedJwtIssuerId: ISSUER_ID,
      providerEnabled: true,
      allowedEmailDomains: ["example.com"],
      consoleSecretStored: true,
    },
    signedInWithAllowedIdentity: true,
    claimSucceeded: false,
  };

  test("names_the_console_issuer_and_omits_scopes_and_setup_token", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await claimAdminIdentity(clientFor(stub), committedWizard, {
      consoleIssuer: CONSOLE_ISSUER,
      subject: "sub-abc",
      email: "ops@example.com",
      emailVerified: true,
    });

    const body = stub.bodyOf("POST /api/v1/admin/setup/claim") as Record<string, unknown>;
    expect(body).toEqual({
      issuer: CONSOLE_ISSUER,
      subject: "sub-abc",
      email: "ops@example.com",
      email_verified: true,
    });
    // `scopes: []` would create a grant with zero scopes; `setup_token` is a
    // hard 400. Both must be absent, not empty and not null.
    expect(Object.keys(body)).not.toContain("scopes");
    expect(Object.keys(body)).not.toContain("setup_token");
  });

  test("uses_the_system_key_and_never_a_bearer_token", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await claimAdminIdentity(clientFor(stub), committedWizard, {
      consoleIssuer: CONSOLE_ISSUER,
      subject: "sub-abc",
      email: "ops@example.com",
      emailVerified: true,
    });

    const claim = stub.requestsFor("POST /api/v1/admin/setup/claim")[0];
    expect(claim?.headers["X-Moira-System-Key"]).toBe("sk_test_bootstrap");
    expect(Object.keys(claim?.headers ?? {})).not.toContain("Authorization");
  });

  test("the_idempotency_key_is_deterministic_in_issuer_and_subject", async () => {
    const a = await claimIdempotencyKey(CONSOLE_ISSUER, "sub-abc");
    const b = await claimIdempotencyKey(CONSOLE_ISSUER, "sub-abc");
    const c = await claimIdempotencyKey(CONSOLE_ISSUER, "sub-xyz");
    expect(a).toBe(b);
    expect(a).not.toBe(c);
    expect(a).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });

  test("an_unverified_email_is_refused_before_the_request", async () => {
    const stub = createMoiraStub(happyPathHandlers());
    await expect(
      claimAdminIdentity(clientFor(stub), committedWizard, {
        consoleIssuer: CONSOLE_ISSUER,
        subject: "sub-abc",
        email: "ops@example.com",
        emailVerified: false,
      }),
    ).rejects.toThrow(SetupOrderingError);
    expect(stub.requests).toHaveLength(0);
  });
});

/* -------------------------------------------------------------------------- */
/* Provider body shape                                                        */
/* -------------------------------------------------------------------------- */

describe("buildProviderCreateBody", () => {
  test("refuses an empty allow-list", () => {
    expect(() =>
      buildProviderCreateBody({ ...providerConfig, allowedEmailDomains: [] }, ISSUER_ID),
    ).toThrow(SetupOrderingError);
  });

  test("always includes display_name", () => {
    const body = buildProviderCreateBody(providerConfig, ISSUER_ID);
    expect(body.display_name).toBe("Google Workspace");
  });

  test("declares no key the committed schema does not have", () => {
    const body = buildProviderCreateBody(providerConfig, ISSUER_ID) as Record<string, unknown>;
    const allowed = new Set([
      "method",
      "display_name",
      "allowed_algorithms",
      "allowed_email_domains",
      "authorization_url",
      "client_id",
      "discovery_url",
      "enabled",
      "expected_audiences",
      "issuer",
      "jwks_url",
      "metadata",
      "redirect_uris",
      "requested_scopes",
      "token_url",
      "trusted_jwt_issuer_id",
      "userinfo_url",
    ]);
    // `additionalProperties: false` — an extra field is a hard 400, not a drop.
    for (const key of Object.keys(body)) expect(allowed.has(key)).toBe(true);
  });
});

/* -------------------------------------------------------------------------- */
/* Wave 4B — which console issuer a provider is provisioned under             */
/* -------------------------------------------------------------------------- */

describe("consoleIssuerConfigFor", () => {
  const env = { bffIssuerUrl: "https://console.example.com", adminApiAudience: "moira-admin-api" };
  const JWKS = "https://console.example.com/api/auth/.well-known/jwks.json";

  test("the INCUMBENT is provisioned under the deployment's own issuer, unchanged", () => {
    // `slug === null` is the incumbent. Its issuer string must stay exactly
    // `env.bffIssuerUrl`, because every `admin_identities` grant made before
    // wave 4B carries that string and the key is `(issuer, subject)`.
    const config = consoleIssuerConfigFor(env, JWKS, null);
    expect(config.issuer).toBe("https://console.example.com");
    expect(config.audience).toBe("moira-admin-api");
    expect(config.jwksUrl).toBe(JWKS);
  });

  test("an ADDITIONAL provider gets its own issuer under the console's namespace", () => {
    expect(consoleIssuerConfigFor(env, JWKS, "contractors").issuer).toBe(
      "https://console.example.com/idp/contractors",
    );
    // Same JWKS URL, deliberately: `trusted_jwt_issuers` has a unique index on
    // `issuer` alone and none on `jwks_url`, and better-auth's `getLatestKey`
    // sorts the `jwks` table globally with no issuer awareness — so per-issuer
    // key pairs are not implementable and are not attempted.
    expect(consoleIssuerConfigFor(env, JWKS, "contractors").jwksUrl).toBe(JWKS);
  });

  test("two providers never share an issuer string", () => {
    const a = consoleIssuerConfigFor(env, JWKS, null).issuer;
    const b = consoleIssuerConfigFor(env, JWKS, "github").issuer;
    const c = consoleIssuerConfigFor(env, JWKS, "contractors").issuer;
    expect(new Set([a, b, c]).size).toBe(3);
  });

  test("an unusable slug is refused BEFORE any Moira write", () => {
    // One-way failure: a trusted issuer registered under a string the console
    // cannot parse back into a `providerId` cannot be deleted once a grant
    // exists under it (409 trusted_issuer_has_active_grants), so the provider
    // would be permanently unofferable and its admins permanently unreachable.
    for (const bad of ["", "Bad Slug", "a/b", "-lead", "UPPER"]) {
      expect(() => consoleIssuerConfigFor(env, JWKS, bad)).toThrow();
    }
  });

  test("the algorithm list defaults to the pin and is overridable", () => {
    expect(consoleIssuerConfigFor(env, JWKS, null).allowedAlgorithms).toBeUndefined();
    expect(consoleIssuerConfigFor(env, JWKS, null, ["ES256"]).allowedAlgorithms).toEqual(["ES256"]);
  });
});
