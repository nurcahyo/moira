// The sign-in surface, in every state the server can hand it.
//
// ============================================================================
// WHY NOTHING BELOW ASSERTS ON A LITERAL ENGLISH STRING
// ============================================================================
//
// `expect(screen.getByText("Sign-in is not available"))` passes in two worlds:
// the one where the component resolved the string through `t()`, and the one
// where it hardcoded the same string and never called `t()` at all. It also
// passes when the key is missing from the catalog, because `t()` falls back to
// the key and the test would simply have been written against whatever rendered.
//
// So every assertion here compares the rendered text to
// `CONSOLE_CATALOG[key].message`, read from the catalog module at test time. A
// missing entry fails, a hardcoded duplicate fails on the next copy edit, and
// the assertion cannot drift from the source of truth because it IS the source
// of truth.
//
// ============================================================================
// AND WHY EVERY REFUSAL STATE ASSERTS ZERO BUTTONS
// ============================================================================
//
// `app/api/auth/[...all]/route.ts:61-66` answers 503 on exactly the deployment
// this page exists for. A panel that renders a button in a refusal state is a
// button that 503s on click — the failure mode the whole server-side resolution
// in `app/login/page.tsx` exists to prevent — and it would look completely
// correct in a screenshot.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import { AUTH_CONFIG_PROBLEM_MESSAGE_KEYS } from "@/lib/auth-config";
import { CONSOLE_SECRET_DRIFT_MESSAGE_KEYS } from "@/lib/console-secrets";
import { SignInPanel, type SignInPanelState } from "@/modules/signIn/SignInPanel";

/** The catalog's English for a key. Never a literal in this file. */
const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

const READY: SignInPanelState = {
  kind: "ready",
  providerId: "moira-console-idp",
  displayName: "Example IdP",
};

/**
 * Every refusal the server can resolve to.
 *
 * Six come from `resolveAuthConfig` (`AUTH_CONFIG_PROBLEM_MESSAGE_KEYS`), three
 * from the D7 drift discriminant, and one from the bootstrap deadlock in
 * `lib/auth-runtime.ts:194-202`. They are read from the shipped tables rather
 * than re-listed, so a key renamed in `lib/` fails here instead of silently
 * dropping a case.
 */
const REFUSAL_KEYS: readonly string[] = [
  ...Object.values(AUTH_CONFIG_PROBLEM_MESSAGE_KEYS),
  ...Object.values(CONSOLE_SECRET_DRIFT_MESSAGE_KEYS),
  CONSOLE_MESSAGE_KEYS.auth_config_unavailable,
  CONSOLE_MESSAGE_KEYS.moira_unreachable,
];

describe("the refusal states are real, distinct, and catalogued", () => {
  test("the shipped tables still cover the six documented problems", () => {
    // A floor. If `AUTH_CONFIG_PROBLEM_MESSAGE_KEYS` shrank, the loop below
    // would quietly test fewer states and still be green.
    expect(Object.keys(AUTH_CONFIG_PROBLEM_MESSAGE_KEYS)).toHaveLength(7);
    expect(Object.keys(CONSOLE_SECRET_DRIFT_MESSAGE_KEYS)).toHaveLength(3);
    expect(new Set(REFUSAL_KEYS).size).toBeGreaterThanOrEqual(8);
  });

  test("every refusal key resolves to real English, not to itself", () => {
    const bare = [...new Set(REFUSAL_KEYS)].filter(
      (key) => CONSOLE_CATALOG[key as ConsoleMessageKey] === undefined,
    );
    expect(bare, "these refusal keys have no catalog entry").toEqual([]);
  });
});

describe("each refusal state renders its own message and no button", () => {
  for (const key of [...new Set(REFUSAL_KEYS)]) {
    test(`${key}`, () => {
      render(<SignInPanel state={{ kind: "unavailable", messageKey: key }} />);

      // Compared against the CATALOG, never against a literal — see the header.
      expect(screen.getByRole("alert")).toHaveTextContent(copy(key));
      expect(screen.getByRole("heading")).toHaveTextContent(
        copy(CONSOLE_MESSAGE_KEYS.sign_in_unavailable_heading),
      );

      // The load-bearing one.
      expect(
        screen.queryAllByRole("button"),
        "a refusal state must render no sign-in button — it would 503 on click",
      ).toEqual([]);
    });
  }
});

describe("the server-supplied message is the fallback, and the catalog wins", () => {
  test("a key the console catalogues ignores the server's prose", () => {
    render(
      <SignInPanel
        state={{
          kind: "unavailable",
          messageKey: CONSOLE_MESSAGE_KEYS.no_enabled_auth_provider,
          message: "prose from the server",
        }}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.no_enabled_auth_provider),
    );
    expect(screen.queryByText("prose from the server")).not.toBeInTheDocument();
  });

  test("a Moira key the console does not catalogue falls back to the server's prose", () => {
    render(
      <SignInPanel
        state={{
          kind: "unavailable",
          messageKey: "moira.error.database_unavailable",
          message: "Moira's own English",
        }}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Moira's own English");
  });

  test("a 503 body with a key and NO message still renders English", () => {
    // `app/api/auth/[...all]/route.ts:39-44` returns `{error:{code,message_key}}`
    // with no `message` field at all. Before the console catalog existed there
    // was nothing to degrade to and this rendered a bare key.
    render(
      <SignInPanel
        state={{
          kind: "unavailable",
          messageKey: CONSOLE_MESSAGE_KEYS.auth_config_unavailable,
        }}
      />,
    );
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent(copy(CONSOLE_MESSAGE_KEYS.auth_config_unavailable));
    expect(alert.textContent).not.toContain("console.error.");
  });
});

describe("the ready state renders exactly one button", () => {
  test("one button, named from the anonymous projection's display_name", () => {
    render(<SignInPanel state={READY} />);
    const buttons = screen.getAllByRole("button");
    expect(buttons, "at most one button by construction — see the component header").toHaveLength(
      1,
    );
    expect(buttons[0]).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.sign_in_button).replace("{provider}", "Example IdP"),
    );
  });

  test("a missing display_name falls back to the generic label, still one button", () => {
    render(<SignInPanel state={{ ...READY, displayName: null }} />);
    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(1);
    expect(buttons[0]).toHaveTextContent(copy(CONSOLE_MESSAGE_KEYS.sign_in_button_generic));
  });

  test("no password field, ever", () => {
    // `lib/auth.ts:220` sets `emailAndPassword: { enabled: false }`. A password
    // input here would be a second, weaker way in that the deployment's own auth
    // policy never sees.
    const { container } = render(<SignInPanel state={READY} />);
    expect(container.querySelector('input[type="password"]')).toBeNull();
    expect(container.querySelectorAll("input")).toHaveLength(0);
  });
});

describe("the sign-in request uses the wire format the integration test drives", () => {
  test("POST /api/auth/sign-in/oauth2 with providerId and callbackURL, then navigate", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const navigated: string[] = [];
    const fetchImpl = (async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ url: "https://idp.example/authorize?x=1" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;

    render(<SignInPanel state={READY} fetchImpl={fetchImpl} navigate={(u) => navigated.push(u)} />);
    await userEvent.click(screen.getByRole("button"));

    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("/api/auth/sign-in/oauth2");
    expect(calls[0]!.init.method).toBe("POST");
    expect((calls[0]!.init.headers as Record<string, string>)["content-type"]).toBe(
      "application/json",
    );
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({
      providerId: "moira-console-idp",
      callbackURL: "/",
    });
    expect(navigated).toEqual(["https://idp.example/authorize?x=1"]);
  });

  test("429 renders the rate-limit message and does not navigate", async () => {
    // `lib/auth.ts:245` puts the rate limiter on `storage: "database"`, so the
    // limit is shared across replicas — a user can hit it without having clicked
    // many times themselves, which is why it gets its own message.
    const navigated: string[] = [];
    const fetchImpl = (async () => new Response("{}", { status: 429 })) as unknown as typeof fetch;

    render(<SignInPanel state={READY} fetchImpl={fetchImpl} navigate={(u) => navigated.push(u)} />);
    await userEvent.click(screen.getByRole("button"));

    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.sign_in_rate_limited),
    );
    expect(navigated).toEqual([]);
  });

  test("a 503 from the mounted handler does not echo its body", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({ error: { code: "no_enabled_provider", message_key: "console.error.x" } }),
        { status: 503 },
      )) as unknown as typeof fetch;

    render(<SignInPanel state={READY} fetchImpl={fetchImpl} navigate={() => {}} />);
    await userEvent.click(screen.getByRole("button"));

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent(copy(CONSOLE_MESSAGE_KEYS.sign_in_request_failed));
    expect(alert.textContent).not.toContain("no_enabled_provider");
    expect(alert.textContent).not.toContain("console.error.x");
  });

  test("200 with no url is its own message, not a generic failure", async () => {
    const navigated: string[] = [];
    const fetchImpl = (async () =>
      new Response(JSON.stringify({}), { status: 200 })) as unknown as typeof fetch;

    render(<SignInPanel state={READY} fetchImpl={fetchImpl} navigate={(u) => navigated.push(u)} />);
    await userEvent.click(screen.getByRole("button"));

    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.sign_in_no_redirect_url),
    );
    expect(navigated).toEqual([]);
  });

  test("a thrown fetch never echoes the cause", async () => {
    const fetchImpl = (async () => {
      throw new Error("connect ECONNREFUSED 10.0.0.1:443");
    }) as unknown as typeof fetch;

    render(<SignInPanel state={READY} fetchImpl={fetchImpl} navigate={() => {}} />);
    await userEvent.click(screen.getByRole("button"));

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent(copy(CONSOLE_MESSAGE_KEYS.sign_in_request_failed));
    expect(alert.textContent).not.toContain("10.0.0.1");
    expect(alert.textContent).not.toContain("ECONNREFUSED");
  });
});
