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
import { EMPTY_PROVISIONING_STATE, type SetupProvisioningState } from "@/lib/setup-steps";
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
  allowedEmailDomainCount: 1,
  consoleSecretStored: true,
};

const READY: SetupViewModel = {
  kind: "ready",
  claimed: false,
  slug: null,
  methods: [],
  provisioning: EMPTY_PROVISIONING_STATE,
  oauthProviderId: null,
};

/** The view a REVISIT of a provisioned-but-unclaimed deployment resolves. */
const READY_PROVISIONED: SetupViewModel = {
  kind: "ready",
  claimed: false,
  slug: null,
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
  provisioning: COMPLETE_STATE,
  oauthProviderId: "moira-console-idp",
};

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
  /** Response for `POST /api/auth/sign-in/oauth2`. */
  signIn?: { status: number; body: unknown };
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
    } else if (url === "/api/auth/sign-in/oauth2") {
      scripted = routes.signIn;
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
async function provisionThroughForm(slug?: string): Promise<void> {
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: copy(K.setup_welcome_continue) }));
  if (slug !== undefined) await user.type(field(K.setup_auth_slug_label), slug);
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
          state: { ...COMPLETE_STATE, allowedEmailDomainCount: 0 },
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
          slug: null,
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
          provisioning: EMPTY_PROVISIONING_STATE,
          oauthProviderId: null,
        }}
        fetchImpl={fetchImpl}
        navigate={() => {}}
      />,
    );
    await provisionThroughForm();

    expect(await screen.findByText(copy(K.setup_sign_in_intro))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
    // The stage is not a dead end: the provider row's sign-in button exists.
    expect(
      screen.getByRole("button", {
        name: copy(K.sign_in_button).replaceAll("{provider}", "Google Workspace"),
      }),
    ).toBeInTheDocument();
  });
});

describe("a FRESH deployment's sign-in stage is never a dead end", () => {
  // The server-rendered view of a fresh deployment carries NO method rows —
  // `GET /api/setup` ran before anything was provisioned, and nothing
  // re-fetches it after the in-session provision. The stage must still offer a
  // working control, or the operator is stranded on a heading with no buttons.

  test("after provisioning with an empty method list, one generic sign-in button drives the new provider", async () => {
    const { calls, fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" },
      },
      session: { status: 200, body: null },
      signIn: { status: 200, body: { url: "https://idp.example/authorize?state=abc" } },
    });
    const navigated: string[] = [];
    render(
      <SetupWizard view={READY} fetchImpl={fetchImpl} navigate={(url) => navigated.push(url)} />,
    );
    await provisionThroughForm();

    // The sign-in stage renders WITH a control, not just the intro copy.
    expect(await screen.findByText(copy(K.setup_sign_in_intro))).toBeInTheDocument();
    const button = screen.getByRole("button", { name: copy(K.sign_in_button_generic) });

    // The button drives the SAME Better Auth flow as /login, with the provider
    // id the provision response returned, and follows the redirect it gets.
    await userEvent.click(button);
    await waitFor(() => expect(navigated).toEqual(["https://idp.example/authorize?state=abc"]));
    const signIn = calls.find((call) => call.url === "/api/auth/sign-in/oauth2");
    expect(signIn).toBeDefined();
    expect(signIn!.method).toBe("POST");
    expect(signIn!.body).toEqual({ providerId: "moira-console-idp", callbackURL: "/setup" });
  });

  test("without an in-session provider id, the fallback button escapes to /login instead", async () => {
    // Defence in depth: even if the provision response carried no provider id,
    // the operator still gets a control — /login resolves the provider
    // server-side and offers the working buttons.
    const { calls, fetchImpl } = makeFetch({
      provision: { status: 201, body: { state: COMPLETE_STATE } },
      session: { status: 200, body: null },
    });
    const navigated: string[] = [];
    render(
      <SetupWizard view={READY} fetchImpl={fetchImpl} navigate={(url) => navigated.push(url)} />,
    );
    await provisionThroughForm();

    await userEvent.click(
      await screen.findByRole("button", { name: copy(K.sign_in_button_generic) }),
    );
    expect(navigated).toEqual(["/login"]);
    expect(calls.filter((call) => call.url === "/api/auth/sign-in/oauth2")).toHaveLength(0);
  });
});

describe("the sign-in step's controls do not lie about which provider they run", () => {
  // Every button on this step posts the SAME server-derived `oauthProviderId` —
  // the wizard provisions one provider under one console issuer. Rendering one
  // button per interactive row therefore rendered a control per row that all
  // ran the same provider: pressing "Continue with B" signed in through A.

  const rowB = {
    id: "44444444-4444-4444-8444-444444444444",
    method: "generic_oidc" as const,
    displayName: "Contractor IdP",
    interactive: true,
    clientIdConfigured: true,
    discoveryUrlConfigured: true,
    allowedEmailDomainCount: 1,
  };

  test("two interactive rows still yield ONE sign-in button, named after the provisioned row", async () => {
    const { fetchImpl } = makeFetch({ session: { status: 200, body: null } });
    render(
      <SetupWizard
        view={{
          ...READY_PROVISIONED,
          methods: [...READY_PROVISIONED.methods!, rowB],
        }}
        fetchImpl={fetchImpl}
        navigate={() => {}}
      />,
    );

    await screen.findByText(copy(K.setup_sign_in_intro));
    // Named after the row the provisioning state actually points at…
    expect(
      screen.getByRole("button", {
        name: copy(K.sign_in_button).replaceAll("{provider}", "Google Workspace"),
      }),
    ).toBeInTheDocument();
    // …and there is no second control claiming to run the other row.
    expect(
      screen.queryByRole("button", {
        name: copy(K.sign_in_button).replaceAll("{provider}", rowB.displayName),
      }),
    ).toBeNull();
  });

  test("a row the document does not hold gets the generic label, never another provider's name", async () => {
    // The provisioned row is not in the server-rendered list (a fresh
    // deployment's list predates the provision). Labelling the button with
    // whatever row IS in the list would be the same lie in a different shape.
    const { fetchImpl } = makeFetch({ session: { status: 200, body: null } });
    render(
      <SetupWizard
        view={{ ...READY_PROVISIONED, methods: [rowB] }}
        fetchImpl={fetchImpl}
        navigate={() => {}}
      />,
    );

    await screen.findByText(copy(K.setup_sign_in_intro));
    expect(
      screen.getByRole("button", { name: copy(K.sign_in_button_generic) }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", {
        name: copy(K.sign_in_button).replaceAll("{provider}", rowB.displayName),
      }),
    ).toBeNull();
  });
});

describe("a completed provision is not a one-way door", () => {
  // Once provisioning completes the wizard lands on sign-in, and a reload lands
  // there too. Without a control back to the form, an operator who mistyped a
  // discovery URL, a client id, or the client secret has nowhere to fix it —
  // the cursor clamp allowed backward movement all along, but nothing on screen
  // used it.

  test("the sign-in step offers a way back to the auth-settings form", async () => {
    const { fetchImpl } = makeFetch({ session: { status: 200, body: null } });
    render(<SetupWizard view={READY_PROVISIONED} fetchImpl={fetchImpl} navigate={() => {}} />);
    await screen.findByText(copy(K.setup_sign_in_intro));

    await userEvent.click(
      screen.getByRole("button", { name: copy(K.setup_sign_in_edit_settings) }),
    );

    // The form is on screen (not merely mounted-and-hidden) and the sign-in
    // surface is gone.
    expect(
      screen.getByText(copy(K.setup_auth_heading)).closest("div[hidden]"),
    ).toBeNull();
    expect(screen.queryByText(copy(K.setup_sign_in_intro))).toBeNull();
    expect(screen.getByRole("button", { name: copy(K.setup_auth_submit) })).toBeInTheDocument();
  });

  test("the claim step offers it too, and a re-save from there returns to sign-in", async () => {
    // The whole loop: back from the claim surface, correct the form, save, and
    // the wizard advances again — through the ordinary provision action, whose
    // target row the BFF derives. The back button is navigation, not a second
    // way to choose what gets written.
    const { calls, fetchImpl } = makeFetch({
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" },
      },
    });
    render(<SetupWizard view={READY_PROVISIONED} fetchImpl={fetchImpl} navigate={() => {}} />);
    await screen.findByRole("button", { name: copy(K.setup_claim_button) });

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: copy(K.setup_sign_in_edit_settings) }));
    expect(claimButton()).toBeNull();

    await user.type(field(K.setup_auth_display_name_label), "Google Workspace");
    await user.type(field(K.setup_auth_client_id_label), "client-123.apps.example");
    await user.type(
      field(K.setup_auth_discovery_url_label),
      "https://accounts.google.com/.well-known/openid-configuration",
    );
    await user.type(field(K.setup_auth_allowed_domains_label), "example.com");
    await user.click(screen.getByRole("button", { name: copy(K.setup_auth_submit) }));

    expect(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    ).toBeInTheDocument();

    const save = calls.find((call) => call.body?.["action"] === "provision");
    expect(save).toBeDefined();
    // No secret was typed and none was demanded: the console holds one for the
    // row it derived, which is the same fact the BFF checks server-side.
    expect(save!.body!["client_secret"]).toBe("");
    // The body carries the state as a HINT the server checks against its own —
    // it is not what selects the row.
    expect(save!.body!["resume"]).toMatchObject({ providerId: PROVIDER_ID });
  });
});

describe("the wizard survives the OAuth round trip: a FRESH DOCUMENT rehydrates from the view", () => {
  // Sign-in is `location.assign` to the IdP and back — the wizard REMOUNTS with
  // empty React state on return. Everything it needs must come back through the
  // server-derived view model, not through anything this component remembered.

  test("a remount with a rehydrated COMPLETE state and a session reaches the claim step directly", async () => {
    const { calls, fetchImpl } = makeFetch({
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY_PROVISIONED} fetchImpl={fetchImpl} navigate={() => {}} />);

    // No provisioning happened in THIS document — the claim is reachable purely
    // from the rehydrated state plus the session probe.
    expect(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    ).toBeInTheDocument();
    expect(calls.filter((call) => call.body?.["action"] === "provision")).toHaveLength(0);
  });

  test("a remount with a rehydrated COMPLETE state but no session offers sign-in, not welcome", async () => {
    const { fetchImpl } = makeFetch({ session: { status: 200, body: null } });
    render(<SetupWizard view={READY_PROVISIONED} fetchImpl={fetchImpl} navigate={() => {}} />);

    expect(await screen.findByText(copy(K.setup_sign_in_intro))).toBeInTheDocument();
    expect(claimButton()).toBeNull();
    // The welcome organism stays mounted (state preservation) but HIDDEN — the
    // active step is sign-in, not the beginning of the wizard.
    expect(screen.getByText(copy(K.setup_welcome_heading)).closest("div[hidden]")).not.toBeNull();
  });

  test("the claim request carries NO client-side state — the BFF derives its own", async () => {
    const { calls, fetchImpl } = makeFetch({
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
      claim: { status: 201, body: { identity: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY_PROVISIONED} fetchImpl={fetchImpl} navigate={() => {}} />);
    await userEvent.click(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    );

    expect(await screen.findByText(copy(K.setup_done_heading))).toBeInTheDocument();
    const claim = calls.find((call) => call.body?.["action"] === "claim");
    expect(claim).toBeDefined();
    expect(claim!.body).toEqual({ action: "claim" });
  });

  test("a rehydrated INCOMPLETE state still starts at welcome with no claim surface", () => {
    render(<SetupWizard view={READY} fetchImpl={makeFetch({}).fetchImpl} navigate={() => {}} />);
    expect(screen.getByText(copy(K.setup_welcome_heading))).toBeInTheDocument();
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

  test("the instruction is FOLLOWABLE: add the domain, save again without the secret, claim reopens", async () => {
    // The whole remedy, end to end. The re-save must not replay the finished
    // submission's idempotency keys (that is 409 idempotency_conflict), must
    // not demand a secret the console already sealed, and must reopen the
    // claim surface when it lands.
    const calls = await refuseClaim();
    await screen.findByText(copy(K.setup_domain_not_allowed_title));

    const user = userEvent.setup();
    await user.type(field(K.setup_auth_allowed_domains_label), ", gmail.com");
    await user.click(screen.getByRole("button", { name: copy(K.setup_auth_submit) }));

    // Back on the claim step, ready to retry.
    expect(
      await screen.findByRole("button", { name: copy(K.setup_claim_button) }),
    ).toBeInTheDocument();

    const saves = calls.filter((call) => call.body?.["action"] === "provision");
    expect(saves).toHaveLength(2);
    // No secret re-entry: the console still holds it, and the operator was not
    // asked for it (the field was emptied on the first success).
    expect(saves[1]!.body!["client_secret"]).toBe("");
    // The save resumes against the existing row instead of re-creating it…
    expect(saves[1]!.body!["resume"]).toMatchObject({ providerId: PROVIDER_ID });
    // …under a NEW submission, so the finished submission's idempotency keys
    // are never replayed against a changed body.
    expect(saves[1]!.body!["submission_id"]).not.toBe(saves[0]!.body!["submission_id"]);
    expect(saves[1]!.body!["allowed_email_domains"]).toEqual(["example.com", "gmail.com"]);
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

/* -------------------------------------------------------------------------- */
/* A RUN UNDER A REPLACEMENT SLUG STAYS IN ITS OWN NAMESPACE                  */
/* -------------------------------------------------------------------------- */
//
// Provisioning under a new slug is the console's only in-UI way out of a
// provider that was enabled with credentials nobody can sign in with: an enabled
// row may only be re-saved by somebody who authenticated through it, and a
// broken row authenticates nobody.
//
// Creating the row is not enough. The two steps AFTER it each carry a namespace,
// and neither can re-derive it: the sign-in callback returns through a full
// navigation, and the claim names an `admin_identities` namespace the BFF checks
// against the provider the session was actually established through. Drop the
// slug at either point and the operator lands back on the incumbent — the very
// row they are escaping — with a claim that is refused or, worse, granted in the
// wrong namespace.

describe("the namespace a provision wrote to survives the rest of the run", () => {
  test("the OAuth callback returns to /setup SCOPED to the slug", async () => {
    const redirects: string[] = [];
    const { calls, fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp-recovery" },
      },
      signIn: { status: 200, body: { url: "https://accounts.google.com/o/oauth2/v2/auth?x=1" } },
      session: { status: 200, body: null },
    });
    render(
      <SetupWizard view={READY} fetchImpl={fetchImpl} navigate={(url) => redirects.push(url)} />,
    );

    await provisionThroughForm("recovery");
    await screen.findByText(copy(K.setup_sign_in_intro));
    await userEvent.click(
      screen.getByRole("button", { name: copy(K.sign_in_button_generic) }),
    );

    await waitFor(() =>
      expect(calls.some((call) => call.url === "/api/auth/sign-in/oauth2")).toBe(true),
    );
    const signIn = calls.find((call) => call.url === "/api/auth/sign-in/oauth2")!;
    // The provider id is the one the BFF derived for THIS namespace...
    expect(signIn.body?.["providerId"]).toBe("moira-console-idp-recovery");
    // ...and the return trip carries the slug, because a full navigation is the
    // one thing no client-side state survives.
    expect(signIn.body?.["callbackURL"]).toBe("/setup?slug=recovery");
    await waitFor(() => expect(redirects).toHaveLength(1));
  });

  test("the claim names the slug's namespace, not the incumbent's", async () => {
    const { calls, fetchImpl } = makeFetch({
      provision: {
        status: 201,
        body: { state: COMPLETE_STATE, provider_id: "moira-console-idp-recovery" },
      },
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
      claim: { status: 201, body: { identity: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);

    await provisionThroughForm("recovery");
    await userEvent.click(await screen.findByRole("button", { name: copy(K.setup_claim_button) }));

    await waitFor(() =>
      expect(calls.some((call) => call.body?.["action"] === "claim")).toBe(true),
    );
    const claim = calls.find((call) => call.body?.["action"] === "claim")!;
    expect(claim.body?.["slug"]).toBe("recovery");
  });

  test("an ordinary run names no slug at all — absent is the incumbent", async () => {
    // The control. `readSlug` refuses `""`, so a wizard that always sent the
    // field would turn every first run into a keyed 400.
    const { calls, fetchImpl } = makeFetch({
      provision: { status: 201, body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" } },
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
      claim: { status: 201, body: { identity: { email: "ops@example.com" } } },
    });
    render(<SetupWizard view={READY} fetchImpl={fetchImpl} navigate={() => {}} />);

    await provisionThroughForm();
    await userEvent.click(await screen.findByRole("button", { name: copy(K.setup_claim_button) }));

    await waitFor(() =>
      expect(calls.some((call) => call.body?.["action"] === "claim")).toBe(true),
    );
    for (const call of calls.filter((entry) => entry.url === "/api/setup")) {
      expect(Object.keys(call.body ?? {})).not.toContain("slug");
    }
  });

  test("a rehydrated run keeps the SERVER's namespace across the round trip", async () => {
    // The revisit, which is what the OAuth callback actually is: the browser's
    // memory is gone and the server's echo of `?slug=` is all there is. A wizard
    // that seeded `null` here would claim in the incumbent's namespace with a
    // session established through the replacement provider — refused by the BFF's
    // issuer check, and unfixable from the UI.
    const { calls, fetchImpl } = makeFetch({
      session: { status: 200, body: { user: { email: "ops@example.com" } } },
      claim: { status: 201, body: { identity: { email: "ops@example.com" } } },
    });
    render(
      <SetupWizard
        view={{ ...READY_PROVISIONED, slug: "recovery" }}
        fetchImpl={fetchImpl}
        navigate={() => {}}
      />,
    );

    await userEvent.click(await screen.findByRole("button", { name: copy(K.setup_claim_button) }));
    await waitFor(() =>
      expect(calls.some((call) => call.body?.["action"] === "claim")).toBe(true),
    );
    expect(calls.find((call) => call.body?.["action"] === "claim")!.body?.["slug"]).toBe(
      "recovery",
    );
  });
});
