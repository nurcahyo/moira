// `RunnerDetail` — the state machine, driven purely through props (`onRefresh`
// / `onDeleted`, never `useRouter()` — see the component's own header for why)
// and a scripted `fetchImpl`, same standard as every other organism test in
// this console.

import { describe, expect, test } from "bun:test";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { ClaudeRunnerRecord, ProviderRecord } from "@/lib/types";
import { RunnerDetail } from "@/modules/runners/RunnerDetail";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

function field(key: string): HTMLElement {
  const label = copy(key).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return screen.getByLabelText(new RegExp(`^${label}`));
}

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
    return new Response(next.body === undefined ? null : JSON.stringify(next.body), {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch & { calls: typeof calls };
  impl.calls = calls;
  return impl;
}

function runner(overrides: Partial<ClaudeRunnerRecord> = {}): ClaudeRunnerRecord {
  return {
    id: "runner-1",
    label: "claude-1",
    runner_reference: "ref-1",
    state: "provisioning",
    scope: { type: "global" },
    metadata: {},
    created_at: "2026-08-16T00:00:00Z",
    updated_at: "2026-08-16T00:00:00Z",
    version: 3,
    ...overrides,
  };
}

const PROVIDER: ProviderRecord = {
  id: "prov-1",
  provider_type: "anthropic",
  display_name: "Anthropic",
  status: "active",
  metadata: {},
  created_at: "2026-08-01T00:00:00Z",
  updated_at: "2026-08-01T00:00:00Z",
  version: 1,
};

function noop(): void {
  /* not exercised in these tests unless asserted */
}

describe("RunnerDetail — per-state rendering", () => {
  test("awaiting_authorization shows the URL and the code-paste form", () => {
    render(
      <RunnerDetail
        runner={runner({ state: "awaiting_authorization", authorization_url: "https://claude.com/x" })}
        providers={[]}
        onRefresh={noop}
        onDeleted={noop}
      />,
    );
    expect(screen.getByRole("link", { name: "https://claude.com/x" })).toBeDefined();
    expect(field(CONSOLE_MESSAGE_KEYS.runners_authorization_code_label)).toBeDefined();
  });

  test("ready shows the finalize form, defaulted to the first provider", () => {
    render(
      <RunnerDetail runner={runner({ state: "ready" })} providers={[PROVIDER]} onRefresh={noop} onDeleted={noop} />,
    );
    const select = screen.getByLabelText(
      new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.runners_finalize_provider_label)}`),
    ) as HTMLSelectElement;
    expect(select.value).toBe("prov-1");
  });

  test("ready with no providers renders the no-providers message instead of a selector", () => {
    render(
      <RunnerDetail runner={runner({ state: "ready" })} providers={[]} onRefresh={noop} onDeleted={noop} />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.runners_finalize_no_providers))).toBeDefined();
    expect(screen.queryByRole("combobox")).toBeNull();
  });

  test("linked, failed, and expired each render their own heading", () => {
    for (const [state, headingKey] of [
      ["linked", CONSOLE_MESSAGE_KEYS.runners_linked_heading],
      ["failed", CONSOLE_MESSAGE_KEYS.runners_failed_heading],
      ["expired", CONSOLE_MESSAGE_KEYS.runners_expired_heading],
    ] as const) {
      const { unmount } = render(
        <RunnerDetail runner={runner({ state })} providers={[]} onRefresh={noop} onDeleted={noop} />,
      );
      expect(screen.getByText(copy(headingKey))).toBeDefined();
      unmount();
    }
  });
});

describe("RunnerDetail — authorization code", () => {
  test("posts the code and calls onRefresh on success", async () => {
    const send = scriptedFetch([{ status: 200, body: runner({ state: "exchanging" }) }]);
    let refreshed = 0;
    render(
      <RunnerDetail
        runner={runner({ state: "awaiting_authorization" })}
        providers={[]}
        onRefresh={() => {
          refreshed += 1;
        }}
        onDeleted={noop}
        fetchImpl={send}
      />,
    );

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_authorization_code_label), "auth-code-xyz");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_authorization_submit) }),
    );

    await waitFor(() => expect(refreshed).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/runners/runner-1/authorization-code");
    expect(send.calls[0]?.body).toEqual({ code: "auth-code-xyz" });
  });
});

describe("RunnerDetail — finalize", () => {
  test("posts provider_id and calls onRefresh on success", async () => {
    const send = scriptedFetch([{ status: 200, body: runner({ state: "linked" }) }]);
    let refreshed = 0;
    render(
      <RunnerDetail
        runner={runner({ state: "ready" })}
        providers={[PROVIDER]}
        onRefresh={() => {
          refreshed += 1;
        }}
        onDeleted={noop}
        fetchImpl={send}
      />,
    );

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_finalize_submit) }),
    );

    await waitFor(() => expect(refreshed).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/runners/runner-1/finalize");
    expect(send.calls[0]?.body).toEqual({ provider_id: "prov-1" });
  });

  test("an ordinary finalize failure renders as a retryable banner, and the form stays", async () => {
    // The shape `withConsoleSession`'s catch actually produces for a Moira
    // refusal: the copy lives at `error.text`, not at a flat `error.message`.
    const send = scriptedFetch([
      {
        status: 409,
        body: {
          error: {
            kind: "api",
            status: 409,
            code: "runner_wrong_state",
            remedy: "resolve_conflict",
            retryable: true,
            text: {
              messageKey: "moira.error.runner_wrong_state",
              message: "the runner is not ready",
              messageArgs: null,
            },
          },
        },
      },
    ]);
    render(
      <RunnerDetail runner={runner({ state: "ready" })} providers={[PROVIDER]} onRefresh={noop} onDeleted={noop} fetchImpl={send} />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_finalize_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe("the runner is not ready");
    // Still offering the retry — this is NOT the unrecoverable case.
    expect(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_finalize_submit) })).toBeDefined();
  });

  test("runner_token_unavailable replaces the form with the unrecoverable banner — no retry offered", async () => {
    const send = scriptedFetch([
      {
        status: 409,
        body: {
          error: {
            kind: "api",
            status: 409,
            code: "runner_token_unavailable",
            remedy: "denied",
            retryable: false,
            text: {
              messageKey: "moira.error.runner_token_unavailable",
              message: "the token is gone",
              messageArgs: null,
            },
          },
        },
      },
    ]);
    render(
      <RunnerDetail runner={runner({ state: "ready" })} providers={[PROVIDER]} onRefresh={noop} onDeleted={noop} fetchImpl={send} />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_finalize_submit) }),
    );

    expect(
      await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.runners_token_unavailable_heading)),
    ).toBeDefined();
    // The generic server message is NOT shown — this is the console's own
    // stated, honest explanation, not a relayed failure.
    expect(screen.queryByText("the token is gone")).toBeNull();
    // And the finalize form is gone: nothing here invites a retry that cannot work.
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_finalize_submit) }),
    ).toBeNull();
  });
});

describe("RunnerDetail — delete", () => {
  test("requires confirmation before a DELETE is sent", async () => {
    const send = scriptedFetch([{ status: 204, body: undefined }]);
    render(
      <RunnerDetail runner={runner()} providers={[]} onRefresh={noop} onDeleted={noop} fetchImpl={send} />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_delete_button) }),
    );
    expect(send.calls.length).toBe(0);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_delete_confirm_action) }),
    );
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.method).toBe("DELETE");
    expect(send.calls[0]?.url).toBe("/api/runners/runner-1");
  });

  test("onDeleted fires only after a successful delete", async () => {
    const send = scriptedFetch([{ status: 204, body: undefined }]);
    let deleted = 0;
    render(
      <RunnerDetail
        runner={runner()}
        providers={[]}
        onRefresh={noop}
        onDeleted={() => {
          deleted += 1;
        }}
        fetchImpl={send}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_delete_button) }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_delete_confirm_action) }),
    );
    await waitFor(() => expect(deleted).toBe(1));
  });

  test("a failed delete does not fire onDeleted, and shows the refusal", async () => {
    const send = scriptedFetch([
      {
        status: 409,
        body: {
          error: {
            code: "resource_version_conflict",
            message_key: CONSOLE_MESSAGE_KEYS.runners_delete_conflict_exhausted,
            message: "kept changing",
          },
        },
      },
    ]);
    let deleted = 0;
    render(
      <RunnerDetail
        runner={runner()}
        providers={[]}
        onRefresh={noop}
        onDeleted={() => {
          deleted += 1;
        }}
        fetchImpl={send}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_delete_button) }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_delete_confirm_action) }),
    );
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.runners_delete_conflict_exhausted))).toBeDefined();
    expect(deleted).toBe(0);
  });
});
