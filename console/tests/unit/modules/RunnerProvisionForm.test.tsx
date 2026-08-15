// `RunnerProvisionForm` — same standard `tests/unit/modules/ConnectClaudeSubscriptionPanel.test.tsx`
// holds its siblings to: no assertion compares against an English literal,
// every rendered string is checked against `CONSOLE_CATALOG[key].message`, and
// `fetchImpl` stands in for the browser's real `fetch`.

import { describe, expect, test } from "bun:test";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import { RunnerProvisionForm } from "@/modules/runners/RunnerProvisionForm";

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
    return new Response(JSON.stringify(next.body), {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch & { calls: typeof calls };
  impl.calls = calls;
  return impl;
}

const RECORD = {
  id: "runner-1",
  label: "claude-1",
  runner_reference: "ref-1",
  state: "provisioning",
  scope: { type: "global" },
  metadata: {},
  created_at: "2026-08-16T00:00:00Z",
  updated_at: "2026-08-16T00:00:00Z",
  version: 1,
};

describe("RunnerProvisionForm", () => {
  test("submission is blocked until the label matches the runner service's charset", async () => {
    render(<RunnerProvisionForm fetchImpl={scriptedFetch([{ status: 201, body: RECORD }])} />);
    const submit = screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_submit) });
    expect((submit as HTMLButtonElement).disabled).toBe(true);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_label_label), "Not Valid!");
    expect((submit as HTMLButtonElement).disabled).toBe(true);
  });

  test("posts to /api/runners with only the label when nothing else was filled in", async () => {
    const send = scriptedFetch([{ status: 201, body: RECORD }]);
    render(<RunnerProvisionForm fetchImpl={send} />);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_label_label), "claude-1");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/runners");
    expect(send.calls[0]?.method).toBe("POST");
    expect(send.calls[0]?.body).toEqual({ label: "claude-1" });
  });

  test("choosing the tenant option requires a tenant id before submission is allowed", async () => {
    render(<RunnerProvisionForm fetchImpl={scriptedFetch([{ status: 201, body: RECORD }])} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_label_label), "claude-acme");
    await userEvent.click(
      screen.getByRole("radio", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_scope_tenant_option) }),
    );
    const submit = screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_submit) });
    expect((submit as HTMLButtonElement).disabled).toBe(true);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_tenant_id_label), "acme");
    expect((submit as HTMLButtonElement).disabled).toBe(false);
  });

  test("a chosen tenant scope is sent as the existing CredentialScope wire shape", async () => {
    const send = scriptedFetch([{ status: 201, body: RECORD }]);
    render(<RunnerProvisionForm fetchImpl={send} />);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_label_label), "claude-acme");
    await userEvent.click(
      screen.getByRole("radio", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_scope_tenant_option) }),
    );
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_tenant_id_label), "acme");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.body).toEqual({
      label: "claude-acme",
      scope: { type: "tenant", external_tenant_id: "acme" },
    });
  });

  test("onProvisioned fires with the new runner's id, only after a successful submit", async () => {
    const send = scriptedFetch([{ status: 201, body: RECORD }]);
    const provisioned: string[] = [];
    render(<RunnerProvisionForm fetchImpl={send} onProvisioned={(id) => provisioned.push(id)} />);

    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_label_label), "claude-1");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_submit) }),
    );

    await waitFor(() => expect(provisioned).toEqual(["runner-1"]));
  });

  test("a keyed refusal from the server is rendered as its own message", async () => {
    // The shape `withConsoleSession`'s catch actually produces for a Moira
    // refusal (`moiraErrorBody` in `lib/console-api.ts`): the copy lives at
    // `error.text`, not at a flat `error.message`.
    const send = scriptedFetch([
      {
        status: 409,
        body: {
          error: {
            kind: "api",
            status: 409,
            code: "duplicate_runner_label",
            remedy: "fix_form_input",
            retryable: false,
            text: {
              messageKey: "moira.error.duplicate_runner_label",
              message: "a runner with this label already exists",
              messageArgs: null,
            },
          },
        },
      },
    ]);
    render(<RunnerProvisionForm fetchImpl={send} />);
    await userEvent.type(field(CONSOLE_MESSAGE_KEYS.runners_provision_label_label), "claude-1");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.runners_provision_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      "a runner with this label already exists",
    );
  });
});
