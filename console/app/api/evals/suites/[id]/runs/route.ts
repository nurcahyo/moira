// `GET /api/evals/suites/{id}/runs` — the run history for one suite. Read-only:
// `eval_runs` rows are produced by the eval runner, not written through this
// console.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const params = new URL(request.url).searchParams;
    const view = await client.listEvalRuns(id, {
      limit: 50,
      cursor: params.get("cursor") ?? undefined,
    });
    return Response.json(view, { headers: { "cache-control": "no-store" } });
  });
}
