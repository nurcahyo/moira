// `GET   /api/flows/{id}` — one flow, with its ordered steps.
// `PATCH /api/flows/{id}` — correct its name, description, or REPLACE its
//         whole step list. Omitting `steps` leaves the existing ones untouched.
// `DELETE /api/flows/{id}` — soft-delete it. There is no restore operation on
//         this surface.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { parseFlowSteps } from "@/lib/flow-steps";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { ifMatchFor } from "@/lib/moira-client";
import type { AgentFlowPatchRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.getFlow(id);
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
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.flows_request_body_invalid);

    const patch: { -readonly [K in keyof AgentFlowPatchRequest]: AgentFlowPatchRequest[K] } = {};

    if ("display_name" in body) {
      const displayName =
        typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
      if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.flows_display_name_required);
      patch.display_name = displayName;
    }
    if ("description" in body) {
      const description =
        typeof body["description"] === "string" && body["description"].trim() !== ""
          ? body["description"].trim()
          : null;
      patch.description = description;
    }
    if ("steps" in body) {
      const steps = parseFlowSteps(body["steps"]);
      if (steps === null) return badRequest(CONSOLE_MESSAGE_KEYS.flows_steps_invalid);
      patch.steps = steps;
    }
    if (Object.keys(patch).length === 0) {
      return badRequest(CONSOLE_MESSAGE_KEYS.flows_request_body_invalid);
    }

    const current = await client.getFlow(id);
    const record = await client.patchFlow(current.id, patch, ifMatchFor(current));
    return Response.json(record, { headers: NO_STORE });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const current = await client.getFlow(id);
    await client.deleteFlow(current.id, ifMatchFor(current));
    return new Response(null, { status: 204, headers: NO_STORE });
  });
}
