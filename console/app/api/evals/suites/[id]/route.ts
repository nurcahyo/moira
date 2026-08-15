// `GET   /api/evals/suites/{id}` — one eval suite.
// `PATCH /api/evals/suites/{id}` — correct its name or description.
// `DELETE /api/evals/suites/{id}` — soft-delete it. There is no restore
// operation on this surface and no enable/disable on eval suites at all — this
// is genuinely the one-way door, unlike the skills family.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { ifMatchFor } from "@/lib/moira-client";
import type { EvalSuitePatchRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.getEvalSuite(id);
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
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.evals_request_body_invalid);

    const patch: { -readonly [K in keyof EvalSuitePatchRequest]: EvalSuitePatchRequest[K] } = {};

    if ("display_name" in body) {
      const displayName =
        typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
      if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.evals_display_name_required);
      patch.display_name = displayName;
    }
    if ("description" in body) {
      const description =
        typeof body["description"] === "string" && body["description"].trim() !== ""
          ? body["description"].trim()
          : null;
      patch.description = description;
    }
    if (Object.keys(patch).length === 0) {
      return badRequest(CONSOLE_MESSAGE_KEYS.evals_request_body_invalid);
    }

    const current = await client.getEvalSuite(id);
    const record = await client.patchEvalSuite(current.id, patch, ifMatchFor(current));
    return Response.json(record, { headers: NO_STORE });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const current = await client.getEvalSuite(id);
    await client.deleteEvalSuite(current.id, ifMatchFor(current));
    return new Response(null, { status: 204, headers: NO_STORE });
  });
}
