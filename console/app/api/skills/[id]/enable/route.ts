// `POST /api/skills/{id}/enable` — the review step after authoring or importing
// a skill. Skills land in `draft` on creation/import (fail-closed) and an agent
// may not call one until it is enabled.

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
    // Already enabled: answer with the row rather than writing. Enabling an
    // already-enabled row would burn a version for nothing and turn a
    // double-click into a lost `If-Match` race for whoever clicked next.
    if (current.status === "enabled") {
      return Response.json(current, { headers: NO_STORE });
    }
    const record = await client.enableSkill(current.id, ifMatchFor(current));
    return Response.json(record, { headers: NO_STORE });
  });
}
