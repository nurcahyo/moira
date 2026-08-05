// The contract between the AUTHENTICATED e2e stack and the specs that drive it.
//
// ============================================================================
// WHY THERE IS A SECOND STACK AT ALL
// ============================================================================
//
// The scaffold's e2e environment (`console-env.ts`) is deliberately unreachable:
// `MOIRA_API_URL` is `https://moira.invalid`, `CONSOLE_DATABASE_URL` points at
// `db.invalid:1`, and no identity provider runs inside it. That is right for the
// suites it serves — the secret-leak tap and the smoke spec are stronger when
// nothing real is in reach — and it is exactly why `a11y.e2e.ts` and
// `llm-settings.e2e.ts` both record the same REVERSAL CONDITION: "when an
// authenticated Playwright project exists".
//
// This module, `moira-fixture.ts` and `authenticated-stack.ts` are that project's
// harness. They stand up a SECOND console — same standalone artifact, different
// environment — with everything it needs to admit a human:
//
//   * a real OpenID Connect provider on a real TLS socket
//     (`tests/support/mock-idp.ts`, reused verbatim rather than re-implemented);
//   * a stateful Moira on a real TLS socket, which RECORDS every request in
//     order (`moira-fixture.ts`);
//   * a real OpenAI-compatible `/v1/models` endpoint on a real socket, so the
//     "Connect local vLLM" panel discovers from something that answers rather
//     than from a patched `fetch`;
//   * the console's own PostgreSQL database, because Better Auth needs somewhere
//     to keep the session it mints and the ES256 key it signs with.
//
// ============================================================================
// NOTHING HERE MAY REACH `https://local-llm.motrait.com`
// ============================================================================
//
// `LOCAL_VLLM_BASE_URL` is the field's PRE-FILLED DEFAULT and it names a real
// deployment. Every spec in the authenticated project types over it with
// `fixture.vllmBaseUrl` before pressing anything, and `authenticated-stack.ts`
// asserts the two are different so a future edit that made the fixture default
// to the shipped constant fails loudly instead of probing production from CI.
//
// ============================================================================
// TWO FIXED PORTS, AND EVERYTHING ELSE OS-ASSIGNED
// ============================================================================
//
// `playwright.config.ts` has to know the console's origin before the stack
// starts, and a spec has to know where to ask the harness what it recorded. Those
// two are derived from `CONSOLE_E2E_PORT` and are the only fixed ports. The mock
// Moira, the mock IdP and the mock endpoint all bind port 0 and report
// themselves through the control server, so two runs on one machine collide on
// two ports instead of six.

/** Base for every port this stack derives. Same knob the scaffold uses. */
const BASE_PORT = Number(process.env["CONSOLE_E2E_PORT"] ?? 3210);

/** The browser-facing origin: a TLS terminator in front of the standalone server. */
export const AUTH_CONSOLE_PORT = BASE_PORT + 1;

/** Where a spec asks the harness what it recorded, and resets it. Plain http, loopback. */
export const AUTH_CONTROL_PORT = BASE_PORT + 2;

/** The standalone Next server itself. Loopback only; the browser never sees it. */
export const AUTH_CONSOLE_INTERNAL_PORT = BASE_PORT + 3;

/**
 * `localhost` and not `127.0.0.1`: the fixture certificate is issued for
 * `CN=localhost` and the console's cookies are keyed to whatever this says.
 */
export const AUTH_CONSOLE_ORIGIN = `https://localhost:${AUTH_CONSOLE_PORT}`;

export const AUTH_CONTROL_ORIGIN = `http://127.0.0.1:${AUTH_CONTROL_PORT}`;

/** Where the authenticated project's signed-in storage state is written. */
export const AUTH_STORAGE_STATE = "test-results/authenticated-state.json";

/* -------------------------------------------------------------------------- */
/* The operator, and the deployment they are signing in to                    */
/* -------------------------------------------------------------------------- */

export const AUTH_CLIENT_ID = "moira-console.apps.authenticated-e2e.test";
export const AUTH_CLIENT_SECRET = "authenticated-e2e-client-secret-do-not-reuse";

export const AUTH_OPERATOR = {
  sub: "authenticated-e2e-subject-7c2ad4",
  email: "operator@example.com",
  emailVerified: true,
  name: "Console Operator",
} as const;

/** `allowed_email_domains` on the fixture provider row. */
export const AUTH_ALLOWED_EMAIL_DOMAIN = "example.com";

/**
 * `display_name` on the fixture provider row.
 *
 * It decides the sign-in button's ACCESSIBLE NAME: `SignInPanel.providerLabel`
 * uses `console.signIn.button` with the display name whenever the anonymous
 * sign-in-methods projection supplies one, and only falls back to
 * `console.signIn.button_generic` when it does not. Named here so the setup spec
 * asks for the button by the name the console will actually give it.
 */
export const AUTH_PROVIDER_DISPLAY_NAME = "Fixture OIDC";

export const AUTH_ADMIN_API_AUDIENCE = "moira-admin-api";

/* -------------------------------------------------------------------------- */
/* Deterministic Moira row ids                                                */
/* -------------------------------------------------------------------------- */

/**
 * Fixed rather than random, so a spec can assert the exact request PATH the
 * console produced — `POST /api/v1/admin/providers/<id>/models` names the row it
 * nests under, and an assertion on a path with a random id in it can only ever
 * be a pattern match.
 */
export const MOIRA_IDS = {
  authProvider: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  trustedJwtIssuer: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  generalRoute: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
  provider: "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
  providerModel: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
  providerCredential: "ffffffff-ffff-4fff-8fff-ffffffffffff",
  routingPolicy: "99999999-9999-4999-8999-999999999999",
} as const;

/** What the mock `/v1/models` endpoint advertises, in the order it lists them. */
export const DISCOVERABLE_MODEL_KEYS = ["Qwen3-4B", "gemma-3-4b-it"] as const;

/**
 * An address nothing is listening on.
 *
 * Port 1 is privileged and unbound, so a connection to it is refused
 * immediately rather than hanging until the 5 s discovery deadline — the same
 * `:1` idiom `console-env.ts` uses for its unreachable DSN. A port the harness
 * bound and then released would be a race: something else can take it.
 */
export const UNREACHABLE_ENDPOINT_URL = "http://127.0.0.1:1";

/* -------------------------------------------------------------------------- */
/* The control surface                                                        */
/* -------------------------------------------------------------------------- */

/** One recorded call to the fixture Moira. */
export interface RecordedMoiraRequest {
  /** `"<METHOD> <path>"` — the key every ordering assertion is written against. */
  readonly route: string;
  readonly method: string;
  readonly path: string;
  /** Present only for the headers a test asserts on; never the whole set. */
  readonly authenticated: boolean;
  readonly idempotencyKey: string | null;
  readonly body: unknown;
}

/** What the harness will tell a spec about itself. */
export interface FixtureReport {
  /** The canonical base URL of the mock OpenAI-compatible endpoint. */
  readonly vllmBaseUrl: string;
  /** The origin the mock endpoint listens on, without the version segment. */
  readonly vllmOrigin: string;
  readonly moiraBaseUrl: string;
  readonly idpIssuer: string;
  /** Every Moira request since the last reset, in order. */
  readonly moiraRequests: readonly RecordedMoiraRequest[];
  /** `"<METHOD> <path>"` for every call the mock endpoint served since reset. */
  readonly vllmRequests: readonly string[];
  /** Rows the fixture Moira currently holds, by family. */
  readonly rows: {
    readonly providers: number;
    readonly providerModels: number;
    readonly providerCredentials: number;
    readonly routingPolicies: number;
  };
}

async function control(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(`${AUTH_CONTROL_ORIGIN}${path}`, init);
  if (!response.ok) {
    throw new Error(
      `the authenticated e2e control server answered ${response.status} for ${path}. ` +
        "It is started by `e2e/support/authenticated-stack.ts` as the second `webServer` " +
        "entry in playwright.config.ts.",
    );
  }
  return response;
}

/** Everything the harness knows right now. */
export async function readFixture(): Promise<FixtureReport> {
  return (await (await control("/fixture")).json()) as FixtureReport;
}

/**
 * Forget every recorded request, WITHOUT touching the rows.
 *
 * The distinction is the whole point of having two verbs: an ordering assertion
 * needs a recording that starts at the click it is about, and it must not be
 * bought by wiping the state the click is supposed to find.
 *
 * ==========================================================================
 * IT SETTLES FIRST, AND THAT IS NOT DEFENSIVE PADDING
 * ==========================================================================
 *
 * `ConsoleHeader` renders navigation links, and the App Router PREFETCHES a
 * `<Link>` once the page hydrates. `/settings/llm` is `force-dynamic`, so a
 * prefetch of it is a full server render — three more Moira reads, arriving
 * some tens of milliseconds after `page.goto` resolved. Resetting the recording
 * without waiting for that put those three reads INSIDE the window a test
 * believed described one click, which is how this was found: two specs failed
 * with a page load's reads in a recording that should have held one.
 *
 * So this waits for the recording to stop growing, then clears it. A test that
 * asserts an exact sequence gets a window that starts empty and stays empty
 * until it acts.
 */
export async function resetRecording(): Promise<void> {
  let previous = -1;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const { moiraRequests } = await readFixture();
    if (moiraRequests.length === previous) break;
    previous = moiraRequests.length;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  await control("/recording", { method: "DELETE" });
}

/** Put the fixture Moira back to a freshly-migrated deployment, and forget the recording. */
export async function resetFixture(): Promise<void> {
  await control("/fixture", { method: "DELETE" });
}
