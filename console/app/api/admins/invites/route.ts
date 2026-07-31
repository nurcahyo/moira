// `POST /api/admins/invites` — mint an invitation on behalf of the signed-in admin.
//
// The mutation transport decision W5-D5 chose over server actions. See
// `lib/console-api.ts` for the argument; the short version is that a
// `"use client"` organism may not import `lib/moira-client.ts`, and adopting
// server actions would mean adding `nextCookies()` to the Better Auth plugin
// list in a wave that should not touch the auth instance.
//
// THIS HANDLER RE-CHECKS THE SESSION ITSELF. `app/api/**` sits outside every
// route group, so it inherits nothing from `(console)/layout.tsx`.
// `withConsoleSession` is what performs the check, and
// `tests/unit/architecture/route-handler-session.test.ts` fails if a handler
// under `app/api/**` neither calls it nor is named on the exemption list.
//
// The response is Moira's `AdminInviteSecretResponse` verbatim, INCLUDING the
// raw `secret`. That is the one and only time the token crosses this boundary,
// it is `cache-control: no-store`, and the browser hands it straight to
// `OnceOnlySecretModal`. Nothing here logs the response, and nothing rebuilds
// it — a rebuild is how the required `notice` gets dropped.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { createInvite, inviteCreateIdempotencyKey, isAcceptableInviteLifetime } from "@/lib/invites";
import type { AdminInviteConstraint } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function isConstraint(value: unknown): value is AdminInviteConstraint {
  return value === "email" || value === "domain";
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.admins_invite_value_required);

    const constraint = body["constraint"];
    const value = body["value"];
    const expiresInSeconds = body["expires_in_seconds"];

    if (!isConstraint(constraint) || typeof value !== "string" || value.trim() === "") {
      return badRequest(CONSOLE_MESSAGE_KEYS.admins_invite_value_required);
    }
    // Checked here as well as in the picker. The picker offers a closed set of
    // durations, but the handler is reachable without it, and Moira's refusal
    // for the FLOOR is a plain `invalid_request` that a form would render as a
    // generic validation failure rather than as "too short".
    if (typeof expiresInSeconds !== "number" || !isAcceptableInviteLifetime(expiresInSeconds)) {
      return badRequest(CONSOLE_MESSAGE_KEYS.expiry_hint);
    }

    const envelope = await createInvite(
      client,
      { constraint, value: value.trim(), expires_in_seconds: expiresInSeconds },
      // A nonce per request, so re-inviting the same address after the first
      // invitation expired is a NEW request rather than a replay that returns
      // `secret: null` and no usable link. A double-submit of one form is
      // deduplicated by the browser's own in-flight request, not by this key.
      inviteCreateIdempotencyKey(constraint, value, crypto.randomUUID()),
    );

    return Response.json(envelope, { status: 201, headers: { "cache-control": "no-store" } });
  });
}
