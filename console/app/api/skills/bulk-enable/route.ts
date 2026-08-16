// `POST /api/skills/bulk-enable` — the reason reviewing an imported spec's 30
// draft skills does not become 30 clicks (plan 12 §5 decision 22). No
// `If-Match`: it is a multi-row operation with no single row version.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.skills_request_body_invalid);

    const raw = body["skill_ids"];
    const skillIds = Array.isArray(raw)
      ? raw.filter((entry): entry is string => typeof entry === "string" && entry.trim() !== "")
      : [];
    if (skillIds.length === 0) return badRequest(CONSOLE_MESSAGE_KEYS.skills_bulk_enable_empty);

    const result = await client.bulkEnableSkills(skillIds);
    return Response.json(result, { headers: NO_STORE });
  });
}
