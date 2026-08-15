// `GET  /api/evals/suites` — every eval suite this deployment has authored.
// `POST /api/evals/suites` — author one.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import type { EvalSuiteCreateRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const params = new URL(request.url).searchParams;
    const view = await client.listEvalSuites({
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
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.evals_request_body_invalid);

    const suiteKey = typeof body["suite_key"] === "string" ? body["suite_key"].trim() : "";
    if (suiteKey === "") return badRequest(CONSOLE_MESSAGE_KEYS.evals_suite_key_required);

    const displayName = typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
    if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.evals_display_name_required);

    const description =
      typeof body["description"] === "string" && body["description"].trim() !== ""
        ? body["description"].trim()
        : null;

    const create: EvalSuiteCreateRequest = { suite_key: suiteKey, display_name: displayName, description };

    const record = await client.createEvalSuite(create, {
      // Derived from the suite's own identity, so a double-submit replays
      // rather than landing two rows with the same `suite_key`.
      idempotencyKey: `eval-suite:${suiteKey}`,
    });

    return Response.json(record, { status: 201, headers: NO_STORE });
  });
}
