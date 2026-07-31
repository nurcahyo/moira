// The invitee's side of the invitation flow.
//
// Every assertion compares rendered text to `CONSOLE_CATALOG[key].message`,
// never to an English literal — a literal assertion passes whether or not `t()`
// was ever called.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import { InviteAcceptPanel, tokenFromPathname } from "@/modules/invite/InviteAcceptPanel";
import {
  extractPropsInterfaces,
  readConsoleFile,
  secretPropViolations,
} from "../../support/secret-props-scan";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

const PATHNAME = "/invite/raw-invitation-token";

function panel(props: Partial<Parameters<typeof InviteAcceptPanel>[0]> = {}) {
  return (
    <InviteAcceptPanel
      constraint="email"
      value="colleague@corp.test"
      expiresAt="2026-08-01T00:00:00Z"
      signedIn
      resolvePathname={() => PATHNAME}
      {...props}
    />
  );
}

describe("the token comes from the URL, never from a prop", () => {
  test("tokenFromPathname reads the last segment of /invite/<token>", () => {
    expect(tokenFromPathname("/invite/abc123")).toBe("abc123");
    expect(tokenFromPathname("/invite/abc%2F123")).toBe("abc/123");
    // Anything that is not this route yields null rather than a guess: a guess
    // would send an arbitrary path segment to the redemption endpoint.
    expect(tokenFromPathname("/invite")).toBeNull();
    expect(tokenFromPathname("/admins/abc")).toBeNull();
    expect(tokenFromPathname("/")).toBeNull();
  });

  test("the props interface carries no token-shaped member", () => {
    // Asserted here as well as by `no-secret-props.test.ts` rule (a), because
    // this is the component where the temptation is strongest: the page already
    // has the token, and passing it down is one line.
    //
    // Parsed rather than grepped, so the assertion is about the INTERFACE and
    // not about the word `token` appearing in a doc comment — this file's header
    // says the word repeatedly, and a text scan would be satisfied by deleting
    // the explanation.
    const file = "modules/invite/InviteAcceptPanel.tsx";
    const declared = extractPropsInterfaces(file, readConsoleFile(file));
    expect(declared.map((entry) => entry.name)).toContain("InviteAcceptPanelProps");
    expect(secretPropViolations(declared)).toEqual([]);
    // And the members really parsed — an interface read as empty would make the
    // assertion above vacuous.
    const props = declared.find((entry) => entry.name === "InviteAcceptPanelProps")!;
    expect(props.members).toContain("constraint");
    expect(props.members.length).toBeGreaterThanOrEqual(5);
  });

  test("accepting posts to the redemption route derived from the path, with no body", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const fetchImpl = (async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), method: init?.method ?? "GET", body: init?.body });
      return new Response(
        JSON.stringify({
          notice: { message_key: "moira.notice.admin_invite_redeemed", message: "Redeemed." },
        }),
        { status: 201 },
      );
    }) as unknown as typeof fetch;

    render(panel({ fetchImpl }));
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    );

    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("/api/invite/raw-invitation-token/redeem");
    expect(calls[0]!.method).toBe("POST");
    // No body at all: duplicating the token into a payload would be a second
    // copy travelling a second route.
    expect(calls[0]!.body).toBeUndefined();
  });
});

describe("the three states", () => {
  test("without a session it explains rather than offering a control that cannot work", () => {
    render(panel({ signedIn: false }));
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.invite_sign_in_first))).toBeDefined();
    expect(
      screen.queryByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    ).toBeNull();
  });

  test("success renders the console's next step AND Moira's own notice", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          notice: {
            message_key: "moira.notice.admin_invite_redeemed",
            message: "Your admin identity has been granted.",
          },
        }),
        { status: 201 },
      )) as unknown as typeof fetch;

    render(panel({ fetchImpl }));
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.invite_accepted))).toBeDefined();
    expect(screen.getByText("Your admin identity has been granted.")).toBeDefined();
  });

  test("the heading distinguishes an email invitation from a domain one", () => {
    const { unmount } = render(panel({ constraint: "email", value: "a@corp.test" }));
    expect(
      screen.getByText(
        copy(CONSOLE_MESSAGE_KEYS.invite_heading_email).replace("{value}", "a@corp.test"),
      ),
    ).toBeDefined();
    unmount();
    render(panel({ constraint: "domain", value: "corp.test" }));
    expect(
      screen.getByText(
        copy(CONSOLE_MESSAGE_KEYS.invite_heading_domain).replace("{value}", "corp.test"),
      ),
    ).toBeDefined();
  });
});

describe("the two refusals the console owns, and the ones it does not", () => {
  function refusal(code: string, message: string) {
    return (async () =>
      new Response(
        JSON.stringify({
          error: {
            kind: "api",
            code,
            status: 403,
            remedy: "denied",
            retryable: false,
            text: { messageKey: `moira.error.${code}`, message_key: `moira.error.${code}`, message },
          },
        }),
        { status: 403 },
      )) as unknown as typeof fetch;
  }

  test("admin_claim_domain_not_allowed becomes an actionable instruction", async () => {
    // Decision D3: an invitation is a scoping token, never a policy exemption.
    // The console owns this copy because the remedy — "ask whoever invited you
    // to add your domain, then use this link again" — is true only because a
    // policy-denied redemption does not consume the invitation, and Moira's own
    // message cannot say that.
    render(
      panel({
        fetchImpl: refusal("admin_claim_domain_not_allowed", "This email domain is not allowed."),
      }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.invite_domain_not_allowed))).toBeDefined();
    // The server's English is suppressed, not rendered alongside.
    expect(screen.queryByText("This email domain is not allowed.")).toBeNull();
  });

  test("admin_identity_already_claimed does NOT tell the reader they have admin", async () => {
    // Finding F24: `admin_identities` is keyed on (issuer, subject) with the
    // console's own issuer on every row, so the holder of that grant may be
    // somebody else entirely.
    render(
      panel({ fetchImpl: refusal("admin_identity_already_claimed", "Already claimed.") }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    );
    const rendered = copy(CONSOLE_MESSAGE_KEYS.invite_already_claimed);
    expect(screen.getByText(rendered)).toBeDefined();
    expect(rendered.toLowerCase()).not.toContain("you already have");
  });

  test("invite_email_mismatch keeps MOIRA's copy — its remedy is a new invitation", async () => {
    // Deliberately NOT conflated with the domain-policy refusal above: one is
    // fixed by changing the deployment's allow-list, the other by issuing a new
    // invitation. Moira separates the codes on exactly those grounds.
    render(
      panel({
        fetchImpl: refusal(
          "invite_email_mismatch",
          "this invitation was issued for a different email address",
        ),
      }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    );
    expect(
      screen.getByText("this invitation was issued for a different email address"),
    ).toBeDefined();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.invite_domain_not_allowed))).toBeNull();
  });

  test("a transport failure does not echo the thrown cause", async () => {
    const fetchImpl = (async () => {
      throw new Error("https://user:hunter2@moira.internal/api unreachable");
    }) as unknown as typeof fetch;
    render(panel({ fetchImpl }));
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.invite_accept) }),
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.invite_request_failed))).toBeDefined();
    expect(document.body.textContent).not.toContain("hunter2");
  });
});
