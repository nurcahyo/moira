// Ownership transfer (`PATCH`) and grant revocation (`DELETE`) for one grant.
//
// ============================================================================
// TRANSFER IS ONE `PATCH`, AND THE PLAN SAYS TWO
// ============================================================================
//
// Plan 09's body describes "two sequential calls … promote the target, then
// demote the actor". After PR #39 that is wrong and destructive:
// `set_primary` calls `demote_active_primaries_other_than` inside the same
// transaction, and `admin_identities_single_active_primary` is UNIQUE on
// `(is_primary)` for live rows — so there is never a second owner to demote, and
// the second call would either demote the person just promoted or 409 on a
// version the actor no longer holds.
//
// ============================================================================
// `If-Match` COMES FROM THE BROWSER, AND THAT IS FINE
// ============================================================================
//
// The version travels in the request body, from the render the operator is
// looking at. It is OPTIMISTIC CONCURRENCY, not an authorization input: a wrong
// version is `409 resource_version_conflict` and nothing else, and a right one
// still has to pass `require_primary_actor`, which reads the CALLER's own
// `admin_identities` row server-side. The alternative — re-reading the record
// here — is not available: the spec has no `GET /admin-identities/{id}`, only a
// list.
//
// ============================================================================
// `DELETE` DECLARES NO `If-Match`, AND THE SOLE ADMIN CANNOT BE REVOKED
// ============================================================================
//
// Decision D-F20: `revoke_grant` clears `is_primary`, and the last-primary guard
// refuses that, so on a deployment with one admin this is a permanent
// `409 admin_identity_last_primary` for that row. The console renders it as a
// stated rule beside a disabled control (`console.admins.owner_not_revocable`)
// rather than as a failed request.
//
// Re-checks the session itself — see `lib/console-api.ts`.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { ifMatchFor } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function PATCH(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    const version = body?.["version"];
    if (typeof version !== "number" || !Number.isInteger(version)) {
      return badRequest(CONSOLE_MESSAGE_KEYS.admins_request_failed);
    }

    const record = await client.patchAdminIdentity(
      id,
      // ONE FIELD, and it is always `true`. There is no console flow that clears
      // the flag: `is_primary: false` on the only primary is refused by the
      // last-primary guard, and on a non-primary row it is a no-op whose
      // `Idempotency-Key` ledger entry describes nothing.
      { is_primary: true },
      ifMatchFor({ version }),
      { idempotencyKey: `admin-identity-primary:${id}:${version}` },
    );
    return Response.json(record, { headers: { "cache-control": "no-store" } });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.deleteAdminIdentity(id, {
      idempotencyKey: `admin-identity-revoke:${id}`,
    });
    return Response.json(record, { headers: { "cache-control": "no-store" } });
  });
}
