// The wizard coordinator, driven end to end through its fetch seam.
//
// What this file owns (acceptance criteria of the setup-wizard item):
//   * the claim step is UNREACHABLE until the provision response confirms an
//     enabled provider bound to the trusted issuer with a non-empty allow-list
//     (`reachableSetupStep` is the gate, not free-form local state);
//   * `admin_claim_domain_not_allowed` renders the ACTIONABLE instruction —
//     never a generic banner — returns the operator to the auth-settings step
//     with the form state preserved, and focuses the allow-list field.
//
// Copy is always compared against the catalog, never a literal — see
// `SignInPanel.test.tsx`'s header for why.

import { describe, expect, test } from "bun:test";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { SetupProvisioningState } from "@/lib/setup-steps";
import { SetupWizard, type SetupViewModel } from "@/modules/setup/SetupWizard";

const K = CONSOLE_MESSAGE_KEYS;

/** The catalog's English for a key. Never a literal in this file. */
const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

const ISSUER_ID = "11111111-1111-4111-8111-111111111111";
const PROVIDER_ID = "22222222-2222-4222-8222-222222222222";

const COMPLETE_STATE: SetupProvisioningState = {
  trustedJwtIssuerId: ISSUER_ID,
  trustedJwtIssuerVersion: 1,
  providerId: PROVIDER_ID,
  providerVersion: 2,
  providerTrustedJwtIssuerId: ISSUER_ID,
  providerEnabled: true,
  allowedEmailDomains: ["example.com"],
  consoleSecretStored: true,
};

const READY: SetupViewModel = { kind: "ready", claimed: false, methods: [] };

interface RecordedCall {
  readonly url: string;
  readonly method: string;
  readonly body: Record<string, unknown> | null;
}

interface ScriptedRoutes {
  /** Response for `POST /api/setup {action: "provision"}`. */
  provision?: { status: number; body: unknown };
  /** Response for `POST /api/setup {action: "claim"}`. */
  claim?: { status: number; body: unknown };
  /** Response for `GET /api/auth/get-session`. */
  session?: { status: number; body: unknown };
}

function makeFetch(routes: ScriptedRoutes) {
  const calls: RecordedCall[] = [];
  const fetchImpl = (async (url: string, init?: RequestInit) => {
    const body =
      init?.body === undefined ? null : (JSON.parse(String(init.body)) as Record<string, unknown>);
    calls.push({ url, method: init?.method ?? "GET", body });

    let scripted: { status: number; body: unknown } | undefined;
    if (url === "/api/auth/get-session") {
      scripted = routes.session ?? { status: 200, body: null };
    } else if (url === "/api/setup" && body?.["action"] === "provision") {
      scripted = routes.provision;
    } else if (url === "/api/setup" && body?.["action"] === "claim") {
      scripted = routes.claim;
    }
    return new Response(JSON.stringify(scripted?.body ?? {}), {
      status: scripted?.status ?? 500,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;
  return { calls, fetchImpl };
}

function field(labelKey: string): HTMLInputElement {
  return screen.getByLabelText((name) => name.startsWith(copy(labelKey))) as HTMLInputElement;
}

const claimButton = () => screen.queryByRole("button", { name: copy(K.setup_claim_button) });

/** Walk the wizard from welcome through a submitted provision form. */
async function provisionThroughForm(): Promise<void> {
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: copy(K.setup_welcome_continue) }));
  await user.type(field(K.setup_auth_display_name_label), "Google Workspace");
  await user.type(field(K.setup_auth_client_id_label), "client-123.apps.example");
  await user.type(field(K.setup_auth_client_secret_label), "unit-secret-3f81b2");
  await user.type(
    field(K.setup_auth_discovery_url_label),
    "https://accounts.google.com/.well-known/openid-configuration",
  );
  await user.type(field(K.setup_auth_allowed_domains_label), "example.com");
  await user.click(screen.getByRole("button", { name: copy(K.setup_auth_submit) }));
}

describe("the claim step is unreachable until the gate conditions are CONFIRMED", () => {
  test("a fresh wizard renders welcome, and no claim control exists anywhere", () => {
    render(<SetupWizard view={READY} fetchImpl={makeFetch({}).fetchImpl} navigate={() => {}} />);
    expect(screen.getByText(copy(K.setup_welcome_heading))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
    // The step list names the claim step, but only as navigation state.
    expect(screen.getByRole("navigation", { name: copy(K.setup_steps_label) })).toBeInTheDocument();
  });

  test("a provision response with the provider NOT enabled does not unlock the claim", async () => {
    const { fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: {
          state: { ...COMPLETE_STATE, providerEnabled: false },
          provider_id: "moira-console-idp",
        },
      },
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);
    await provisionThroughForm();

    expect(await screen.findByText(copy(K.setup_auth_not_complete))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
    expect(screen.queryByText(copy(K.setup_sign_in_heading))).not.toBeInTheDocument();
  });

  test("a provision response with an EMPTY allow-list does not unlock the claim", async () => {
    const { fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: {
          state: { ...COMPLETE_STATE, allowedEmailDomains: [] },
          provider_id: "moira-console-idp",
        },
      },
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);
    await provisionThroughForm();

    expect(await screen.findByText(copy(K.setup_auth_not_complete))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
  });

  test("a confirmed provision plus a signed-in session reaches the claim step", async () => {
    const { calls, fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" },
      },
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);

    expect(claimButton()).toBeNull();
    await provisionThroughForm();

    // Provisioning confirmed -> the session probe runs -> the claim unlocks.
    expect(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        copy(K.setup_claim_signed_in_as).replaceAll("{email}", "ops@example.com"),
      ),
    ).toBeInTheDocument();
    // The probe went to the console's own session endpoint, once.
    expect(calls.filter((call) => call.url === "/api/auth/get-session")).toHaveLength(1);
  });

  test("without a session, the sign-in stage renders instead of the claim", async () => {
    const { fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" },
      },
      session: { status: 200, body: null },
    });
    render(
      <SetupWizard
        view={{
          kind: "ready",
          claimed: false,
          methods: [
            {
              id: PROVIDER_ID,
              method: "google_oauth",
              displayName: "Google Workspace",
              interactive: true,
              clientIdConfigured: true,
              discoveryUrlConfigured: true,
              allowedEmailDomainCount: 1,
            },
          ],
        }}
        fetchImpl={fetchImpl}
        navigate={() => {}}
      />,
    );
    await provisionThroughForm();

    expect(await screen.findByText(copy(K.setup_sign_in_intro))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
  });
});

describe("admin_claim_domain_not_allowed is an actionable instruction, not a banner", () => {
  async function refuseClaim() {
    const { calls, fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" },
      },
      session: { status: 200, body: { user: { email: "ops@gmail.com" } } },
      claim: {
        status: 403,
        body: {
          error: {
            code: "admin_claim_domain_not_allowed",
            message_key: K.setup_claim_domain_not_allowed,
            message_args: { domain: "gmail.com" },
          },
        },
      },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);
    await provisionThroughForm();
    await userEvent.click(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    );
    return calls;
  }

  test("the refusal renders the three-part instruction with the offending domain", async () => {
    await refuseClaim();

    expect(
      await screen.findByText(copy(K.setup_domain_not_allowed_title)),
    ).toBeInTheDocument();
    expect(
      screen.getByText(copy(K.setup_domain_not_allowed_body).replaceAll("{domain}", "gmail.com")),
    ).toBeInTheDocument();
    expect(screen.getByText(copy(K.setup_domain_not_allowed_action))).toBeInTheDocument();

    // NOT the generic re-keyed error copy — that is the banner this criterion
    // forbids on this screen.
    expect(
      screen.queryByText(
        copy(K.setup_claim_domain_not_allowed).replaceAll("{domain}", "gmail.com"),
      ),
    ).not.toBeInTheDocument();
  });

  test("the wizard returns to auth-settings with state preserved and the allow-list focused", async () => {
    await refuseClaim();
    await screen.findByText(copy(K.setup_domain_not_allowed_title));

    // Back on the auth-settings step: the claim control is gone…
    expect(claimButton()).toBeNull();
    // …the envelope survived (the organism stayed mounted)…
    expect(field(K.setup_auth_display_name_label).value).toBe("Google Workspace");
    expect(field(K.setup_auth_allowed_domains_label).value).toBe("example.com");
    // …and the field the operator must edit holds focus.
    await waitFor(() =>
      expect(document.activeElement).toBe(field(K.setup_auth_allowed_domains_label)),
    );
  });
});

describe("the terminal states", () => {
  test("a successful claim lands on done with the admin email and the console link", async () => {
    const { fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" },
      },
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
      claim: { status: 201, body: { identity: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);
    await provisionThroughForm();
    await userEvent.click(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    );

    expect(await screen.findByText(copy(K.setup_done_heading))).toBeInTheDocument();
    expect(
      screen.getByText(copy(K.setup_done_admin_email).replaceAll("{email}", "ops@example.com")),
    ).toBeInTheDocument();
    const link = screen.getByRole("link", { name: copy(K.setup_done_open_console) });
    expect(link.getAttribute("href")).toBe("/");
  });

  test("an already-claimed deployment renders done, with the existing keyed copy", () => {
    render(
      <SetupWizard view={{ kind: "claimed" }} fetchImpl={makeFetch({}).fetchImpl} navigate={() => {}} />,
    );
    expect(screen.getByText(copy(K.setup_done_heading))).toBeInTheDocument();
    expect(screen.getByText(copy(K.setup_already_claimed))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
  });

  test("an unavailable window renders the keyed refusal and no wizard", () => {
    render(
      <SetupWizard
        view={{ kind: "unavailable", messageKey: K.setup_system_key_absent }}
        fetchImpl={makeFetch({}).fetchImpl}
        navigate={() => {}}
      />,
    );
    expect(screen.getByText(copy(K.setup_unavailable_heading))).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(copy(K.setup_system_key_absent));
    expect(screen.queryAllByRole("button")).toEqual([]);
  });
});
