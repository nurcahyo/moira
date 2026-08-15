// `GET    /api/skills/{id}/executor` — the skill's HTTP executor, if it has one.
// `PATCH  /api/skills/{id}/executor` — correct its method, URL, timeout or
//         bound credential row.
// `DELETE /api/skills/{id}/executor` — remove it.
//
// ============================================================================
// NOT EVERY SKILL HAS ONE, AND THAT IS THE NORMAL CASE FOR A HAND-AUTHORED ROW
// ============================================================================
//
// There is no `POST .../executor` in Moira's registry — an executor is created
// only by the OpenAPI import pipeline, one per imported operation. A skill
// authored by hand through `POST /api/skills` has none, and `GET` answers
// Moira's own 404 for it, forwarded as-is by `withConsoleSession`'s catch. The
// organism renders that as "no HTTP executor" rather than as a page error.
//
// ============================================================================
// `If-Match` HERE IS A QUOTED TIMESTAMP, NOT A VERSION NUMBER
// ============================================================================
//
// `skill_http_executors` has no `version` column — see
// `SkillHttpExecutorRecord`'s doc comment in `lib/types.ts`. Both mutations
// below read the row first for its CURRENT `updated_at` and build the
// precondition with `skillExecutorIfMatchFor`, never `ifMatchFor` (which reads
// a `version` this record does not have).
//
// `allowed_host` is never accepted from the body — it is derived server-side
// from `url_template`'s host, exactly as `SkillHttpExecutorPatchRequest`'s own
// doc comment states. A client-settable `allowed_host` could point execution at
// a host the URL no longer names.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { skillExecutorIfMatchFor } from "@/lib/moira-client";
import type { SkillExecutorMethod, SkillHttpExecutorPatchRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

const EXECUTOR_METHODS: readonly SkillExecutorMethod[] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const record = await client.getSkillExecutor(id);
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

    const patch: { -readonly [K in keyof SkillHttpExecutorPatchRequest]: SkillHttpExecutorPatchRequest[K] } =
      {};

    if ("method" in body) {
      const method = body["method"];
      if (typeof method !== "string" || !EXECUTOR_METHODS.includes(method as SkillExecutorMethod)) {
        return badRequest(CONSOLE_MESSAGE_KEYS.skills_executor_method_invalid);
      }
      patch.method = method as SkillExecutorMethod;
    }
    if ("url_template" in body) {
      const urlTemplate = typeof body["url_template"] === "string" ? body["url_template"].trim() : "";
      if (urlTemplate === "") return badRequest(CONSOLE_MESSAGE_KEYS.skills_executor_url_required);
      patch.url_template = urlTemplate;
    }
    if ("timeout_ms" in body) {
      const timeoutMs = body["timeout_ms"];
      if (typeof timeoutMs !== "number" || !Number.isFinite(timeoutMs) || timeoutMs <= 0) {
        return badRequest(CONSOLE_MESSAGE_KEYS.skills_executor_timeout_invalid);
      }
      patch.timeout_ms = timeoutMs;
    }
    if ("credential_id" in body) {
      const credentialId = typeof body["credential_id"] === "string" ? body["credential_id"].trim() : "";
      patch.credential_id = credentialId === "" ? null : credentialId;
    }
    if (Object.keys(patch).length === 0) {
      return badRequest(CONSOLE_MESSAGE_KEYS.skills_request_body_invalid);
    }

    const current = await client.getSkillExecutor(id);
    const record = await client.patchSkillExecutor(id, patch, skillExecutorIfMatchFor(current));
    return Response.json(record, { headers: NO_STORE });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const current = await client.getSkillExecutor(id);
    await client.deleteSkillExecutor(id, skillExecutorIfMatchFor(current));
    return new Response(null, { status: 204, headers: NO_STORE });
  });
}
