// `GET  /api/evals/suites/{id}/cases` — every case in one suite.
// `POST /api/evals/suites/{id}/cases` — add one.
//
// `EvalCaseRecord` carries no `version`, so there is no `PATCH` on this family
// and no `If-Match` on the create — a case is authored once and either stays or
// is deleted, never edited in place.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import type { EvalCaseCreateRequest, GradingKind } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

const GRADING_KINDS: readonly GradingKind[] = ["exact_match", "contains", "schema_valid"];

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const params = new URL(request.url).searchParams;
    const view = await client.listEvalCases(id, {
      limit: 200,
      cursor: params.get("cursor") ?? undefined,
    });
    return Response.json(view, { headers: NO_STORE });
  });
}

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.evals_request_body_invalid);

    const gradingKind = body["grading_kind"];
    if (typeof gradingKind !== "string" || !GRADING_KINDS.includes(gradingKind as GradingKind)) {
      return badRequest(CONSOLE_MESSAGE_KEYS.evals_grading_kind_required);
    }
    if (!("input" in body) || body["input"] === undefined) {
      return badRequest(CONSOLE_MESSAGE_KEYS.evals_case_input_required);
    }
    if (!("expected" in body) || body["expected"] === undefined) {
      return badRequest(CONSOLE_MESSAGE_KEYS.evals_case_expected_required);
    }

    const create: EvalCaseCreateRequest = {
      input: body["input"] as EvalCaseCreateRequest["input"],
      expected: body["expected"] as EvalCaseCreateRequest["expected"],
      grading_kind: gradingKind as GradingKind,
    };

    const record = await client.createEvalCase(id, create);
    return Response.json(record, { status: 201, headers: NO_STORE });
  });
}
