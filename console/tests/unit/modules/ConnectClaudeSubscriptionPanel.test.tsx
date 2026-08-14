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
import { ConnectClaudeSubscriptionPanel } from "@/modules/llm/ConnectClaudeSubscriptionPanel";

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
