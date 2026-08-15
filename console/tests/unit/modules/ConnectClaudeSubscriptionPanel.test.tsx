// `ConnectClaudeSubscriptionPanel` — same standard `tests/unit/modules/LlmScreen.test.tsx`
// holds its siblings to: nothing here asserts on an English literal, every
// assertion compares rendered text to `CONSOLE_CATALOG[key].message`, and
// `fetchImpl` stands in for the browser's real `fetch` so no test in this file
// makes a network call or needs a real subscription token — the owner-approved
// testing policy (`plans/12-feature-expansion-brainstorm.md` §1) requires
// exactly that.

import { describe, expect, test } from "bun:test";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { ClaudeCredentialStatusView } from "@/lib/llm-view";
import { ConnectClaudeSubscriptionPanel } from "@/modules/llm/ConnectClaudeSubscriptionPanel";

const NOT_CONNECTED: ClaudeCredentialStatusView = { kind: "not_connected", status: null, expiresAt: null };
const CONNECTED_ACTIVE: ClaudeCredentialStatusView = {
  kind: "connected",
  status: "active",
  expiresAt: null,
};

/** Unmistakable, and asserted absent from every rendered node. */
const TOKEN = "sk-ant-oat01-unmistakable-subscription-token-4f9c2b";

/** The catalog's English for a key. Never a literal in this file. */
const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

/** A field, found by its label — same helper `LlmScreen.test.tsx` uses. */
function field(key: string): HTMLElement {
  const label = copy(key).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return screen.getByLabelText(new RegExp(`^${label}`));
}

/** A `fetch` that answers each call from a queue and records what it was sent. */
function scriptedFetch(
  responses: ReadonlyArray<{ status: number; body: unknown }>,
): typeof fetch & { calls: Array<{ url: string; method: string; body: unknown }> } {
  const calls: Array<{ url: string; method: string; body: unknown }> = [];
  let index = 0;
  const impl = (async (url: RequestInfo | URL, init?: RequestInit) => {
    calls.push({
      url: String(url),
      method: (init?.method ?? "GET").toUpperCase(),
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    });
    const next = responses[Math.min(index, responses.length - 1)]!;
    index += 1;
    return new Response(JSON.stringify(next.body), {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch & { calls: typeof calls };
  impl.calls = calls;
  return impl;
}

describe("ConnectClaudeSubscriptionPanel", () => {
  test("submission is blocked until the field carries something", () => {
    render(<ConnectClaudeSubscriptionPanel fetchImpl={scriptedFetch([{ status: 200, body: {} }])} />);
    const submit = screen.getByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit),
    });
    expect((submit as HTMLButtonElement).disabled).toBe(true);
  });

  test("the token field is masked, exactly like the client-secret form's field", () => {
    render(<ConnectClaudeSubscriptionPanel fetchImpl={scriptedFetch([{ status: 200, body: {} }])} />);
    const input = field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label) as HTMLInputElement;
    expect(input.type).toBe("password");
  });

  test("it posts to the console's OWN endpoint, never to Moira directly", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label), TOKEN);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/settings/llm/claude-subscription");
    expect(send.calls[0]?.method).toBe("POST");
    expect(send.calls[0]?.body).toEqual({ token: TOKEN });
  });

  test("the field is cleared and the token never appears in the rendered DOM after a successful save", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    const { container } = render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);

    const input = field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label) as HTMLInputElement;
    await userEvent.type(input, TOKEN);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit) }),
    );

    await waitFor(() => expect(input.value).toBe(""));
    expect(container.innerHTML).not.toContain(TOKEN);
  });

  // Two separate tests, not one test with two `render()` calls: `cleanup()`
  // (`tests/support/dom-setup.ts`) runs in `afterEach`, between tests — a
  // second `render()` inside the SAME test leaves both trees mounted at once,
  // which makes every query below ambiguous rather than proving anything.
  test("a first save (nothing existed yet) announces 'created'", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label), TOKEN);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit) }),
    );
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_created))).toBeDefined();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_rotated))).toBeNull();
  });

  test("a second save (a credential already existed) announces 'rotated', not 'created'", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "rotated" } },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label), TOKEN);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit) }),
    );
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_rotated))).toBeDefined();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_created))).toBeNull();
  });

  test("`onConnected` fires only after a successful save", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    const connected: string[] = [];
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} onConnected={() => connected.push("yes")} />);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label), TOKEN);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit) }),
    );

    await waitFor(() => expect(connected).toEqual(["yes"]));
  });

  test("a keyed refusal from the server is rendered as its own message, and the token is not sent again on its own", async () => {
    const send = scriptedFetch([
      {
        status: 400,
        body: {
          error: {
            code: "invalid_request",
            message_key: CONSOLE_MESSAGE_KEYS.claude_subscription_token_invalid,
          },
        },
      },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label), `${TOKEN}\nx`);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.claude_subscription_token_invalid),
    );
    expect(send.calls.length).toBe(1);
  });
});

describe("ConnectClaudeSubscriptionPanel — Mode A (CLI-assisted)", () => {
  test("disabled by default: shows the disabled notice and no acquire button", () => {
    render(<ConnectClaudeSubscriptionPanel fetchImpl={scriptedFetch([{ status: 200, body: {} }])} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled_notice))).toBeDefined();
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
    ).toBeNull();
  });

  test("when enabled and nothing is connected, the button reads 'acquire', not 'reacquire'", () => {
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={scriptedFetch([{ status: 200, body: {} }])}
        cliAcquisitionEnabled
        subscriptionStatus={NOT_CONNECTED}
      />,
    );
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
    ).toBeDefined();
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_reacquire) }),
    ).toBeNull();
  });

  test("when enabled and already connected, the button reads 're-acquire and rotate'", () => {
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={scriptedFetch([{ status: 200, body: {} }])}
        cliAcquisitionEnabled
        subscriptionStatus={CONNECTED_ACTIVE}
      />,
    );
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_reacquire) }),
    ).toBeDefined();
  });

  test("clicking posts an empty body to the START endpoint — nothing user-supplied travels with it", async () => {
    const send = scriptedFetch([
      { status: 200, body: { job_id: "job-1", authorization_url: null } },
      { status: 200, body: { status: "succeeded", provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={send}
        cliAcquisitionEnabled
        subscriptionStatus={NOT_CONNECTED}
        cliPollIntervalMs={1}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBeGreaterThanOrEqual(1));
    expect(send.calls[0]?.url).toBe("/api/settings/llm/claude-subscription/acquire/start");
    expect(send.calls[0]?.method).toBe("POST");
    expect(send.calls[0]?.body).toEqual({});
  });

  test("a successful acquisition — start, then a poll that reports success — announces 'created' and fires onConnected", async () => {
    const send = scriptedFetch([
      { status: 200, body: { job_id: "job-1", authorization_url: null } },
      { status: 200, body: { status: "succeeded", provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    const connected: string[] = [];
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={send}
        cliAcquisitionEnabled
        subscriptionStatus={NOT_CONNECTED}
        onConnected={() => connected.push("yes")}
        cliPollIntervalMs={1}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
    );
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_created))).toBeDefined();
    await waitFor(() => expect(connected).toEqual(["yes"]));
    // Exactly one start call, one status poll — the loop stops once terminal.
    await waitFor(() => expect(send.calls.length).toBe(2));
    expect(send.calls[1]?.url).toBe("/api/settings/llm/claude-subscription/acquire/status?job=job-1");
    expect(send.calls[1]?.method).toBe("GET");
  });

  test("while awaiting login, shows a link to the authorization URL a status poll reports, and opens it once", async () => {
    const originalOpen = window.open;
    const opened: string[] = [];
    window.open = ((url?: string | URL) => {
      opened.push(String(url));
      return null;
    }) as typeof window.open;

    try {
      const send = scriptedFetch([
        { status: 200, body: { job_id: "job-1", authorization_url: null } },
        { status: 200, body: { status: "running", authorization_url: "https://example.com/authorize?x=1" } },
        { status: 200, body: { status: "running", authorization_url: "https://example.com/authorize?x=1" } },
        { status: 200, body: { status: "succeeded", provider_id: "p1", credential_id: "c1", outcome: "created" } },
      ]);
      render(
        <ConnectClaudeSubscriptionPanel
          fetchImpl={send}
          cliAcquisitionEnabled
          subscriptionStatus={NOT_CONNECTED}
          cliPollIntervalMs={1}
        />,
      );
      await userEvent.click(
        screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
      );

      const link = await screen.findByRole("link", {
        name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_open_link),
      });
      expect(link.getAttribute("href")).toBe("https://example.com/authorize?x=1");

      await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.claude_subscription_created));
      // Reported the SAME URL on two consecutive polls; opened exactly once.
      expect(opened).toEqual(["https://example.com/authorize?x=1"]);
    } finally {
      window.open = originalOpen;
    }
  });

  test("a keyed CLI refusal (e.g. not signed in), discovered via a status poll, is rendered as its own alert", async () => {
    const send = scriptedFetch([
      { status: 200, body: { job_id: "job-1", authorization_url: null } },
      {
        status: 409,
        body: {
          error: {
            code: "claude_cli_mint_failed",
            message_key: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_not_signed_in,
          },
        },
      },
    ]);
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={send}
        cliAcquisitionEnabled
        subscriptionStatus={NOT_CONNECTED}
        cliPollIntervalMs={1}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_not_signed_in),
    );
  });

  test("a failure at start time (e.g. the deployment turned this off mid-flow) never polls status", async () => {
    const send = scriptedFetch([
      {
        status: 403,
        body: {
          error: {
            code: "claude_cli_acquisition_disabled",
            message_key: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled,
          },
        },
      },
    ]);
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={send}
        cliAcquisitionEnabled
        subscriptionStatus={NOT_CONNECTED}
        cliPollIntervalMs={1}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled),
    );
    expect(send.calls.length).toBe(1);
  });
});

describe("ConnectClaudeSubscriptionPanel — Mode B (Anthropic API key)", () => {
  const API_KEY = "sk-ant-unmistakable-console-api-key-9f1a2b";

  test("the field is masked, exactly like the subscription-token field", () => {
    render(<ConnectClaudeSubscriptionPanel fetchImpl={scriptedFetch([{ status: 200, body: {} }])} />);
    const input = field(CONSOLE_MESSAGE_KEYS.claude_api_key_label) as HTMLInputElement;
    expect(input.type).toBe("password");
  });

  test("submission is blocked until the field carries something", () => {
    render(<ConnectClaudeSubscriptionPanel fetchImpl={scriptedFetch([{ status: 200, body: {} }])} />);
    const submit = screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_api_key_submit) });
    expect((submit as HTMLButtonElement).disabled).toBe(true);
  });

  test("it posts to the console's own API-key endpoint with { api_key }", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_api_key_label), API_KEY);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_api_key_submit) }));

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/settings/llm/claude-api-key");
    expect(send.calls[0]?.method).toBe("POST");
    expect(send.calls[0]?.body).toEqual({ api_key: API_KEY });
  });

  test("the field is cleared and the key never appears in the rendered DOM after a successful save", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    const { container } = render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);

    const input = field(CONSOLE_MESSAGE_KEYS.claude_api_key_label) as HTMLInputElement;
    await userEvent.type(input, API_KEY);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_api_key_submit) }));

    await waitFor(() => expect(input.value).toBe(""));
    expect(container.innerHTML).not.toContain(API_KEY);
  });

  // Two separate tests, not one test with two `render()` calls — same
  // reasoning as the paste-mode tests above: `cleanup()` runs between tests,
  // not between two renders inside one.
  test("a first save (nothing existed yet) announces 'created'", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "created" } },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_api_key_label), API_KEY);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_api_key_submit) }));
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.claude_api_key_created))).toBeDefined();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.claude_api_key_updated))).toBeNull();
  });

  test("a second save (a credential already existed) announces 'updated', not 'created'", async () => {
    const send = scriptedFetch([
      { status: 200, body: { provider_id: "p1", credential_id: "c1", outcome: "rotated" } },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_api_key_label), API_KEY);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_api_key_submit) }));
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.claude_api_key_updated))).toBeDefined();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.claude_api_key_created))).toBeNull();
  });

  test("a keyed refusal (wrong shape) is rendered as its own alert", async () => {
    const send = scriptedFetch([
      {
        status: 400,
        body: {
          error: { code: "invalid_request", message_key: CONSOLE_MESSAGE_KEYS.claude_api_key_wrong_shape },
        },
      },
    ]);
    render(<ConnectClaudeSubscriptionPanel fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.claude_api_key_label), "sk-proj-not-anthropic");
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.claude_api_key_submit) }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.claude_api_key_wrong_shape),
    );
  });
});

describe("ConnectClaudeSubscriptionPanel — status display", () => {
  test("renders the neutral 'unknown' status when no status prop is supplied (the safe default)", () => {
    render(<ConnectClaudeSubscriptionPanel fetchImpl={scriptedFetch([{ status: 200, body: {} }])} />);
    expect(
      screen.getAllByText(copy(CONSOLE_MESSAGE_KEYS.claude_connect_status_unknown)).length,
    ).toBeGreaterThanOrEqual(1);
  });

  test("renders 'connected' with no expiry when the subscription row is active", () => {
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={scriptedFetch([{ status: 200, body: {} }])}
        subscriptionStatus={CONNECTED_ACTIVE}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.claude_connect_status_connected))).toBeDefined();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.claude_connect_status_no_expiry))).toBeDefined();
  });

  test("renders 'connected, but disabled' when the row exists but is not active", () => {
    render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={scriptedFetch([{ status: 200, body: {} }])}
        keyStatus={{ kind: "connected", status: "disabled", expiresAt: null }}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.claude_connect_status_disabled))).toBeDefined();
  });

  test("never renders a masked secret or a fingerprint — the status view carries neither", () => {
    const { container } = render(
      <ConnectClaudeSubscriptionPanel
        fetchImpl={scriptedFetch([{ status: 200, body: {} }])}
        subscriptionStatus={CONNECTED_ACTIVE}
        keyStatus={CONNECTED_ACTIVE}
      />,
    );
    expect(container.innerHTML).not.toMatch(/mask|fingerprint/i);
  });
});
