// `GET /api/providers/health` — the rolling reachability window for every
// enabled provider (issue #83). Read-only: nothing here writes, so there is no
// `POST`/`PATCH`/`DELETE` in this family.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const view = await client.getProviderHealth();
    return Response.json(view, { headers: { "cache-control": "no-store" } });
  });
}
