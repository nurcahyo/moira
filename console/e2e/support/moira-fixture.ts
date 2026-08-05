// A STATEFUL Moira for the authenticated e2e stack, on a real socket.
//
// ============================================================================
// WHY STATEFUL, WHEN `tests/support/moira-stub.ts` IS NOT
// ============================================================================
//
// That stub answers from a handler table and is exactly right for a test that
// drives one route handler and asserts what went on the wire. This one has to
// survive a BROWSER doing the whole thing: the panel finishes the chain, calls
// `router.refresh()`, and the page re-reads the same lists a moment later. A
// table that answered "no providers" both times would render a screen that
// contradicts the trace it just printed, and the spec would be asserting against
// a fixture bug rather than against the console.
//
// So writes land in memory and reads see them. The row shapes are lifted from
// `tests/integration/llm-connect-flow.test.ts`, which is where they were first
// proved to satisfy `lib/moira-client.ts`'s response guards.
//
// ============================================================================
// IT RECORDS, AND THAT IS THE POINT
// ============================================================================
//
// The ORDER of the calls is the property the connect chain is held to — provider,
// then model, then credential, then a policy on the existing route. A test that
// counted four rows at the end would pass on an implementation that created them
// in an order Moira rejects. So every request is appended to a list a spec can
// read back through the control server, with the method, the path, whether it
// carried a credential at all, and its `Idempotency-Key`.
//
// The ids are FIXED (`MOIRA_IDS`), so an assertion can name the exact path a
// nested create used instead of matching a pattern.

import {
  AUTH_PROVIDER_DISPLAY_NAME,
  DISCOVERABLE_MODEL_KEYS,
  MOIRA_IDS,
  type FixtureReport,
  type RecordedCredential,
  type RecordedMoiraRequest,
} from "./authenticated-fixture";

const NOW = "2026-08-04T00:00:00Z";

/**
 * WHICH credential the caller presented — not merely whether it presented one.
 *
 * `X-Moira-System-Key` is checked FIRST because that is the order `MoiraClient`
 * itself resolves them in: its `admin` arm prefers the system key whenever one is
 * configured and only falls back to the bearer token. A request carrying both is
 * therefore a system-key request as far as Moira is concerned, and reporting it
 * as `operator` would describe the wrong caller.
 *
 * See `RecordedCredential` in `authenticated-fixture.ts` for why the boolean this
 * replaced could not fail.
 */
function credentialOf(headers: Headers): RecordedCredential {
  if (headers.get("x-moira-system-key") !== null) return "system_key";
  if (headers.get("authorization") !== null) return "operator";
  return "none";
}

interface Row {
  readonly id: string;
  status: string;
  [field: string]: unknown;
}

function stamped(id: string, extra: Record<string, unknown>): Row {
  return {
    id,
    status: "active",
    metadata: {},
    created_at: NOW,
    updated_at: NOW,
    version: 1,
    ...extra,
  } as Row;
}

function page(rows: readonly unknown[]) {
  return { data: rows, pagination: { has_more: false, next_cursor: null } };
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** A Moira `ErrorResponse` envelope, for the paths this fixture refuses. */
function errorEnvelope(code: string, message: string) {
  return {
    error: {
      code,
      message_key: `moira.error.${code}`,
      message,
      message_args: {},
      request_id: "req_authenticated_e2e",
      details: {},
    },
  };
}

export interface MoiraFixtureOptions {
  /** The IdP the fixture provider row points a browser at. */
  readonly idp: {
    readonly issuer: string;
    readonly discoveryUrl: string;
    readonly authorizationUrl: string;
    readonly tokenUrl: string;
    readonly userInfoUrl: string;
  };
  readonly clientId: string;
  readonly allowedEmailDomain: string;
  readonly adminApiAudience: string;
  /** `env.bffIssuerUrl` — the console's own issuer, and the derivation root. */
  readonly consoleIssuer: string;
  readonly consoleJwksUrl: string;
}

export interface MoiraFixture {
  handle(request: Request): Promise<Response>;
  /** Every request since the last `forgetRecording()`, in order. */
  recording(): readonly RecordedMoiraRequest[];
  forgetRecording(): void;
  /** Back to a freshly-migrated deployment: the seeded route, and nothing else. */
  reset(): void;
  rows(): FixtureReport["rows"];
}

export function createMoiraFixture(options: MoiraFixtureOptions): MoiraFixture {
  let recorded: RecordedMoiraRequest[] = [];

  let providers: Row[] = [];
  let providerModels: Row[] = [];
  let providerCredentials: Row[] = [];
  let routingPolicies: Row[] = [];

  /**
   * The one row migration `0005` seeds. Present from the start and never
   * created here, because the console deliberately refuses to create it — see
   * `runConnectChain` step 5.
   */
  const routes: Row[] = [
    stamped(MOIRA_IDS.generalRoute, {
      route_key: "general",
      display_name: "General",
      selection_strategy: "default",
    }),
  ];

  const authProviders: Row[] = [
    stamped(MOIRA_IDS.authProvider, {
      method: "generic_oidc",
      display_name: AUTH_PROVIDER_DISPLAY_NAME,
      enabled: true,
      requested_scopes: ["openid", "email", "profile"],
      allowed_email_domains: [options.allowedEmailDomain],
      allowed_algorithms: ["ES256"],
      expected_audiences: [options.adminApiAudience],
      redirect_uris: [],
      client_id: options.clientId,
      issuer: options.idp.issuer,
      discovery_url: options.idp.discoveryUrl,
      authorization_url: options.idp.authorizationUrl,
      token_url: options.idp.tokenUrl,
      userinfo_url: options.idp.userInfoUrl,
      trusted_jwt_issuer_id: MOIRA_IDS.trustedJwtIssuer,
      jwks_url: null,
    }),
  ];

  const trustedJwtIssuers: Row[] = [
    stamped(MOIRA_IDS.trustedJwtIssuer, {
      // The CONSOLE's issuer, not the IdP's. `consoleProviderIdFor` maps this
      // string onto Better Auth's `providerId`, and a value outside
      // `${bffIssuerUrl}` derives nothing and renders no sign-in button.
      issuer: options.consoleIssuer,
      jwks_url: options.consoleJwksUrl,
      expected_audiences: [options.adminApiAudience],
      allowed_algorithms: ["ES256"],
      subject_claim: "sub",
      clock_skew_seconds: 60,
      allow_delegation: false,
    }),
  ];

  function reset(): void {
    providers = [];
    providerModels = [];
    providerCredentials = [];
    routingPolicies = [];
    recorded = [];
  }

  async function handle(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const path = url.pathname;
    const method = request.method.toUpperCase();
    const raw = method === "GET" || method === "HEAD" ? null : await request.text();
    let body: unknown;
    try {
      body = raw === null || raw === "" ? undefined : JSON.parse(raw);
    } catch {
      body = raw;
    }

    recorded.push({
      route: `${method} ${path}`,
      method,
      path,
      credential: credentialOf(request.headers),
      idempotencyKey: request.headers.get("idempotency-key"),
      body,
    });

    const fields = (body ?? {}) as Record<string, unknown>;

    /* ---- what the console reads before it can offer a sign-in button ----- */

    if (method === "GET" && path === "/api/v1/admin/auth/providers") {
      return json(page(authProviders));
    }
    if (method === "GET" && path === "/api/v1/admin/jwt-issuers") {
      return json(page(trustedJwtIssuers));
    }
    if (method === "GET" && path === "/api/v1/admin/setup/sign-in-methods") {
      // Anonymous, and deliberately the narrowed projection: `/login` uses it
      // for display names only and tolerates its absence.
      return json({
        methods: authProviders.map((row) => ({
          id: row["id"],
          method: row["method"],
          display_name: row["display_name"],
          enabled: row["enabled"],
        })),
      });
    }

    /* ---- the four families the chain writes ------------------------------ */

    if (path === "/api/v1/admin/providers") {
      if (method === "GET") return json(page(providers));
      if (method === "POST") {
        const created = stamped(MOIRA_IDS.provider, {
          provider_type: fields["provider_type"],
          display_name: fields["display_name"],
          base_url: fields["base_url"],
        });
        providers.push(created);
        return json(created, 201);
      }
    }

    const nestedModels = /^\/api\/v1\/admin\/providers\/([^/]+)\/models$/.exec(path);
    if (nestedModels !== null) {
      const providerId = nestedModels[1]!;
      if (method === "GET") {
        return json(page(providerModels.filter((row) => row["provider_id"] === providerId)));
      }
      if (method === "POST") {
        const created = stamped(MOIRA_IDS.providerModel, {
          provider_id: providerId,
          model_key: fields["model_key"],
          capabilities: fields["capabilities"] ?? { text: true, streaming: true, tools: true },
        });
        providerModels.push(created);
        return json(created, 201);
      }
    }

    if (path === "/api/v1/admin/provider-credentials") {
      if (method === "GET") {
        const providerId = url.searchParams.get("provider_id");
        return json(
          page(
            providerId === null
              ? providerCredentials
              : providerCredentials.filter((row) => row["provider_id"] === providerId),
          ),
        );
      }
      if (method === "POST") {
        const created = stamped(MOIRA_IDS.providerCredential, {
          provider_id: fields["provider_id"],
          credential_type: fields["credential_type"],
          scope: fields["scope"],
          // Both of these are things the console must never render. They are
          // here so `secret-leak`-shaped assertions have something to look for.
          secret_fingerprint: "sha256:authenticated-e2e-fingerprint",
          masked_secret: "sk-****mask",
          priority: 0,
        });
        providerCredentials.push(created);
        return json(created, 201);
      }
    }

    if (method === "GET" && path === "/api/v1/admin/routes") {
      const routeKey = url.searchParams.get("route_key");
      return json(
        page(routeKey === null ? routes : routes.filter((r) => r["route_key"] === routeKey)),
      );
    }

    if (path === "/api/v1/admin/routing-policies") {
      if (method === "GET") return json(page(routingPolicies));
      if (method === "POST") {
        const created = stamped(MOIRA_IDS.routingPolicy, {
          route_id: fields["route_id"],
          provider_id: fields["provider_id"],
          provider_model_id: fields["provider_model_id"],
          priority: fields["priority"],
          weight: fields["weight"],
          cost_weight: 1,
          latency_weight: 1,
          quality_weight: 1,
          required_capabilities: [],
          retry_policy: null,
        });
        routingPolicies.push(created);
        return json(created, 201);
      }
    }

    // Anything else is a route this fixture has not been taught. A 501 naming
    // the route is the honest answer: a 404 would be indistinguishable from
    // Moira saying the row does not exist, and a spec would chase the wrong bug.
    return json(
      errorEnvelope(
        "fixture_route_not_implemented",
        `the authenticated e2e Moira fixture has no handler for ${method} ${path}`,
      ),
      501,
    );
  }

  return {
    handle,
    recording: () => recorded,
    forgetRecording: () => {
      recorded = [];
    },
    reset,
    rows: () => ({
      providers: providers.length,
      providerModels: providerModels.length,
      providerCredentials: providerCredentials.length,
      routingPolicies: routingPolicies.length,
    }),
  };
}

/** What the mock OpenAI-compatible endpoint serves at `/v1/models`. */
export function discoveryBody() {
  return {
    object: "list",
    data: DISCOVERABLE_MODEL_KEYS.map((id) => ({ id, object: "model", owned_by: "fixture" })),
  };
}
