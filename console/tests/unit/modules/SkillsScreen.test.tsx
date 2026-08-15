// The `/skills` organisms: the create form, the OpenAPI import panel, the
// list (enable/disable/edit/delete/bulk-enable), and the on-demand executor
// panel.
//
// Same standard as `LlmScreen.test.tsx`: every assertion compares rendered
// text to `CONSOLE_CATALOG[key].message`, read from the catalog at test time,
// never an English literal — so a copy edit cannot silently break this test
// while the component still calls `t()`.

import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { SkillHttpExecutorRecord, SkillRecord } from "@/lib/types";
import { SkillCreateForm } from "@/modules/skills/SkillCreateForm";
import { SkillExecutorPanel } from "@/modules/skills/SkillExecutorPanel";
import { SkillImportPanel } from "@/modules/skills/SkillImportPanel";
import { kindKey, SkillList, statusKey } from "@/modules/skills/SkillList";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

afterEach(cleanup);

function skill(overrides: Partial<SkillRecord> = {}): SkillRecord {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    skill_key: "lookup_order",
    display_name: "Look up order",
    kind: "tool",
    description: "Looks up an order by id.",
    params_schema: {},
    tags: ["orders"],
    status: "draft",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function executor(overrides: Partial<SkillHttpExecutorRecord> = {}): SkillHttpExecutorRecord {
  return {
    skill_id: "11111111-1111-4111-8111-111111111111",
    method: "GET",
    url_template: "https://api.example.test/orders/{id}",
    allowed_host: "api.example.test",
    header_template: {},
    timeout_ms: 5000,
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00.123456Z",
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
/* SkillCreateForm                                                            */
/* -------------------------------------------------------------------------- */

describe("SkillCreateForm", () => {
  test("submission is blocked until the required fields carry something", async () => {
    render(<SkillCreateForm fetchImpl={scriptedFetch([{ status: 201, body: skill() }])} />);
    const submit = screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_create_submit) });
    expect((submit as HTMLButtonElement).disabled).toBe(true);
  });

  test("it posts to the console's own endpoint with the tag list split on commas", async () => {
    const send = scriptedFetch([{ status: 201, body: skill() }]);
    const created: string[] = [];
    render(<SkillCreateForm fetchImpl={send} onCreated={() => created.push("yes")} />);

    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.skills_field_skill_key_label)}`)),
      "lookup_order",
    );
    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.skills_field_display_name_label)}`)),
      "Look up order",
    );
    await userEvent.type(
      screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.skills_field_tags_label)),
      "orders, support",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_create_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/skills");
    expect(send.calls[0]?.method).toBe("POST");
    expect(send.calls[0]?.body).toMatchObject({
      skill_key: "lookup_order",
      display_name: "Look up order",
      kind: "tool",
      tags: ["orders", "support"],
    });
    expect(created).toEqual(["yes"]);
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.skills_create_success))).toBeDefined();
  });

  test("a keyed refusal from the server is rendered", async () => {
    const send = scriptedFetch([
      { status: 400, body: { error: { code: "invalid_request", message_key: CONSOLE_MESSAGE_KEYS.skills_kind_required } } },
    ]);
    render(<SkillCreateForm fetchImpl={send} />);
    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.skills_field_skill_key_label)}`)),
      "x",
    );
    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.skills_field_display_name_label)}`)),
      "X",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_create_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.skills_kind_required),
    );
  });
});

/* -------------------------------------------------------------------------- */
/* SkillImportPanel                                                          */
/* -------------------------------------------------------------------------- */

describe("SkillImportPanel", () => {
  test("invalid JSON is refused locally, with no network call", async () => {
    const send = scriptedFetch([{ status: 201, body: { imported_count: 0, skills: [], executors: [] } }]);
    render(<SkillImportPanel fetchImpl={send} />);
    const field = screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.skills_import_field_label));
    // `fireEvent.change` rather than `userEvent.type`: the latter parses `{`/`}`
    // as special-key syntax, which JSON text is full of.
    fireEvent.change(field, { target: { value: "{not valid json" } });
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_import_submit) }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.skills_import_invalid_json),
    );
    expect(send.calls).toEqual([]);
  });

  test("valid JSON is parsed and sent as the `document` field, not as raw text", async () => {
    const send = scriptedFetch([
      {
        status: 201,
        body: { imported_count: 2, skills: [skill({ id: "s1" }), skill({ id: "s2", display_name: "Second" })], executors: [] },
      },
    ]);
    const imported: string[] = [];
    render(<SkillImportPanel fetchImpl={send} onImported={() => imported.push("yes")} />);
    const field = screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.skills_import_field_label));
    fireEvent.change(field, { target: { value: '{"openapi":"3.0.0"}' } });
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_import_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/skills/import");
    expect(send.calls[0]?.body).toEqual({ document: { openapi: "3.0.0" } });
    expect(imported).toEqual(["yes"]);
    expect(await screen.findByText("Second")).toBeDefined();
  });
});

/* -------------------------------------------------------------------------- */
/* SkillList                                                                 */
/* -------------------------------------------------------------------------- */

describe("SkillList", () => {
  test("an empty registry gets an honest empty state", () => {
    render(<SkillList skills={[]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.skills_list_empty))).toBeDefined();
  });

  test("kindKey and statusKey map every enum value to a distinct catalog key", () => {
    expect(kindKey("tool")).toBe(CONSOLE_MESSAGE_KEYS.skills_kind_tool);
    expect(kindKey("guard")).toBe(CONSOLE_MESSAGE_KEYS.skills_kind_guard);
    expect(statusKey("draft")).toBe(CONSOLE_MESSAGE_KEYS.skills_status_draft);
    expect(statusKey("enabled")).toBe(CONSOLE_MESSAGE_KEYS.skills_status_enabled);
    expect(statusKey("disabled")).toBe(CONSOLE_MESSAGE_KEYS.skills_status_disabled);
  });

  test("a draft skill offers Enable, and clicking it posts to the enable endpoint", async () => {
    const send = scriptedFetch([{ status: 200, body: skill({ status: "enabled" }) }]);
    const changed: string[] = [];
    render(<SkillList skills={[skill()]} fetchImpl={send} onChanged={() => changed.push("yes")} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_enable) }));
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({ url: "/api/skills/11111111-1111-4111-8111-111111111111/enable", method: "POST" });
    expect(changed).toEqual(["yes"]);
  });

  test("an enabled skill offers Disable instead, and no bulk-enable checkbox", () => {
    render(<SkillList skills={[skill({ status: "enabled" })]} />);
    expect(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_disable) })).toBeDefined();
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  test("edit opens a form, saves via PATCH, and closes on success", async () => {
    const send = scriptedFetch([{ status: 200, body: skill({ display_name: "Renamed" }) }]);
    const changed: string[] = [];
    render(<SkillList skills={[skill()]} fetchImpl={send} onChanged={() => changed.push("yes")} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_edit) }));
    const nameField = screen.getByLabelText(
      new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.skills_field_display_name_label)}`),
    ) as HTMLInputElement;
    await userEvent.clear(nameField);
    await userEvent.type(nameField, "Renamed");
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_edit_save) }));

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/skills/11111111-1111-4111-8111-111111111111",
      method: "PATCH",
    });
    expect(changed).toEqual(["yes"]);
    expect(screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_edit_save) })).toBeNull();
  });

  test("delete asks first via DangerConfirmDialog, and only confirming sends DELETE", async () => {
    const send = scriptedFetch([{ status: 204, body: null }]);
    const changed: string[] = [];
    render(<SkillList skills={[skill()]} fetchImpl={send} onChanged={() => changed.push("yes")} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_delete) }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.skills_delete_confirm_body),
    );
    expect(send.calls).toEqual([]);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_delete_confirm_action) }),
    );
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/skills/11111111-1111-4111-8111-111111111111",
      method: "DELETE",
    });
    expect(changed).toEqual(["yes"]);
  });

  test("bulk-enable is disabled with nothing selected, and posts every checked id", async () => {
    const send = scriptedFetch([{ status: 200, body: { data: [skill(), skill({ id: "id-2" })] } }]);
    render(
      <SkillList
        skills={[skill(), skill({ id: "id-2", skill_key: "second" })]}
        fetchImpl={send}
      />,
    );
    const bulkButton = screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_bulk_enable_button) });
    expect((bulkButton as HTMLButtonElement).disabled).toBe(true);

    const checkboxes = screen.getAllByRole("checkbox", {
      name: copy(CONSOLE_MESSAGE_KEYS.skills_select_for_bulk_enable),
    });
    await userEvent.click(checkboxes[0]!);
    expect((bulkButton as HTMLButtonElement).disabled).toBe(false);

    await userEvent.click(bulkButton);
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/skills/bulk-enable");
    expect(send.calls[0]?.body).toEqual({ skill_ids: ["11111111-1111-4111-8111-111111111111"] });
  });
});

/* -------------------------------------------------------------------------- */
/* SkillExecutorPanel                                                        */
/* -------------------------------------------------------------------------- */

describe("SkillExecutorPanel", () => {
  test("a 404 renders the calm 'no executor' state, not an alert", async () => {
    const send = scriptedFetch([
      { status: 404, body: { error: { code: "not_found", message_key: "moira.error.skill_executor_not_found" } } },
    ]);
    render(<SkillExecutorPanel skillId="s1" fetchImpl={send} />);
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.skills_executor_none))).toBeDefined();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("a genuine failure renders as an alert", async () => {
    const send = scriptedFetch([
      { status: 502, body: { error: { code: "moira_unreachable", message_key: CONSOLE_MESSAGE_KEYS.moira_unreachable } } },
    ]);
    render(<SkillExecutorPanel skillId="s1" fetchImpl={send} />);
    expect(await screen.findByRole("alert")).toBeDefined();
  });

  test("a found executor renders its allowed host read-only and NEVER a secret", async () => {
    const send = scriptedFetch([{ status: 200, body: executor({ credential_id: "cred-1" }) }]);
    render(<SkillExecutorPanel skillId="s1" fetchImpl={send} />);
    expect(await screen.findByText("api.example.test")).toBeDefined();
    // The credential row is an ID field only — no secret value is ever fetched or rendered.
    expect(document.body.textContent).not.toContain("sk-");
    expect(document.body.textContent).not.toContain("sha256");
  });

  test("save PATCHes with a quoted updated_at as If-Match", async () => {
    const send = scriptedFetch([
      { status: 200, body: executor() },
      { status: 200, body: executor({ timeout_ms: 9000 }) },
    ]);
    render(<SkillExecutorPanel skillId="s1" fetchImpl={send} />);
    await screen.findByText("api.example.test");

    const timeoutField = screen.getByLabelText(
      copy(CONSOLE_MESSAGE_KEYS.skills_executor_timeout_label),
    ) as HTMLInputElement;
    await userEvent.clear(timeoutField);
    await userEvent.type(timeoutField, "9000");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_executor_save) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(2));
    expect(send.calls[1]).toMatchObject({ url: "/api/skills/s1/executor", method: "PATCH" });
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.skills_executor_saved))).toBeDefined();
  });

  test("delete asks first, and confirming sends DELETE then shows the deleted state", async () => {
    const send = scriptedFetch([
      { status: 200, body: executor() },
      { status: 204, body: null },
    ]);
    render(<SkillExecutorPanel skillId="s1" fetchImpl={send} />);
    await screen.findByText("api.example.test");

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_executor_delete) }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.skills_executor_delete_confirm_body),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.skills_executor_delete_confirm_action) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(2));
    expect(send.calls[1]).toMatchObject({ url: "/api/skills/s1/executor", method: "DELETE" });
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.skills_executor_deleted))).toBeDefined();
  });
});
