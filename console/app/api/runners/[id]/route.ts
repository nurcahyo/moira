// `GET /api/runners/[id]` (refresh) and `DELETE /api/runners/[id]` — one
// containerised Claude runner. Issue #275/#272 workstream R3.
//
// ============================================================================
// GET WRITES. THE CONSOLE MUST NEVER CACHE THE VERSION IT READS HERE.
// ============================================================================
//
// `client.getRunner` refreshes the row from the runner service first (that is
// what surfaces `authorization_url`), so `version` — and therefore the `ETag`
// a caller would build from it — ADVANCES ON EVERY CALL. The delete handler
// below re-reads for exactly this reason rather than trusting a version the
// browser sends back from a page it loaded a few seconds ago; see
// `lib/runners.ts`'s `deleteRunnerSafely`.
//
// Re-checks the session itself — see `app/api/llm/connect-vllm/route.ts` for
// the rationale.

import { withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { deleteRunnerSafely } from "@/lib/runners";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

function keyed(status: number, code: string, messageKey: string): Response {
  return Response.json({ error: { code, message_key: messageKey } }, { status, headers: NO_STORE });
}

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.getRunner(id);
    return Response.json(record, { headers: NO_STORE });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const outcome = await deleteRunnerSafely(client, id);
    if (outcome.ok) return new Response(null, { status: 204, headers: NO_STORE });

    if (outcome.failure.kind === "not_found") {
      return keyed(404, "runner_not_found", CONSOLE_MESSAGE_KEYS.runners_delete_not_found);
    }
    return keyed(
      409,
      "resource_version_conflict",
      CONSOLE_MESSAGE_KEYS.runners_delete_conflict_exhausted,
    );
  });
}
