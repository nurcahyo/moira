// `GET|POST /api/setup` — the one door the setup wizard writes through.
//
// ============================================================================
// WHY THIS HANDLER DOES NOT CALL `withConsoleSession`
// ============================================================================
//
// Every other handler under `app/api/**` re-checks the console session, and
// `tests/unit/architecture/route-handler-session.test.ts` fails on one that
// neither does nor is named on its exemption list. This file is the third entry
// on that list, and the reason is not a relaxation: setup runs BEFORE the first
// admin exists. There is no grant to check against, no operator to be signed in
// as, and the `claim` action below is the request that creates the first one.
// Requiring a session here would require the outcome as its own precondition.
//
// `withSetupWindow` (`lib/setup-window.ts`) is the gate instead, and it is
// narrower rather than weaker: no bootstrap system key is a 404, and a
// deployment Moira already reports as claimed is a 409 — re-read from Moira on
// every request, never cached.
//
// ============================================================================
// THE OAUTH CLIENT SECRET DOES NOT REACH MOIRA, AND CANNOT
// ============================================================================
//
// Decision D7: the console owns the OAuth client secret, Moira never stores it
// and has no endpoint that would return it. The mechanism here is not review, it
// is scope. `runSetupProvisioning` receives a CLOSURE (`storeClientSecret`) and
// never the value, so no code path inside `lib/setup-flow.ts` can place the
// secret in a request body — that module never names it. This file is where the
// closure is built, and it is the FIRST production caller of
// `ConsoleSecretStore.put()`.
//
// The secret is read out of the request body into one `const`, handed to that
// closure, and never written to a response, a log, or a trace entry. The success
// and failure payloads below are built field by field for the same reason.
//
// ============================================================================
// WHY `GET` AGGREGATES SERVER-SIDE (decision D4)
// ============================================================================
//
// `GET /api/v1/admin/setup/auth-methods` is authenticated on purpose: its
// `PublicAuthMethod` projection carries `allowed_email_domains`, which is the
// deny-by-default admin-claim policy (plan 07 decision D3). Moira's ANONYMOUS
// projection — `PublicSignInMethod` — deliberately omits it for exactly that
// reason.
//
// The setup window is reachable without a session, so anything this route
// returns is anonymous-visible in practice. The raw auth-methods response
// therefore never crosses to the browser: the view model below is built field by
// field, publishes COUNTS and PRESENCE rather than the domain list or the
// endpoint URLs, and the browser is not offered a way to call auth-methods
// itself.

import { isInteractiveMethod, isProviderSlug } from "@/lib/auth-config";
import { readJsonBody } from "@/lib/console-api";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import {
  SetupOrderingError,
  SetupProvisioningError,
  claimAdminIdentity,
  consoleIssuerConfigFor,
  runSetupProvisioning,
  type AuthProviderConfig,
  type SetupWizardState,
} from "@/lib/setup-flow";
import {
  parseProvisioningState,
  setupBadRequest,
  setupError,
  setupIdempotencyKeys,
  setupJson,
  withSetupWindow,
  type SetupWindowContext,
} from "@/lib/setup-window";
import type { AuthMethod, PublicAuthMethod } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/* -------------------------------------------------------------------------- */
/* GET — the display-safe view model                                          */
/* -------------------------------------------------------------------------- */

/**
 * One provider row, reduced to what a wizard needs to decide "is this already
 * configured?".
 *
 * Deliberately NOT the row. `allowed_email_domains` becomes a count,
 * `client_id`/`discovery_url` become booleans, and the endpoint URLs are absent
 * entirely — see the header note on D4.
 */
interface SetupMethodView {
  readonly id: string;
  readonly method: AuthMethod;
  readonly display_name: string;
  readonly interactive: boolean;
  readonly has_client_id: boolean;
  readonly has_discovery_url: boolean;
  readonly allowed_email_domain_count: number;
}

function methodView(method: PublicAuthMethod): SetupMethodView {
  const clientId = method.client_id ?? "";
  const discoveryUrl = method.discovery_url ?? "";
  return {
    id: method.id,
    method: method.method,
    display_name: method.display_name,
    interactive: isInteractiveMethod(method.method),
    has_client_id: clientId !== "",
    has_discovery_url: discoveryUrl !== "",
    allowed_email_domain_count: method.allowed_email_domains.length,
  };
}

export async function GET(): Promise<Response> {
  return withSetupWindow(async (context) => {
    const methods = await context.client.getSetupAuthMethods();
    return setupJson({
      claimed: context.claimed,
      storage_mode: context.storageMode,
      // Public by construction: the console publishes this JWKS document and
      // registers this issuer string with Moira. Neither is a credential.
      console_issuer: context.env.bffIssuerUrl,
      jwks_url: context.env.jwksUrl,
      audience: context.env.adminApiAudience,
      methods: methods.methods.map(methodView),
    });
  });
}

/* -------------------------------------------------------------------------- */
/* POST — one action per request                                              */
/* -------------------------------------------------------------------------- */

export async function POST(request: Request): Promise<Response> {
  const body = await readJsonBody(request);
  if (body === null) return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_request_body_invalid);

  const action = body["action"];
  if (action !== "provision" && action !== "claim") {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_action_unknown);
  }

  return withSetupWindow(async (context) =>
    action === "provision" ? provision(context, body) : claim(context, request, body),
  );
}

/* -------------------------------------------------------------------------- */
/* provision                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The parsed JSON body, before either action has interpreted it.
 *
 * Named rather than spelled inline because both actions read from the SAME
 * object and neither owns a DTO for it: every field is narrowed at the point of
 * use, so a shared interface here would be a shape that claims a validation the
 * handlers actually perform one field at a time.
 */
type SetupRequestBody = Record<string, unknown>;

function trimmedString(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function optionalUrl(value: unknown): string | null {
  const trimmed = trimmedString(value);
  return trimmed === "" ? null : trimmed;
}

function stringList(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((entry): entry is string => typeof entry === "string")
    .map((entry) => entry.trim())
    .filter((entry) => entry !== "");
}

/** The slug this provider is provisioned under, or a keyed refusal. */
function readSlug(value: unknown): { readonly slug: string | null } | Response {
  if (value === undefined || value === null) return { slug: null };
  const slug = trimmedString(value);
  // `consoleIssuerForSlug` throws a developer diagnostic on a bad slug, and that
  // refusal is one an OPERATOR can trigger from a form here — so it is caught
  // before the call rather than surfacing as a 500. The rule itself is not
  // restated: `isProviderSlug` is the one spelling of it.
  if (slug === "" || !isProviderSlug(slug)) {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_provider_slug_invalid);
  }
  return { slug };
}

async function provision(context: SetupWindowContext, body: SetupRequestBody): Promise<Response> {
  const slugResult = readSlug(body["slug"]);
  if (slugResult instanceof Response) return slugResult;

  const method = body["method"];
  if (typeof method !== "string" || !isInteractiveMethod(method as AuthMethod)) {
    // `jwks` is a bearer-token trust method with no OAuth client and no sign-in
    // button; provisioning one through the wizard would produce a provider row
    // the console can never offer.
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_method_unsupported);
  }

  const displayName = trimmedString(body["display_name"]);
  if (displayName === "") {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_display_name_required);
  }

  const clientId = trimmedString(body["client_id"]);
  if (clientId === "") return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_client_id_required);

  // Read once, into one binding, and never rebound. Nothing below puts it into a
  // response, and `runSetupProvisioning` receives the closure rather than this.
  const clientSecret = typeof body["client_secret"] === "string" ? body["client_secret"] : "";
  if (clientSecret === "") {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_client_secret_required);
  }

  const issuer = optionalUrl(body["issuer"]);
  const discoveryUrl = optionalUrl(body["discovery_url"]);
  const authorizationUrl = optionalUrl(body["authorization_url"]);
  const tokenUrl = optionalUrl(body["token_url"]);
  if (
    discoveryUrl === null &&
    (issuer === null || authorizationUrl === null || tokenUrl === null)
  ) {
    // Moira refuses the same shape with `auth_provider_method_config_incomplete`,
    // several hundred milliseconds and one partial write later. Refusing here
    // costs nothing and leaves no orphan trusted-issuer row behind.
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_issuer_or_discovery_required);
  }

  const allowedEmailDomains = stringList(body["allowed_email_domains"]);
  if (allowedEmailDomains.length === 0) {
    // Deny-by-default with NO first-claim exemption: an empty list refuses every
    // claim including the operator's own, so an empty submission would provision
    // a deployment nobody can ever become admin of.
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_allowed_email_domains_required);
  }

  const resumeValue = body["resume"];
  const resume =
    resumeValue === undefined || resumeValue === null ? null : parseProvisioningState(resumeValue);
  if (resumeValue !== undefined && resumeValue !== null && resume === null) {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_resume_state_invalid);
  }

  // Stable per SUBMISSION, so a resume replays the same keys rather than minting
  // a second provider row beside the one the first attempt left behind.
  const submissionId = trimmedString(body["submission_id"]) || crypto.randomUUID();
  const idempotencyKeys = await setupIdempotencyKeys(submissionId);

  const consoleConfig = consoleIssuerConfigFor(context.env, context.env.jwksUrl, slugResult.slug);

  const provider: AuthProviderConfig = {
    method: method as AuthMethod,
    displayName,
    issuer,
    discoveryUrl,
    authorizationUrl,
    tokenUrl,
    userinfoUrl: optionalUrl(body["userinfo_url"]),
    jwksUrl: optionalUrl(body["jwks_url"]),
    clientId,
    allowedEmailDomains,
    requestedScopes: stringList(body["requested_scopes"]),
    redirectUris: stringList(body["redirect_uris"]),
  };

  try {
    const result = await runSetupProvisioning(context.client, {
      console: consoleConfig,
      provider,
      storeClientSecret: async (providerId, storedClientId) => {
        if (storedClientId === null || storedClientId === "") {
          // The AEAD binds `(providerId, clientId)`, so there is nothing to seal
          // against. Thrown rather than skipped: a provider enabled with no
          // console-side secret cannot run a code exchange, and the drift would
          // surface at the first sign-in as an opaque `invalid_client`.
          throw new Error(
            "Moira returned a provider row with no client_id; the console cannot bind a " +
              "client secret to it",
          );
        }
        await context.store.put(providerId, storedClientId, clientSecret);
      },
      idempotencyKeys,
      ...(resume === null ? {} : { resume }),
    });

    return setupJson(
      {
        submission_id: submissionId,
        console_issuer: consoleConfig.issuer,
        state: result.state,
        trace: result.trace,
      },
      201,
    );
  } catch (error) {
    if (error instanceof SetupProvisioningError) return provisioningFailure(error, submissionId);
    if (error instanceof SetupOrderingError) {
      // The B1 invariant, or an adopted trusted issuer that declares
      // `scopes_claim`. Both are configuration the operator must change before a
      // retry can differ, so neither is a transport failure.
      return setupError(
        409,
        "setup_ordering_violated",
        CONSOLE_MESSAGE_KEYS.setup_ordering_violated,
      );
    }
    throw error;
  }
}

/**
 * A partial write, rendered so the wizard can resume rather than restart.
 *
 * Restarting is what re-POSTs the trusted JWT issuer into
 * `trusted_jwt_issuers_issuer_active_unique` — an index Moira does not map to a
 * 409, so the operator would see an opaque `500 database_error`. The `state` in
 * this payload is what a retry sends back as `resume`, and
 * `ensureTrustedJwtIssuer` is reuse-first for the same reason.
 *
 * `requires_client_secret_re_entry` is derived from the STATE rather than from
 * the remedy string: the secret has to be re-entered exactly when the console
 * has not stored it, and on remedy `retry_enable_no_secret_re_entry` it already
 * has. The secret itself appears nowhere in this response, on any remedy.
 */
function provisioningFailure(error: SetupProvisioningError, submissionId: string): Response {
  const moiraError = error.moiraError;
  return Response.json(
    {
      error: {
        code: "setup_provisioning_failed",
        step: error.step,
        remedy: error.remedy,
        message_key: error.messageKey,
        message_args: null,
        requires_client_secret_re_entry: !error.state.consoleSecretStored,
        submission_id: submissionId,
        state: error.state,
        moira: moiraError,
      },
    },
    {
      status: moiraError === null ? 500 : moiraError.kind === "api" ? moiraError.status : 502,
      headers: { "cache-control": "no-store" },
    },
  );
}

/* -------------------------------------------------------------------------- */
/* claim                                                                      */
/* -------------------------------------------------------------------------- */

function emailDomain(email: string): string {
  const at = email.lastIndexOf("@");
  return at === -1 ? "" : email.slice(at + 1).toLowerCase();
}

async function claim(
  context: SetupWindowContext,
  request: Request,
  body: SetupRequestBody,
): Promise<Response> {
  const slugResult = readSlug(body["slug"]);
  if (slugResult instanceof Response) return slugResult;

  const state = parseProvisioningState(body["state"]);
  if (state === null) return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_resume_state_invalid);

  const session = await context.readSession(request.headers);
  if (!session.ok) {
    // `session.messageKey` is already the keyed reason `checkSession` decided —
    // re-keying it here would be a second spelling of one rule.
    return setupError(
      session.rejection === "no_session" ? 401 : 403,
      session.rejection,
      session.messageKey,
    );
  }
  const identity = session.identity;

  // `signedInWithAllowedIdentity` is TRUE because `checkSession` has just
  // applied the deployment's `allowed_email_domains` to this session — the same
  // list Moira applies at claim time. It is not an assumption restated here.
  const wizard: SetupWizardState = {
    claimed: context.claimed,
    provisioning: state,
    signedInWithAllowedIdentity: true,
    claimSucceeded: false,
  };

  // The CONSOLE's issuer, derived from the slug rather than accepted from the
  // body: a caller-supplied issuer string would be a caller-chosen
  // `admin_identities` namespace.
  const consoleIssuer = consoleIssuerConfigFor(
    context.env,
    context.env.jwksUrl,
    slugResult.slug,
  ).issuer;

  try {
    const record = await claimAdminIdentity(context.client, wizard, {
      consoleIssuer,
      subject: identity.idpSubject,
      email: identity.email,
      emailVerified: identity.emailVerified,
    });
    return setupJson({ identity: record }, 201);
  } catch (error) {
    if (error instanceof SetupOrderingError) {
      // Two distinct conditions, and merging them would send an operator to the
      // wrong remedy: an unverified address needs a different identity, an
      // unreachable claim step needs the auth-settings step finished.
      return identity.emailVerified
        ? setupError(
            409,
            "setup_claim_step_unreachable",
            CONSOLE_MESSAGE_KEYS.setup_claim_step_unreachable,
          )
        : setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_email_not_verified);
    }
    if (
      isMoiraRequestError(error) &&
      error.moiraError.kind === "api" &&
      error.moiraError.code === "admin_claim_domain_not_allowed"
    ) {
      // The expected first-run misconfiguration, named with the domain that
      // caused it. Passing Moira's generic envelope through instead would tell
      // the operator that "the domain is not allowed" without saying which one,
      // on the one screen where they can still fix it.
      return setupError(
        403,
        "admin_claim_domain_not_allowed",
        CONSOLE_MESSAGE_KEYS.setup_claim_domain_not_allowed,
        { domain: emailDomain(identity.email) },
      );
    }
    throw error;
  }
}
