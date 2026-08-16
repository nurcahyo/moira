// `GET  /api/flows` — every flow this deployment has authored.
// `POST /api/flows` — author one, optionally with its ordered steps.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { parseFlowSteps } from "@/lib/flow-steps";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import type { AgentFlowCreateRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const params = new URL(request.url).searchParams;
    const view = await client.listFlows({
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
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.flows_request_body_invalid);

    const flowKey = typeof body["flow_key"] === "string" ? body["flow_key"].trim() : "";
    if (flowKey === "") return badRequest(CONSOLE_MESSAGE_KEYS.flows_flow_key_required);

    const displayName = typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
    if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.flows_display_name_required);

    const description =
      typeof body["description"] === "string" && body["description"].trim() !== ""
        ? body["description"].trim()
        : null;

    const steps = parseFlowSteps(body["steps"]);
    if (steps === null) return badRequest(CONSOLE_MESSAGE_KEYS.flows_steps_invalid);

    const create: AgentFlowCreateRequest = {
      flow_key: flowKey,
      display_name: displayName,
      description,
      steps,
    };

    const record = await client.createFlow(create, {
      // Derived from the flow's own identity, so a double-submit replays
      // rather than landing two rows with the same `flow_key`.
      idempotencyKey: `flow:${flowKey}`,
    });

    return Response.json(record, { status: 201, headers: NO_STORE });
  });
}
