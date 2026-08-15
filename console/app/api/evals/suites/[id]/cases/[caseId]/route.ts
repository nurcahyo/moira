// `DELETE /api/evals/suites/{id}/cases/{caseId}` — remove one case.
//
// Both ids are in Moira's own path (`.../eval-suites/{id}/cases/{case_id}`), so
// a `case_id` that does not belong to `id` is Moira's own 404 rather than
// something this console needs to pre-check — unlike the LLM surface's flat
// `provider-models/{id}/disable`, this operation is already scoped by its
// parent in the URL.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string; caseId: string }> },
): Promise<Response> {
  const { id, caseId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    await client.deleteEvalCase(id, caseId);
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  });
}
