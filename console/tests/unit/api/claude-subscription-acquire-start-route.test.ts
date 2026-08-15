// `POST /api/settings/llm/claude-subscription/acquire/start` — Mode A, phase 1
// (issue #269). Same shape as PR #263's own route tests: the session gate is
// real, only the environment/runtime/Moira transport are substituted
// (`setConsoleApiDependenciesForTests`), and the `claude` CLI itself is
// substituted through `setClaudeCliSpawnerForTests` — never a real process.
// Per the owner-approved testing policy
// (`plans/12-feature-expansion-brainstorm.md` §1), no test in this repository
// may spawn the real `claude` CLI.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as ACQUIRE_START_POST } from "@/app/api/settings/llm/claude-subscription/acquire/start/route";
import { resetClaudeCliJobRegistryForTests, setClaudeCliSpawnerForTests } from "@/lib/claude-cli";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleAuth } from "@/lib/auth";
import type { ConsoleRuntime } from "@/lib/auth-runtime";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "@/lib/env";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import { fakeClaudeChild } from "../../support/fake-claude-cli";
import { createMoiraStub, MOIRA_STUB_BASE_URL, type MoiraStub, type StubHandler } from "../../support/moira-stub";

const PROVIDER_CONFIG_ID = "moira-console-idp";

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
  stub = createMoiraStub(options.handlers ?? {});
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
      new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: "sk_test_stub", fetch: stub.fetch }),
  });
}

function request(): Request {
  return new Request("https://console.example.com/api/settings/llm/claude-subscription/acquire/start", {
    method: "POST",
  });
}

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
  setConsoleApiDependenciesForTests(null);
  setClaudeCliSpawnerForTests(null);
  resetClaudeCliJobRegistryForTests();
});

/* -------------------------------------------------------------------------- */
/* The opt-in gate                                                            */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire/start — the opt-in gate", () => {
  test("refuses with a keyed 403 when the deployment has not enabled it, and never spawns the CLI", async () => {
    install({ env: DISABLED_ENV });
    const child = fakeClaudeChild();
    setClaudeCliSpawnerForTests(child.spawner);

    const response = await ACQUIRE_START_POST(request());
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled,
    );
    expect(child.calls.length).toBe(0);
    expect(stub.routes()).toEqual([]);
  });

  test("no response on this surface is cacheable", async () => {
    setClaudeCliSpawnerForTests(fakeClaudeChild().spawner);
    const response = await ACQUIRE_START_POST(request());
    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

/* -------------------------------------------------------------------------- */
/* The session gate — same as every other route on this surface              */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire/start — the session gate", () => {
  test("no session is a 401, and nothing is spawned", async () => {
    install({ session: null });
    const child = fakeClaudeChild();
    setClaudeCliSpawnerForTests(child.spawner);

    const response = await ACQUIRE_START_POST(request());
    expect(response.status).toBe(401);
    expect(child.calls.length).toBe(0);
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Starting a job                                                             */
/* -------------------------------------------------------------------------- */

describe("POST .../acquire/start — starting a job", () => {
  test("spawns the fixed command, returns immediately with a job id and a null URL, and touches Moira not at all", async () => {
    const child = fakeClaudeChild();
    setClaudeCliSpawnerForTests(child.spawner);

    const response = await ACQUIRE_START_POST(request());
    expect(response.status).toBe(200);
    const body = await json(response);
    expect(typeof body["job_id"]).toBe("string");
    expect((body["job_id"] as string).length).toBeGreaterThan(0);
    expect(body["authorization_url"]).toBeNull();

    expect(child.calls.length).toBe(1);
    expect(child.calls[0]?.args).toEqual(["setup-token"]);
    // The child has not exited; nothing has been stored anywhere yet.
    expect(stub.routes()).toEqual([]);
  });

  test("two starts spawn two independent jobs with distinct ids", async () => {
    // A fresh fake child per spawn — this test only cares that the two jobs
    // the registry hands back are distinct, not about either child's output.
    setClaudeCliSpawnerForTests((file, args) => fakeClaudeChild().spawner(file, args));
    const first = await json(await ACQUIRE_START_POST(request()));
    const second = await json(await ACQUIRE_START_POST(request()));
    expect(first["job_id"]).not.toBe(second["job_id"]);
  });
});
