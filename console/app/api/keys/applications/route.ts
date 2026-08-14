// `POST /api/keys/applications` — create the application a consumer key hangs off.
//
// Not a separate feature from `/api/keys`: `ConsumerKeyCreateRequest` requires
// an `application_id`, so on a fresh deployment there is nothing to mint a key
// against until this runs. It lives beside the keys route for that reason rather
// than under an `/api/applications` family of its own — no other screen in this
// console manages applications, and inventing a family for one handler would
// imply one that does.
//
// The response is the id and nothing else. The panel re-reads `GET /api/keys`
// afterwards, which is the same list the page rendered from, so a handler that
// returned a second projection of the same row would create a shape that has to
// stay in step with the first for no gain.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { normalizeApplicationSlug } from "@/lib/consumer-keys";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.keys_request_body_invalid);

    const displayName = typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
    if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.keys_display_name_required);

    // A slug the operator typed that is not slug-shaped becomes `null` — the
    // application is still created, with no slug. Refusing the whole request
    // over a convenience field would fail the operator's real intent (an
    // application) on a field Moira does not require.
    const slug = normalizeApplicationSlug(body["application_slug"]);

    const record = await client.createApplication(
      {
        display_name: displayName,
        ...(slug === null ? {} : { application_slug: slug }),
      },
      // Derived from the application's own identity so a double-submit replays
      // instead of landing two applications an operator then tells apart by
      // their creation timestamps. The slug when there is one — it is the field
      // Moira treats as an identity — and the display name otherwise.
      { idempotencyKey: `application:${slug ?? displayName.toLowerCase()}` },
    );

    return Response.json(
      { id: record.id },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  });
}
