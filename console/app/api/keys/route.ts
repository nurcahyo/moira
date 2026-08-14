// `GET  /api/keys` — everything `/settings/keys` renders.
// `POST /api/keys` — mint one consumer key for an application (issue #180).
//
// ============================================================================
// ORDINARY ADMINISTRATION, NOT SETUP
// ============================================================================
//
// Behind `withConsoleSession`, like `app/api/llm/**` and `app/api/admins/**`,
// and absent from `route-handler-session.test.ts`'s exemption list. Minting a
// credential that can send prompts is the last thing that should be reachable
// on a deployment whose first admin has not been claimed.
//
// ============================================================================
// THE POST RESPONSE CARRIES A LIVE CREDENTIAL
// ============================================================================
//
// It is Moira's `ApiKeySecretResponse` — `resource` NARROWED to the display-safe
// projection, plus `secret` verbatim. That is the one and only time a plaintext
// consumer key crosses this boundary; it is `cache-control: no-store`, nothing
// here logs it, and the browser hands it straight to `OnceOnlySecretModal`.
//
// `resource` is narrowed and `secret` is not, and the asymmetry is deliberate:
// the raw key is what the operator came for and cannot be projected away, while
// `fingerprint` and `pepper_version` are values about the key that no screen
// needs. Forwarding the record verbatim "because we are already forwarding the
// secret" would put a permanent hash of a live credential in the browser for the
// life of the page.
//
// `secret: null` IS A SUCCESS. Moira returns the sanitized record with
// `secret_retrievable: false` on an idempotent replay; the modal renders that as
// "already shown", not as a failure.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { loadConsumerKeys, narrowScopes } from "@/lib/consumer-keys";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const view = await loadConsumerKeys(client);
    return Response.json(view, { headers: { "cache-control": "no-store" } });
  });
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.keys_request_body_invalid);

    const applicationId = body["application_id"];
    if (typeof applicationId !== "string" || applicationId.trim() === "") {
      return badRequest(CONSOLE_MESSAGE_KEYS.keys_application_required);
    }

    const displayName = typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
    if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.keys_display_name_required);

    const envelope = await client.createConsumerKey(
      {
        application_id: applicationId.trim(),
        display_name: displayName,
        // Narrowed HERE, not in the form. The checkboxes are a client component
        // and therefore a suggestion; this is the line that decides.
        scopes: narrowScopes(body["scopes"]),
      },
      // A nonce per request. Minting a SECOND key for the same application with
      // the same name is a legitimate act — the first one leaked, or is being
      // rotated by hand — and a key derived from the pair would answer it with a
      // replay: `secret: null`, no usable credential, and an operator staring at
      // "already shown" for a key they have never seen. A double-submit of one
      // form is deduplicated by the browser's own in-flight request.
      { idempotencyKey: `consumer-key:${crypto.randomUUID()}` },
    );

    return Response.json(
      {
        resource: {
          id: envelope.resource.id,
          display_name: envelope.resource.display_name,
          key_prefix: envelope.resource.key_prefix,
          scopes: envelope.resource.scopes,
          status: envelope.resource.status,
          created_at: envelope.resource.created_at,
          last_used_at: envelope.resource.last_used_at ?? null,
          expires_at: envelope.resource.expires_at ?? null,
        },
        secret: envelope.secret ?? null,
        secret_retrievable: envelope.secret_retrievable,
      },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  });
}
