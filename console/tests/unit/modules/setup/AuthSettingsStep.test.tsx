// The auth-settings step, driven through its own fetch seam.
//
// Every copy assertion compares against `CONSOLE_CATALOG[key].message`, never a
// literal — the same rule `SignInPanel.test.tsx`'s header states: a literal
// passes both in the world where the component resolved it through `t()` and in
// the world where it hardcoded the string.
//
// What this file owns (acceptance criteria of the setup-wizard item):
//   * the client secret is WRITE-ONLY: never pre-filled, from any source;
//   * an empty allow-list BLOCKS the submission — nothing is sent;
//   * the step is not complete until all four conditions (Moira row bound,
//     console secret, enable, allow-list) are confirmed by the returned state;
//   * a forced secret-write failure renders incomplete with Retry AND Discard;
//   * the secret goes to `/api/setup` and to no other origin — the organism
//     cannot address Moira at all.

import { describe, expect, test } from "bun:test";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import {
  EMPTY_PROVISIONING_STATE,
  isProvisioningComplete,
  type SetupProvisioningState,
} from "@/lib/setup-steps";
import { AuthSettingsStep } from "@/modules/setup/AuthSettingsStep";
import type { SetupMethodSummary } from "@/modules/setup/setup-view";

const K = CONSOLE_MESSAGE_KEYS;

/** The catalog's English for a key. Never a literal in this file. */
const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

const ISSUER_ID = "11111111-1111-4111-8111-111111111111";
const PROVIDER_ID = "22222222-2222-4222-8222-222222222222";

/** Unmistakable, and asserted to reach exactly one URL. */
const CLIENT_SECRET = "oauth-client-secret-unit-fixture-9a4d02c7e1";

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

const EXISTING_METHOD: SetupMethodSummary = {
  id: PROVIDER_ID,
  method: "google_oauth",
  displayName: "Google Workspace",
  interactive: true,
  clientIdConfigured: true,
  discoveryUrlConfigured: true,
  allowedEmailDomainCount: 2,
};

interface RecordedCall {
  readonly url: string;
  readonly body: Record<string, unknown>;
}

/** A fetch stub that records every call and answers from a queue. */
function makeFetch(responses: ReadonlyArray<{ status: number; body: unknown }>) {
  const calls: RecordedCall[] = [];
  let next = 0;
  const fetchImpl = (async (url: string, init?: RequestInit) => {
    calls.push({
      url,
      body: init?.body === undefined ? {} : (JSON.parse(String(init.body)) as Record<string, unknown>),
    });
    const scripted = responses[Math.min(next, responses.length - 1)];
    next += 1;
    return new Response(JSON.stringify(scripted?.body ?? {}), {
      status: scripted?.status ?? 500,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;
  return { calls, fetchImpl };
}

/** A field by its label, tolerating the Label atom's required suffix. */
function field(labelKey: string): HTMLInputElement {
  return screen.getByLabelText((name) => name.startsWith(copy(labelKey))) as HTMLInputElement;
}

async function fillEverything(options: { readonly domains?: string } = {}): Promise<void> {
  const user = userEvent.setup();
  await user.type(field(K.setup_auth_display_name_label), "Google Workspace");
  await user.type(field(K.setup_auth_client_id_label), "client-123.apps.example");
  await user.type(field(K.setup_auth_client_secret_label), CLIENT_SECRET);
  await user.type(
    field(K.setup_auth_discovery_url_label),
    "https://accounts.google.com/.well-known/openid-configuration",
  );
  const domains = options.domains ?? "example.com";
  if (domains !== "") {
    await user.type(field(K.setup_auth_allowed_domains_label), domains);
  }
}

async function submit(): Promise<void> {
  await userEvent.click(screen.getByRole("button", { name: copy(K.setup_auth_submit) }));
}

describe("the client secret is write-only", () => {
  test("the field is a password input and is never pre-filled", () => {
    render(
      <AuthSettingsStep
        methods={[EXISTING_METHOD]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={makeFetch([]).fetchImpl}
      />,
    );
    const secret = field(K.setup_auth_client_secret_label);
    expect(secret.type).toBe("password");
    // The load-bearing half: an EXISTING provider row must not seed the field.
    // The console has no read-back path for the secret, so any value here could
    // only be an invented or a leaked one.
    expect(secret.value).toBe("");
  });

  test("a revisit shows the masked 'configured' value derived from row PRESENCE", () => {
    render(
      <AuthSettingsStep
        methods={[EXISTING_METHOD]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={makeFetch([]).fetchImpl}
      />,
    );
    expect(screen.getByText(copy(K.setup_auth_existing_configured))).toBeInTheDocument();
    expect(screen.getByText("Google Workspace")).toBeInTheDocument();
    // Presence, never a value: nothing secret-shaped is anywhere in the DOM.
    expect(document.body.textContent).not.toContain(CLIENT_SECRET);
  });

  test("a row with no client id gets no masked value", () => {
    render(
      <AuthSettingsStep
        methods={[{ ...EXISTING_METHOD, clientIdConfigured: false }]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={makeFetch([]).fetchImpl}
      />,
    );
    expect(screen.queryByText(copy(K.setup_auth_existing_configured))).not.toBeInTheDocument();
  });
});

describe("an empty submission is blocked, not sent", () => {
  test("empty allowed-domains blocks the submit entirely", async () => {
    const { calls, fetchImpl } = makeFetch([{ status: 201, body: {} }]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );
    await fillEverything({ domains: "" });
    await submit();

    // NOTHING went out — the refusal is client-side, before any write.
    expect(calls).toHaveLength(0);
    expect(screen.getByText(copy(K.setup_allowed_email_domains_required))).toBeInTheDocument();
    expect(screen.getByText(copy(K.setup_auth_form_incomplete))).toBeInTheDocument();
  });

  test("a wholly empty form is blocked with every per-field refusal", async () => {
    const { calls, fetchImpl } = makeFetch([{ status: 201, body: {} }]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );
    await submit();
    expect(calls).toHaveLength(0);
    for (const key of [
      K.setup_display_name_required,
      K.setup_client_id_required,
      K.setup_client_secret_required,
      K.setup_issuer_or_discovery_required,
      K.setup_allowed_email_domains_required,
    ]) {
      expect(screen.getByText(copy(key))).toBeInTheDocument();
    }
  });
});

describe("the step is not complete until all four conditions are confirmed", () => {
  const partials: ReadonlyArray<{ readonly what: string; readonly state: SetupProvisioningState }> =
    [
      {
        what: "the Moira row is not bound to the trusted issuer",
        state: { ...COMPLETE_STATE, providerTrustedJwtIssuerId: null },
      },
      {
        what: "the console-side secret was not stored",
        state: { ...COMPLETE_STATE, consoleSecretStored: false },
      },
      {
        what: "the provider was not enabled",
        state: { ...COMPLETE_STATE, providerEnabled: false },
      },
      {
        what: "the allow-list came back empty",
        state: { ...COMPLETE_STATE, allowedEmailDomainCount: 0 },
      },
    ];

  for (const partial of partials) {
    test(partial.what, async () => {
      const reported: SetupProvisioningState[] = [];
      const { fetchImpl } = makeFetch([
        { status: 201, body: { state: partial.state, provider_id: "moira-console-idp" } },
      ]);
      render(
        <AuthSettingsStep
          methods={[]}
          provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
          onProvisioned={(state) => reported.push(state)}
          fetchImpl={fetchImpl}
        />,
      );
      await fillEverything();
      await submit();

      // The returned state reaches the wizard, and it fails the gate — so the
      // component refuses to read a 2xx as done.
      await waitFor(() => expect(reported).toHaveLength(1));
      expect(isProvisioningComplete(reported[0]!)).toBe(false);
      expect(await screen.findByText(copy(K.setup_auth_not_complete))).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: copy(K.setup_auth_retry) }),
      ).toBeInTheDocument();
    });
  }

  test("a fully confirmed state completes silently", async () => {
    const reported: SetupProvisioningState[] = [];
    const { fetchImpl } = makeFetch([
      { status: 201, body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" } },
    ]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={(state) => reported.push(state)}
        fetchImpl={fetchImpl}
      />,
    );
    await fillEverything();
    await submit();

    await waitFor(() => expect(reported).toHaveLength(1));
    expect(isProvisioningComplete(reported[0]!)).toBe(true);
    expect(screen.queryByText(copy(K.setup_auth_not_complete))).not.toBeInTheDocument();
    // The envelope drops the secret once it is sealed console-side.
    expect(field(K.setup_auth_client_secret_label).value).toBe("");
  });
});

describe("a forced secret-write failure is incomplete, with Retry AND Discard", () => {
  test("remedy retry_or_discard_provider renders both controls and the keyed remedy", async () => {
    const partial = { ...COMPLETE_STATE, providerEnabled: false, consoleSecretStored: false };
    const { fetchImpl } = makeFetch([
      {
        status: 500,
        body: {
          error: {
            code: "setup_provisioning_failed",
            step: "store_console_secret",
            remedy: "retry_or_discard_provider",
            message_key: K.auth_provider_secret_write_failed,
            message_args: null,
            requires_client_secret_re_entry: true,
            state: partial,
            moira: null,
          },
        },
      },
    ]);
    const reported: SetupProvisioningState[] = [];
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={(state) => reported.push(state)}
        fetchImpl={fetchImpl}
      />,
    );
    await fillEverything();
    await submit();

    // The i18n remedy that already exists for this step, resolved via t().
    expect(
      await screen.findByText(copy(K.auth_provider_secret_write_failed)),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: copy(K.setup_auth_retry) })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: copy(K.setup_auth_discard) })).toBeInTheDocument();

    // The recorded partial state reached the wizard and it is INCOMPLETE.
    await waitFor(() => expect(reported).toHaveLength(1));
    expect(isProvisioningComplete(reported[0]!)).toBe(false);
  });

  test("an enable failure offers Retry but NOT Discard, and never re-asks for the secret", async () => {
    const partial = { ...COMPLETE_STATE, providerEnabled: false };
    const { calls, fetchImpl } = makeFetch([
      {
        status: 409,
        body: {
          error: {
            code: "setup_provisioning_failed",
            step: "enable_auth_provider",
            remedy: "retry_enable_no_secret_re_entry",
            message_key: K.auth_provider_enable_failed,
            message_args: null,
            requires_client_secret_re_entry: false,
            state: partial,
            moira: null,
          },
        },
      },
      { status: 201, body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" } },
    ]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );
    await fillEverything();
    await submit();

    expect(await screen.findByText(copy(K.auth_provider_enable_failed))).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: copy(K.setup_auth_discard) })).not.toBeInTheDocument();

    // Retry resumes with the RECORDED state and the SAME submission id — the
    // operator is never asked for the secret again; the held envelope resends
    // it unprompted.
    await userEvent.click(screen.getByRole("button", { name: copy(K.setup_auth_retry) }));
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[1]!.body["resume"]).toEqual(partial as unknown as Record<string, unknown>);
    expect(calls[1]!.body["submission_id"]).toBe(calls[0]!.body["submission_id"]);
    expect(calls[1]!.body["client_secret"]).toBe(CLIENT_SECRET);
  });
});

describe("the secret reaches the BFF and nothing else", () => {
  test("every request goes to /api/setup; no Moira origin is ever addressed", async () => {
    const { calls, fetchImpl } = makeFetch([
      { status: 201, body: { state: COMPLETE_STATE, provider_id: "moira-console-idp" } },
    ]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );
    await fillEverything();
    await submit();

    await waitFor(() => expect(calls).toHaveLength(1));
    // The one door. A relative same-origin path — the organism cannot name a
    // Moira URL, so the secret cannot reach an outbound Moira request from here.
    expect(calls.map((call) => call.url)).toEqual(["/api/setup"]);
    expect(calls[0]!.body["client_secret"]).toBe(CLIENT_SECRET);
    expect(calls[0]!.body["action"]).toBe("provision");
  });
});

describe("a re-save of an already-provisioned provider — the domain-refusal remedy", () => {
  async function fillWithoutSecret(domains: string): Promise<void> {
    const user = userEvent.setup();
    await user.type(field(K.setup_auth_display_name_label), "Google Workspace");
    await user.type(field(K.setup_auth_client_id_label), "client-123.apps.example");
    await user.type(
      field(K.setup_auth_discovery_url_label),
      "https://accounts.google.com/.well-known/openid-configuration",
    );
    await user.type(field(K.setup_auth_allowed_domains_label), domains);
  }

  test("saves WITHOUT re-entering the sealed secret, resuming against the existing row", async () => {
    // The instruction the domain-refusal copy gives — "add {domain} below, save
    // the provider again, then retry the claim" — must be followable by an
    // operator who no longer holds the OAuth client secret: the console sealed
    // it on the earlier save, and `consoleSecretStored` says so.
    const updated = { ...COMPLETE_STATE, allowedEmailDomainCount: 2 };
    const { calls, fetchImpl } = makeFetch([
      { status: 201, body: { state: updated, provider_id: "moira-console-idp" } },
    ]);
    const reported: SetupProvisioningState[] = [];
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={COMPLETE_STATE}
        refusedDomain="gmail.com"
        slug={null}
        onProvisioned={(state) => reported.push(state)}
        fetchImpl={fetchImpl}
      />,
    );
    await fillWithoutSecret("example.com, gmail.com");
    await submit();

    // NOT blocked on the empty secret field, and nothing asked for it.
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(screen.queryByText(copy(K.setup_client_secret_required))).not.toBeInTheDocument();
    // The save RESUMES against the known row — this is what routes the BFF to
    // the PATCH path instead of a second create.
    expect(calls[0]!.body["resume"]).toEqual(COMPLETE_STATE as unknown as Record<string, unknown>);
    expect(calls[0]!.body["client_secret"]).toBe("");
    expect(calls[0]!.body["allowed_email_domains"]).toEqual(["example.com", "gmail.com"]);

    await waitFor(() => expect(reported).toHaveLength(1));
    expect(isProvisioningComplete(reported[0]!)).toBe(true);
  });

  test("a FRESH form still requires the secret — nothing provisioned means nothing sealed", async () => {
    const { calls, fetchImpl } = makeFetch([{ status: 201, body: {} }]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );
    await fillWithoutSecret("example.com");
    await submit();
    expect(calls).toHaveLength(0);
    expect(screen.getByText(copy(K.setup_client_secret_required))).toBeInTheDocument();
  });

  test("a failed re-save keeps the complete provider in the wizard's model", async () => {
    // The compounding failure the review named: a failed save used to report a
    // PARTIAL state upward and erase the already-working provider from the
    // wizard for the rest of the session.
    const { fetchImpl } = makeFetch([
      {
        status: 400,
        body: {
          error: {
            code: "setup_provisioning_failed",
            step: "update_auth_provider",
            remedy: "retry",
            message_key: K.auth_provider_update_failed,
            message_args: null,
            requires_client_secret_re_entry: false,
            state: COMPLETE_STATE,
            moira: null,
          },
        },
      },
    ]);
    const reported: SetupProvisioningState[] = [];
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={COMPLETE_STATE}
        refusedDomain="gmail.com"
        slug={null}
        onProvisioned={(state) => reported.push(state)}
        fetchImpl={fetchImpl}
      />,
    );
    await fillWithoutSecret("example.com, gmail.com");
    await submit();

    expect(await screen.findByText(copy(K.auth_provider_update_failed))).toBeInTheDocument();
    // The COMPLETE state was not re-reported: the wizard's model keeps the
    // provider it already confirmed, and the cursor stays on this failure.
    expect(reported).toHaveLength(0);
  });
});

describe("a claim-time domain refusal focuses the allow-list with the instruction", () => {
  test("refusedDomain renders the actionable copy and moves focus", async () => {
    const { rerender } = render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={makeFetch([]).fetchImpl}
      />,
    );
    expect(screen.queryByText(copy(K.setup_domain_not_allowed_title))).not.toBeInTheDocument();

    rerender(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain="gmail.com"
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={makeFetch([]).fetchImpl}
      />,
    );

    expect(screen.getByText(copy(K.setup_domain_not_allowed_title))).toBeInTheDocument();
    expect(
      screen.getByText(copy(K.setup_domain_not_allowed_body).replaceAll("{domain}", "gmail.com")),
    ).toBeInTheDocument();
    expect(screen.getByText(copy(K.setup_domain_not_allowed_action))).toBeInTheDocument();
    await waitFor(() =>
      expect(document.activeElement).toBe(field(K.setup_auth_allowed_domains_label)),
    );
  });
});

/* -------------------------------------------------------------------------- */
/* The provider slug, in the UI rather than only in the BFF                    */
/* -------------------------------------------------------------------------- */
//
// The slug picks the console-issuer namespace a provider is registered under. It
// is a permanent choice — a URL path segment and part of the issuer string Moira
// pins tokens to — so an operator who wants their own name for the provider has
// to be able to make it BEFORE the first save, not discover it afterwards. The
// form had no field for it at all, so every wizard provision resolved to the
// incumbent console issuer and the only way to choose otherwise was a
// hand-crafted POST.
//
// What it is NOT is an escape hatch from a provider enabled with credentials
// nobody can sign in with. The console supports one enabled sign-in provider at
// a time, so that run is refused `409 setup_single_enabled_provider_only` before
// anything is written (`tests/unit/api/setup-route.test.ts` owns that property).
// The tests below are about the field's plumbing: what it sends, what it seeds
// from, and what it invalidates when it changes.

describe("the wizard can provision under a chosen provider slug", () => {
  test("a typed slug reaches the BFF on the provision body", async () => {
    const { calls, fetchImpl } = makeFetch([
      { status: 201, body: { state: COMPLETE_STATE, provider_id: "moira-console-idp-recovery" } },
    ]);
    render(
      <AuthSettingsStep
        methods={[EXISTING_METHOD]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );

    await userEvent.type(field(K.setup_auth_slug_label), "recovery");
    await fillEverything();
    await submit();

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]!.body["slug"]).toBe("recovery");
  });

  test("an empty slug is OMITTED, not sent as an empty string", async () => {
    // `readSlug` treats an absent slug as "the incumbent" and refuses `""` with
    // `setup_provider_slug_invalid`, so sending the empty field verbatim would
    // turn every ordinary first run into a keyed 400.
    const { calls, fetchImpl } = makeFetch([{ status: 201, body: { state: COMPLETE_STATE } }]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );

    await fillEverything();
    await submit();

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(Object.keys(calls[0]!.body)).not.toContain("slug");
  });

  test("the field is SEEDED from the run's namespace, so a reload stays in it", () => {
    // The OAuth round trip and every reload go through the server, and the
    // server's echo of `?slug=` is the only thing that survives them. A field
    // that always started empty would quietly re-point the next save at the
    // incumbent issuer.
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug="recovery"
        onProvisioned={() => {}}
        fetchImpl={makeFetch([]).fetchImpl}
      />,
    );
    expect(field(K.setup_auth_slug_label).value).toBe("recovery");
  });

  test("changing the slug drops the resume state — a new namespace is a CREATE", async () => {
    // A new slug means a NEW row. `resume` names a provider the BFF derived for
    // the OLD console issuer, and replaying it under a new slug is the
    // disagreement answered `409 setup_resume_state_conflict`.
    const { calls, fetchImpl } = makeFetch([{ status: 201, body: { state: COMPLETE_STATE } }]);
    render(
      <AuthSettingsStep
        methods={[EXISTING_METHOD]}
        // A revisit: the wizard holds a complete state for the incumbent row.
        provisioning={COMPLETE_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );

    await userEvent.type(field(K.setup_auth_slug_label), "recovery");
    await fillEverything();
    await submit();

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]!.body["slug"]).toBe("recovery");
    expect(Object.keys(calls[0]!.body)).not.toContain("resume");
  });

  test("an UNCHANGED slug still resumes against the row the wizard holds", async () => {
    // The control for the test above: dropping `resume` whenever a slug field
    // exists would break the domain-refusal re-save, which depends on resuming
    // against the derived row (and on `consoleSecretStored`, which is what lets
    // the operator save without re-typing a secret the console holds).
    const { calls, fetchImpl } = makeFetch([{ status: 201, body: { state: COMPLETE_STATE } }]);
    render(
      <AuthSettingsStep
        methods={[EXISTING_METHOD]}
        provisioning={COMPLETE_STATE}
        refusedDomain="gmail.com"
        slug="recovery"
        onProvisioned={() => {}}
        fetchImpl={fetchImpl}
      />,
    );

    await fillEverything();
    await submit();

    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]!.body["slug"]).toBe("recovery");
    expect(calls[0]!.body["resume"]).toEqual(
      COMPLETE_STATE as unknown as Record<string, unknown>,
    );
  });

  test("the slug provisioned under is reported upward with the state", async () => {
    // Everything after this step — the OAuth callback URL and the claim's
    // `admin_identities` namespace — has to stay in the namespace that was
    // actually written. This is the only place that fact exists.
    const reported: Array<string | null> = [];
    const { fetchImpl } = makeFetch([
      { status: 201, body: { state: COMPLETE_STATE, provider_id: "moira-console-idp-recovery" } },
    ]);
    render(
      <AuthSettingsStep
        methods={[]}
        provisioning={EMPTY_PROVISIONING_STATE}
        refusedDomain={null}
        slug={null}
        onProvisioned={(_state, _providerId, provisionedSlug) => reported.push(provisionedSlug)}
        fetchImpl={fetchImpl}
      />,
    );

    await userEvent.type(field(K.setup_auth_slug_label), "recovery");
    await fillEverything();
    await submit();

    await waitFor(() => expect(reported).toEqual(["recovery"]));
  });
});
