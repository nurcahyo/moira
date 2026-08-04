// `POST /api/llm/providers/{id}/models` — register one model on one provider.
//
// The provider id comes from the PATH and is resolved against Moira before
// anything is written. That is not decoration: `createProviderModel` puts the id
// into the URL of a privileged write, and an id lifted straight out of a request
// body would let whoever sent it attach a model to a provider they were never
// shown.
//
// `capabilities` is sent EXPLICITLY, always — see `DEFAULT_MODEL_CAPABILITIES`.
// An omitted value is stored as SQL `null`, matches no capability filter, and
// surfaces later as `no_eligible_model`, which names neither the model nor the
// missing column.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { DEFAULT_MODEL_CAPABILITIES } from "@/lib/llm-settings";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const modelKey = typeof body["model_key"] === "string" ? body["model_key"].trim() : "";
    if (modelKey === "") return badRequest(CONSOLE_MESSAGE_KEYS.llm_model_key_required);

    const provider = await client.getProvider(id);
    const record = await client.createProviderModel(
      provider.id,
      { model_key: modelKey, capabilities: DEFAULT_MODEL_CAPABILITIES },
      { idempotencyKey: `llm-model:${provider.id}:${modelKey}` },
    );
    return Response.json(
      { id: record.id },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  });
}
