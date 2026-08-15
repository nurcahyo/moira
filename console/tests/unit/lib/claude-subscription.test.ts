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
  CLAUDE_API_KEY_CREDENTIAL_DISPLAY_NAME,
  CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_CREDENTIAL_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
  connectClaudeApiKey,
  connectClaudeSubscription,
  isClaudeSubscriptionError,
  loadClaudeApiKeyStatus,
  loadClaudeSubscriptionStatus,
  MAX_API_KEY_LENGTH,
  MAX_SUBSCRIPTION_TOKEN_LENGTH,
  resolveAnthropicApiKey,
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

/** Unmistakable, and asserted absent from every returned/serialised shape. */
const TOKEN = "sk-ant-oat01-unmistakable-subscription-token-4f9c2b";

const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const PROVIDER_ENABLE = `POST /api/v1/admin/providers/${PROVIDER_ID}/enable`;
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const CREDENTIAL_ROTATE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/rotate`;
const CREDENTIAL_ENABLE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/enable`;

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

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [PROVIDER_LIST]: () => ({ status: 200, body: page([]) }),
    [PROVIDER_CREATE]: () => ({ status: 201, body: providerRecord() }),
    [PROVIDER_ENABLE]: () => ({ status: 200, body: providerRecord({ status: "active", version: 2 }) }),
    [CREDENTIAL_LIST]: () => ({ status: 200, body: page([]) }),
    [CREDENTIAL_CREATE]: () => ({ status: 201, body: credentialRecord() }),
    [CREDENTIAL_ROTATE]: () => ({ status: 200, body: credentialRecord({ version: 2 }) }),
    [CREDENTIAL_ENABLE]: () => ({ status: 200, body: credentialRecord({ status: "active", version: 3 }) }),
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

  test("never rotates an oauth2 credential belonging to a DIFFERENT provider", async () => {
    // Regression for the plan-12 review finding. `listProviderCredentials` is called with
    // `providerId`, but Moira's handler ignores every `PageQuery` filter and answers 200 with an
    // unfiltered page (`src/domain/admin.rs:40-50`, pinned by a test). Until this predicate
    // existed, the match was `credential_type === "oauth2"` alone, so the page's first oauth2 row
    // — from any provider in the deployment — was rotated: its sealed secret overwritten with the
    // Claude token, and the intended provider left with none.
    //
    // The stub is what hid this. It keys on the bare path and ignores the query, and every row
    // the fixtures built already carried the right `provider_id`, so the bug was structurally
    // untestable. This row deliberately carries a foreign one.
    const FOREIGN_PROVIDER_ID = "11111111-1111-4111-8111-111111111111";
    const FOREIGN_CREDENTIAL_ID = "22222222-2222-4222-8222-222222222222";
    const { stub, client } = clientFor({
      [CREDENTIAL_LIST]: () => ({
        status: 200,
        body: page([
          credentialRecord({ id: FOREIGN_CREDENTIAL_ID, provider_id: FOREIGN_PROVIDER_ID }),
        ]),
      }),
    });

    const result = await connectClaudeSubscription(client, { accessToken: TOKEN });

    expect(stub.routes()).not.toContain(CREDENTIAL_ROTATE);
    expect(stub.routes()).toContain(CREDENTIAL_CREATE);
    expect(result.outcome).toBe("created");
    expect(result.credentialId).not.toBe(FOREIGN_CREDENTIAL_ID);
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

/* -------------------------------------------------------------------------- */
/* Mode B: connectClaudeApiKey — a DIFFERENT dedicated provider row           */
/* -------------------------------------------------------------------------- */

const API_KEY_PROVIDER_ID = "aaaaaaaa-3333-4333-8333-333333333333";
const API_KEY_CREDENTIAL_ID = "bbbbbbbb-3333-4333-8333-333333333333";
const ANTHROPIC_API_KEY = "sk-ant-unmistakable-console-api-key-9f1a2b";

const API_KEY_PROVIDER_LIST = "GET /api/v1/admin/providers";
const API_KEY_PROVIDER_CREATE = "POST /api/v1/admin/providers";
const API_KEY_CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const API_KEY_CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const API_KEY_CREDENTIAL_ROTATE = `POST /api/v1/admin/provider-credentials/${API_KEY_CREDENTIAL_ID}/rotate`;

function apiKeyProviderRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: API_KEY_PROVIDER_ID,
    provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
    display_name: CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME,
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function apiKeyCredentialRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: API_KEY_CREDENTIAL_ID,
    provider_id: API_KEY_PROVIDER_ID,
    credential_type: "api_key",
    scope: { type: "global" },
    secret_fingerprint: "sha256:another-fingerprint-that-must-not-cross",
    masked_secret: "api_key****mask",
    status: "active",
    priority: 0,
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function apiKeyHandlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [API_KEY_PROVIDER_LIST]: () => ({ status: 200, body: page([]) }),
    [API_KEY_PROVIDER_CREATE]: () => ({ status: 201, body: apiKeyProviderRecord() }),
    [API_KEY_CREDENTIAL_LIST]: () => ({ status: 200, body: page([]) }),
    [API_KEY_CREDENTIAL_CREATE]: () => ({ status: 201, body: apiKeyCredentialRecord() }),
    [API_KEY_CREDENTIAL_ROTATE]: () => ({ status: 200, body: apiKeyCredentialRecord({ version: 2 }) }),
    ...overrides,
  };
}

function apiKeyClientFor(overrides: Record<string, StubHandler> = {}) {
  const stub = createMoiraStub(apiKeyHandlers(overrides));
  const client = new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    systemKey: "sk_test_stub",
    fetch: stub.fetch,
  });
  return { stub, client };
}

describe("connectClaudeApiKey — a DIFFERENT dedicated provider row from the subscription one", () => {
  test("creates a dedicated anthropic provider named for the API key, not the subscription", async () => {
    const { stub, client } = apiKeyClientFor();
    await connectClaudeApiKey(client, { apiKey: ANTHROPIC_API_KEY });

    expect(stub.routes().slice(0, 2)).toEqual([API_KEY_PROVIDER_LIST, API_KEY_PROVIDER_CREATE]);
    const sent = stub.bodyOf(API_KEY_PROVIDER_CREATE) as Record<string, unknown>;
    expect(sent).toEqual({
      provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
      display_name: CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME,
    });
    // Never the subscription row's own display name.
    expect(sent["display_name"]).not.toBe(CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME);
  });

  test("creates the credential with a secret shaped as EXACTLY { api_key }", async () => {
    const { stub, client } = apiKeyClientFor();
    const result = await connectClaudeApiKey(client, { apiKey: ANTHROPIC_API_KEY });

    const sent = stub.bodyOf(API_KEY_CREDENTIAL_CREATE) as Record<string, unknown>;
    expect(sent["provider_id"]).toBe(API_KEY_PROVIDER_ID);
    expect(sent["credential_type"]).toBe("api_key");
    expect(sent["display_name"]).toBe(CLAUDE_API_KEY_CREDENTIAL_DISPLAY_NAME);
    const secret = sent["secret"] as Record<string, unknown>;
    expect(Object.keys(secret)).toEqual(["api_key"]);
    expect(secret["api_key"]).toBe(ANTHROPIC_API_KEY);
    expect(result.outcome).toBe("created");
  });

  test("rotates an existing api_key credential in place rather than creating a second row", async () => {
    const { stub, client } = apiKeyClientFor({
      [API_KEY_CREDENTIAL_LIST]: () => ({ status: 200, body: page([apiKeyCredentialRecord()]) }),
    });
    const result = await connectClaudeApiKey(client, { apiKey: ANTHROPIC_API_KEY });

    expect(stub.routes()).not.toContain(API_KEY_CREDENTIAL_CREATE);
    expect(stub.routes()).toContain(API_KEY_CREDENTIAL_ROTATE);
    expect(result.outcome).toBe("rotated");
  });

  test("the key reaches Moira and nothing else — the result carries no secret", async () => {
    const { stub, client } = apiKeyClientFor();
    const result = await connectClaudeApiKey(client, { apiKey: ANTHROPIC_API_KEY });

    expect(JSON.stringify(stub.bodyOf(API_KEY_CREDENTIAL_CREATE))).toContain(ANTHROPIC_API_KEY);
    expect(JSON.stringify(result)).not.toContain(ANTHROPIC_API_KEY);
    expect(Object.keys(result).sort()).toEqual(["credentialId", "outcome", "providerId"]);
  });
});

/* -------------------------------------------------------------------------- */
/* resolveAnthropicApiKey                                                     */
/* -------------------------------------------------------------------------- */

describe("resolveAnthropicApiKey", () => {
  test("refuses an empty or whitespace-only value", () => {
    expect(resolveAnthropicApiKey("")).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_required,
    });
    expect(resolveAnthropicApiKey("   ")).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_required,
    });
  });

  test("refuses a non-string value", () => {
    expect(resolveAnthropicApiKey(undefined)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_required,
    });
  });

  test("refuses a value longer than the bound", () => {
    const tooLong = `sk-ant-${"a".repeat(MAX_API_KEY_LENGTH)}`;
    expect(resolveAnthropicApiKey(tooLong)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_too_long,
    });
  });

  test("refuses a value carrying a control character", () => {
    expect(resolveAnthropicApiKey(`${ANTHROPIC_API_KEY}\nextra-line`)).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_invalid,
    });
  });

  test("refuses a value that does not start with sk-ant-", () => {
    expect(resolveAnthropicApiKey("sk-proj-this-is-an-openai-key")).toEqual({
      ok: false,
      messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_wrong_shape,
    });
  });

  test("accepts a well-formed key, trimmed", () => {
    expect(resolveAnthropicApiKey(`  ${ANTHROPIC_API_KEY}  `)).toEqual({
      ok: true,
      apiKey: ANTHROPIC_API_KEY,
    });
  });
});

/* -------------------------------------------------------------------------- */
/* Read-only status                                                           */
/* -------------------------------------------------------------------------- */

describe("loadClaudeSubscriptionStatus / loadClaudeApiKeyStatus", () => {
  test("not_connected when no matching provider row exists", async () => {
    const { client } = clientFor();
    expect(await loadClaudeSubscriptionStatus(client)).toEqual({
      kind: "not_connected",
      status: null,
      expiresAt: null,
    });
  });

  test("not_connected when the provider exists but carries no matching credential", async () => {
    const { client } = clientFor({ [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }) });
    expect(await loadClaudeSubscriptionStatus(client)).toEqual({
      kind: "not_connected",
      status: null,
      expiresAt: null,
    });
  });

  test("connected, with the credential's status and expiry", async () => {
    const { client } = clientFor({
      [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
      [CREDENTIAL_LIST]: () => ({
        status: 200,
        body: page([credentialRecord({ status: "disabled", expires_at: "2026-12-01T00:00:00Z" })]),
      }),
    });
    expect(await loadClaudeSubscriptionStatus(client)).toEqual({
      kind: "connected",
      status: "disabled",
      expiresAt: "2026-12-01T00:00:00Z",
    });
  });

  test("never carries masked_secret or a fingerprint, even though the underlying record does", async () => {
    const { client } = clientFor({
      [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
    });
    const status = await loadClaudeSubscriptionStatus(client);
    const text = JSON.stringify(status);
    expect(text).not.toContain("mask");
    expect(text).not.toContain("fingerprint");
  });

  test("unknown when the provider list is truncated before a match is confirmed absent", async () => {
    const { client } = clientFor({ [PROVIDER_LIST]: () => ({ status: 200, body: page([], true) }) });
    expect(await loadClaudeSubscriptionStatus(client)).toEqual({
      kind: "unknown",
      status: null,
      expiresAt: null,
    });
  });

  test("unknown when the credential list is truncated before a match is confirmed absent", async () => {
    const { client } = clientFor({
      [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }),
      [CREDENTIAL_LIST]: () => ({ status: 200, body: page([], true) }),
    });
    expect(await loadClaudeSubscriptionStatus(client)).toEqual({
      kind: "unknown",
      status: null,
      expiresAt: null,
    });
  });

  test("loadClaudeApiKeyStatus reads the DIFFERENT dedicated row, independently", async () => {
    const { client } = apiKeyClientFor({
      [API_KEY_PROVIDER_LIST]: () => ({ status: 200, body: page([apiKeyProviderRecord()]) }),
      [API_KEY_CREDENTIAL_LIST]: () => ({ status: 200, body: page([apiKeyCredentialRecord()]) }),
    });
    expect(await loadClaudeApiKeyStatus(client)).toEqual({
      kind: "connected",
      status: "active",
      expiresAt: null,
    });
  });
});
