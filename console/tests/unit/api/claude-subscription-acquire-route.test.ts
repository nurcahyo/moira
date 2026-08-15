// `POST /api/settings/llm/claude-subscription/acquire` — Mode A, driven as a
// route handler. Same shape as `claude-subscription-route.test.ts`: the
// session gate is real, only the environment/runtime/Moira transport are
// substituted (`setConsoleApiDependenciesForTests`), and the `claude` CLI
// itself is substituted through `setClaudeCliExecutorForTests` — never a real
// process. Per the owner-approved testing policy
// (`plans/12-feature-expansion-brainstorm.md` §1), no test in this repository
// may spawn the real `claude` CLI.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as ACQUIRE_POST } from "@/app/api/settings/llm/claude-subscription/acquire/route";
import { setClaudeCliExecutorForTests, type ExecOutcome } from "@/lib/claude-cli";
import {
  CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
} from "@/lib/claude-subscription";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleAuth } from "@/lib/auth";
import type { ConsoleRuntime } from "@/lib/auth-runtime";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "@/lib/env";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import {
  createMoiraStub,
  MOIRA_STUB_BASE_URL,
  type MoiraStub,
  type StubHandler,
} from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const PROVIDER_ID = "aaaaaaaa-1111-4111-8111-111111111111";
const CREDENTIAL_ID = "bbbbbbbb-1111-4111-8111-111111111111";
const PROVIDER_CONFIG_ID = "moira-console-idp";

/** Unmistakable, and asserted absent from every response body. */
const MINTED_TOKEN = "sk-ant-oat01-minted-by-the-cli-4f9c2b";

const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const CREDENTIAL_ROTATE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/rotate`;

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
    [CREDENTIAL_LIST]: () => ({ status: 200, body: page([]) }),
    [CREDENTIAL_CREATE]: () => ({ status: 201, body: credentialRecord() }),
    [CREDENTIAL_ROTATE]: () => ({ status: 200, body: credentialRecord({ version: 2 }) }),
    ...overrides,
  };
}

const ADMITTED = {
  session: { id: "session-1", providerId: PROVIDER_CONFIG_ID },
  user: { id: "user-1", email: "ops@example.com", emailVerified: true },
};

const CONFIG = {
  providerId: PROVIDER_CONFIG_ID,
  allowedEmailDomains: ["example.com"],
} as unknown as ResolvedAuthConfig;

function authWith(session: unknown): ConsoleAuth {
  return {
    api: { getSession: async () => session },
    $context: Promise.resolve({
      internalAdapter: {
        findAccountByUserId: async () => [
          { providerId: PROVIDER_CONFIG_ID, accountId: "idp-subject-abc" },
        ],
      },
    }),
  } as unknown as ConsoleAuth;
}

const BASE_ENV_SOURCE: EnvSource = {
  NODE_ENV: "test",
  MOIRA_API_URL: MOIRA_STUB_BASE_URL,
  CONSOLE_PUBLIC_ORIGIN: "https://console.example.com",
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin-audience",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
};

const ENABLED_ENV: ConsoleEnv = readConsoleEnv({
  ...BASE_ENV_SOURCE,
  CONSOLE_ALLOW_LOCAL_CLI_CREDENTIALS: "true",
});
const DISABLED_ENV: ConsoleEnv = readConsoleEnv(BASE_ENV_SOURCE);

let stub: MoiraStub;

function install(
  options: {
    readonly handlers?: Record<string, StubHandler>;
    readonly session?: unknown;
    readonly runtime?: ConsoleRuntime;
    readonly env?: ConsoleEnv;
  } = {},
): void {
  stub = createMoiraStub(options.handlers ?? handlers());
  const auth = authWith("session" in options ? options.session : ADMITTED);
  const runtime: ConsoleRuntime = options.runtime ?? {
    ok: true,
    auth,
    configs: [CONFIG],
    problems: [],
    stale: false,
  };
  setConsoleApiDependenciesForTests({
    runtime: async () => runtime,
    env: () => options.env ?? ENABLED_ENV,
    clientFor: () =>
      new MoiraClient({
        baseUrl: MOIRA_STUB_BASE_URL,
        systemKey: "sk_test_stub",
        fetch: stub.fetch,
      }),
  });
}

function request(): Request {
  return new Request("https://console.example.com/api/settings/llm/claude-subscription/acquire", {
    method: "POST",
  });
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

function errorOf(body: Record<string, unknown>): Record<string, unknown> {
  return body["error"] as Record<string, unknown>;
}

function scriptedExecutor(outcome: ExecOutcome) {
  return async () => outcome;
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setConsoleApiDependenciesForTests(null);
  setClaudeCliExecutorForTests(null);
});

/* -------------------------------------------------------------------------- */
/* The opt-in gate                                                            */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire — the opt-in gate", () => {
  test("refuses with a keyed 403 when the deployment has not enabled it, and never runs the CLI", async () => {
    install({ env: DISABLED_ENV });
    let executorCalled = false;
    setClaudeCliExecutorForTests(async () => {
      executorCalled = true;
      return { kind: "success", stdout: MINTED_TOKEN, stderr: "" };
    });

    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled,
    );
    expect(executorCalled).toBe(false);
    expect(stub.routes()).toEqual([]);
  });

  test("no response on this surface is cacheable", async () => {
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "success", stdout: MINTED_TOKEN, stderr: "" }));
    const response = await ACQUIRE_POST(request());
    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

/* -------------------------------------------------------------------------- */
/* The session gate — same as every other route on this surface              */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire — the session gate", () => {
  test("no session is a 401, and nothing runs", async () => {
    install({ session: null });
    let executorCalled = false;
    setClaudeCliExecutorForTests(async () => {
      executorCalled = true;
      return { kind: "success", stdout: MINTED_TOKEN, stderr: "" };
    });

    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(401);
    expect(executorCalled).toBe(false);
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Success                                                                    */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire — success", () => {
  test("mints, stores, and the response carries no secret", async () => {
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "success", stdout: MINTED_TOKEN, stderr: "" }));

    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(200);

    const sent = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    expect((sent["secret"] as Record<string, unknown>)["access_token"]).toBe(MINTED_TOKEN);

    const body = await json(response);
    const bodyText = JSON.stringify(body);
    expect(bodyText).not.toContain(MINTED_TOKEN);
    expect(body).toEqual({ provider_id: PROVIDER_ID, credential_id: CREDENTIAL_ID, outcome: "created" });
  });

  test("a second acquisition rotates the existing row rather than duplicating it", async () => {
    install({ handlers: handlers({ [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }) }) });
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "success", stdout: MINTED_TOKEN, stderr: "" }));

    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(200);
    expect((await json(response))["outcome"]).toBe("rotated");
  });
});

/* -------------------------------------------------------------------------- */
/* Every CLI failure reason maps to a keyed, non-raw response                 */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire — CLI failures are keyed, never a raw stderr dump", () => {
  test("binary missing", async () => {
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "spawn_error", code: "ENOENT" }));
    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_binary_missing);
    expect(stub.routes()).toEqual([]);
  });

  test("not signed in", async () => {
    setClaudeCliExecutorForTests(
      scriptedExecutor({
        kind: "exit_error",
        exitCode: 1,
        stdout: "",
        stderr: "you are not currently logged in — highly sensitive internal detail",
      }),
    );
    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_not_signed_in);
    // The raw stderr text must never appear in the client-visible response.
    expect(JSON.stringify(body)).not.toContain("highly sensitive internal detail");
  });

  test("timeout", async () => {
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "timeout" }));
    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_timeout,
    );
  });

  test("output too large", async () => {
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "max_buffer_exceeded" }));
    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_output_too_large,
    );
  });

  test("a generic non-zero exit", async () => {
    setClaudeCliExecutorForTests(
      scriptedExecutor({ kind: "exit_error", exitCode: 1, stdout: "", stderr: "boom, internal detail" }),
    );
    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_failed);
    expect(JSON.stringify(body)).not.toContain("boom, internal detail");
  });

  test("invalid output (exit zero, nothing usable on stdout)", async () => {
    setClaudeCliExecutorForTests(scriptedExecutor({ kind: "success", stdout: "   \n", stderr: "" }));
    const response = await ACQUIRE_POST(request());
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_invalid_output,
    );
    expect(stub.routes()).toEqual([]);
  });
});
