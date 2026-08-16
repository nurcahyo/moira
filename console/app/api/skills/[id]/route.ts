// `GET   /api/skills/{id}` — one skill.
// `PATCH /api/skills/{id}` — correct its name, description, tags or metadata.
// `DELETE /api/skills/{id}` — soft-delete it. There is no restore operation on
// this surface; disabling (`POST .../disable`) is the reversible undo.
//
// The version is READ FROM MOIRA, not taken from the body — reading the record
// here also confirms the path id names a skill that exists before anything is
// written. Re-checks the session itself: `app/api/**` is outside every route
// group.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { ifMatchFor } from "@/lib/moira-client";
import type { SkillPatchRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((entry): entry is string => typeof entry === "string")
    .map((entry) => entry.trim())
    .filter((entry) => entry !== "");
}

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.getSkill(id);
    return Response.json(record, { headers: NO_STORE });
  });
}

export async function PATCH(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.skills_request_body_invalid);

    const patch: { -readonly [K in keyof SkillPatchRequest]: SkillPatchRequest[K] } = {};

    if ("display_name" in body) {
      const displayName =
        typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
      if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.skills_display_name_required);
      patch.display_name = displayName;
    }
    if ("description" in body) {
      const description =
        typeof body["description"] === "string" && body["description"].trim() !== ""
          ? body["description"].trim()
          : null;
      patch.description = description;
    }
    if ("tags" in body) {
      patch.tags = stringList(body["tags"]);
    }
    if (Object.keys(patch).length === 0) {
      return badRequest(CONSOLE_MESSAGE_KEYS.skills_request_body_invalid);
    }

    const current = await client.getSkill(id);
    const record = await client.patchSkill(current.id, patch, ifMatchFor(current));
    return Response.json(record, { headers: NO_STORE });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const current = await client.getSkill(id);
    await client.deleteSkill(current.id, ifMatchFor(current));
    return new Response(null, { status: 204, headers: NO_STORE });
  });
}
