// `GET  /api/skills` — every skill (tool or guard) this deployment has registered.
// `POST /api/skills` — author one by hand.
//
// ============================================================================
// ORDINARY ADMINISTRATION, NOT SETUP
// ============================================================================
//
// Every handler in this family sits behind `withConsoleSession`, the same gate
// `app/api/llm/**` and `app/api/admins/**` use. Skill authoring is not
// first-run bootstrap: there is an `admin_identities` grant to check against,
// and nothing here creates the first one.
//
// ============================================================================
// `params_schema` IS NOT ON THIS FORM
// ============================================================================
//
// A JSON-Schema authoring UI is out of scope for the hand-authored path — the
// field is optional in `SkillCreateRequest` and simply omitted here. The
// OpenAPI import pipeline (`POST /api/skills/import`) is the path that
// populates it, straight from the imported operation's request schema.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import type { SkillCreateRequest, SkillKind } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

const SKILL_KINDS: readonly SkillKind[] = ["tool", "guard"];

function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((entry): entry is string => typeof entry === "string")
    .map((entry) => entry.trim())
    .filter((entry) => entry !== "");
}

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const params = new URL(request.url).searchParams;
    const view = await client.listSkills({
      limit: 200,
      cursor: params.get("cursor") ?? undefined,
      status: params.get("status") ?? undefined,
      search: params.get("search") ?? undefined,
    });
    return Response.json(view, { headers: NO_STORE });
  });
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.skills_request_body_invalid);

    const skillKey = typeof body["skill_key"] === "string" ? body["skill_key"].trim() : "";
    if (skillKey === "") return badRequest(CONSOLE_MESSAGE_KEYS.skills_skill_key_required);

    const displayName = typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
    if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.skills_display_name_required);

    const kind = body["kind"];
    if (typeof kind !== "string" || !SKILL_KINDS.includes(kind as SkillKind)) {
      return badRequest(CONSOLE_MESSAGE_KEYS.skills_kind_required);
    }

    const description =
      typeof body["description"] === "string" && body["description"].trim() !== ""
        ? body["description"].trim()
        : null;
    const tags = stringList(body["tags"]);

    const create: SkillCreateRequest = {
      skill_key: skillKey,
      display_name: displayName,
      kind: kind as SkillKind,
      description,
      tags,
    };

    const record = await client.createSkill(create, {
      // Derived from the skill's own identity, so a double-submit replays
      // rather than landing two rows with the same `skill_key`.
      idempotencyKey: `skill:${skillKey}`,
    });

    return Response.json(record, { status: 201, headers: NO_STORE });
  });
}
