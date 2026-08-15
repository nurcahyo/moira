// `GET /api/playground/executions/{executionId}` — the baseline
// routing-transparency follow-up after a chat run: which route and model
// actually served, `attempt_count`, `latency_ms`, usage. No extra scope
// beyond the execution itself, and available whether or not the caller can
// reach the diagnostics endpoint — see `.../diagnose/route.ts`'s header for
// what THAT surface adds on top.
//
// `executionId` is `PublicResponse.execution_id` / `PublicSseEnvelope.execution_id`
// verbatim, `exec_` prefix and all — `modules/playground/PlaygroundScreen.tsx`
// passes it straight through without stripping anything.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function GET(
  request: Request,
  context: { params: Promise<{ executionId: string }> },
): Promise<Response> {
  const { executionId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const summary = await client.getExecution(executionId);
    return Response.json(summary, { headers: NO_STORE });
  });
}
