// The `/settings/llm` organisms.
//
// ============================================================================
// NOTHING HERE ASSERTS ON AN ENGLISH LITERAL
// ============================================================================
//
// `expect(screen.getByText("Add provider"))` passes in two worlds: the one where
// the component resolved the string through `t()`, and the one where it
// hardcoded the same string and never called `t()` at all. It also passes when
// the key is missing from the catalog, because `t()` falls back to the key.
//
// So every assertion compares rendered text to `CONSOLE_CATALOG[key].message`,
// read from the catalog module at test time — the standard the shipped organism
// tests already hold to.

import { describe, expect, test } from "bun:test";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import { LOCAL_VLLM_BASE_URL, type LlmProviderView } from "@/lib/llm-view";
import {
  ConnectVllmPanel,
  outcomeLabelKey,
  stepLabelKey,
} from "@/modules/llm/ConnectVllmPanel";
import { missingStepKeys, ProviderChainPanel } from "@/modules/llm/ProviderChainPanel";
import { ProviderForm } from "@/modules/llm/ProviderForm";
import { ProviderList, statusKey } from "@/modules/llm/ProviderList";
import { readFailure } from "@/modules/llm/request";

/** The catalog's English for a key. Never a literal in this file. */
const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

/**
 * A field, found by its label.
 *
 * The `Label` atom appends a required marker and the visually-hidden
 * " (required)" suffix, so a required field's ACCESSIBLE NAME is never exactly
 * the catalog string. Matching on a prefix keeps the assertion about the
 * catalog rather than about the atom's decoration, which is what
 * `FormField.test.tsx` already pins character for character.
 */
function field(key: string): HTMLElement {
  const label = copy(key).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return screen.getByLabelText(new RegExp(`^${label}`));
}

const MODEL_KEY = "Qwen3-4B";

function provider(overrides: Partial<LlmProviderView> = {}): LlmProviderView {
  return {
    id: "provider-1",
    displayName: "Local endpoint",
    providerType: "open_ai_compatible",
    baseUrl: "https://local-llm.example.test/v1",
    status: "active",
    version: 1,
    models: [],
    keyRows: [],
    policies: [],
    ...overrides,
  };
}

function model(overrides: Record<string, unknown> = {}) {
  return {
    id: "model-1",
    modelKey: MODEL_KEY,
    status: "active",
    version: 1,
    capabilities: { text: true },
    ...overrides,
  } as LlmProviderView["models"][number];
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

/* -------------------------------------------------------------------------- */
/* ConnectVllmPanel                                                           */
/* -------------------------------------------------------------------------- */

describe("ConnectVllmPanel — the shortcut", () => {
  test("the endpoint field is pre-filled from the constant, not from catalog copy", () => {
    // The catalog gate refuses any message containing a URL, and it is right to.
    // The address is a constant rendered as a FIELD VALUE.
    render(<ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} />);
    const endpoint = field(CONSOLE_MESSAGE_KEYS.llm_connect_endpoint_label) as HTMLInputElement;
    expect(endpoint.value).toBe(LOCAL_VLLM_BASE_URL);
    expect(copy(CONSOLE_MESSAGE_KEYS.llm_connect_endpoint_label)).not.toContain("http");
  });

  test("DISCOVERY WRITES NOTHING — the first click is a probe and only a probe", async () => {
    const send = scriptedFetch([{ status: 200, body: { base_url: "https://x.test/v1", models: [MODEL_KEY] } }]);
    render(<ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} fetchImpl={send} />);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/llm/connect-vllm");
    expect(send.calls[0]?.body).toEqual({ action: "discover", base_url: LOCAL_VLLM_BASE_URL });
    // Exactly one call. A shortcut that provisioned on the first click would
    // leave a provider row behind for a mistyped address.
    expect(
      send.calls.filter(
        (call) => (call.body as { action?: string } | undefined)?.action === "connect",
      ),
    ).toEqual([]);
  });

  test("the discovered models are OFFERED, so nobody types a model id", async () => {
    const send = scriptedFetch([
      { status: 200, body: { base_url: "https://x.test/v1", models: [MODEL_KEY, "other-model"] } },
    ]);
    render(<ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} fetchImpl={send} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) }),
    );

    const first = await screen.findByRole("checkbox", { name: MODEL_KEY });
    expect((first as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "other-model" }) as HTMLInputElement).checked).toBe(
      false,
    );
    // The canonical base came BACK from the server and replaced what was typed.
    expect(
      (field(CONSOLE_MESSAGE_KEYS.llm_connect_endpoint_label) as HTMLInputElement).value,
    ).toBe("https://x.test/v1");
  });

  test("the second click sends the selected models and renders the trace", async () => {
    const send = scriptedFetch([
      { status: 200, body: { base_url: "https://x.test/v1", models: [MODEL_KEY] } },
      {
        status: 201,
        body: {
          provider_id: "provider-1",
          trace: [
            { step: "provider", outcome: "created", detail: "provider-1" },
            { step: "provider_credential", outcome: "created", detail: "cred-1" },
            { step: "provider_enable", outcome: "skipped", detail: null },
          ],
        },
      },
    ]);
    render(<ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} fetchImpl={send} />);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) }),
    );
    await screen.findByRole("checkbox", { name: MODEL_KEY });
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(2));
    expect(send.calls[1]?.body).toEqual({
      action: "connect",
      base_url: "https://x.test/v1",
      model_keys: [MODEL_KEY],
    });

    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.llm_connect_done))).toBeDefined();
    const trace = screen.getByRole("list");
    expect(within(trace).getAllByRole("listitem")).toHaveLength(3);
    expect(within(trace).getByText(copy(CONSOLE_MESSAGE_KEYS.llm_step_provider))).toBeDefined();
    expect(within(trace).getByText(copy(CONSOLE_MESSAGE_KEYS.llm_outcome_skipped))).toBeDefined();
  });

  test("AN UNREACHABLE ENDPOINT RENDERS A KEYED MESSAGE AND THE PAGE STILL WORKS", async () => {
    // A laptop with the tunnel down. The panel must stay usable.
    const send = scriptedFetch([
      {
        status: 502,
        body: {
          error: {
            code: "discovery_failed",
            message_key: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable,
          },
        },
      },
    ]);
    render(<ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} fetchImpl={send} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) }),
    );

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(copy(CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable));
    // No model list appeared, and the button is still there to try again.
    expect(screen.queryAllByRole("checkbox")).toEqual([]);
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) }),
    ).toBeDefined();
  });

  test("A PARTIAL CHAIN SHOWS WHAT WAS ALREADY WRITTEN", async () => {
    const send = scriptedFetch([
      { status: 200, body: { base_url: "https://x.test/v1", models: [MODEL_KEY] } },
      {
        status: 409,
        body: {
          error: {
            code: "llm_provisioning_failed",
            message_key: CONSOLE_MESSAGE_KEYS.llm_connect_step_failed,
            step: "provider_credential",
            state: { providerId: "provider-1" },
            trace: [
              { step: "provider", outcome: "created", detail: "provider-1" },
              { step: "provider_model", outcome: "created", detail: MODEL_KEY },
            ],
          },
        },
      },
    ]);
    render(<ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} fetchImpl={send} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit) }),
    );
    await screen.findByRole("checkbox", { name: MODEL_KEY });
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_connect_submit) }),
    );

    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.llm_connect_step_failed),
    );
    // The two rows that DO exist are named, so a second click is informed.
    const trace = screen.getByRole("list");
    expect(within(trace).getAllByRole("listitem")).toHaveLength(2);
    expect(within(trace).getByText(copy(CONSOLE_MESSAGE_KEYS.llm_step_provider_model))).toBeDefined();
  });

  test("every step and outcome the server can send has a catalog label", () => {
    for (const step of [
      "provider",
      "provider_model",
      "provider_credential",
      "provider_enable",
      "routing_policy",
    ]) {
      expect(copy(stepLabelKey(step)), step).not.toBe("");
    }
    expect(stepLabelKey("a-step-added-later")).toBe(CONSOLE_MESSAGE_KEYS.llm_step_unknown);
    for (const outcome of ["created", "reused", "enabled", "skipped"]) {
      expect(copy(outcomeLabelKey(outcome)), outcome).not.toBe("");
    }
    // `enabled` is its own label and NOT a synonym for `reused`. A step that
    // found a disabled row and turned it back on did not reuse anything routing
    // would have accepted, and reporting it as reuse is how the shortcut
    // announced success on a deployment no prompt could reach.
    expect(outcomeLabelKey("enabled")).not.toBe(outcomeLabelKey("reused"));
    expect(copy(outcomeLabelKey("enabled"))).not.toBe(copy(outcomeLabelKey("reused")));
  });
});

/* -------------------------------------------------------------------------- */
/* ProviderForm                                                               */
/* -------------------------------------------------------------------------- */

describe("ProviderForm — adding a provider by hand", () => {
  test("submission is blocked until both fields carry something", async () => {
    render(<ProviderForm fetchImpl={scriptedFetch([{ status: 201, body: { id: "p" } }])} />);
    const submit = screen.getByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.llm_add_provider_submit),
    });
    expect((submit as HTMLButtonElement).disabled).toBe(true);

    await userEvent.type(
      field(CONSOLE_MESSAGE_KEYS.llm_provider_name_label),
      "Local endpoint",
    );
    expect((submit as HTMLButtonElement).disabled).toBe(true);

    await userEvent.type(
      field(CONSOLE_MESSAGE_KEYS.llm_provider_base_url_label),
      "https://local-llm.example.test",
    );
    expect((submit as HTMLButtonElement).disabled).toBe(false);
  });

  test("it posts to the console's own endpoint, never to Moira", async () => {
    const send = scriptedFetch([{ status: 201, body: { id: "provider-1" } }]);
    const created: string[] = [];
    render(<ProviderForm fetchImpl={send} onCreated={() => created.push("yes")} />);

    await userEvent.type(
      field(CONSOLE_MESSAGE_KEYS.llm_provider_name_label),
      "Local endpoint",
    );
    await userEvent.type(
      field(CONSOLE_MESSAGE_KEYS.llm_provider_base_url_label),
      "https://local-llm.example.test",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_add_provider_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/llm/providers");
    expect(send.calls[0]?.method).toBe("POST");
    expect(send.calls[0]?.body).toEqual({
      display_name: "Local endpoint",
      base_url: "https://local-llm.example.test",
    });
    expect(created).toEqual(["yes"]);
    expect(
      await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.llm_provider_created)),
    ).toBeDefined();
  });

  test("a keyed refusal from the server is rendered as its own message", async () => {
    const send = scriptedFetch([
      {
        status: 400,
        body: {
          error: {
            code: "invalid_request",
            message_key: CONSOLE_MESSAGE_KEYS.llm_base_url_userinfo_rejected,
          },
        },
      },
    ]);
    render(<ProviderForm fetchImpl={send} />);
    await userEvent.type(
      field(CONSOLE_MESSAGE_KEYS.llm_provider_name_label),
      "x",
    );
    await userEvent.type(
      field(CONSOLE_MESSAGE_KEYS.llm_provider_base_url_label),
      "https://user:pass@local-llm.example.test",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_add_provider_submit) }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      copy(CONSOLE_MESSAGE_KEYS.llm_base_url_userinfo_rejected),
    );
  });
});

/* -------------------------------------------------------------------------- */
/* ProviderChainPanel                                                         */
/* -------------------------------------------------------------------------- */

describe("ProviderChainPanel — what exists and what is still missing", () => {
  test("a bare provider names all three remaining steps", () => {
    render(<ProviderChainPanel provider={provider()} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.llm_chain_incomplete))).toBeDefined();
    for (const key of [
      CONSOLE_MESSAGE_KEYS.llm_step_model_missing,
      CONSOLE_MESSAGE_KEYS.llm_key_row_missing,
      CONSOLE_MESSAGE_KEYS.llm_policy_missing,
    ]) {
      expect(screen.getByText(copy(key)), key).toBeDefined();
    }
    // The provider is active, so "enable" is NOT listed.
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.llm_step_enable_missing))).toBeNull();
  });

  test("a complete chain says so and lists nothing missing", () => {
    render(
      <ProviderChainPanel
        provider={provider({
          models: [model()],
          keyRows: [{ id: "cred-1", kind: "api_key", status: "active", version: 1 }],
          policies: [
            {
              id: "policy-1",
              routeId: "route-1",
              routeKey: "general",
              providerModelId: "model-1",
              priority: 100,
              weight: 1,
              status: "active",
              version: 1,
            },
          ],
        })}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.llm_chain_complete))).toBeDefined();
    expect(missingStepKeys(provider({ models: [model()] }))).not.toEqual([]);
  });

  test("a disabled provider names the enable step", () => {
    expect(missingStepKeys(provider({ status: "disabled" }))).toContain(
      CONSOLE_MESSAGE_KEYS.llm_step_enable_missing,
    );
  });

  test("THE KEY FIELD IS OPTIONAL — a blank one still creates the row", async () => {
    // The whole point of the credential step for a keyless endpoint. The button
    // is never disabled by an empty field, and the body carries an empty string
    // that the SERVER turns into a placeholder.
    const send = scriptedFetch([{ status: 201, body: { id: "cred-1" } }]);
    render(<ProviderChainPanel provider={provider()} fetchImpl={send} />);

    const submit = screen.getByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.llm_add_key_row_submit),
    });
    expect((submit as HTMLButtonElement).disabled).toBe(false);
    await userEvent.click(submit);

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/llm/providers/provider-1/credentials");
    expect(send.calls[0]?.body).toEqual({ api_key: "" });
  });

  test("adding a model posts the identifier and clears the field", async () => {
    const send = scriptedFetch([{ status: 201, body: { id: "model-1" } }]);
    render(<ProviderChainPanel provider={provider()} fetchImpl={send} />);

    const modelField = field(CONSOLE_MESSAGE_KEYS.llm_add_model_label);
    await userEvent.type(modelField, MODEL_KEY);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_add_model_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/llm/providers/provider-1/models");
    expect(send.calls[0]?.body).toEqual({ model_key: MODEL_KEY });
    await waitFor(() => expect((modelField as HTMLInputElement).value).toBe(""));
  });

  test("routing sends only a model id — the route is the server's to choose", async () => {
    const send = scriptedFetch([{ status: 201, body: { id: "policy-1" } }]);
    render(<ProviderChainPanel provider={provider({ models: [model()] })} fetchImpl={send} />);

    await userEvent.selectOptions(
      field(CONSOLE_MESSAGE_KEYS.llm_bind_routing_model_label),
      "model-1",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_bind_routing_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/llm/providers/provider-1/routing");
    // No `route_id`. A body that could name a route could bind a policy to one
    // the operator was never shown.
    expect(send.calls[0]?.body).toEqual({ provider_model_id: "model-1" });
  });

  test("routing cannot be submitted before a model is chosen", () => {
    render(<ProviderChainPanel provider={provider({ models: [model()] })} />);
    expect(
      (
        screen.getByRole("button", {
          name: copy(CONSOLE_MESSAGE_KEYS.llm_bind_routing_submit),
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
  });

  test("A PROVIDER WHOSE ONLY MODEL IS DISABLED IS NOT READY", () => {
    // `chainReadiness` tested `status !== "deleted"`, and `provider_models.status`
    // cannot BE "deleted" — migration 0003 constrains the column to
    // ('active','disabled','deprecated') and disable writes 'disabled'. So the
    // test degraded to "a row exists" while Moira's routing joins
    // `pm.status = 'active'`. The screen said "Ready: a prompt can reach this
    // provider" directly under the model's own Disabled badge, and the first
    // prompt then failed with a routing error naming neither.
    const halfDead = provider({
      models: [model({ status: "disabled" })],
      keyRows: [{ id: "cred-1", kind: "api_key", status: "active", version: 1 }],
      policies: [
        {
          id: "policy-1",
          routeId: "route-1",
          routeKey: "general",
          providerModelId: "model-1",
          priority: 100,
          weight: 1,
          status: "active",
          version: 1,
        },
      ],
    });
    render(<ProviderChainPanel provider={halfDead} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.llm_chain_incomplete))).toBeDefined();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.llm_chain_complete))).toBeNull();
    expect(missingStepKeys(halfDead)).toContain(CONSOLE_MESSAGE_KEYS.llm_step_model_missing);
    // A 'deprecated' model, set outside this console, is the same answer.
    expect(missingStepKeys(provider({ models: [model({ status: "deprecated" })] }))).toContain(
      CONSOLE_MESSAGE_KEYS.llm_step_model_missing,
    );
  });

  test("a disabled model is not offered for binding — that policy would never route", () => {
    render(
      <ProviderChainPanel
        provider={provider({ models: [model({ status: "disabled" }), model({ id: "model-2", modelKey: "live" })] })}
      />,
    );
    const select = field(CONSOLE_MESSAGE_KEYS.llm_bind_routing_model_label) as HTMLSelectElement;
    const values = Array.from(select.options).map((option) => option.value);
    // The placeholder plus the one active model, and not the disabled one:
    // Moira stores a policy pointing at it and then never selects it.
    expect(values).toEqual(["", "model-2"]);
  });
});

/* -------------------------------------------------------------------------- */
/* ProviderList                                                               */
/* -------------------------------------------------------------------------- */

describe("ProviderList — the inventory and its undo", () => {
  test("an empty deployment gets an honest empty state, not a blank panel", () => {
    render(<ProviderList providers={[]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.llm_providers_empty))).toBeDefined();
  });

  test("a provider with nothing hanging off it says so three times over", () => {
    render(<ProviderList providers={[provider()]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.llm_models_empty))).toBeDefined();
    // Both the credential and the routing empty states are present.
    expect(screen.getAllByText(copy(CONSOLE_MESSAGE_KEYS.llm_key_row_missing)).length).toBeGreaterThan(0);
    expect(screen.getAllByText(copy(CONSOLE_MESSAGE_KEYS.llm_policy_missing)).length).toBeGreaterThan(0);
  });

  test("NO SECRET MATERIAL IS RENDERED FOR A CREDENTIAL ROW", () => {
    // The view model carries no mask and no fingerprint, so there is nothing to
    // render even by accident. This asserts the rendered surface as well.
    render(
      <ProviderList
        providers={[
          provider({ keyRows: [{ id: "cred-1", kind: "api_key", status: "active", version: 1 }] }),
        ]}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.llm_key_row_present))).toBeDefined();
    expect(document.body.textContent).not.toContain("sk-");
    expect(document.body.textContent).not.toContain("sha256");
  });

  test("disabling a provider calls DELETE on its own path", async () => {
    const send = scriptedFetch([{ status: 200, body: { id: "provider-1" } }]);
    const changed: string[] = [];
    render(
      <ProviderList
        providers={[provider()]}
        fetchImpl={send}
        onChanged={() => changed.push("yes")}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_disable_provider) }),
    );
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({ url: "/api/llm/providers/provider-1", method: "DELETE" });
    expect(changed).toEqual(["yes"]);
  });

  test("an already-disabled provider offers no second disable", () => {
    render(<ProviderList providers={[provider({ status: "disabled" })]} />);
    expect(
      (
        screen.getByRole("button", {
          name: copy(CONSOLE_MESSAGE_KEYS.llm_disable_provider),
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    expect(screen.getAllByText(copy(CONSOLE_MESSAGE_KEYS.llm_status_disabled)).length).toBeGreaterThan(
      0,
    );
  });

  test("A DISABLED ROW OFFERS THE WAY BACK, and it is the same path POSTed", async () => {
    // "Disable is reversible" is the stated justification for choosing disable
    // over delete. It was false for three of the four row types: the client's
    // `enableProviderModel`, `enableProviderCredential` and `enableRoutingPolicy`
    // had no caller anywhere, so the only undo this screen offered had no undo
    // of its own — and re-adding a model with the same identifier collides with
    // the disabled row still holding the partial unique index.
    const send = scriptedFetch([{ status: 200, body: { id: "model-1" } }]);
    const changed: string[] = [];
    render(
      <ProviderList
        providers={[provider({ models: [model({ status: "disabled" })] })]}
        fetchImpl={send}
        onChanged={() => changed.push("yes")}
      />,
    );

    // One control, and it says what it will do. Not a disabled "Disable" beside
    // an enabled "Enable".
    expect(screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_disable_model) })).toBeNull();
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_enable_model) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/llm/providers/provider-1/models/model-1",
      method: "POST",
    });
    expect(changed).toEqual(["yes"]);
  });

  test("an active row still offers only the disable control", () => {
    render(<ProviderList providers={[provider({ models: [model()] })]} />);
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_disable_model) }),
    ).toBeDefined();
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_enable_model) }),
    ).toBeNull();
  });

  test("the credential row and the routing policy get the same two-state control", async () => {
    const send = scriptedFetch([{ status: 200, body: { id: "x" } }]);
    render(
      <ProviderList
        providers={[
          provider({
            keyRows: [{ id: "cred-1", kind: "api_key", status: "disabled", version: 1 }],
            policies: [
              {
                id: "policy-1",
                routeId: "route-1",
                routeKey: "general",
                providerModelId: "model-1",
                priority: 100,
                weight: 1,
                status: "disabled",
                version: 1,
              },
            ],
          }),
        ]}
        fetchImpl={send}
      />,
    );

    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_enable_key_row) }),
    ).toBeDefined();
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.llm_enable_policy) }),
    );
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/llm/providers/provider-1/routing/policy-1",
      method: "POST",
    });
  });

  test("statusKey maps active and everything else to two distinct keys", () => {
    expect(statusKey("active")).toBe(CONSOLE_MESSAGE_KEYS.llm_status_active);
    expect(statusKey("deleted")).toBe(CONSOLE_MESSAGE_KEYS.llm_status_disabled);
  });
});

/* -------------------------------------------------------------------------- */
/* readFailure — the two shapes the BFF answers with                          */
/* -------------------------------------------------------------------------- */

describe("readFailure understands both refusal shapes", () => {
  test("the console's own keyed refusal", () => {
    expect(
      readFailure({ error: { code: "not_found", message_key: "console.llm.model_not_found" } }),
    ).toMatchObject({ messageKey: "console.llm.model_not_found", step: null });
  });

  test("a narrowed Moira failure keeps its own copy and its step", () => {
    expect(
      readFailure({
        error: {
          kind: "api",
          status: 409,
          code: "resource_version_conflict",
          remedy: "resolve_conflict",
          retryable: false,
          step: "routing_policy",
          text: { messageKey: "moira.error.conflict", message: "prose", messageArgs: {} },
        },
      }),
    ).toMatchObject({ messageKey: "moira.error.conflict", message: "prose", step: "routing_policy" });
  });

  test("an unreadable body still yields a renderable key", () => {
    expect(readFailure(undefined).messageKey).toBe(CONSOLE_MESSAGE_KEYS.llm_request_failed);
    expect(readFailure("<html>proxy error</html>").messageKey).toBe(
      CONSOLE_MESSAGE_KEYS.llm_request_failed,
    );
  });
});
