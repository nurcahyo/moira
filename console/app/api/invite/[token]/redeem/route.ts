// `POST /api/invite/{token}/redeem` — the invitee redeems their invitation.
//
// ============================================================================
// THE TOKEN IS IN THE PATH, AND IT NEVER LEAVES THE SERVER FROM HERE
// ============================================================================
//
// The browser already holds the token: it is in the URL of the page the invitee
// is looking at. `InviteAcceptPanel` reads it out of `location.pathname` at
// click time — the same design `CopyButton` uses for the once-only token — so
// this handler receives it as a route parameter and never as a prop, a form
// field, or a serialised payload.
//
// From here it goes into the request BODY of
// `POST /api/v1/admin/admin-invites/redeem`, never a query string, so it does
// not reach Moira's access logs or any proxy in between.
//
// ============================================================================
// THE CREDENTIAL IS THE INVITEE'S, AND IT MUST NOT BE THE SYSTEM KEY
// ============================================================================
//
// `redeem_admin_invite` declares `bearerAuth` ALONE. `withConsoleSession` builds
// the client with `moiraClientForSession`, which passes no `systemKey` — and if
// one were passed, `MoiraClient` throws on this operation rather than sending
// it, because a system-key redemption would be the console granting Moira admin
// to an identity of its own choosing.
//
// ============================================================================
// THE SESSION CHECK IS NOT A POLICY EXEMPTION
// ============================================================================
//
// `withConsoleSession` applies the console's `allowed_email_domains` gate to the
// invitee exactly as it does to an admin. That is decision D3 — an invitation is
// a scoping token, never a policy bypass — and it is deliberate that an invitee
// outside the allow-list is refused here as well as by Moira: the console-side
// refusal is named (`email_domain_not_allowed`), where Moira's arrives as
// `admin_claim_domain_not_allowed` on the same grounds.
//
// A policy-denied redemption does NOT consume the invitation, so the same link
// works once the allow-list widens. `InviteAcceptPanel` says so in its copy.

import { withConsoleSession } from "@/lib/console-api";
import { redeemIdempotencyKey, redeemInvite } from "@/lib/invites";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  request: Request,
  context: { params: Promise<{ token: string }> },
): Promise<Response> {
  const params = await context.params;
  return withConsoleSession(request, async ({ client, identity }) => {
    const record = await redeemInvite(
      client,
      params.token,
      identity,
      redeemIdempotencyKey(identity),
    );
    return Response.json(record, { status: 201, headers: { "cache-control": "no-store" } });
  });
}
