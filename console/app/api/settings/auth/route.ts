// `POST /api/settings/auth` — the owner corrects how operators sign in (#185).
//
// ============================================================================
// TWO GATES, AND ONLY ONE OF THEM IS THE ENFORCEMENT
// ============================================================================
//
// `withConsoleSession` proves there is an operator. `requireConsoleOwner` then
// proves it is the OWNER — and that second one is a PRE-CHECK, not the
// enforcement. Moira gates its own auth-provider write surface on
// `require_primary_actor` (issue #185's Rust half), so a non-owner is refused at
// the source whatever this console believes. What the pre-check buys is a screen
// that does not offer a control the caller may not use, and a refusal that says
// "only the owner can change this" instead of surfacing a raw 403 from a request
// the operator did not know they were making.
//
// A console-side gate alone would be worthless here: every admin holds a bearer
// token this console minted, and can call Moira directly with it.
//
// ============================================================================
// TWO ACTIONS, BECAUSE THEY TOUCH DIFFERENT SYSTEMS
// ============================================================================
//
//   `update`          Moira's row, plus this console's seal when a secret came
//                     with it. The client id and the secret are ONE atomic unit:
//                     a moving client id with no secret is refused before any
//                     request leaves this process (see `lib/auth-settings.ts`).
//   `replace_secret`  this console's seal and nothing else — no Moira request,
//                     no `If-Match`, no version bump. The right shape when the
//                     identity provider issued a new secret for the same client
//                     id, and the only way to follow the key-rotation runbook in
//                     `docs/console-storage.md` after setup has closed.
//
// ============================================================================
// THE CACHE IS INVALIDATED ON SUCCESS, AND THAT IS NOT OPTIONAL
// ============================================================================
//
// `consoleRuntime()` memoises the resolved sign-in configuration. Without
// `invalidateAuthConfig()` a correct save would keep serving the broken
// configuration until the TTL expired — which, on the screen whose whole purpose
// is repairing sign-in, reads as "the fix did not work" and invites a second,
// worse edit. `app/api/setup/route.ts` was the only caller until this one.

import {
  badRequest,
  readJsonBody,
  requireConsoleOwner,
  withConsoleSession,
} from "@/lib/console-api";
import { consoleSecretStore, invalidateAuthConfig } from "@/lib/auth-runtime";
import {
  replaceStoredClientSecret,
  updateAuthProvider,
  type AuthProviderUpdate,
} from "@/lib/auth-settings";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** A trimmed string, or `undefined` when the field was absent or blank. */
function optionalText(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? undefined : trimmed;
}

/**
 * `a.example, b.example` -> `["a.example", "b.example"]`.
 *
 * Lower-cased because the allow-list is compared against an email domain, which
 * arrives from the IdP in whatever case it chose, and because Moira stores what
 * it is given.
 */
function domainList(value: unknown): readonly string[] | undefined {
  if (typeof value !== "string") return undefined;
  const entries = value
    .split(",")
    .map((entry) => entry.trim().toLowerCase())
    .filter((entry) => entry !== "");
  return entries.length === 0 ? undefined : entries;
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async (context) => {
    const owner = await requireConsoleOwner(context);
    if (!owner.ok) return owner.response;

    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.authsettings_request_body_invalid);

    const store = consoleSecretStore(context.env);
    // NEVER a row id off the body: the row is the one this caller's own session
    // was resolved through. See `lib/auth-settings.ts`.
    const providerId = context.moiraProviderId;
    const secret = typeof body["client_secret"] === "string" ? body["client_secret"] : "";

    if (body["action"] === "replace_secret") {
      if (secret.trim() === "") {
        return badRequest(CONSOLE_MESSAGE_KEYS.authsettings_secret_required);
      }
      const result = await replaceStoredClientSecret(
        context.client,
        store,
        providerId,
        secret.trim(),
      );
      if (!result.ok) {
        // The generic key, not the drift's own: what the operator most needs to
        // know here is that the write LANDED and left the two stores
        // disagreeing, so they do not retry a save that already happened. The
        // specific drift state is on the summary panel above the form.
        return badRequest(CONSOLE_MESSAGE_KEYS.authsettings_drift_after_write);
      }
      invalidateAuthConfig();
      return Response.json({ ok: true }, { headers: { "cache-control": "no-store" } });
    }

    const clientId = optionalText(body["client_id"]);
    // Present-but-blank is a refusal rather than a silent "leave it". An operator
    // who cleared this field meant something by it, and Moira's PATCH cannot
    // express "clear" — so the honest answer is that the field is required, not
    // a save that quietly keeps the old value.
    if (typeof body["client_id"] === "string" && clientId === undefined) {
      return badRequest(CONSOLE_MESSAGE_KEYS.authsettings_client_id_required);
    }

    const domains = domainList(body["allowed_email_domains"]);
    // Same reasoning, and worse consequences: Moira accepts an empty allow-list,
    // and the deployment it produces denies every operator including this one.
    if (typeof body["allowed_email_domains"] === "string" && domains === undefined) {
      return badRequest(CONSOLE_MESSAGE_KEYS.authsettings_domains_required);
    }

    const update: AuthProviderUpdate = {
      ...(optionalText(body["display_name"]) === undefined
        ? {}
        : { displayName: optionalText(body["display_name"])! }),
      ...(clientId === undefined ? {} : { clientId }),
      ...(optionalText(body["discovery_url"]) === undefined
        ? {}
        : { discoveryUrl: optionalText(body["discovery_url"])! }),
      ...(optionalText(body["issuer"]) === undefined
        ? {}
        : { issuer: optionalText(body["issuer"])! }),
      ...(optionalText(body["authorization_url"]) === undefined
        ? {}
        : { authorizationUrl: optionalText(body["authorization_url"])! }),
      ...(optionalText(body["token_url"]) === undefined
        ? {}
        : { tokenUrl: optionalText(body["token_url"])! }),
      ...(domains === undefined ? {} : { allowedEmailDomains: domains }),
    };

    const result = await updateAuthProvider(
      context.client,
      store,
      providerId,
      update,
      secret.trim() === "" ? null : secret.trim(),
    );

    if (!result.ok) {
      if (result.failure.kind === "secret_required") {
        return badRequest(
          result.failure.reason === "client_id_moving"
            ? CONSOLE_MESSAGE_KEYS.authsettings_secret_required_for_new_client_id
            : // Nothing is stored, so nothing is "changing" — the deployment
              // simply has no secret to sign in with.
              CONSOLE_MESSAGE_KEYS.oauth_client_secret_missing,
        );
      }
      // As above: the write landed, and saying so is the difference between an
      // operator entering the secret again and one retrying the save.
      return badRequest(CONSOLE_MESSAGE_KEYS.authsettings_drift_after_write);
    }

    invalidateAuthConfig();
    return Response.json({ ok: true }, { headers: { "cache-control": "no-store" } });
  });
}
