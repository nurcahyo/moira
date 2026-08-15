// `POST /api/skills/import` — the OpenAPI import pipeline (plan 12 §5, issue
// #237). Parses `document`, caps it at 300 operations, SSRF-validates the
// server URL, and creates one `draft` skill plus one HTTP executor per
// operation. Live execution is a separate concern (#84) — this endpoint only
// ever creates disabled rows for an operator to review and enable via
// `POST /api/skills/{id}/enable` or `POST /api/skills/bulk-enable`.
//
// Every refusal this can produce — the 300-operation cap, an SSRF-blocked host,
// a document that does not parse as OpenAPI — arrives as Moira's own coded
// error and is forwarded as-is by `withConsoleSession`'s catch. This handler's
// own validation covers only the one thing Moira cannot: a body with no
// `document` at all.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null || !("document" in body) || body["document"] === null) {
      return badRequest(CONSOLE_MESSAGE_KEYS.skills_import_document_required);
    }

    const result = await client.importSkills(body["document"]);
    return Response.json(result, { status: 201, headers: NO_STORE });
  });
}
