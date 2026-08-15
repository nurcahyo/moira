// The `/evals` organisms: the create-suite form, the suite list
// (edit/delete/expand), and the on-demand cases/runs panels.
//
// Same standard as `LlmScreen.test.tsx`: every assertion compares rendered
// text to `CONSOLE_CATALOG[key].message`, never an English literal.

import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { EvalSuiteRecord } from "@/lib/types";
import { EvalCasesPanel } from "@/modules/evals/EvalCasesPanel";
import { EvalRunsPanel } from "@/modules/evals/EvalRunsPanel";
import { EvalSuiteCreateForm } from "@/modules/evals/EvalSuiteCreateForm";
import { EvalSuiteList } from "@/modules/evals/EvalSuiteList";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

afterEach(cleanup);

function suite(overrides: Partial<EvalSuiteRecord> = {}): EvalSuiteRecord {
  return {
    id: "22222222-2222-4222-8222-222222222222",
    suite_key: "support_replies",
    display_name: "Support replies",
    description: "Grades canned support responses.",
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
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
    return new Response(next.status === 204 ? null : JSON.stringify(next.body), {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch & { calls: typeof calls };
  impl.calls = calls;
  return impl;
}

/* -------------------------------------------------------------------------- */
/* EvalSuiteCreateForm                                                       */
/* -------------------------------------------------------------------------- */

describe("EvalSuiteCreateForm", () => {
  test("it posts suite_key and display_name to the console's own endpoint", async () => {
    const send = scriptedFetch([{ status: 201, body: suite() }]);
    const created: string[] = [];
    render(<EvalSuiteCreateForm fetchImpl={send} onCreated={() => created.push("yes")} />);

    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.evals_field_suite_key_label)}`)),
      "support_replies",
    );
    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.evals_field_display_name_label)}`)),
      "Support replies",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_create_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/evals/suites");
    expect(send.calls[0]?.body).toMatchObject({ suite_key: "support_replies", display_name: "Support replies" });
    expect(created).toEqual(["yes"]);
  });
});

/* -------------------------------------------------------------------------- */
/* EvalSuiteList                                                             */
/* -------------------------------------------------------------------------- */

describe("EvalSuiteList", () => {
  test("an empty deployment gets an honest empty state", () => {
    render(<EvalSuiteList suites={[]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.evals_suites_empty))).toBeDefined();
  });

  test("delete asks first, and only confirming sends DELETE — there is no undo on this surface", async () => {
    const send = scriptedFetch([{ status: 204, body: null }]);
    const changed: string[] = [];
    render(<EvalSuiteList suites={[suite()]} fetchImpl={send} onChanged={() => changed.push("yes")} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_delete) }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.evals_delete_confirm_body),
    );
    expect(send.calls).toEqual([]);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_delete_confirm_action) }),
    );
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/evals/suites/22222222-2222-4222-8222-222222222222",
      method: "DELETE",
    });
    expect(changed).toEqual(["yes"]);
  });

  test("edit saves via PATCH and closes the form", async () => {
    const send = scriptedFetch([{ status: 200, body: suite({ display_name: "Renamed" }) }]);
    render(<EvalSuiteList suites={[suite()]} fetchImpl={send} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_edit) }));
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_edit_save) }));
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/evals/suites/22222222-2222-4222-8222-222222222222",
      method: "PATCH",
    });
  });

  test("expanding a suite renders its cases and runs panels", async () => {
    const send = scriptedFetch([
      { status: 200, body: { data: [], pagination: { has_more: false } } },
      { status: 200, body: { data: [], pagination: { has_more: false } } },
    ]);
    render(<EvalSuiteList suites={[suite()]} fetchImpl={send} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_expand) }));
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.evals_cases_heading))).toBeDefined();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.evals_runs_heading))).toBeDefined();
  });
});

/* -------------------------------------------------------------------------- */
/* EvalCasesPanel                                                            */
/* -------------------------------------------------------------------------- */

describe("EvalCasesPanel", () => {
  test("adding a case parses JSON client-side and posts input/expected/grading_kind", async () => {
    const send = scriptedFetch([
      { status: 200, body: { data: [], pagination: { has_more: false } } },
      { status: 201, body: { id: "case-1" } },
      { status: 200, body: { data: [{ id: "case-1", suite_id: "s1", input: { q: 1 }, expected: { a: 2 }, grading_kind: "exact_match", metadata: {}, created_at: "2026-08-14T00:00:00Z" }], pagination: { has_more: false } } },
    ]);
    render(<EvalCasesPanel suiteId="s1" fetchImpl={send} />);
    await waitFor(() => expect(send.calls.length).toBe(1));

    // `fireEvent.change` rather than `userEvent.type`: the latter parses `{`/`}`
    // as special-key syntax, which JSON text is full of.
    fireEvent.change(screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.evals_case_input_label)), {
      target: { value: '{"q":1}' },
    });
    fireEvent.change(screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.evals_case_expected_label)), {
      target: { value: '{"a":2}' },
    });
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_case_add_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(3));
    expect(send.calls[1]).toMatchObject({ url: "/api/evals/suites/s1/cases", method: "POST" });
    expect(send.calls[1]?.body).toEqual({ input: { q: 1 }, expected: { a: 2 }, grading_kind: "exact_match" });
  });

  test("invalid JSON in either field is refused locally", async () => {
    const send = scriptedFetch([{ status: 200, body: { data: [], pagination: { has_more: false } } }]);
    render(<EvalCasesPanel suiteId="s1" fetchImpl={send} />);
    await waitFor(() => expect(send.calls.length).toBe(1));

    await userEvent.type(screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.evals_case_input_label)), "not json");
    await userEvent.type(screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.evals_case_expected_label)), "1");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_case_add_submit) }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.evals_case_invalid_json),
    );
    expect(send.calls.length).toBe(1);
  });
});

/* -------------------------------------------------------------------------- */
/* EvalRunsPanel                                                             */
/* -------------------------------------------------------------------------- */

describe("EvalRunsPanel — the deferred trigger", () => {
  test("a 501 stub is tolerated gracefully, rendered as a notice rather than a crash", async () => {
    const send = scriptedFetch([
      { status: 200, body: { data: [], pagination: { has_more: false } } },
      {
        status: 501,
        body: { error: { code: "eval_run_not_available", message_key: CONSOLE_MESSAGE_KEYS.evals_run_not_available } },
      },
    ]);
    render(<EvalRunsPanel suiteId="s1" fetchImpl={send} />);
    await waitFor(() => expect(send.calls.length).toBe(1));

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.evals_run_trigger) }),
    );
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.evals_run_not_available))).toBeDefined();
    // Not an `alert` — the deferred stub is a known, calm state, not a crash.
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.evals_run_not_available)).getAttribute("role")).toBe(
      "status",
    );
  });

  test("an empty run history says so", async () => {
    const send = scriptedFetch([{ status: 200, body: { data: [], pagination: { has_more: false } } }]);
    render(<EvalRunsPanel suiteId="s1" fetchImpl={send} />);
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.evals_runs_empty))).toBeDefined();
  });
});
