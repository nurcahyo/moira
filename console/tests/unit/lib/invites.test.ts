// Token handling: what leaves this module, and what never does.

import { describe, expect, test } from "bun:test";

import {
  DEFAULT_INVITE_EXPIRY_SECONDS,
  INVITE_PATH_PREFIX,
  MAX_INVITE_EXPIRY_SECONDS,
  MIN_INVITE_EXPIRY_SECONDS,
  inviteBaseUrl,
  inviteRedeemPath,
  isAcceptableInviteLifetime,
} from "@/lib/invite-bounds";
import {
  createInvite,
  inviteCreateIdempotencyKey,
  previewInvite,
  redeemIdempotencyKey,
  redeemInvite,
} from "@/lib/invites";
import { MoiraClient } from "@/lib/moira-client";
import type { ConsoleSessionIdentity } from "@/lib/moira-session";
import { MOIRA_STUB_BASE_URL, createMoiraStub } from "../../support/moira-stub";

const IDENTITY: ConsoleSessionIdentity = {
  email: "Colleague@Corp.Test",
  emailVerified: true,
  idpSubject: "idp-subject-1",
};

const TOKEN = "raw-invitation-token-value";

function clientWith(handlers: Parameters<typeof createMoiraStub>[0]) {
  const stub = createMoiraStub(handlers);
  const client = new MoiraClient({
    baseUrl: MOIRA_STUB_BASE_URL,
    bearerToken: () => "invitee-jwt",
    fetch: stub.fetch,
  });
  return { stub, client };
}

describe("the bounds are mirrored, and the default sits inside them", () => {
  test("the constants match Moira's", () => {
    expect(MIN_INVITE_EXPIRY_SECONDS).toBe(60);
    expect(MAX_INVITE_EXPIRY_SECONDS).toBe(259_200);
  });

  test("the form's default is acceptable, which a literal in the form would not guarantee", () => {
    expect(isAcceptableInviteLifetime(DEFAULT_INVITE_EXPIRY_SECONDS)).toBe(true);
  });
});

describe("link construction", () => {
  test("the base URL is the origin plus the public path, with no double slash", () => {
    expect(inviteBaseUrl("https://console.example")).toBe(
      `https://console.example${INVITE_PATH_PREFIX}`,
    );
    expect(inviteBaseUrl("https://console.example/")).toBe(
      `https://console.example${INVITE_PATH_PREFIX}`,
    );
  });

  test("the redemption path percent-encodes the token", () => {
    // A token is opaque; if one ever contained a `/` an unencoded path would
    // address a different route entirely.
    expect(inviteRedeemPath("a/b")).toBe("/api/invite/a%2Fb/redeem");
  });
});

describe("idempotency keys carry no token", () => {
  test("the create key is derived from the invite's identity", () => {
    expect(inviteCreateIdempotencyKey("email", " Person@Corp.Test ", "nonce-1")).toBe(
      "admin-invite:email:person@corp.test:nonce-1",
    );
  });

  test("the redeem key is derived from the IdP subject, NOT from the token", () => {
    // A key echoing the token would put it into a header, an idempotency ledger
    // row, and every access log along the way — which is the one thing the body
    // placement exists to prevent.
    const key = redeemIdempotencyKey(IDENTITY);
    expect(key).toBe("admin-invite-redeem:idp-subject-1");
    expect(key).not.toContain(TOKEN);
  });
});

describe("the exchanges put the token in the BODY and return something else", () => {
  test("preview sends no credential and echoes nothing back", async () => {
    const { stub, client } = clientWith({
      "POST /api/v1/admin/admin-invites/preview": () => ({
        status: 200,
        body: { constraint: "email", value: "colleague@corp.test", expires_at: "2026-08-01T00:00:00Z" },
      }),
    });

    const result = await previewInvite(client, TOKEN);

    expect(stub.requests[0]!.url).not.toContain(TOKEN);
    expect(stub.bodyOf("POST /api/v1/admin/admin-invites/preview")).toEqual({ token: TOKEN });
    expect(JSON.stringify(result)).not.toContain(TOKEN);
  });

  test("redeem asserts email and email_verified from the SESSION, unmodified", async () => {
    // Not forced to `true`: Moira refuses an unverified address with
    // `403 admin_claim_email_not_verified`, and a console that asserted `true`
    // here would be lying to the server about the one claim the grant's policy
    // rests on.
    const { stub, client } = clientWith({
      "POST /api/v1/admin/admin-invites/redeem": () => ({ status: 201, body: { id: "grant-1" } }),
    });

    await redeemInvite(
      client,
      TOKEN,
      { ...IDENTITY, emailVerified: false },
      redeemIdempotencyKey(IDENTITY),
    );

    expect(stub.bodyOf("POST /api/v1/admin/admin-invites/redeem")).toEqual({
      token: TOKEN,
      email: "Colleague@Corp.Test",
      email_verified: false,
    });
    expect(stub.requests[0]!.headers["Authorization"]).toBe("Bearer invitee-jwt");
    expect(Object.keys(stub.requests[0]!.headers)).not.toContain("X-Moira-System-Key");
  });

  test("create is a pass-through — the required notice is not rebuilt away", async () => {
    // `AdminInviteSecretResponse` carries a REQUIRED `notice` that
    // `ApiKeySecretResponse` does not, and every rebuild of that object is a
    // chance to drop the one string meant to be rendered to the operator.
    const envelope = {
      resource: { id: "invite-1" },
      secret_retrievable: false,
      notice: { message_key: "moira.notice.admin_invite_created", message: "Created." },
      secret: "plain",
    };
    const { client } = clientWith({
      "POST /api/v1/admin/admin-invites": () => ({ status: 201, body: envelope }),
    });
    const result = await createInvite(
      client,
      { constraint: "email", value: "a@corp.test", expires_in_seconds: 3600 },
      "idem-1",
    );
    expect(result).toEqual(envelope as never);
  });
});

describe("the module logs nothing", () => {
  test("no console.* call appears in its source", () => {
    // The property the header claims, asserted rather than trusted: the two
    // exchange functions are the only place the raw token is in scope on the
    // server, so a stray `console.debug(token)` here would put it in every
    // deployment's log aggregator.
    const source = require("node:fs").readFileSync(
      new URL("../../../lib/invites.ts", import.meta.url).pathname,
      "utf8",
    ) as string;
    const code = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
    expect(/\bconsole\s*\.\s*(log|info|warn|error|debug|trace)\s*\(/.test(code)).toBe(false);
  });
});
