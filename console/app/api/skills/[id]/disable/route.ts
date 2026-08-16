// `POST /api/skills/{id}/disable` — take a skill out of an agent's tool set
// without deleting its definition. Reversible: `POST .../enable` is the way back.

import { withConsoleSession } from "@/lib/console-api";
import { ifMatchFor } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const current = await client.getSkill(id);
    if (current.status === "disabled") {
      return Response.json(current, { headers: NO_STORE });
    }
    const record = await client.disableSkill(current.id, ifMatchFor(current));
    return Response.json(record, { headers: NO_STORE });
  });
}
