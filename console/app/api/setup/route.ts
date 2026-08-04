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
//
// ============================================================================
// WHICH ROW A PRIVILEGED WRITE MAY TOUCH IS DERIVED, NEVER SUBMITTED
// ============================================================================
//
// The whole setup window runs on the BOOTSTRAP SYSTEM KEY with no session in
// front of it, so any body field that selects a Moira row is a field that
// selects what an anonymous caller can rewrite with that key. `resume` used to
// be exactly that: a shape-checked payload whose `providerId` steered
// `runSetupProvisioning` into `getAuthProvider` + `patchAuthProvider` on ANY
// auth-provider row — including an enabled incumbent — and whose
// `consoleSecretStored` boolean stood in for proof that the console already
// holds that row's OAuth client secret. `GET` even publishes the row ids.
//
// So the authority moved server-side, and there is exactly one of it:
// `deriveProvisioningState` (`lib/setup-flow.ts`) reads the trusted issuer for
// THIS console issuer out of Moira and then the provider row BOUND to it, and
// asks the console's own secret store whether a secret is sealed for that row.
// The same function already backed `GET` and the claim gate; provisioning now
// uses it too, which makes the derived state the single answer to both "which
// row may be written" and "may the client secret be omitted".
//
// `body.resume` survives only as a HINT — something to CHECK the derived state
// against, never something to act on. When the two disagree the request is
// refused (`409 setup_resume_state_conflict`) instead of resolved in the
// caller's favour, because a disagreement is either a stale browser or a
// caller naming a row that is not this console's, and neither should write.
//
// ============================================================================
// AN ENABLED PROVIDER MAY ONLY BE RE-SAVED BY SOMEBODY SIGNED IN THROUGH IT
// ============================================================================
//
// Deriving the row server-side settles WHICH row a write may touch. It does not
// settle WHO may ask for that write, and for one shape of row that second
// question is the whole of the risk: an anonymous caller needs no `resume` at
// all to reach the update path, because `deriveProvisioningState` finds the
// console's own provider row for them. If that row is already ENABLED it is a
// live authenticator, and PATCHing it re-points sign-in — client id, discovery,
// authorization, token, userinfo and jwks URLs — at an IdP of the caller's
// choosing, with `allowed_email_domains` widened to a domain they own. Sign in,
// claim, done. The allow-list was the only thing in the way.
//
// So the three states of the derived row are treated differently, and the
// difference is about escalation rather than about tidiness:
//
//   absent       CREATE, ungated. An unclaimed, unprovisioned deployment is
//                claimable by whoever completes setup first. That is inherent
//                to first-run bootstrap and is DOCUMENTED
//                (`docs/console-architecture.md`) rather than silently assumed.
//   disabled     PATCH, ungated. A disabled row authenticates nobody, so
//                rewriting it escalates nothing.
//   enabled      PATCH only for a caller carrying a valid console session whose
//                authenticating provider IS that row.
//
// The session is resolved by the same function the claim step uses —
// `context.readSession` -> `consoleSessionCheck` — and compared on
// `SessionCheck.moiraProviderId`, the Moira row the resolved configuration came
// from. Derived value against derived value, one resolution, no second spelling.
//
// It is resolved LAZILY, only once the derived row turns out to be enabled.
// Resolving it costs a Moira read of the auth configuration, and on the fresh
// deployment that provisioning normally runs against there is no enabled
// provider to resolve — the honest answer there is "no session" and the request
// does not need to pay for it.
//
// This is exactly the shape of the legitimate remedy: the domain-refusal
// correction happens AFTER a successful sign-in (sign in, claim, `403
// admin_claim_domain_not_allowed`, come back and widen the allow-list), so the
// operator holds a session through this very provider at the moment they need
// to re-save it.
//
// ============================================================================
// `body.slug`: WHAT IT MAY STILL CHOOSE, AND WHAT IT MAY NOT
// ============================================================================
//
// The slug is the one caller-supplied selector left, and it selects the CONSOLE
// ISSUER NAMESPACE (`consoleIssuerForSlug`) rather than a row. The two actions
// treat it differently, on purpose:
//
//   claim      the namespace is CHECKED against the provider the session was
//              actually established through (`SessionCheck.consoleIssuer`).
//              Without that check, signing in through provider A and posting
//              provider B's slug wrote an `admin_identities` grant in B's
//              namespace for an identity that never authenticated against B —
//              and B's allow-list was never applied to it either. Refused 403,
//              nothing written.
//
//   provision  the namespace stays caller-chosen, and that is the feature: an
//              additional provider is registered under an operator-chosen
//              stable slug (see `consoleIssuerForSlug`'s note on why the slug
//              and not the row UUID). What the slug CANNOT do is choose a row:
//              `deriveProvisioningState` reads the trusted issuer for that
//              namespace and then only the provider BOUND to it, so a slug this
//              console does not already own can only ever create, never rewrite
//              — and `findProviderBoundTo` filters on exactly that binding. The
//              blast radius of a well-formed slug nobody asked for is therefore
//              a new, disabled-then-enabled provider row under a namespace with
//              no admins, inside a window that is already gated on holding the
//              bootstrap system key.

import { consoleProviderIdFor, isInteractiveMethod, isProviderSlug } from "@/lib/auth-config";
import { readJsonBody } from "@/lib/console-api";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import type { ConsoleSecretStore } from "@/lib/console-secrets";
import {
  SetupEnabledProviderSessionError,
  SetupOrderingError,
  SetupProvisioningError,
  claimAdminIdentity,
  consoleIssuerConfigFor,
  deriveProvisioningState,
  runSetupProvisioning,
  type AuthProviderConfig,
  type SetupProvisioningState,
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

/**
 * "Is a client secret sealed for this row" — presence, never the value.
 *
 * Passed to `deriveProvisioningState` as a bound predicate, for the same reason
 * `storeClientSecret` is a closure: `setup-flow.ts` must not be handed anything
 * that can reveal.
 */
async function hasSealedSecret(store: ConsoleSecretStore, providerId: string): Promise<boolean> {
  return (await store.read(providerId)) !== null;
}

export async function GET(): Promise<Response> {
  return withSetupWindow(async (context) => {
    const methods = await context.client.getSetupAuthMethods();
    // Rehydrate what has ALREADY been provisioned for the incumbent issuer, so
    // the wizard survives the OAuth round trip (sign-in is a full navigation
    // away from /setup and back) and any revisit of a provisioned-but-unclaimed
    // deployment. The state is display-safe by construction: ids, versions,
    // booleans and a domain COUNT — never the allow-list itself (D4).
    const consoleConfig = consoleIssuerConfigFor(context.env, context.env.jwksUrl, null);
    const state = await deriveProvisioningState(
      context.client,
      (providerId) => hasSealedSecret(context.store, providerId),
      consoleConfig.issuer,
    );
    return setupJson({
      claimed: context.claimed,
      storage_mode: context.storageMode,
      // Public by construction: the console publishes this JWKS document and
      // registers this issuer string with Moira. Neither is a credential.
      console_issuer: context.env.bffIssuerUrl,
      jwks_url: context.env.jwksUrl,
      audience: context.env.adminApiAudience,
      methods: methods.methods.map(methodView),
      state,
      // Better Auth's providerId for the incumbent issuer — an identifier the
      // sign-in step posts, not a credential. Derived by the one server
      // function that owns the derivation.
      provider_id: consoleProviderIdFor(context.env.bffIssuerUrl, consoleConfig.issuer),
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
    action === "provision"
      ? provision(context, request, body)
      : claim(context, request, body),
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

/**
 * Does the caller's `resume` hint contradict what the console derived?
 *
 * Only the three members that carry AUTHORITY are compared, and each is
 * compared for exact equality:
 *
 *   * `providerId` — which row a privileged write would target;
 *   * `trustedJwtIssuerId` — which trusted issuer that row must be bound to;
 *   * `consoleSecretStored` — whether the client secret may be omitted.
 *
 * Versions and the domain COUNT are deliberately NOT compared. They drift for
 * legitimate reasons — an enable bumps the version, a second tab saves, the
 * allow-list changes between the render and the submit — and comparing them
 * would turn the domain-refusal remedy ("add the domain and save again") back
 * into a dead end without protecting anything: neither field selects a row and
 * neither grants a permission.
 *
 * A hint that agrees adds nothing and is discarded; the derived state is what
 * provisioning runs on either way. The check exists so a browser that believes
 * something else is TOLD, rather than silently having its belief overwritten —
 * and so a caller that names a row this console does not own is refused with
 * nothing written.
 */
function resumeHintDisagrees(
  hint: SetupProvisioningState,
  derived: SetupProvisioningState,
): boolean {
  return (
    hint.providerId !== derived.providerId ||
    hint.trustedJwtIssuerId !== derived.trustedJwtIssuerId ||
    hint.consoleSecretStored !== derived.consoleSecretStored
  );
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

/**
 * May this caller re-save the row the console derived, and which row is their
 * session established through?
 *
 * Returns the caller's authenticating row id (or `null`) on success, and a
 * keyed refusal otherwise. Only an ENABLED derived row asks the question at
 * all — see the header note on why the answer is lazily resolved and why the
 * other two states are ungated.
 */
async function operatorSessionFor(
  context: SetupWindowContext,
  request: Request,
  derived: SetupProvisioningState,
): Promise<{ readonly sessionProviderId: string | null } | Response> {
  if (derived.providerId === null || !derived.providerEnabled) {
    return { sessionProviderId: null };
  }

  const session = await context.readSession(request.headers);
  if (!session.ok) {
    // 401 when there is nothing to identify the caller at all, 403 when there
    // is a session and it may not act. The same split the claim step makes, and
    // for the same reason: only one of the two is fixable by signing in.
    return setupError(
      session.rejection === "no_session" ? 401 : 403,
      "setup_enabled_provider_requires_session",
      CONSOLE_MESSAGE_KEYS.setup_enabled_provider_requires_session,
    );
  }
  if (session.moiraProviderId !== derived.providerId) {
    // A real session, through a DIFFERENT provider. On a multi-provider
    // deployment that is an operator who signed in through the wrong one; on an
    // unclaimed one it is the same escalation with one extra step, since any
    // enabled provider can mint a session. Either way the answer is the same,
    // and nothing is written.
    return setupError(
      403,
      "setup_enabled_provider_session_mismatch",
      CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
    );
  }
  return { sessionProviderId: session.moiraProviderId };
}

async function provision(
  context: SetupWindowContext,
  request: Request,
  body: SetupRequestBody,
): Promise<Response> {
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

  // Narrowed here only so a payload the console cannot read is a keyed 400
  // rather than a silent restart. Nothing is DECIDED from it — see
  // `resumeHintDisagrees` below.
  const resumeValue = body["resume"];
  const resumeHint =
    resumeValue === undefined || resumeValue === null ? null : parseProvisioningState(resumeValue);
  if (resumeValue !== undefined && resumeValue !== null && resumeHint === null) {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_resume_state_invalid);
  }

  // Read once, into one binding, and never rebound. Nothing below puts it into a
  // response, and `runSetupProvisioning` receives the closure rather than this.
  const clientSecret = typeof body["client_secret"] === "string" ? body["client_secret"] : "";

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

  // Stable per SUBMISSION, so a resume replays the same keys rather than minting
  // a second provider row beside the one the first attempt left behind.
  const submissionId = trimmedString(body["submission_id"]) || crypto.randomUUID();
  const idempotencyKeys = await setupIdempotencyKeys(submissionId);

  const consoleConfig = consoleIssuerConfigFor(context.env, context.env.jwksUrl, slugResult.slug);

  // ---- the authority ------------------------------------------------------
  //
  // Everything above this line is SHAPE, refused without asking Moira anything.
  // From here on the console needs to know what actually exists, and it asks
  // the source of truth rather than the caller: Moira's records for this
  // console issuer, plus the console's own secret store. `derived.providerId`
  // is the ONLY row a privileged write below may target, and it can only ever
  // be a row bound to this console's trusted issuer — `findProviderBoundTo`
  // filters on exactly that binding.
  const derived = await deriveProvisioningState(
    context.client,
    (providerId) => hasSealedSecret(context.store, providerId),
    consoleConfig.issuer,
  );

  // ---- and who may spend that authority --------------------------------
  //
  // FIRST, before any other refusal: which row may be written is settled above,
  // but an ENABLED row is a live authenticator and rewriting it re-points
  // sign-in. That takes an operator, and inside this window a session
  // established through that same row is the only available proof of one.
  // Ordered ahead of the body checks so an unauthorised caller learns nothing
  // about the deployment's shape from which refusal they get.
  const operator = await operatorSessionFor(context, request, derived);
  if (operator instanceof Response) return operator;

  // An EMPTY secret is acceptable in exactly one shape: a re-save of a provider
  // whose secret the console has ACTUALLY sealed. The domain-refusal remedy
  // says "add the domain and save again", and demanding the operator re-type a
  // secret the console holds — and they may no longer have — would make that
  // instruction unfollowable. What must never stand in for that fact is a
  // boolean the caller sent.
  if (clientSecret === "" && !derived.consoleSecretStored) {
    return setupBadRequest(CONSOLE_MESSAGE_KEYS.setup_client_secret_required);
  }

  if (resumeHint !== null && resumeHintDisagrees(resumeHint, derived)) {
    // Either a browser whose copy went stale (another tab saved, the row was
    // changed elsewhere) or a caller naming a row that is not this console's.
    // The console cannot tell those apart and does not need to: neither may
    // write, and a reload re-derives the truth.
    return setupError(
      409,
      "setup_resume_state_conflict",
      CONSOLE_MESSAGE_KEYS.setup_resume_state_conflict,
    );
  }

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
        if (clientSecret === "") {
          // A re-save with no new secret: the guard above admitted this shape
          // only because the DERIVED state says the console has already sealed
          // one for the bound row, so there is nothing to write.
          return;
        }
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
      // The DERIVED state, never the body's. When it names a provider row the
      // flow PATCHes that row; when it does not, the flow creates one. Either
      // way the row is this console's own, because that is the only kind
      // `deriveProvisioningState` can return.
      resume: derived,
      // The second lock on the same rule, asserted inside the module that
      // issues the write and against the row it reads back — so a row that
      // became enabled between the derivation above and the PATCH is refused
      // too, and so the property holds for every caller of
      // `runSetupProvisioning` rather than for this one audited route.
      sessionProviderId: operator.sessionProviderId,
    });

    return setupJson(
      {
        submission_id: submissionId,
        console_issuer: consoleConfig.issuer,
        // Better Auth's providerId for this provider, derived by the ONE server
        // function that owns the derivation. The wizard's sign-in step posts it
        // to `/api/auth/sign-in/oauth2`; it is an identifier, not a credential.
        provider_id: consoleProviderIdFor(context.env.bffIssuerUrl, consoleConfig.issuer),
        state: result.state,
        trace: result.trace,
      },
      201,
    );
  } catch (error) {
    if (error instanceof SetupProvisioningError) return provisioningFailure(error, submissionId);
    if (error instanceof SetupEnabledProviderSessionError) {
      // The in-module lock fired where the pre-check did not: the row was
      // reported disabled by the derivation and came back ENABLED on the fresh
      // read. Answered with the same keyed refusal the pre-check gives, because
      // the operator's next move is the same one — sign in through that
      // provider, then save again. Caught BEFORE `SetupOrderingError`, whose
      // subclass this is; the generic "correct the configuration" copy would
      // send them looking for a misconfiguration that is not there.
      return setupError(
        403,
        error.reason === "no_session"
          ? "setup_enabled_provider_requires_session"
          : "setup_enabled_provider_session_mismatch",
        error.reason === "no_session"
          ? CONSOLE_MESSAGE_KEYS.setup_enabled_provider_requires_session
          : CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
      );
    }
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

  // The CONSOLE's issuer, derived from the slug rather than accepted from the
  // body: a caller-supplied issuer string would be a caller-chosen
  // `admin_identities` namespace.
  const consoleIssuer = consoleIssuerConfigFor(
    context.env,
    context.env.jwksUrl,
    slugResult.slug,
  ).issuer;

  // ...and the slug is CHECKED against the provider the session was actually
  // established through, which is the half that was missing.
  //
  // Deriving the issuer from the slug bounds the STRING (PROVIDER_SLUG_PATTERN)
  // but not the AUTHORITY: `consoleIssuerForSlug` is total over well-formed
  // slugs, so an operator signed in through provider A could post provider B's
  // slug and be granted `moira:admin` in B's namespace. The allow-list
  // `checkSession` applied a few lines above is A's — that is the list of the
  // configuration that resolved the cookie — so nothing had yet compared the
  // namespace being written to against the provider that authenticated.
  //
  // `session.consoleIssuer` comes from the `ResolvedAuthConfig` that resolved
  // this session (`lib/auth.ts`'s `consoleSessionCheck`), never from the body,
  // so this compares a derived value against a derived value.
  if (session.consoleIssuer !== consoleIssuer) {
    return setupError(
      403,
      "setup_claim_issuer_mismatch",
      CONSOLE_MESSAGE_KEYS.setup_claim_issuer_mismatch,
    );
  }

  // The provisioning state is DERIVED HERE, never read off the request body.
  // The browser's copy does not survive the OAuth round trip (sign-in is a full
  // navigation away from /setup and back), and a claim gated on whatever the
  // client echoes would be gated on nothing at all. Moira's records plus the
  // console's own secret store are the source of truth on every claim.
  const state = await deriveProvisioningState(
    context.client,
    (providerId) => hasSealedSecret(context.store, providerId),
    consoleIssuer,
  );

  // `signedInWithAllowedIdentity` is TRUE because `checkSession` has just
  // applied the deployment's `allowed_email_domains` to this session — the same
  // list Moira applies at claim time. It is not an assumption restated here.
  const wizard: SetupWizardState = {
    claimed: context.claimed,
    provisioning: state,
    signedInWithAllowedIdentity: true,
    claimSucceeded: false,
  };

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
