// `POST /api/admins/invites/{id}/revoke` — withdraw an invitation.
//
// A POST to a sub-resource, mirroring Moira's own shape. There is no
// `DELETE /admin-invites/{id}` in the committed spec, and inventing one here
// would have made this handler the place where the console's URL vocabulary
// diverged from the API it proxies.
//
// No `If-Match`: `revoke_admin_invite` declares an optional `Idempotency-Key`
// and no version precondition, because a second revocation of a revoked
// invitation is `409 invite_revoked` rather than a silent overwrite.
//
// Re-checks the session itself — see `lib/console-api.ts`.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.revokeAdminInvite(id, {
      // Derived from the resource, so a double-click replays rather than
      // producing a second audit entry for one operator action.
      idempotencyKey: `admin-invite-revoke:${id}`,
    });
    return Response.json(record, { headers: { "cache-control": "no-store" } });
  });
}
