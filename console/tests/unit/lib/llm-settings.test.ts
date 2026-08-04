// The provisioning chain and the discovery probe, against a recording stub.
//
// ============================================================================
// WHAT THESE TESTS ARE FOR, AND WHAT THEY CANNOT SEE
// ============================================================================
//
// The ORDER of the writes, the reuse-first lookups, and the shape of every body
// that goes on the wire. What they cannot see is whether anything calls
// `runConnectChain` at all — that is finding F25's shape and it belongs to
// `tests/unit/api/llm-routes.test.ts`, which drives the exported route handlers.
//
// The stub is a `fetch` replacement rather than a fake client, deliberately: a
// fake client would let the order, the headers and the bodies all drift while
// every assertion here stayed green.

import { describe, expect, test } from "bun:test";

import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import {
  canonicalOpenAiBaseUrl,
  discoverModels,
  isLlmProvisioningError,
  loadLlmSettings,
  modelKeysFromDiscoveryBody,
  narrowModelKeys,
  runConnectChain,
  DISCOVERY_MAX_BYTES,
  DISCOVERY_MAX_MODEL_KEY_LENGTH,
  DISCOVERY_MAX_MODELS,
} from "@/lib/llm-settings";
import { MoiraClient } from "@/lib/moira-client";
import {
  createMoiraStub,
  errorEnvelope,
  MOIRA_STUB_BASE_URL,
  type MoiraStub,
  type StubHandler,
} from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const PROVIDER_ID = "11111111-1111-4111-8111-111111111111";
const MODEL_ID = "22222222-2222-4222-8222-222222222222";
const CREDENTIAL_ID = "33333333-3333-4333-8333-333333333333";
const ROUTE_ID = "44444444-4444-4444-8444-444444444444";
const POLICY_ID = "55555555-5555-4555-8555-555555555555";

const BASE_URL = "https://local-llm.example.test/v1";
const MODEL_KEY = "Qwen3-4B";

const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const PROVIDER_ENABLE = `POST /api/v1/admin/providers/${PROVIDER_ID}/enable`;
const MODEL_LIST = `GET /api/v1/admin/providers/${PROVIDER_ID}/models`;
const MODEL_CREATE = `POST /api/v1/admin/providers/${PROVIDER_ID}/models`;
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const ROUTE_LIST = "GET /api/v1/admin/routes";
const POLICY_LIST = "GET /api/v1/admin/routing-policies";
const POLICY_CREATE = "POST /api/v1/admin/routing-policies";

const EMPTY_PAGE = { data: [], pagination: { has_more: false, next_cursor: null } };

function page(rows: readonly unknown[], hasMore = false) {
  return { data: rows, pagination: { has_more: hasMore, next_cursor: null } };
}

function providerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: PROVIDER_ID,
    provider_type: "open_ai_compatible",
    display_name: "Local endpoint",
    status: "active",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    base_url: BASE_URL,
    ...overrides,
  };
}

function modelRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: MODEL_ID,
    provider_id: PROVIDER_ID,
    model_key: MODEL_KEY,
    capabilities: { text: true, streaming: true, tools: true },
    status: "active",
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function credentialRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: CREDENTIAL_ID,
    provider_id: PROVIDER_ID,
    credential_type: "api_key",
    scope: { type: "global" },
    secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
    masked_secret: "sk-****mask",
    status: "active",
    priority: 0,
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function routeRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: ROUTE_ID,
    route_key: "general",
    display_name: "General",
    status: "active",
    selection_strategy: "default",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function policyRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: POLICY_ID,
    route_id: ROUTE_ID,
    provider_id: PROVIDER_ID,
    provider_model_id: MODEL_ID,
    priority: 100,
    weight: 1,
    cost_weight: 1,
    latency_weight: 1,
    quality_weight: 1,
    required_capabilities: [],
    retry_policy: null,
    status: "active",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [PROVIDER_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [PROVIDER_CREATE]: () => ({ status: 201, body: providerRecord() }),
    [PROVIDER_ENABLE]: () => ({ status: 200, body: providerRecord({ version: 2 }) }),
    [MODEL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [MODEL_CREATE]: () => ({ status: 201, body: modelRecord() }),
    [CREDENTIAL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [CREDENTIAL_CREATE]: () => ({ status: 201, body: credentialRecord() }),
    [ROUTE_LIST]: () => ({ status: 200, body: page([routeRecord()]) }),
    [POLICY_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [POLICY_CREATE]: () => ({ status: 201, body: policyRecord() }),
    ...overrides,
  };
}

function clientFor(stub: MoiraStub): MoiraClient {
  return new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "sk_test_stub",
    fetch: stub.fetch,
  });
}

function connect(stub: MoiraStub, modelKeys: readonly string[] = [MODEL_KEY]) {
  return runConnectChain(clientFor(stub), {
    baseUrl: BASE_URL,
    displayName: "Local endpoint",
    modelKeys,
    placeholderKey: "placeholder-for-a-keyless-endpoint",
  });
}

/* -------------------------------------------------------------------------- */
/* The order, which is the whole point                                        */
/* -------------------------------------------------------------------------- */

describe("the chain runs the seed script's order and nothing else", () => {
  test("provider -> model -> credential -> routing policy, on a clean deployment", async () => {
    const stub = createMoiraStub(handlers());
    const result = await connect(stub);

    expect(stub.routes()).toEqual([
      PROVIDER_LIST,
      PROVIDER_CREATE,
      MODEL_LIST,
      MODEL_CREATE,
      CREDENTIAL_LIST,
      CREDENTIAL_CREATE,
      // No enable: `POST /providers` creates an ACTIVE row, exactly as the seed
      // script assumes when it omits the step entirely.
      ROUTE_LIST,
      POLICY_LIST,
      POLICY_CREATE,
    ]);
    expect(result.providerId).toBe(PROVIDER_ID);
    expect(result.state.routingPolicyIds).toEqual([POLICY_ID]);
    expect(result.trace.map((entry) => entry.step)).toEqual([
      "provider",
      "provider_model",
      "provider_credential",
      "provider_enable",
      "routing_policy",
    ]);
    expect(result.trace.find((entry) => entry.step === "provider_enable")?.outcome).toBe("skipped");
  });

  test("THE CREDENTIAL IS WRITTEN EVEN THOUGH THE ENDPOINT IS KEYLESS", async () => {
    // The 404 `credential_not_found` in `scripts/seed-local.sh`'s header. Routing
    // resolves a credential ROW before it builds any request, so skipping this
    // step for a keyless endpoint produces an error that reads as "your key is
    // wrong" when the truth is "there is no key at all".
    const stub = createMoiraStub(handlers());
    await connect(stub);
    const body = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    expect(body["provider_id"]).toBe(PROVIDER_ID);
    expect(body["credential_type"]).toBe("api_key");
    expect(body["scope"]).toEqual({ type: "global" });
    // EXACTLY `{ api_key }`. `endpoint`, even as null, makes the untagged union
    // ambiguous with the azure arm and the request is refused naming no field.
    expect(Object.keys(body["secret"] as Record<string, unknown>)).toEqual(["api_key"]);
    expect((body["secret"] as { api_key: string }).api_key.length).toBeGreaterThan(0);
  });

  test("`capabilities` is sent explicitly, never omitted", async () => {
    // An absent value is stored as SQL null, matches no capability filter, and
    // surfaces later as `no_eligible_model` — an error naming neither the model
    // nor the missing column.
    const stub = createMoiraStub(handlers());
    await connect(stub);
    const body = stub.bodyOf(MODEL_CREATE) as Record<string, unknown>;
    expect(body["capabilities"]).toEqual({ text: true, streaming: true, tools: true });
    expect(body["model_key"]).toBe(MODEL_KEY);
  });

  test("the routing policy binds to the SEEDED route, and no route is ever created", async () => {
    const stub = createMoiraStub(handlers());
    await connect(stub);
    const body = stub.bodyOf(POLICY_CREATE) as Record<string, unknown>;
    expect(body["route_id"]).toBe(ROUTE_ID);
    expect(body["provider_id"]).toBe(PROVIDER_ID);
    expect(body["provider_model_id"]).toBe(MODEL_ID);
    expect(body["priority"]).toBe(100);
    expect(body["weight"]).toBe(1);
    // The other half of the seed script's header: a route is never sent as an
    // override, and `POST /routes` is never called at all.
    expect(stub.routes()).not.toContain("POST /api/v1/admin/routes");
    expect(JSON.stringify(stub.requests)).not.toContain('"route":"general"');
  });

  test("a provider that is not active IS enabled, and the step is not skipped", async () => {
    // The one departure from the seed script, and the state it exists for: an
    // operator disabled a provider after a wrong endpoint, then reconnected.
    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord({ status: "disabled" })]) }),
      }),
    );
    const result = await connect(stub);
    expect(stub.routes()).toContain(PROVIDER_ENABLE);
    expect(result.trace.find((entry) => entry.step === "provider_enable")?.outcome).toBe("created");
    // `If-Match` from the record that was read, never fabricated.
    expect(stub.requestsFor(PROVIDER_ENABLE)[0]?.headers["If-Match"]).toBe("1");
  });
});

/* -------------------------------------------------------------------------- */
/* Reuse-first: a second click continues rather than duplicating              */
/* -------------------------------------------------------------------------- */

describe("every step is reuse-first", () => {
  test("nothing is created twice when everything already exists", async () => {
    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
        [MODEL_LIST]: () => ({ status: 200, body: page([modelRecord()]) }),
        [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
        [POLICY_LIST]: () => ({ status: 200, body: page([policyRecord()]) }),
      }),
    );
    const result = await connect(stub);

    expect(stub.routes()).toEqual([
      PROVIDER_LIST,
      MODEL_LIST,
      CREDENTIAL_LIST,
      ROUTE_LIST,
      POLICY_LIST,
    ]);
    expect(result.trace.every((entry) => entry.outcome !== "created")).toBe(true);
  });

  test("a chain that failed at the credential step resumes without a second provider", async () => {
    // The §0 partial state. `POST /providers` documents a 409, but
    // `POST /routing-policies` documents NONE — so a blind retry is how two
    // eligible policies get created, and the lookup is what prevents it.
    const failing = createMoiraStub(
      handlers({
        [CREDENTIAL_CREATE]: () => ({ status: 503, body: errorEnvelope("database_unavailable") }),
      }),
    );
    let caught: unknown;
    try {
      await connect(failing);
    } catch (error) {
      caught = error;
    }
    expect(isLlmProvisioningError(caught)).toBe(true);
    const failure = caught as { step: string; state: { providerId: string | null } };
    expect(failure.step).toBe("provider_credential");
    // WHAT WAS ALREADY WRITTEN. Without this the operator retries blind.
    expect(failure.state.providerId).toBe(PROVIDER_ID);

    const retry = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
        [MODEL_LIST]: () => ({ status: 200, body: page([modelRecord()]) }),
      }),
    );
    await connect(retry);
    expect(retry.routes()).not.toContain(PROVIDER_CREATE);
    expect(retry.routes()).not.toContain(MODEL_CREATE);
    expect(retry.routes()).toContain(CREDENTIAL_CREATE);
    expect(retry.routes()).toContain(POLICY_CREATE);
  });

  test("A DISABLED ROW IS REUSED **AND ENABLED**, not reported as if it worked", async () => {
    // The state an operator reaches with this page's own buttons: connect, then
    // disable a model, a credential row and a policy. Every reuse-first match
    // accepted a disabled row and traced it as "reused", so the shortcut
    // answered 201 announcing that everything exists — while `runtime.rs` joins
    // all three on `status = 'active'` and no prompt could route. Narrowing the
    // match instead would not do: a disabled model still occupies the partial
    // unique index on `model_key`, so creating a second one is a unique
    // violation Moira has no mapping for, i.e. an opaque 500.
    const MODEL_ENABLE = `POST /api/v1/admin/provider-models/${MODEL_ID}/enable`;
    const CREDENTIAL_ENABLE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/enable`;
    const POLICY_ENABLE = `POST /api/v1/admin/routing-policies/${POLICY_ID}/enable`;

    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
        [MODEL_LIST]: () => ({
          status: 200,
          body: page([modelRecord({ status: "disabled", version: 4 })]),
        }),
        [MODEL_ENABLE]: () => ({ status: 200, body: modelRecord({ version: 5 }) }),
        [CREDENTIAL_LIST]: () => ({
          status: 200,
          body: page([credentialRecord({ status: "disabled", version: 6 })]),
        }),
        [CREDENTIAL_ENABLE]: () => ({ status: 200, body: credentialRecord({ version: 7 }) }),
        [POLICY_LIST]: () => ({
          status: 200,
          body: page([policyRecord({ status: "disabled", version: 8 })]),
        }),
        [POLICY_ENABLE]: () => ({ status: 200, body: policyRecord({ version: 9 }) }),
      }),
    );
    const result = await connect(stub);

    expect(stub.routes()).toEqual([
      PROVIDER_LIST,
      MODEL_LIST,
      MODEL_ENABLE,
      CREDENTIAL_LIST,
      CREDENTIAL_ENABLE,
      ROUTE_LIST,
      POLICY_LIST,
      POLICY_ENABLE,
    ]);
    // Nothing was duplicated on the way.
    for (const write of [MODEL_CREATE, CREDENTIAL_CREATE, POLICY_CREATE]) {
      expect(stub.routes(), `${write} duplicated a row that already existed`).not.toContain(write);
    }
    // Each `If-Match` is the version that was READ, never fabricated.
    expect(stub.requestsFor(MODEL_ENABLE)[0]?.headers["If-Match"]).toBe("4");
    expect(stub.requestsFor(CREDENTIAL_ENABLE)[0]?.headers["If-Match"]).toBe("6");
    expect(stub.requestsFor(POLICY_ENABLE)[0]?.headers["If-Match"]).toBe("8");

    // AND THE REPORT SAYS SO. "reused" here would be the console telling the
    // operator a deployment works when it does not.
    const outcomes = Object.fromEntries(result.trace.map((entry) => [entry.step, entry.outcome]));
    expect(outcomes["provider_model"]).toBe("enabled");
    expect(outcomes["provider_credential"]).toBe("enabled");
    expect(outcomes["routing_policy"]).toBe("enabled");
  });

  test("an ACTIVE row is still merely reused — no version is burned on a no-op", async () => {
    // The negative control for the test above. Enabling what is already enabled
    // would move the version under a concurrent editor for no reason.
    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
        [MODEL_LIST]: () => ({ status: 200, body: page([modelRecord()]) }),
        [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
        [POLICY_LIST]: () => ({ status: 200, body: page([policyRecord()]) }),
      }),
    );
    const result = await connect(stub);
    expect(stub.routes().filter((route) => route.endsWith("/enable"))).toEqual([]);
    expect(result.trace.every((entry) => entry.outcome !== "enabled")).toBe(true);
  });

  test("a truncated list is refused rather than guessed", async () => {
    // `scripts/seed-local.sh`'s `find_by`: "not on this page" is not "does not
    // exist", and creating the row anyway makes the duplicate the dedupe existed
    // to prevent.
    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([], true) }),
      }),
    );
    let caught: unknown;
    try {
      await connect(stub);
    } catch (error) {
      caught = error;
    }
    expect(isLlmProvisioningError(caught)).toBe(true);
    expect((caught as { messageKey: string }).messageKey).toBe(
      CONSOLE_MESSAGE_KEYS.llm_list_truncated,
    );
    expect(stub.routes()).not.toContain(PROVIDER_CREATE);
  });

  test("a truncation at step five still reports the four steps that succeeded", async () => {
    // `findOnPage` threw with `state: emptyConnectState()` and `trace: []`,
    // which discards exactly what `LlmProvisioningError` exists to carry: the
    // operator is told a provider, a model and a credential row exist, or the
    // second click is made blind.
    const stub = createMoiraStub(
      handlers({ [POLICY_LIST]: () => ({ status: 200, body: page([], true) }) }),
    );
    let caught: unknown;
    try {
      await connect(stub);
    } catch (error) {
      caught = error;
    }
    expect(isLlmProvisioningError(caught)).toBe(true);
    const failure = caught as {
      step: string;
      state: { providerId: string | null; providerModelIds: readonly string[] };
      trace: readonly { step: string }[];
    };
    expect(failure.step).toBe("routing_policy");
    expect(failure.state.providerId).toBe(PROVIDER_ID);
    expect(failure.state.providerModelIds).toEqual([MODEL_ID]);
    expect(failure.trace.map((entry) => entry.step)).toEqual([
      "provider",
      "provider_model",
      "provider_credential",
      "provider_enable",
    ]);
    expect(stub.routes()).not.toContain(POLICY_CREATE);
  });

  test("a missing `general` route is an actionable keyed error, not a crash", async () => {
    const stub = createMoiraStub(
      handlers({
        [ROUTE_LIST]: () => ({ status: 200, body: page([routeRecord({ route_key: "other" })]) }),
      }),
    );
    let caught: unknown;
    try {
      await connect(stub);
    } catch (error) {
      caught = error;
    }
    expect(isLlmProvisioningError(caught)).toBe(true);
    const failure = caught as { step: string; messageKey: string };
    expect(failure.step).toBe("routing_policy");
    expect(failure.messageKey).toBe(CONSOLE_MESSAGE_KEYS.llm_general_route_missing);
    expect(stub.routes()).not.toContain("POST /api/v1/admin/routes");
    expect(stub.routes()).not.toContain(POLICY_CREATE);
  });
});

/* -------------------------------------------------------------------------- */
/* The base URL                                                               */
/* -------------------------------------------------------------------------- */

describe("canonicalOpenAiBaseUrl", () => {
  test("the version segment is appended when it is absent", () => {
    // A provider row created from a bare origin sends completions one path
    // segment short and 404s at request time, long after this screen said it
    // worked.
    expect(canonicalOpenAiBaseUrl("https://local-llm.example.test")).toEqual({
      ok: true,
      baseUrl: "https://local-llm.example.test/v1",
    });
    expect(canonicalOpenAiBaseUrl("https://local-llm.example.test/")).toEqual({
      ok: true,
      baseUrl: "https://local-llm.example.test/v1",
    });
  });

  test("an address that already carries one is left alone", () => {
    expect(canonicalOpenAiBaseUrl("http://127.0.0.1:8000/v1")).toEqual({
      ok: true,
      baseUrl: "http://127.0.0.1:8000/v1",
    });
  });

  const refusals: ReadonlyArray<{ what: string; input: unknown; key: string }> = [
    { what: "an empty field", input: "  ", key: CONSOLE_MESSAGE_KEYS.llm_base_url_required },
    { what: "a non-string", input: 42, key: CONSOLE_MESSAGE_KEYS.llm_base_url_required },
    { what: "not an address", input: "not an address", key: CONSOLE_MESSAGE_KEYS.llm_base_url_invalid },
    {
      what: "a scheme the console will not fetch",
      input: "file:///etc/passwd",
      key: CONSOLE_MESSAGE_KEYS.llm_base_url_scheme_unsupported,
    },
    {
      what: "an address carrying sign-in details",
      input: "https://user:pass@local-llm.example.test/v1",
      key: CONSOLE_MESSAGE_KEYS.llm_base_url_userinfo_rejected,
    },
  ];

  for (const refusal of refusals) {
    test(`${refusal.what} is refused with its own key`, () => {
      expect(canonicalOpenAiBaseUrl(refusal.input)).toEqual({ ok: false, messageKey: refusal.key });
    });
  }
});

/* -------------------------------------------------------------------------- */
/* Discovery                                                                  */
/* -------------------------------------------------------------------------- */

function respond(body: unknown, status = 200): typeof fetch {
  return (async () =>
    new Response(typeof body === "string" ? body : JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    })) as unknown as typeof fetch;
}

describe("modelKeysFromDiscoveryBody — the validation, on its own", () => {
  test("a well-formed listing yields its ids, de-duplicated and in order", () => {
    expect(
      modelKeysFromDiscoveryBody({ data: [{ id: "a" }, { id: "b" }, { id: "a" }] }),
    ).toEqual(["a", "b"]);
  });

  const rejected: ReadonlyArray<{ what: string; body: unknown }> = [
    { what: "an array at the top level", body: [{ id: "a" }] },
    { what: "a string", body: "a" },
    { what: "no `data`", body: { models: [{ id: "a" }] } },
    { what: "`data` that is not an array", body: { data: { id: "a" } } },
    { what: "an entry that is not an object", body: { data: ["a"] } },
    { what: "an entry with no `id`", body: { data: [{ name: "a" }] } },
    { what: "a numeric `id`", body: { data: [{ id: 7 }] } },
    { what: "a blank `id`", body: { data: [{ id: "   " }] } },
    {
      // Written as an ESCAPE, not as a raw 0x00 byte. The implementation states the
      // convention in as many words ("Written as escapes rather than as raw bytes so
      // the range stays readable in a diff"), and a literal NUL in the source is
      // invisible in a review, survives a copy-paste as nothing at all, and makes the
      // test file itself the counter-example to the rule it is asserting.
      what: "an `id` carrying a control character",
      body: { data: [{ id: "a\u0000b" }] },
    },
    {
      what: "an unboundedly long `id`",
      body: { data: [{ id: "x".repeat(DISCOVERY_MAX_MODEL_KEY_LENGTH + 1) }] },
    },
    {
      what: "more entries than the cap",
      body: { data: Array.from({ length: 500 }, (_, index) => ({ id: `m${index}` })) },
    },
  ];

  for (const scenario of rejected) {
    test(`${scenario.what} is rejected outright`, () => {
      expect(modelKeysFromDiscoveryBody(scenario.body)).toBeNull();
    });
  }
});

describe("discoverModels", () => {
  test("it probes `<base>/models` and returns what the endpoint advertised", async () => {
    const seen: string[] = [];
    const spy = (async (url: RequestInfo | URL) => {
      seen.push(String(url));
      return new Response(JSON.stringify({ data: [{ id: MODEL_KEY }] }), { status: 200 });
    }) as unknown as typeof fetch;

    const outcome = await discoverModels("https://local-llm.example.test", { fetchImpl: spy });
    expect(outcome).toEqual({
      ok: true,
      baseUrl: "https://local-llm.example.test/v1",
      models: [MODEL_KEY],
    });
    expect(seen).toEqual(["https://local-llm.example.test/v1/models"]);
  });

  test("AN UNREACHABLE ENDPOINT IS A KEYED MESSAGE, NOT A THROW", async () => {
    // A laptop with the tunnel down is the ordinary case. This must not reject.
    const dead = (async () => {
      throw new TypeError("fetch failed");
    }) as unknown as typeof fetch;
    expect(await discoverModels(BASE_URL, { fetchImpl: dead })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable,
    });
  });

  test("a hung endpoint is abandoned at the timeout rather than held open", async () => {
    const hang = ((_url: RequestInfo | URL, init?: RequestInit) =>
      new Promise((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new Error("aborted")));
      })) as unknown as typeof fetch;
    expect(await discoverModels(BASE_URL, { fetchImpl: hang, timeoutMs: 10 })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable,
    });
  });

  test("a failure status is reported as a refusal, not as unreachable", async () => {
    expect(await discoverModels(BASE_URL, { fetchImpl: respond({ error: "no" }, 503) })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_refused,
    });
  });

  test("a body that is not JSON is a keyed failure", async () => {
    expect(await discoverModels(BASE_URL, { fetchImpl: respond("<html>nope</html>") })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_invalid_response,
    });
  });

  test("A HOSTILE SHAPE NEVER REACHES A CALLER — the validation is load-bearing", async () => {
    // The mutation this test exists for: delete the `modelKeysFromDiscoveryBody`
    // call in `discoverModels` and this goes red, because the object below would
    // otherwise be handed on as a model listing.
    expect(
      await discoverModels(BASE_URL, {
        fetchImpl: respond({ data: [{ id: { toString: "not a string" } }] }),
      }),
    ).toEqual({ ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_invalid_response });
  });

  test("an oversized body is refused rather than read", async () => {
    const huge = (async () =>
      new Response(JSON.stringify({ data: [{ id: "a" }] }), {
        status: 200,
        headers: { "content-length": String(64 * 1024 * 1024) },
      })) as unknown as typeof fetch;
    expect(await discoverModels(BASE_URL, { fetchImpl: huge })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_response_too_large,
    });
  });

  test("an empty listing is a failure, because there is nothing to offer", async () => {
    expect(await discoverModels(BASE_URL, { fetchImpl: respond({ data: [] }) })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_invalid_response,
    });
  });

  test("the probe refuses to follow a redirect to a host the operator never named", async () => {
    // `redirect: "error"` had no test at all, so deleting the option — or
    // setting it to `"follow"` — was invisible: no fake inspects `init`, and the
    // integration fixture answers 200 directly. An endpoint that 302s moves the
    // request, and the credential-free GET, to somewhere nobody typed.
    let seen: RequestInit | undefined;
    const spy = (async (_url: RequestInfo | URL, init?: RequestInit) => {
      seen = init;
      return new Response(JSON.stringify({ data: [{ id: MODEL_KEY }] }), { status: 200 });
    }) as unknown as typeof fetch;

    await discoverModels(BASE_URL, { fetchImpl: spy });
    expect(seen?.redirect).toBe("error");
    // And no credential of any kind travels with it.
    expect(JSON.stringify(seen?.headers ?? {}).toLowerCase()).not.toContain("authorization");
  });
});

/* -------------------------------------------------------------------------- */
/* The two bounds that only exist once the HEADERS have arrived               */
/* -------------------------------------------------------------------------- */

/**
 * A `fetch` whose RESPONSE HEADERS arrive at once and whose BODY then behaves
 * badly.
 *
 * ============================================================================
 * WHY EVERY EXISTING DISCOVERY FAKE WAS BLIND TO THIS
 * ============================================================================
 *
 * They all return a fully-buffered `Response`, or a promise that never settles.
 * Both are pre-header shapes: `fetch` resolves when the headers land, so a
 * deadline armed only around the fetch call covers exactly the hang those fakes
 * produce, and nothing after it. The one test named "a hung endpoint is
 * abandoned at the timeout" hangs the fetch PROMISE — which is the one hang the
 * old timer already handled — so it stayed green through a defect that let a
 * trickling endpoint hold a Node request handler open indefinitely.
 *
 * This builds the missing shape: 200, headers immediately, then a body that
 * stalls, errors, or never stops. `cancelled()` reports whether the console
 * closed the stream, which is what closes the socket.
 */
function bodyAfterHeaders(build: (controller: ReadableStreamDefaultController<Uint8Array>) => void): {
  readonly impl: typeof fetch;
  readonly cancelled: () => boolean;
} {
  let cancelled = false;
  const impl = (async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        build(controller);
      },
      cancel() {
        cancelled = true;
      },
    });
    return new Response(stream, {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;
  return { impl, cancelled: () => cancelled };
}

const encode = (text: string): Uint8Array => new TextEncoder().encode(text);

/** Let a cancellation that was started synchronously actually run. */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("the discovery deadline covers the BODY, not merely the headers", () => {
  test("A BODY THAT STALLS AFTER THE HEADERS IS ABANDONED, and the stream is closed", async () => {
    // THE MUTATION THIS BUYS: move `clearTimeout(timer)` back into the fetch's
    // own `finally`, or drop the signal from `readBounded`, and this test never
    // finishes — which is precisely the production behaviour it describes. A
    // signed-in caller can point `action: "discover"` at a host that answers 200
    // in ten milliseconds and then writes one byte every thirty seconds; the
    // byte cap is never reached, `done` never arrives, and the handler is pinned
    // for as long as that host chooses. undici's `bodyTimeout` is a 300 s
    // INACTIVITY timer and resets on every byte, so it is not a backstop.
    const trickle = bodyAfterHeaders((controller) => {
      controller.enqueue(encode('{"data":['));
      // ... and nothing more, ever.
    });

    expect(await discoverModels(BASE_URL, { fetchImpl: trickle.impl, timeoutMs: 25 })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable,
    });
    await settle();
    expect(trickle.cancelled(), "the stalled body was left holding the socket").toBe(true);
  });

  test("A BODY THAT FAILS MID-STREAM IS A KEYED REFUSAL, NOT A THROW", async () => {
    // `discoverModels`'s own header promises no path throws. It promised that
    // only as far as the headers: `readBounded` was called bare, so a reset, a
    // TLS shutdown or a premature close rejected out of here, out of the route
    // handler, past `withConsoleSession`'s catch — which rethrows anything that
    // is not a MoiraRequestError — and rendered as a Next 500 with a stack.
    // A half-open network on the operator's own LAN is this screen's stated
    // normal case.
    const broken = bodyAfterHeaders((controller) => {
      controller.enqueue(encode('{"data":['));
      controller.error(new TypeError("terminated"));
    });

    let thrown: unknown = null;
    let outcome: unknown = null;
    try {
      outcome = await discoverModels(BASE_URL, { fetchImpl: broken.impl });
    } catch (error) {
      thrown = error;
    }
    expect(thrown, "discoverModels threw instead of returning a keyed outcome").toBeNull();
    expect(outcome).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable,
    });
  });

  test("the 256 KiB cap is enforced on a STREAM, not only on `content-length`", async () => {
    // The only oversize test declares a 64 MiB `content-length` on a tiny body,
    // so it exercises the pre-check and never the loop. An endpoint that simply
    // omits the header — trivially, and every chunked response does — walked
    // straight past it.
    const chunk = new Uint8Array(64 * 1024);
    let queued = 0;
    const flood = bodyAfterHeaders((controller) => {
      while (queued <= DISCOVERY_MAX_BYTES + chunk.byteLength) {
        controller.enqueue(chunk);
        queued += chunk.byteLength;
      }
    });

    expect(await discoverModels(BASE_URL, { fetchImpl: flood.impl })).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_response_too_large,
    });
    await settle();
    expect(flood.cancelled(), "the oversized body was read but never cancelled").toBe(true);
  });

  test("a body that arrives in pieces and then ENDS is read normally", async () => {
    // The negative control for all three above: chunking on its own is ordinary,
    // and a bound that refused it would be a bound that broke the feature.
    const chunked = bodyAfterHeaders((controller) => {
      controller.enqueue(encode('{"data":[{"id":"'));
      controller.enqueue(encode(MODEL_KEY));
      controller.enqueue(encode('"}]}'));
      controller.close();
    });
    expect(await discoverModels(BASE_URL, { fetchImpl: chunked.impl })).toEqual({
      ok: true,
      baseUrl: BASE_URL,
      models: [MODEL_KEY],
    });
  });
});

/* -------------------------------------------------------------------------- */
/* The other door a model id comes in through                                 */
/* -------------------------------------------------------------------------- */

describe("narrowModelKeys — the `connect` stage gets discovery's bounds too", () => {
  test("a plain list is trimmed and de-duplicated", () => {
    expect(narrowModelKeys([" a ", "b", "a"])).toEqual(["a", "b"]);
  });

  const rejected: ReadonlyArray<{ what: string; input: unknown }> = [
    { what: "not an array", input: "a" },
    { what: "an empty selection", input: [] },
    { what: "a non-string entry", input: ["a", 7] },
    { what: "a blank entry", input: ["a", "   "] },
    {
      what: "an id past the length cap",
      input: ["x".repeat(DISCOVERY_MAX_MODEL_KEY_LENGTH + 1)],
    },
    { what: "an id carrying a control character", input: ["a\u0000b"] },
    {
      what: "more ids than the ceiling",
      input: Array.from({ length: DISCOVERY_MAX_MODELS + 5 }, (_, index) => `m${index}`),
    },
  ];

  for (const scenario of rejected) {
    test(`${scenario.what} is refused`, () => {
      expect(narrowModelKeys(scenario.input)).toBeNull();
    });
  }

  test("the two doors agree, which is the whole point", () => {
    // A body posted to `action: "connect"` is not obliged to carry what
    // discovery offered, so a cap applied to one of them is a cap applied to
    // neither.
    const hostile = "a".repeat(DISCOVERY_MAX_MODEL_KEY_LENGTH + 1);
    expect(modelKeysFromDiscoveryBody({ data: [{ id: hostile }] })).toBeNull();
    expect(narrowModelKeys([hostile])).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Reading the screen                                                         */
/* -------------------------------------------------------------------------- */

describe("loadLlmSettings projects the credential row", () => {
  test("neither the mask nor the fingerprint survives the projection", async () => {
    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
        [MODEL_LIST]: () => ({ status: 200, body: page([modelRecord()]) }),
        [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
        [POLICY_LIST]: () => ({ status: 200, body: page([policyRecord()]) }),
      }),
    );
    const view = await loadLlmSettings(clientFor(stub));

    const serialised = JSON.stringify(view);
    expect(serialised).not.toContain("secret_fingerprint");
    expect(serialised).not.toContain("sha256:fingerprint-that-must-not-cross");
    expect(serialised).not.toContain("masked_secret");
    expect(serialised).not.toContain("sk-****mask");

    expect(view.generalRouteId).toBe(ROUTE_ID);
    expect(view.providers).toHaveLength(1);
    expect(view.providers[0]?.keyRows).toEqual([
      { id: CREDENTIAL_ID, kind: "api_key", status: "active", version: 1 },
    ]);
    expect(view.providers[0]?.policies[0]?.routeKey).toBe("general");
  });

  test("the credential list is filtered server-side by provider", async () => {
    const stub = createMoiraStub(
      handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
      }),
    );
    await loadLlmSettings(clientFor(stub));
    const request = stub.requestsFor(CREDENTIAL_LIST)[0];
    expect(new URL(request!.url).searchParams.get("provider_id")).toBe(PROVIDER_ID);
  });

  test("`generalRouteId` is null when migration 0005's route is absent", async () => {
    const stub = createMoiraStub(
      handlers({ [ROUTE_LIST]: () => ({ status: 200, body: EMPTY_PAGE }) }),
    );
    expect((await loadLlmSettings(clientFor(stub))).generalRouteId).toBeNull();
  });
});
