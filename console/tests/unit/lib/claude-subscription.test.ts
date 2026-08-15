// The "Connect Claude subscription" chain, against a recording stub — same
// shape as `tests/unit/lib/llm-settings.test.ts`: what goes on the wire, in
// what order, and never whether the token comes back out.
//
// Per the owner-approved testing policy (`plans/12-feature-expansion-brainstorm.md`
// §1, "Testing policy for this workstream"), no test in this repository may
// require a real Claude subscription token or the real `claude` CLI — every
// assertion below runs against `createMoiraStub`, never a live provider.

import { describe, expect, test } from "bun:test";

import {
  CLAUDE_SUBSCRIPTION_CREDENTIAL_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
  connectClaudeSubscription,
  isClaudeSubscriptionError,
  MAX_SUBSCRIPTION_TOKEN_LENGTH,
  resolveSubscriptionToken,
} from "@/lib/claude-subscription";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import { createMoiraStub, MOIRA_STUB_BASE_URL, type StubHandler } from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const PROVIDER_ID = "aaaaaaaa-1111-4111-8111-111111111111";
const OTHER_PROVIDER_ID = "aaaaaaaa-2222-4222-8222-222222222222";
const CREDENTIAL_ID = "bbbbbbbb-1111-4111-8111-111111111111";
const FOREIGN_CREDENTIAL_ID = "bbbbbbbb-2222-4222-8222-222222222222";

/** Unmistakable, and asserted absent from every returned/serialised shape. */
const TOKEN = "sk-ant-oat01-unmistakable-subscription-token-4f9c2b";

const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const PROVIDER_ENABLE = `POST /api/v1/admin/providers/${PROVIDER_ID}/enable`;
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const CREDENTIAL_ROTATE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/rotate`;
const CREDENTIAL_ENABLE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/enable`;

// Registered so that a chain which mis-targets another provider's credential
// gets RECORDED and fails on an assertion, instead of dying inside the stub on
// "no handler registered" — a stack trace that says nothing about which row was
// about to be overwritten.
const FOREIGN_ROTATE = `POST /api/v1/admin/provider-credentials/${FOREIGN_CREDENTIAL_ID}/rotate`;
const FOREIGN_ENABLE = `POST /api/v1/admin/provider-credentials/${FOREIGN_CREDENTIAL_ID}/enable`;

function page(rows: readonly unknown[], hasMore = false) {
  return { data: rows, pagination: { has_more: hasMore, next_cursor: null } };
}

function providerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: PROVIDER_ID,
    provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
    display_name: CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function credentialRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: CREDENTIAL_ID,
    provider_id: PROVIDER_ID,
    credential_type: "oauth2",
    scope: { type: "global" },
    secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
    masked_secret: "oauth2****mask",
    status: "active",
    priority: 0,
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

/**
 * An `oauth2` credential belonging to a DIFFERENT provider — the row that makes
 * these tests model the real server.
 *
 * `GET /api/v1/admin/provider-credentials` declares `provider_id` and then
 * ignores it (`PageQuery`'s docstring at `src/domain/admin.rs` says so;
 * `CredentialAdminService::list_credentials` forwards only cursor and limit),
 * so every credential page the console sees is the newest N rows in the WHOLE
 * deployment. A user-scoped `oauth2` credential on some unrelated provider is
 * an ordinary thing for a deployment to hold, and it lands on this page.
 *
 * The stub keys handlers on the bare `"<METHOD> <path>"` for exactly this
 * reason — filtering the fixture by the query string would make the stub MORE
 * capable than Moira and hide the bug this row exists to catch. The query is
 * still recorded on `RecordedRequest.url` for tests that want to assert it.
 */
function foreignOauth2Record(overrides: Record<string, unknown> = {}) {
  return credentialRecord({
    id: FOREIGN_CREDENTIAL_ID,
    provider_id: OTHER_PROVIDER_ID,
    credential_type: "oauth2",
    display_name: "Someone else's OAuth2 credential",
    ...overrides,
  });
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [PROVIDER_LIST]: () => ({ status: 200, body: page([]) }),
    [PROVIDER_CREATE]: () => ({ status: 201, body: providerRecord() }),
    [PROVIDER_ENABLE]: () => ({ status: 200, body: providerRecord({ status: "active", version: 2 }) }),
    // NOT an empty page: the unfiltered global list realistically carries other
    // providers' rows, and the chain must ignore them.
    [CREDENTIAL_LIST]: () => ({ status: 200, body: page([foreignOauth2Record()]) }),
    [CREDENTIAL_CREATE]: () => ({ status: 201, body: credentialRecord() }),
    [CREDENTIAL_ROTATE]: () => ({ status: 200, body: credentialRecord({ version: 2 }) }),
    [CREDENTIAL_ENABLE]: () => ({ status: 200, body: credentialRecord({ status: "active", version: 3 }) }),
    [FOREIGN_ROTATE]: () => ({ status: 200, body: foreignOauth2Record({ version: 2 }) }),
    [FOREIGN_ENABLE]: () => ({ status: 200, body: foreignOauth2Record({ status: "active", version: 3 }) }),
    ...overrides,
  };
}

function clientFor(overrides: Record<string, StubHandler> = {}) {
  const stub = createMoiraStub(handlers(overrides));
  const client = new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "sk_test_stub",
    fetch: stub.fetch,
  });
  return { stub, client };
}

/* -------------------------------------------------------------------------- */
/* connectClaudeSubscription                                                  */
/* -------------------------------------------------------------------------- */

describe("connectClaudeSubscription — the provider row", () => {
  test("creates a dedicated anthropic provider when none matches", async () => {
    const { stub, client } = clientFor();
    await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes().slice(0, 2)).toEqual([PROVIDER_LIST, PROVIDER_CREATE]);
    const sent = stub.bodyOf(PROVIDER_CREATE) as Record<string, unknown>;
    expect(sent).toEqual({
      provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
      display_name: CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
    });
  });

  test("reuses an existing active row matched by provider_type AND display_name, ignoring a look-alike", async () => {
    const { stub, client } = clientFor({
      [PROVIDER_LIST]: () => ({
        status: 200,
        body: page([
          // Same provider_type, different name — must be ignored, not reused.
          providerRecord({ id: OTHER_PROVIDER_ID, display_name: "Anthropic (API key)" }),
          providerRecord(),
        ]),
      }),
    });
    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes()).not.toContain(PROVIDER_CREATE);
    expect(stub.routes()).not.toContain(PROVIDER_ENABLE);
    expect(result.providerId).toBe(PROVIDER_ID);
  });

  test("ignores a deleted row with the matching name and creates a fresh one", async () => {
    const { stub, client } = clientFor({
      [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord({ status: "deleted" })]) }),
    });
    await connectClaudeSubscription(client, { accessToken: TOKEN });
    expect(stub.routes()).toContain(PROVIDER_CREATE);
  });

  test("repairs (re-enables) a matched provider that is disabled, before touching its credential", async () => {
    const { stub, client } = clientFor({
      [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord({ status: "disabled" })]) }),
    });
    await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes().slice(0, 3)).toEqual([PROVIDER_LIST, PROVIDER_ENABLE, CREDENTIAL_LIST]);
    expect(stub.routes()).not.toContain(PROVIDER_CREATE);
  });

  test("a truncated provider list with no match refuses rather than guesses", async () => {
    const { stub, client } = clientFor({
      [PROVIDER_LIST]: () => ({ status: 200, body: page([], true) }),
    });
    let caught: unknown;
    try {
      await connectClaudeSubscription(client, { accessToken: TOKEN });
    } catch (error) {
      caught = error;
    }
    expect(isClaudeSubscriptionError(caught)).toBe(true);
    expect((caught as { messageKey: string }).messageKey).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_list_truncated,
    );
    // Nothing was written.
    expect(stub.routes()).toEqual([PROVIDER_LIST]);
  });
});

describe("connectClaudeSubscription — the oauth2 credential", () => {
  test("creates the credential with a secret shaped as EXACTLY { access_token }", async () => {
    const { stub, client } = clientFor();
    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    const sent = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    expect(sent["provider_id"]).toBe(PROVIDER_ID);
    expect(sent["credential_type"]).toBe("oauth2");
    expect(sent["scope"]).toEqual({ type: "global" });
    expect(sent["display_name"]).toBe(CLAUDE_SUBSCRIPTION_CREDENTIAL_DISPLAY_NAME);
    const secret = sent["secret"] as Record<string, unknown>;
    expect(Object.keys(secret)).toEqual(["access_token"]);
    expect(secret["access_token"]).toBe(TOKEN);
    expect(result.outcome).toBe("created");
  });

  test("ignores an api_key credential on the same provider and creates an oauth2 one alongside it", async () => {
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({
        status: 200,
        body: page([credentialRecord({ id: "cccccccc-0000-4000-8000-000000000000", credential_type: "api_key" })]),
      }),
    });
    await connectClaudeSubscription(client, { accessToken: TOKEN });
    expect(stub.routes()).toContain(CREDENTIAL_CREATE);
    expect(stub.routes()).not.toContain(CREDENTIAL_ROTATE);
  });

  test("never rotates another provider's oauth2 row — the server's provider_id filter is inert", async () => {
    // The only oauth2 row on the page belongs to OTHER_PROVIDER_ID. Rotating it
    // would overwrite that credential's sealed secret with the Claude
    // subscription token, in place and unrecoverably, and leave the dedicated
    // provider with none. The chain must create instead.
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([foreignOauth2Record()]) }),
    });
    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes()).not.toContain(FOREIGN_ROTATE);
    expect(stub.routes()).not.toContain(FOREIGN_ENABLE);
    expect(stub.routes()).toContain(CREDENTIAL_CREATE);
    expect((stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>)["provider_id"]).toBe(PROVIDER_ID);
    expect(result).toEqual({
      providerId: PROVIDER_ID,
      credentialId: CREDENTIAL_ID,
      outcome: "created",
    });
    // The filter is still sent, so the console becomes correct for free if
    // Moira ever honours it — but nothing above depends on that.
    const listed = stub.requestsFor(CREDENTIAL_LIST)[0];
    expect(new URL(listed!.url).searchParams.get("provider_id")).toBe(PROVIDER_ID);
  });

  test("a foreign oauth2 row does not shadow this provider's own row", async () => {
    // Ordering matters: the foreign row is FIRST on the page, so a predicate
    // that matches on credential_type alone picks it and rotates the wrong row.
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([foreignOauth2Record(), credentialRecord()]) }),
    });
    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes()).toContain(CREDENTIAL_ROTATE);
    expect(stub.routes()).not.toContain(FOREIGN_ROTATE);
    expect(result.credentialId).toBe(CREDENTIAL_ID);
  });

  test("rotates an existing oauth2 credential in place rather than creating a second row", async () => {
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
    });
    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes()).not.toContain(CREDENTIAL_CREATE);
    expect(stub.routes()).toContain(CREDENTIAL_ROTATE);
    const sent = stub.bodyOf(CREDENTIAL_ROTATE) as Record<string, unknown>;
    expect(sent).toEqual({ secret: { access_token: TOKEN } });
    expect(result.outcome).toBe("rotated");
    expect(result.credentialId).toBe(CREDENTIAL_ID);
  });

  test("re-enables a rotated credential that came back disabled", async () => {
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
      [CREDENTIAL_ROTATE]: () => ({ status: 200, body: credentialRecord({ status: "disabled", version: 2 }) }),
    });
    await connectClaudeSubscription(client, { accessToken: TOKEN });
    expect(stub.routes()).toContain(CREDENTIAL_ENABLE);
  });

  test("a truncated credential list with no match refuses without creating a second row", async () => {
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([], true) }),
    });
    let caught: unknown;
    try {
      await connectClaudeSubscription(client, { accessToken: TOKEN });
    } catch (error) {
      caught = error;
    }
    expect(isClaudeSubscriptionError(caught)).toBe(true);
    expect(stub.routes()).not.toContain(CREDENTIAL_CREATE);
    expect(stub.routes()).not.toContain(CREDENTIAL_ROTATE);
  });

  test("the token reaches Moira and nothing else — the result carries no secret", async () => {
    const { stub, client } = clientFor();
    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(JSON.stringify(stub.bodyOf(CREDENTIAL_CREATE))).toContain(TOKEN);
    expect(JSON.stringify(result)).not.toContain(TOKEN);
    expect(Object.keys(result).sort()).toEqual(["credentialId", "outcome", "providerId"]);
  });
});

/* -------------------------------------------------------------------------- */
/* resolveSubscriptionToken                                                   */
/* -------------------------------------------------------------------------- */

describe("resolveSubscriptionToken", () => {
  test("refuses an empty or whitespace-only value", () => {
    expect(resolveSubscriptionToken("")).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_required,
    });
    expect(resolveSubscriptionToken("   ")).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_required,
    });
  });

  test("refuses a non-string value", () => {
    expect(resolveSubscriptionToken(undefined)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_required,
    });
    expect(resolveSubscriptionToken(42)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_required,
    });
  });

  test("refuses a value longer than the bound", () => {
    const tooLong = "a".repeat(MAX_SUBSCRIPTION_TOKEN_LENGTH + 1);
    expect(resolveSubscriptionToken(tooLong)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_too_long,
    });
  });

  test("accepts exactly the bound", () => {
    const exact = "a".repeat(MAX_SUBSCRIPTION_TOKEN_LENGTH);
    expect(resolveSubscriptionToken(exact)).toEqual({ ok: true, token: exact });
  });

  test("refuses a value carrying a control character, most likely a stray newline", () => {
    expect(resolveSubscriptionToken(`${TOKEN}\nextra-line`)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_invalid,
    });
    expect(resolveSubscriptionToken(`tab${String.fromCharCode(9)}here`)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_invalid,
    });
  });

  test("trims leading and trailing whitespace on an otherwise valid token", () => {
    expect(resolveSubscriptionToken(`  ${TOKEN}  `)).toEqual({ ok: true, token: TOKEN });
  });
});
