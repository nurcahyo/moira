// The /admins organisms: grants, ownership, invitations.
//
// ============================================================================
// NOTHING HERE ASSERTS ON AN ENGLISH LITERAL
// ============================================================================
//
// `expect(screen.getByText("Owner"))` passes in two worlds: the one where the
// component resolved the string through `t()`, and the one where it hardcoded
// the same string and never called `t()` at all. It also passes when the key is
// missing from the catalog, because `t()` falls back to the key.
//
// So every assertion compares rendered text to `CONSOLE_CATALOG[key].message`,
// read from the catalog module at test time. That is the standard the shipped
// organism tests already hold to, and it is the reason a copy edit cannot
// silently break a component while its test stays green.

import { describe, expect, test } from "bun:test";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { AdminIdentityRecord, AdminInviteRecord } from "@/lib/types";
import { AdminTable } from "@/modules/admins/AdminTable";
import { InviteList, inviteStateKey, isWithdrawable } from "@/modules/admins/InviteList";
import {
  gateVerdict,
  invitedDomain,
  InviteAdminForm,
  type InviteProviderPolicy,
} from "@/modules/admins/InviteAdminForm";
import { TransferPrimaryPanel } from "@/modules/admins/TransferPrimaryPanel";

/** The catalog's English for a key. Never a literal in this file. */
const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

function grant(overrides: Partial<AdminIdentityRecord> = {}): AdminIdentityRecord {
  return {
    id: "grant-1",
    issuer: "https://console.test/idp/google",
    subject: "sub-1",
    email: "owner@corp.test",
    email_verified: true,
    granted_scopes: ["moira:admin"],
    status: "active",
    created_at: "2026-07-01T00:00:00Z",
    version: 2,
    notice: { message_key: "moira.notice.admin_identity_claimed", message: "Granted." },
    is_primary: false,
    ...overrides,
  };
}

function invite(overrides: Partial<AdminInviteRecord> = {}): AdminInviteRecord {
  return {
    id: "invite-1",
    constraint: "email",
    value: "colleague@corp.test",
    status: "pending",
    expired: false,
    expires_at: "2026-08-01T00:00:00Z",
    created_at: "2026-07-31T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

/* -------------------------------------------------------------------------- */

describe("AdminTable — one row is one GRANT, not one person (finding F24)", () => {
  test("it leads with email, because every other column is the same on every row", () => {
    // `issuer` is the CONSOLE's own string on every grant, so an issuer column
    // would show one constant value; `subject` is an opaque IdP identifier.
    // Email is the only human-identifiable attribute, which is why D5 makes it
    // required.
    render(
      <AdminTable
        identities={[grant({ id: "a", email: "first@corp.test" })]}
        busyId={null}
        onTransfer={() => {}}
        onRevoke={() => {}}
      />,
    );
    const headers = screen.getAllByRole("columnheader").map((cell) => cell.textContent);
    expect(headers[0]).toBe(copy(CONSOLE_MESSAGE_KEYS.admins_column_email));
    expect(screen.getByText("first@corp.test")).toBeDefined();
    // The console's own issuer is not rendered anywhere: it identifies nobody.
    expect(screen.queryByText("https://console.test/idp/google")).toBeNull();
  });

  test("two grants for one human render as two rows, and neither is merged", () => {
    // The F24 shape: same person, two providers, two `(issuer, subject)` pairs.
    // A table that de-duplicated by email would be claiming a person-level
    // identity the data model does not have — and revoking "the" row would leave
    // the other live.
    render(
      <AdminTable
        identities={[
          grant({ id: "a", subject: "google-1", email: "same@corp.test" }),
          grant({ id: "b", subject: "github-1", email: "same@corp.test" }),
        ]}
        busyId={null}
        onTransfer={() => {}}
        onRevoke={() => {}}
      />,
    );
    expect(screen.getAllByText("same@corp.test")).toHaveLength(2);
  });

  test("the owner row states the rule instead of offering a control that 409s", () => {
    // Decision D-F20: `revoke_grant` clears `is_primary` and the last-primary
    // guard refuses that, so the owner's grant cannot be revoked at all. A
    // rendered-but-failing button would teach the operator that the console is
    // unreliable rather than that the model is deliberate.
    render(
      <AdminTable
        identities={[grant({ is_primary: true })]}
        busyId={null}
        onTransfer={() => {}}
        onRevoke={() => {}}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_owner_not_revocable))).toBeDefined();
    expect(screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_revoke) })).toBeNull();
    // And no transfer control either: transferring to the current owner is a
    // request whose whole effect is already true.
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_transfer) }),
    ).toBeNull();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_owner_badge))).toBeDefined();
  });

  test("a non-owner row offers both controls", () => {
    render(
      <AdminTable
        identities={[grant({ is_primary: false })]}
        busyId={null}
        onTransfer={() => {}}
        onRevoke={() => {}}
      />,
    );
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_transfer) }),
    ).toBeDefined();
    expect(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_revoke) }),
    ).toBeDefined();
  });

  test("the empty state is catalogue copy, not a blank table", () => {
    render(<AdminTable identities={[]} busyId={null} onTransfer={() => {}} onRevoke={() => {}} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_empty))).toBeDefined();
    expect(screen.queryByRole("table")).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */

describe("TransferPrimaryPanel — transfer is ONE request", () => {
  test("confirming a transfer sends exactly one PATCH", async () => {
    // Plan 09's body says two calls: promote, then demote the actor. After
    // PR #39 `set_primary` demotes every other active primary in the same
    // transaction, so a second call would demote the person just promoted or
    // 409 on a stale version. This asserts the COUNT, which is the part that
    // would silently regress.
    const calls: Array<{ url: string; method: string }> = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), method: init?.method ?? "GET" });
      return new Response(JSON.stringify(grant({ is_primary: true })), { status: 200 });
    }) as unknown as typeof fetch;

    render(
      <TransferPrimaryPanel
        identities={[grant({ id: "target", email: "next@corp.test" })]}
        fetchImpl={fetchImpl}
      />,
    );

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_transfer) }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_transfer_confirm_action) }),
    );

    expect(calls).toHaveLength(1);
    expect(calls[0]!.method).toBe("PATCH");
    expect(calls[0]!.url).toBe("/api/admins/identities/target");
  });

  test("revocation is a DELETE to the same resource", async () => {
    const calls: string[] = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push(`${init?.method ?? "GET"} ${String(url)}`);
      return new Response(JSON.stringify(grant({ status: "revoked" })), { status: 200 });
    }) as unknown as typeof fetch;

    render(<TransferPrimaryPanel identities={[grant({ id: "target" })]} fetchImpl={fetchImpl} />);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_revoke) }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_revoke_confirm_action) }),
    );

    expect(calls).toEqual(["DELETE /api/admins/identities/target"]);
  });

  test("a refusal renders MOIRA's key, not a generic banner", async () => {
    // `admin_identity_not_primary` is the constructible authorization-denial
    // case — a live grant with `is_primary = false` meeting
    // `require_primary_actor`. The panel must surface that key rather than
    // collapsing every 4xx into one message.
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          error: {
            kind: "api",
            code: "admin_identity_not_primary",
            status: 403,
            remedy: "denied",
            retryable: false,
            text: {
              messageKey: "moira.error.admin_identity_not_primary",
              message_key: "moira.error.admin_identity_not_primary",
              message: "only a primary admin identity may manage other admin identities",
            },
          },
        }),
        { status: 403 },
      )) as unknown as typeof fetch;

    render(<TransferPrimaryPanel identities={[grant({ id: "target" })]} fetchImpl={fetchImpl} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_transfer) }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_transfer_confirm_action) }),
    );

    const region = screen.getByRole("status", {
      name: copy(CONSOLE_MESSAGE_KEYS.admins_activity_label),
    });
    // No console catalogue entry for this Moira key, so `t()` falls through to
    // the server's own message — which is the documented resolution order.
    expect(region.textContent).toContain("only a primary admin identity");
  });

  test("the live region exists BEFORE it is populated", () => {
    // A live region created and filled in the same tick is frequently missed by
    // assistive technology, which is why the shipped a11y standard requires the
    // region to be present from first render.
    render(<TransferPrimaryPanel identities={[grant()]} />);
    expect(
      screen.getByRole("status", { name: copy(CONSOLE_MESSAGE_KEYS.admins_activity_label) }),
    ).toBeDefined();
  });

  test("the per-grant note is rendered, not implied", () => {
    render(<TransferPrimaryPanel identities={[grant()]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_per_grant_note))).toBeDefined();
  });
});

/* -------------------------------------------------------------------------- */

describe("InviteAdminForm — the gate blocks, warns, and says which", () => {
  const oneProvider: InviteProviderPolicy = {
    enabledProviderCount: 1,
    allowedEmailDomains: ["corp.test"],
  };

  test("invitedDomain reads the domain half in both modes", () => {
    expect(invitedDomain("email", "Person@Corp.Test")).toBe("corp.test");
    expect(invitedDomain("domain", " Corp.Test ")).toBe("corp.test");
    expect(invitedDomain("email", "not-an-address")).toBeNull();
    expect(invitedDomain("email", "trailing@")).toBeNull();
    expect(invitedDomain("email", "")).toBeNull();
  });

  test("zero providers BLOCKS — nobody could sign in at all", () => {
    const verdict = gateVerdict(
      { enabledProviderCount: 0, allowedEmailDomains: [] },
      "email",
      "a@corp.test",
    );
    expect(verdict).toEqual({
      kind: "block",
      messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_no_enabled_provider,
    });
  });

  test("one provider BLOCKS an uncovered domain — the union provably equals the governing row", () => {
    expect(gateVerdict(oneProvider, "email", "person@other.test")).toEqual({
      kind: "block",
      messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_domain_not_in_allow_list,
    });
    expect(gateVerdict(oneProvider, "email", "person@corp.test")).toEqual({ kind: "allow" });
  });

  test("TWO providers WARN, never block — the console cannot reproduce the resolution", () => {
    // Blocker W5-B11. `PublicAuthMethod` carries neither `trusted_jwt_issuer_id`
    // nor `created_at`, so the console cannot tell which row `admission_policy`
    // resolves; the union is strictly wider than the governing row, and under
    // F23 it can be narrower in the other direction. A block here would be the
    // exact stranding the gate exists to prevent, wearing the gate's own name.
    const two: InviteProviderPolicy = {
      enabledProviderCount: 2,
      allowedEmailDomains: ["corp.test"],
    };
    expect(gateVerdict(two, "email", "person@definitely-not-allowed.test")).toEqual({
      kind: "warn",
      messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_multi_provider_warning,
    });
  });

  test("the warning is rendered and the submit control stays enabled", async () => {
    render(
      <InviteAdminForm
        policy={{ enabledProviderCount: 2, allowedEmailDomains: ["corp.test"] }}
        inviteBaseUrl="https://console.test/invite"
      />,
    );
    expect(
      screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_invite_multi_provider_warning)),
    ).toBeDefined();
    const submit = screen.getByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_submit),
    });
    expect(submit.hasAttribute("disabled")).toBe(false);
  });

  test("a blocked verdict disables submission and says so", () => {
    render(
      <InviteAdminForm
        policy={{ enabledProviderCount: 0, allowedEmailDomains: [] }}
        inviteBaseUrl="https://console.test/invite"
      />,
    );
    expect(
      screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_invite_no_enabled_provider)),
    ).toBeDefined();
    const submit = screen.getByRole("button", {
      name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_submit),
    });
    expect(submit.hasAttribute("disabled")).toBe(true);
  });

  test("a successful create shows the token once; a replay shows the replay copy", async () => {
    // `secret === null` is the NORMAL idempotent-replay case, not a failure.
    // A UI that treated it as one would report a correct operation as broken, on
    // the retry path, which is where people already suspect something went
    // wrong.
    const envelope = (secret: string | null) => ({
      resource: invite(),
      secret_retrievable: false,
      notice: { message_key: "moira.notice.admin_invite_created", message: "Created." },
      ...(secret === null ? {} : { secret }),
    });

    const withSecret = (async () =>
      new Response(JSON.stringify(envelope("plain-token-value")), {
        status: 201,
      })) as unknown as typeof fetch;

    const { unmount } = render(
      <InviteAdminForm
        policy={oneProvider}
        inviteBaseUrl="https://console.test/invite"
        fetchImpl={withSecret}
      />,
    );
    await userEvent.type(screen.getByRole("textbox"), "person@corp.test");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_submit) }),
    );
    expect(screen.getByText("plain-token-value")).toBeDefined();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.secret_shown_once))).toBeDefined();
    unmount();

    const replayed = (async () =>
      new Response(JSON.stringify(envelope(null)), { status: 201 })) as unknown as typeof fetch;
    render(
      <InviteAdminForm
        policy={oneProvider}
        inviteBaseUrl="https://console.test/invite"
        fetchImpl={replayed}
      />,
    );
    await userEvent.type(screen.getByRole("textbox"), "person@corp.test");
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_submit) }),
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.secret_already_shown))).toBeDefined();
  });

  test("an empty value is refused locally without a request", async () => {
    let called = 0;
    const fetchImpl = (async () => {
      called += 1;
      return new Response("{}", { status: 201 });
    }) as unknown as typeof fetch;

    render(
      <InviteAdminForm
        policy={oneProvider}
        inviteBaseUrl="https://console.test/invite"
        fetchImpl={fetchImpl}
      />,
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_submit) }),
    );
    expect(called).toBe(0);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_invite_value_required))).toBeDefined();
  });
});

/* -------------------------------------------------------------------------- */

describe("InviteList — `expired` is derived and beats `status`", () => {
  test("a pending-but-expired invitation reads as expired", () => {
    // `AdminInviteStatus` has no `expired` value, because nothing sweeps for it.
    // A list keying off `status` alone would show a dead invitation as waiting
    // to be redeemed, forever.
    expect(inviteStateKey(invite({ status: "pending", expired: true }))).toBe(
      CONSOLE_MESSAGE_KEYS.admins_invite_status_expired,
    );
    expect(inviteStateKey(invite({ status: "pending", expired: false }))).toBe(
      CONSOLE_MESSAGE_KEYS.admins_invite_status_pending,
    );
    expect(inviteStateKey(invite({ status: "consumed" }))).toBe(
      CONSOLE_MESSAGE_KEYS.admins_invite_status_consumed,
    );
    expect(inviteStateKey(invite({ status: "revoked" }))).toBe(
      CONSOLE_MESSAGE_KEYS.admins_invite_status_revoked,
    );
  });

  test("only a live pending invitation can be withdrawn", () => {
    expect(isWithdrawable(invite())).toBe(true);
    expect(isWithdrawable(invite({ expired: true }))).toBe(false);
    expect(isWithdrawable(invite({ status: "consumed" }))).toBe(false);
    expect(isWithdrawable(invite({ status: "revoked" }))).toBe(false);
  });

  test("withdrawing POSTs to the revoke sub-resource, never DELETEs the invitation", async () => {
    // There is no `DELETE /admin-invites/{id}` in the committed spec. A console
    // that invented one would 404 in front of an operator.
    const calls: string[] = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push(`${init?.method ?? "GET"} ${String(url)}`);
      return new Response(JSON.stringify(invite({ status: "revoked" })), { status: 200 });
    }) as unknown as typeof fetch;

    render(<InviteList invites={[invite()]} fetchImpl={fetchImpl} />);
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_revoke) }),
    );
    await userEvent.click(
      screen.getByRole("button", {
        name: copy(CONSOLE_MESSAGE_KEYS.admins_invite_revoke_confirm_action),
      }),
    );
    expect(calls).toEqual(["POST /api/admins/invites/invite-1/revoke"]);
  });

  test("the privacy note is on the screen that renders the directory", () => {
    render(<InviteList invites={[invite()]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.admins_invites_privacy_note))).toBeDefined();
    // `consumed_subject` is an opaque IdP identifier that names nobody to an
    // operator; rendering it would add a second personal identifier for no gain.
    const table = screen.getByRole("table");
    expect(within(table).queryByText("sub-1")).toBeNull();
  });
});
