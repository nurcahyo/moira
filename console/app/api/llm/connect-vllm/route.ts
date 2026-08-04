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
// deadline that covers the RESPONSE BODY and not merely the headers, a 256 KiB
// read cap, and a cap on how many model ids may be offered — and its response is
// VALIDATED before any of it is rendered. See `discoverModels`, `readBounded`
// and `modelKeysFromDiscoveryBody`.
//
// An unreachable endpoint is an ordinary keyed message. A laptop with the tunnel
// down must still get a usable page.
//
// ============================================================================
// AND IT IS THE ONE HANDLER HERE WHOSE EFFECT NEVER REACHES MOIRA
// ============================================================================
//
// The other ten handlers on this surface are authorized by Moira, because every
// one of their effects IS a Moira admin call: a signed-in employee with no
// `admin_identities` grant gets a 403 from Moira and nothing happens. The
// `discover` stage has no such backstop — its whole effect is an outbound GET
// from inside the deployment's network to a host named in the request body, and
// it used to return before touching `client` at all, which made
// `withConsoleSession` its entire authorization. That check is a session check,
// not an admin check (`lib/console-api.ts` says so itself), so the stage was a
// host and port oracle for anyone inside `allowed_email_domains`.
//
// `requireAdminGrant` runs FIRST, before the URL is even canonicalised. It is an
// admin-plane read, so Moira's `authenticate_admin` applies the grant union and
// answers 403 to a caller who holds none — and that 403 travels as a
// `MoiraRequestError`, which `withConsoleSession` already renders keyed.
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
  narrowModelKeys,
  requireAdminGrant,
  runConnectChain,
} from "@/lib/llm-settings";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const action = body["action"];

    if (action === "discover") {
      // BEFORE the outbound fetch, and before the address is even parsed: this
      // stage's whole effect happens outside Moira, so the grant has to be read
      // rather than assumed. See this file's header.
      await requireAdminGrant(client);
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

      // The SAME narrowing discovery applies — a 256-character cap, a 200-entry
      // ceiling and a control-character refusal. `model_keys` is a plain request
      // body and nothing obliges it to carry what discovery offered.
      const modelKeys = narrowModelKeys(body["model_keys"]);
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
