// The owner-only sign-in settings write path (issue #185).
//
// ============================================================================
// THE ASSERTION THAT MATTERS IS THE REFUSAL, AND WHEN IT HAPPENS
// ============================================================================
//
// A client id moving without its secret is the most likely way to lock a claimed
// deployment out of its own console: Moira stores the new id, this console's seal
// stays bound to the old one, `classifySecretDrift` reports `client_id_mismatch`,
// `resolveAuthConfigs` emits no configuration, and `SignInPanel` renders zero
// buttons — on a deployment whose setup window is shut.
//
// So the test is not merely "it returns an error". It is "it returns an error
// AND NOTHING WENT ON THE WIRE", because a refusal that arrives after the PATCH
// has landed is the failure it was written to prevent.
//
// The stub is a `fetch` replacement rather than a fake client, for the same
// reason `llm-settings.test.ts` uses one: a fake client would let the paths, the
// headers and the bodies drift while every assertion here stayed green.

import { describe, expect, test } from "bun:test";

import {
  loadAuthSettings,
  replaceStoredClientSecret,
  updateAuthProvider,
} from "@/lib/auth-settings";
import { InMemoryConsoleSecretStore } from "@/lib/console-secrets";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import { MOIRA_STUB_BASE_URL, createMoiraStub } from "../../support/moira-stub";

const PROVIDER_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const KEY = Buffer.alloc(32, 7);

const GET = `GET /api/v1/admin/auth/providers/${PROVIDER_ID}`;
const PATCH = `PATCH /api/v1/admin/auth/providers/${PROVIDER_ID}`;

function providerRow(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: PROVIDER_ID,
    method: "generic_oidc",
    display_name: "Corporate IdP",
    enabled: true,
    requested_scopes: ["openid", "email"],
    allowed_email_domains: ["example.test"],
    allowed_algorithms: ["ES256"],
    expected_audiences: ["moira-admin"],
    redirect_uris: [],
    metadata: {},
    status: "active",
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 4,
    client_id: "client-one",
    discovery_url: "https://idp.example.test/.well-known/openid-configuration",
    ...overrides,
  };
}

function harness(handlers: Parameters<typeof createMoiraStub>[0]) {
  const stub = createMoiraStub(handlers);
  const client = new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "sk_test_bootstrap",
    fetch: stub.fetch,
  });
  return { stub, client, store: new InMemoryConsoleSecretStore(KEY) };
}

const readOnly = () => ({ status: 200, body: providerRow() });

describe("a moving client id needs its secret, and the refusal costs no write", () => {
  test("it is refused, and no PATCH is sent", async () => {
    const { stub, client, store } = harness({
      [GET]: readOnly,
      [PATCH]: () => ({ status: 200, body: providerRow({ client_id: "client-two", version: 5 }) }),
    });
    await store.put(PROVIDER_ID, "client-one", "the-old-secret");

    const result = await updateAuthProvider(
      client,
      store,
      PROVIDER_ID,
      { clientId: "client-two" },
      null,
    );

    expect(result.ok).toBe(false);
    expect(result.ok === false && result.failure.kind).toBe("secret_required");
    expect(
      result.ok === false && result.failure.kind === "secret_required" && result.failure.reason,
    ).toBe("client_id_moving");
    // The load-bearing half.
    const methods = stub.requests.map((request) => request.method);
    expect(methods, "a refusal after the write is the failure this prevents").not.toContain(
      "PATCH",
    );
  });

  test("with the secret it goes through, and the seal moves to the NEW id", async () => {
    const { client, store } = harness({
      [GET]: readOnly,
      [PATCH]: () => ({ status: 200, body: providerRow({ client_id: "client-two", version: 5 }) }),
    });
    await store.put(PROVIDER_ID, "client-one", "the-old-secret");

    const result = await updateAuthProvider(
      client,
      store,
      PROVIDER_ID,
      { clientId: "client-two" },
      "the-new-secret",
    );

    expect(result.ok).toBe(true);
    const sealed = await store.read(PROVIDER_ID);
    expect(sealed?.clientId).toBe("client-two");
    // Sealed against the id MOIRA returned, so the envelope opens for the row
    // that now exists.
    expect(await store.reveal(PROVIDER_ID)).toBe("the-new-secret");
  });

  test("an unchanged client id needs no secret", async () => {
    // The common edit — widening the allow-list — must not demand a secret the
    // operator may not have.
    const { client, store } = harness({
      [GET]: readOnly,
      [PATCH]: () => ({ status: 200, body: providerRow({ version: 5 }) }),
    });
    await store.put(PROVIDER_ID, "client-one", "the-old-secret");

    const result = await updateAuthProvider(
      client,
      store,
      PROVIDER_ID,
      { clientId: "client-one", allowedEmailDomains: ["example.test", "other.test"] },
      null,
    );

    expect(result.ok).toBe(true);
  });
});

describe("the PATCH body is built, never round-tripped", () => {
  test("it carries only the supplied fields, and never an immutable one", async () => {
    // `AuthProviderSettingsPatchRequest` is `deny_unknown_fields`, so echoing a
    // loaded record back is a 422 on `method`, `id`, `version` and the rest.
    const { stub, client, store } = harness({
      [GET]: readOnly,
      [PATCH]: () => ({ status: 200, body: providerRow({ version: 5 }) }),
    });
    await store.put(PROVIDER_ID, "client-one", "s");

    await updateAuthProvider(
      client,
      store,
      PROVIDER_ID,
      { displayName: "Renamed", allowedEmailDomains: ["example.test"] },
      null,
    );

    const patch = stub.requests.find((request) => request.method === "PATCH");
    expect(patch).toBeDefined();
    expect(patch!.body).toEqual({
      display_name: "Renamed",
      allowed_email_domains: ["example.test"],
    });
  });

  test("a blank field is omitted, because Moira's PATCH cannot clear anything", async () => {
    const { stub, client, store } = harness({
      [GET]: readOnly,
      [PATCH]: () => ({ status: 200, body: providerRow({ version: 5 }) }),
    });
    await store.put(PROVIDER_ID, "client-one", "s");

    await updateAuthProvider(
      client,
      store,
      PROVIDER_ID,
      { displayName: "   ", issuer: "", allowedEmailDomains: [] },
      null,
    );

    // Nothing to write, so nothing is written — rather than a PATCH storing "".
    expect(stub.requests.map((request) => request.method)).not.toContain("PATCH");
  });

  test("the If-Match is the version READ BACK, not one carried in a form", async () => {
    const { stub, client, store } = harness({
      [GET]: () => ({ status: 200, body: providerRow({ version: 9 }) }),
      [PATCH]: () => ({ status: 200, body: providerRow({ version: 10 }) }),
    });
    await store.put(PROVIDER_ID, "client-one", "s");

    await updateAuthProvider(client, store, PROVIDER_ID, { displayName: "Renamed" }, null);

    const patch = stub.requests.find((request) => request.method === "PATCH");
    expect(patch?.headers["If-Match"]).toBe("9");
  });
});

describe("a write that leaves the two stores disagreeing is NOT reported as saved", () => {
  test("Moira returning no client id refuses rather than sealing against nothing", async () => {
    const { client, store } = harness({
      [GET]: () => ({ status: 200, body: providerRow({ client_id: null }) }),
      [PATCH]: () => ({ status: 200, body: providerRow({ client_id: null, version: 5 }) }),
    });

    const result = await updateAuthProvider(
      client,
      store,
      PROVIDER_ID,
      { displayName: "Renamed" },
      "a-secret",
    );

    expect(result.ok).toBe(false);
    expect(result.ok === false && result.failure.kind).toBe("drift_after_write");
  });

  test("a pre-existing mismatch is reported even when this save changed nothing", async () => {
    // The deployment was already broken. Saying "saved" would send the operator
    // away from the one screen that can repair it.
    const { client, store } = harness({ [GET]: readOnly });
    await store.put(PROVIDER_ID, "a-different-client", "s");

    const result = await updateAuthProvider(client, store, PROVIDER_ID, {}, null);

    expect(result.ok).toBe(false);
    expect(result.ok === false && result.failure.kind).toBe("drift_after_write");
  });
});

describe("replacing the stored secret touches this console only", () => {
  test("it sends no PATCH at all", async () => {
    const { stub, client, store } = harness({ [GET]: readOnly });

    const result = await replaceStoredClientSecret(client, store, PROVIDER_ID, "rotated");

    expect(result.ok).toBe(true);
    expect(stub.requests.map((request) => request.method)).toEqual(["GET"]);
    expect(await store.reveal(PROVIDER_ID)).toBe("rotated");
  });

  test("it seals against MOIRA's client id, not the one already stored", async () => {
    // Sealing against the stored id would preserve an existing drift while
    // reporting success.
    const { client, store } = harness({ [GET]: readOnly });
    await store.put(PROVIDER_ID, "a-stale-client", "s");

    await replaceStoredClientSecret(client, store, PROVIDER_ID, "rotated");

    expect((await store.read(PROVIDER_ID))?.clientId).toBe("client-one");
  });
});

describe("the view carries what the screen needs and no secret", () => {
  test("it reports the sealed client id, the drift, and nothing openable", async () => {
    const { client, store } = harness({ [GET]: readOnly });
    await store.put(PROVIDER_ID, "a-stale-client", "the-secret");

    const view = await loadAuthSettings(client, store, PROVIDER_ID, true);

    expect(view.provider?.clientId).toBe("client-one");
    expect(view.sealedAgainstClientId).toBe("a-stale-client");
    expect(view.hasSealedEnvelope).toBe(true);
    expect(view.driftKey).toBe(CONSOLE_MESSAGE_KEYS.oauth_client_id_drifted);
    expect(view.isOwner).toBe(true);
    // The whole serialized view, in case a field arrives under a new name.
    expect(JSON.stringify(view)).not.toContain("the-secret");
  });

  test("no seal at all is its own state, not a drift", async () => {
    const { client, store } = harness({ [GET]: readOnly });

    const view = await loadAuthSettings(client, store, PROVIDER_ID, false);

    expect(view.hasSealedEnvelope).toBe(false);
    expect(view.sealedAgainstClientId).toBeNull();
    expect(view.driftKey).toBe(CONSOLE_MESSAGE_KEYS.oauth_client_secret_missing);
    expect(view.isOwner).toBe(false);
  });
});
