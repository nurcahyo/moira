// `GET /api/agent-profiles` — read-only, for the flow step builder's "which
// agent profile does this step run" picker. The console owns no create/edit
// surface for agent profiles; only the list operation is exposed.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const params = new URL(request.url).searchParams;
    const view = await client.listAgentProfiles({
      limit: 200,
      status: params.get("status") ?? "active",
    });
    return Response.json(view, { headers: { "cache-control": "no-store" } });
  });
}
