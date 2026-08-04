// `POST /api/llm/connect-vllm` — the shortcut, in two stages.
//
//   { action: "discover", base_url } -> ask the endpoint what it serves
//   { action: "connect",  base_url, display_name, model_keys } -> run the chain
//
// ============================================================================
// AN ACTION DISCRIMINATOR IS NOT A RESOURCE SELECTOR
// ============================================================================
//
// The rule this screen is held to is that a request body may never choose WHICH
// resource a privileged call touches. `action` chooses which STAGE runs, both
// stages are behind the same gate, and neither of them takes an identifier from
// the body: the provider, model, credential and policy ids are all resolved
// inside `runConnectChain` from rows it read or created itself, and the route id
// is looked up by `route_key`. `app/api/setup/route.ts` is the shipped precedent
// for the shape.
//
// ============================================================================
// DISCOVERY IS THE ONLY OUTBOUND CALL THIS CONSOLE MAKES TO A NON-MOIRA HOST
// ============================================================================
//
// It is made HERE, from the BFF, and never from the browser. The endpoint lives
// on the operator's own network: a browser fetch would require that network to
// be reachable from wherever the operator is sitting and CORS headers the
// endpoint has no reason to send, and it would move the choice of which host to
// contact out of this process.
//
// It is bounded on all three axes that can hang a request handler — a 5 s
// timeout, a 256 KiB read cap, and a cap on how many model ids may be offered —
// and its response is VALIDATED before any of it is rendered. See
// `discoverModels` and `modelKeysFromDiscoveryBody`.
//
// An unreachable endpoint is an ordinary keyed message. A laptop with the tunnel
// down must still get a usable page.
//
// ============================================================================
// A PARTIAL CHAIN COMES BACK AS STATE, NOT AS A BARE FAILURE
// ============================================================================
//
// If step three fails, a provider and a model already exist. The operator has to
// be told that, or the second attempt is made blind — and blind retries against
// an endpoint family with no duplicate-detection 409 are how two eligible
// routing policies get created. So `LlmProvisioningError` carries the state and
// the trace, and both are forwarded.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import {
  canonicalOpenAiBaseUrl,
  discoverModels,
  isLlmProvisioningError,
  runConnectChain,
} from "@/lib/llm-settings";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/** Model ids the operator selected, narrowed before they reach a provider row. */
function readModelKeys(value: unknown): readonly string[] | null {
  if (!Array.isArray(value) || value.length === 0) return null;
  const keys: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string") return null;
    const trimmed = entry.trim();
    if (trimmed === "") return null;
    if (!keys.includes(trimmed)) keys.push(trimmed);
  }
  return keys;
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const action = body["action"];

    if (action === "discover") {
      const outcome = await discoverModels(body["base_url"]);
      if (!outcome.ok) {
        return Response.json(
          { error: { code: "discovery_failed", message_key: outcome.messageKey } },
          { status: 502, headers: NO_STORE },
        );
      }
      return Response.json(
        { base_url: outcome.baseUrl, models: outcome.models },
        { headers: NO_STORE },
      );
    }

    if (action === "connect") {
      const resolved = canonicalOpenAiBaseUrl(body["base_url"]);
      if (!resolved.ok) return badRequest(resolved.messageKey);

      const modelKeys = readModelKeys(body["model_keys"]);
      if (modelKeys === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_model_required);

      const displayName =
        typeof body["display_name"] === "string" && body["display_name"].trim() !== ""
          ? body["display_name"].trim()
          : resolved.baseUrl;

      try {
        const result = await runConnectChain(client, {
          baseUrl: resolved.baseUrl,
          displayName,
          modelKeys,
        });
        return Response.json(
          { provider_id: result.providerId, trace: result.trace },
          { status: 201, headers: NO_STORE },
        );
      } catch (error) {
        if (!isLlmProvisioningError(error)) throw error;
        return Response.json(
          {
            error: {
              code: "llm_provisioning_failed",
              message_key: error.messageKey,
              step: error.step,
              // What already exists, so a second attempt is made with knowledge
              // rather than blind.
              state: error.state,
              trace: error.trace,
            },
          },
          { status: 409, headers: NO_STORE },
        );
      }
    }

    return badRequest(CONSOLE_MESSAGE_KEYS.llm_action_unknown);
  });
}
