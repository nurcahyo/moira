// The keys screen's server-side half, against a recording stub.
//
// ============================================================================
// THE ASSERTION THAT MATTERS MOST HERE IS AN ABSENCE
// ============================================================================
//
// `ApiKeyRecord` carries `fingerprint` — a stable hash of a live credential —
// and `pepper_version`. The whole reason `loadConsumerKeys` builds a projection
// instead of forwarding Moira's rows is that neither may reach the browser, and
// a projection is only worth its cost if something fails when it stops dropping
// them. So the stub returns rows that DO carry both, and the tests assert they
// are gone.
//
// The stub is a `fetch` replacement rather than a fake client, for the same
// reason `llm-settings.test.ts` uses one: a fake client would let the paths, the
// headers and the query shape drift while every assertion here stayed green.

import { describe, expect, test } from "bun:test";

import {
  DEFAULT_CONSUMER_SCOPE,
  OFFERED_CONSUMER_SCOPES,
  loadConsumerKeys,
  narrowScopes,
  normalizeApplicationSlug,
} from "@/lib/consumer-keys";
import { MoiraClient } from "@/lib/moira-client";
import { MOIRA_STUB_BASE_URL, createMoiraStub } from "../../support/moira-stub";

const APPLICATION_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_APPLICATION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const KEY_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

const APPLICATION_LIST = "GET /api/v1/admin/applications";
const KEY_LIST = "GET /api/v1/admin/consumer-keys";

/** A key row exactly as Moira sends it — fingerprint and pepper included. */
function keyRow(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: KEY_ID,
    application_id: APPLICATION_ID,
    display_name: "Checkout service",
    key_prefix: "moira_cons_example",
    fingerprint: "sha256:1f0d9c4b8a7e6d5c4b3a2918f7e6d5c4",
    pepper_version: "v1",
    scopes: ["moira:responses:create"],
    status: "active",
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    ...overrides,
  };
}

function applicationRow(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: APPLICATION_ID,
    display_name: "Checkout",
    application_slug: "checkout",
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function clientWith(applications: unknown[], keys: unknown[], hasMore = false) {
  const stub = createMoiraStub({
    [APPLICATION_LIST]: () => ({
      status: 200,
      body: { data: applications, pagination: { has_more: hasMore } },
    }),
    [KEY_LIST]: () => ({ status: 200, body: { data: keys, pagination: { has_more: false } } }),
  });
  const client = new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "sk_test_bootstrap",
    fetch: stub.fetch,
  });
  return { stub, client };
}

describe("the projection drops what the browser must not receive", () => {
  test("no key view carries a fingerprint or a pepper version", async () => {
    const { client } = clientWith([applicationRow()], [keyRow()]);
    const view = await loadConsumerKeys(client);

    const key = view.applications[0]?.keys[0];
    expect(key).toBeDefined();
    // Named individually rather than via a key-set comparison: the failure
    // message should say WHICH field came through.
    expect(Object.keys(key!), "fingerprint reached the view model").not.toContain("fingerprint");
    expect(Object.keys(key!), "pepper_version reached the view model").not.toContain(
      "pepper_version",
    );
    // And the whole serialized view, in case a field arrives under a new name.
    expect(JSON.stringify(view)).not.toContain("sha256:");
  });

  test("key_prefix IS kept — it is the only part of the credential a list may show", async () => {
    const { client } = clientWith([applicationRow()], [keyRow()]);
    const view = await loadConsumerKeys(client);
    expect(view.applications[0]?.keys[0]?.key_prefix).toBe("moira_cons_example");
  });

  test("absent optional timestamps become null rather than undefined", async () => {
    // The view is serialized to the browser as JSON, where `undefined` vanishes
    // and a component's `?? fallback` then runs on a MISSING property instead of
    // a null one. Normalising here keeps the two paths identical.
    const { client } = clientWith([applicationRow()], [keyRow()]);
    const key = (await loadConsumerKeys(client)).applications[0]?.keys[0];
    expect(key?.last_used_at).toBeNull();
    expect(key?.expires_at).toBeNull();
  });
});

describe("keys are grouped under the application that owns them", () => {
  test("a key lands under its own application and nowhere else", async () => {
    const { client } = clientWith(
      [applicationRow(), applicationRow({ id: OTHER_APPLICATION_ID, display_name: "Support" })],
      [keyRow()],
    );
    const view = await loadConsumerKeys(client);

    expect(view.applications).toHaveLength(2);
    expect(view.applications[0]?.keys.map((key) => key.id)).toEqual([KEY_ID]);
    expect(view.applications[1]?.keys).toEqual([]);
    expect(view.unattachedKeys).toEqual([]);
  });

  test("a key whose application is not listed is SURFACED, not dropped", async () => {
    // The property the unattached list exists for: a key that still
    // authenticates and appears on no screen is the one nobody revokes.
    const { client } = clientWith([], [keyRow()]);
    const view = await loadConsumerKeys(client);

    expect(view.applications).toEqual([]);
    expect(view.unattachedKeys.map((key) => key.id)).toEqual([KEY_ID]);
  });

  test("a key with no application_id at all is surfaced too", async () => {
    const { client } = clientWith([applicationRow()], [keyRow({ application_id: null })]);
    const view = await loadConsumerKeys(client);
    expect(view.unattachedKeys.map((key) => key.id)).toEqual([KEY_ID]);
    expect(view.applications[0]?.keys).toEqual([]);
  });

  test("a truncated list says so", async () => {
    const { client } = clientWith([applicationRow()], [keyRow()], true);
    expect((await loadConsumerKeys(client)).truncated).toBe(true);
  });

  test("both lists are read with the system key, on the paths the spec names", async () => {
    const { stub, client } = clientWith([applicationRow()], [keyRow()]);
    await loadConsumerKeys(client);

    const paths = stub.requests.map((request) => new URL(request.url).pathname).sort();
    expect(paths).toEqual(["/api/v1/admin/applications", "/api/v1/admin/consumer-keys"]);
    for (const request of stub.requests) {
      expect(request.headers["X-Moira-System-Key"]).toBe("sk_test_bootstrap");
    }
  });
});

describe("the scope curation is enforced on the server", () => {
  test("a scope outside the offered list is discarded", async () => {
    // The form is a client component and its checkboxes are a suggestion. This
    // is the line that decides, so it is the line that is tested.
    expect(narrowScopes(["moira:responses:create", "moira:system-keys:write"])).toEqual([
      "moira:responses:create",
    ]);
  });

  test("an empty or unusable selection falls back to the default, never to nothing", () => {
    // `scopes: []` is accepted by Moira and produces a credential that
    // authenticates and can do nothing — indistinguishable at the caller from a
    // broken deployment.
    expect(narrowScopes([])).toEqual([DEFAULT_CONSUMER_SCOPE]);
    expect(narrowScopes(["moira:admin"])).toEqual([DEFAULT_CONSUMER_SCOPE]);
    expect(narrowScopes(undefined)).toEqual([DEFAULT_CONSUMER_SCOPE]);
    expect(narrowScopes("moira:responses:create")).toEqual([DEFAULT_CONSUMER_SCOPE]);
    expect(narrowScopes([42, null])).toEqual([DEFAULT_CONSUMER_SCOPE]);
  });

  test("duplicates collapse and the result is ordered", () => {
    expect(
      narrowScopes(["moira:responses:read", "moira:responses:create", "moira:responses:read"]),
    ).toEqual(["moira:responses:create", "moira:responses:read"]);
  });

  test("no administrative scope is offered at all", () => {
    // The absence is the decision. A filter at the edge would still leave the
    // form able to render one.
    const administrative = OFFERED_CONSUMER_SCOPES.filter((scope) =>
      /:(write|delete|rotate|revoke)$|^moira:admin$|system-keys|consumer-keys|credentials|providers|jwt-issuers|auth-settings|applications/.test(
        scope,
      ),
    ).filter((scope) => !scope.startsWith("moira:conversations:"));
    expect(
      administrative,
      "an administrative scope reached the offered list — see lib/consumer-keys.ts's header",
    ).toEqual([]);
  });

  test("the default is one of the offered scopes", () => {
    expect(OFFERED_CONSUMER_SCOPES).toContain(DEFAULT_CONSUMER_SCOPE);
  });
});

describe("the application slug is normalised, never a reason to refuse", () => {
  test("it lower-cases and trims", () => {
    expect(normalizeApplicationSlug("  Checkout-1 ")).toBe("checkout-1");
  });

  test("a value that is not slug-shaped becomes null rather than an error", () => {
    // The application is still created, with no slug. Failing an operator's real
    // intent over a field Moira does not require would be the wrong trade.
    expect(normalizeApplicationSlug("check out!")).toBeNull();
    expect(normalizeApplicationSlug("")).toBeNull();
    expect(normalizeApplicationSlug("   ")).toBeNull();
    expect(normalizeApplicationSlug(undefined)).toBeNull();
    expect(normalizeApplicationSlug(7)).toBeNull();
  });
});
