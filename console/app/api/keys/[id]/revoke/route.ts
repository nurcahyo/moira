// `POST /api/keys/{id}/revoke` — stop a consumer key working, permanently.
//
// ============================================================================
// REVOKE, AND DELIBERATELY NOT DELETE
// ============================================================================
//
// `DELETE /api/v1/admin/consumer-keys/{id}` exists in Moira and is deliberately
// unregistered in `MOIRA_OPERATIONS`. Revocation stops the credential and leaves
// the row readable, so an operator can still answer "what was this key, when was
// it last used, and when did we turn it off" — the questions that get asked
// immediately after a key is turned off, and that a deleted row cannot answer.
//
// It is still one-way: a revoked key never works again, and the screen says so
// before asking. What "reversible" buys here is the audit trail, not the key.
//
// ============================================================================
// NO `If-Match`, AND THAT IS READ OFF THE SPEC
// ============================================================================
//
// Every provider-family disable requires one. `revoke_consumer_key` declares
// neither `If-Match` nor `Idempotency-Key`, so `MoiraClient` sends neither —
// adding one by analogy would send an unknown header, not a safer request. The
// registry entry carries the same note, and `openapi-contract.test.ts` re-derives
// both flags from the committed spec on every run.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.revokeConsumerKey(id);
    // The status, and nothing more. The panel re-reads the whole list after a
    // revocation, and a second projection of one row here would have to stay in
    // step with the list's for no gain.
    return Response.json(
      { id: record.id, status: record.status },
      { headers: { "cache-control": "no-store" } },
    );
  });
}
