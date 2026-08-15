// `GET /api/settings/llm/claude-subscription/acquire/status?job=…` — Mode A,
// phase 2 (issue #269). Same shape as PR #263's own route tests: the session
// gate is real, only the environment/runtime/Moira transport are substituted
// (`setConsoleApiDependenciesForTests`), and the `claude` CLI itself is
// substituted through `setClaudeCliSpawnerForTests` — never a real process.
// Per the owner-approved testing policy
// (`plans/12-feature-expansion-brainstorm.md` §1), no test in this repository
// may spawn the real `claude` CLI.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as ACQUIRE_START_POST } from "@/app/api/settings/llm/claude-subscription/acquire/start/route";
import { GET as ACQUIRE_STATUS_GET } from "@/app/api/settings/llm/claude-subscription/acquire/status/route";
import {
  resetClaudeCliJobRegistryForTests,
  setClaudeCliPtyAvailableForTests,
  setClaudeCliSpawnerForTests,
  startClaudeCliJob,
} from "@/lib/claude-cli";
import {
  CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
} from "@/lib/claude-subscription";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleAuth } from "@/lib/auth";
import type { ConsoleRuntime } from "@/lib/auth-runtime";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv, type EnvSource } from "@/lib/env";
import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import { fakeClaudeChild, type FakeClaudeChild } from "../../support/fake-claude-cli";
import { createMoiraStub, MOIRA_STUB_BASE_URL, type MoiraStub, type StubHandler } from "../../support/moira-stub";

/* -------------------------------------------------------------------------- */
/* Fixtures — same shapes PR #263's route test used                          */
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
      new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: "sk_test_stub", fetch: stub.fetch }),
  });
  // `ptyIsAvailable()` is fixed `false` in shipped code (see
  // `lib/claude-cli.ts`'s header); `startJob` below goes through the real
  // `POST .../acquire/start` handler, which refuses before spawning unless
  // this is overridden — every test in this file is about what happens
  // AFTER a job exists, so it needs one to actually start.
  setClaudeCliPtyAvailableForTests(true);
}

function startRequest(): Request {
  return new Request("https://console.example.com/api/settings/llm/claude-subscription/acquire/start", {
    method: "POST",
  });
}

function statusRequest(jobId: string | null): Request {
  const url = new URL("https://console.example.com/api/settings/llm/claude-subscription/acquire/status");
  if (jobId !== null) url.searchParams.set("job", jobId);
  return new Request(url, { method: "GET" });
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

function errorOf(body: Record<string, unknown>): Record<string, unknown> {
  return body["error"] as Record<string, unknown>;
}

/** Starts a job on the fake spawner and returns its id, ready for the test to drive to completion. */
async function startJob(child: FakeClaudeChild): Promise<string> {
  setClaudeCliSpawnerForTests(child.spawner);
  const started = await json(await ACQUIRE_START_POST(startRequest()));
  return started["job_id"] as string;
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setConsoleApiDependenciesForTests(null);
  setClaudeCliSpawnerForTests(null);
  setClaudeCliPtyAvailableForTests(null);
  resetClaudeCliJobRegistryForTests();
});

/* -------------------------------------------------------------------------- */
/* The opt-in gate                                                            */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — the opt-in gate", () => {
  test("refuses with a keyed 403 when the deployment has not enabled it", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    install({ env: DISABLED_ENV });

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled,
    );
  });
});

/* -------------------------------------------------------------------------- */
/* The session gate                                                          */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — the session gate", () => {
  test("no session is a 401", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    install({ session: null });

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(401);
  });
});

/* -------------------------------------------------------------------------- */
/* Bad or unknown job ids                                                    */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — bad or unknown job ids", () => {
  test("a missing `job` query parameter is a keyed 400", async () => {
    const response = await ACQUIRE_STATUS_GET(statusRequest(null));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_invalid_job,
    );
  });

  test("an unknown job id is a keyed 404, not a 500", async () => {
    const response = await ACQUIRE_STATUS_GET(statusRequest("00000000-0000-4000-8000-000000000000"));
    expect(response.status).toBe(404);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_job_not_found,
    );
  });
});

/* -------------------------------------------------------------------------- */
/* Still running                                                             */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — still running", () => {
  test("reports running with a null URL when the child has not printed anything yet", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(200);
    expect(await json(response)).toEqual({ status: "running", authorization_url: null });
    expect(stub.routes()).toEqual([]);
  });

  test("surfaces a URL scraped from captured output, and never the raw output around it", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStderr("Use the url below to sign in (c to copy)\nhttps://example.com/authorize?x=1 ");

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    const body = await json(response);
    expect(body["status"]).toBe("running");
    expect(body["authorization_url"]).toBe("https://example.com/authorize?x=1");
  });
});

/* -------------------------------------------------------------------------- */
/* Success: storage happens on the first poll that observes it, then caches  */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — success", () => {
  test("stores through the same chain the paste endpoint uses, and the response carries no secret", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStdout(MINTED_TOKEN);
    child.emitExit(0);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(200);

    const sent = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    expect((sent["secret"] as Record<string, unknown>)["access_token"]).toBe(MINTED_TOKEN);

    const body = await json(response);
    const bodyText = JSON.stringify(body);
    expect(bodyText).not.toContain(MINTED_TOKEN);
    expect(body).toEqual({
      status: "succeeded",
      provider_id: PROVIDER_ID,
      credential_id: CREDENTIAL_ID,
      outcome: "created",
    });
  });

  test("a repeated poll after success answers from the cache — never stores twice", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStdout(MINTED_TOKEN);
    child.emitExit(0);

    await ACQUIRE_STATUS_GET(statusRequest(jobId));
    const second = await ACQUIRE_STATUS_GET(statusRequest(jobId));

    expect(await json(second)).toEqual({
      status: "succeeded",
      provider_id: PROVIDER_ID,
      credential_id: CREDENTIAL_ID,
      outcome: "created",
    });
    expect(stub.requestsFor(CREDENTIAL_CREATE).length).toBe(1);
  });

  test("a second acquisition rotates the existing row rather than duplicating it", async () => {
    install({ handlers: handlers({ [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }) }) });
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStdout(MINTED_TOKEN);
    child.emitExit(0);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect((await json(response))["outcome"]).toBe("rotated");
  });
});

/* -------------------------------------------------------------------------- */
/* Every process-level failure reason maps to a keyed, non-raw response       */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — CLI failures are keyed, never a raw stderr dump", () => {
  test("binary missing", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitSpawnError("ENOENT");

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_binary_missing,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("not signed in", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStderr("you are not currently logged in — highly sensitive internal detail");
    child.emitExit(1);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_not_signed_in);
    expect(JSON.stringify(body)).not.toContain("highly sensitive internal detail");
  });

  test("timeout — the mandated, actionable wording (issue #269) is reachable end to end", async () => {
    // The route always starts a job with the real multi-minute budget, so
    // this drives the registry directly with a tiny one (`claude-cli.test.ts`
    // owns the timer/kill mechanics themselves) and polls it through the
    // SAME status route a browser would — proving the route maps a real
    // timeout onto the mandated wording, not just that the wording exists.
    const child = fakeClaudeChild();
    setClaudeCliSpawnerForTests(child.spawner);
    const { jobId } = startClaudeCliJob({ budgetMs: 10, killGraceMs: 10 });
    await new Promise((resolve) => setTimeout(resolve, 40));

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_timeout);
    // The mandated content (issue #269): names the actionable remedy, not a
    // bare "didn't finish in time". Regression guard on the English default.
    const message = CONSOLE_CATALOG[CONSOLE_MESSAGE_KEYS.claude_subscription_cli_timeout].message;
    expect(message).toContain("setup-token");
    expect(message.toLowerCase()).toContain("paste");
  });

  test("output too large", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStdout("x".repeat(70 * 1024));

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_output_too_large,
    );
  });

  test("a generic non-zero exit", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStderr("boom, internal detail");
    child.emitExit(1);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_failed);
    expect(JSON.stringify(body)).not.toContain("boom, internal detail");
  });

  test("invalid output (exit zero, nothing usable on stdout)", async () => {
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStdout("   \n");
    child.emitExit(0);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_cli_invalid_output,
    );
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Storage refusals — a Moira-side failure after the CLI already succeeded    */
/* -------------------------------------------------------------------------- */

describe("GET .../acquire/status — storage refusals", () => {
  test("a truncated provider list is a keyed failure, and the token still never appears", async () => {
    install({
      handlers: handlers({
        [PROVIDER_LIST]: () => ({ status: 200, body: page([], true) }),
      }),
    });
    const child = fakeClaudeChild();
    const jobId = await startJob(child);
    child.emitStdout(MINTED_TOKEN);
    child.emitExit(0);

    const response = await ACQUIRE_STATUS_GET(statusRequest(jobId));
    expect(response.status).toBe(409);
    const body = await json(response);
    expect(JSON.stringify(body)).not.toContain(MINTED_TOKEN);
    expect(errorOf(body)["code"]).toBe("claude_subscription_failed");
  });
});
