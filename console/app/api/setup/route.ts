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
//   absent       CREATE. Ungated on a deployment with NO enabled provider —
//                which is the unclaimed, unprovisioned first run, claimable by
//                whoever completes setup first. That is inherent to first-run
//                bootstrap and is DOCUMENTED (`docs/console-architecture.md`)
//                rather than silently assumed. See the lockout note below for
//                the case where one IS enabled.
//   disabled     PATCH, ungated on the same condition and for the same reason:
//                a disabled row authenticates nobody, so rewriting it escalates
//                nothing.
//   enabled      PATCH only for a caller who AUTHENTICATED THROUGH that row.
//
// The session is resolved by the same function the claim step uses —
// `context.readSession` -> `consoleSessionCheck` — and compared on the Moira row
// the resolved configuration came from: `SessionCheck.moiraProviderId` when the
// session was admitted, `SessionCheck.resolvedProviderId` when it was refused
// only on the allow-list. Derived value against derived value, one resolution,
// no second spelling.
//
// "AUTHENTICATED THROUGH", not "holds an admissible session", and the
// difference is the whole remedy. `checkSession` also applies the row's own
// `allowed_email_domains`, so the operator who is here to WIDEN that list
// arrives refused by it. `operatorSessionFor` admits that one shape — refused
// on the allow-list, resolved through the row being written — and nothing else.
// Its own note states what that widens.
//
// It is resolved LAZILY, only once a run turns out to need proof of an
// operator. Resolving it costs a Moira read of the auth configuration, and on
// the fresh deployment that provisioning normally runs against there is no
// enabled provider to resolve — the honest answer there is "no session" and the
// request does not need to pay for it.
//
// ============================================================================
// ONE ENABLED PROVIDER. A SECOND IS A LOCKOUT, SO IT IS REFUSED OUTRIGHT
// ============================================================================
//
// The gate above covers writes against an EXISTING row. It says nothing about
// CREATING one, and the create path is reachable inside the setup window: POST
// provision with a DIFFERENT slug, and the derived state for that slug's issuer
// namespace is empty, so a new trusted issuer, a new provider row, a sealed
// secret and an enable all follow. Moira permits it (each row binds its own
// trusted issuer, so the partial unique index does not object).
//
// The console then breaks. `ambiguityGuard` (`lib/auth-config.ts`) refuses ANY
// resolution once more than one provider is enabled, so on the next cold resolve
// `consoleRuntime` is not ok, NO sign-in button renders for EITHER provider, and
// session resolution answers "no session" forever — which also makes the
// enabled-row gate above permanently unsatisfiable. Not an escalation: a denial
// of service that locks the operator out of their own wizard.
//
// The console therefore supports exactly ONE enabled auth provider, and
// `provisioningAdmissionFor` says so at the point the second one would be
// created — a 409 `setup_single_enabled_provider_only`, for every caller alike.
// Demanding proof of an operator instead (which is what stood here first) stops
// an anonymous caller and waves through the only person who can satisfy it: the
// legitimate operator, who is then locked out by their own successful request.
// A limit is not a permission, so it is not spelled as one.
//
// The count is the count `ambiguityGuard` itself reads: enabled and `active`,
// DEPLOYMENT-WIDE, from `SetupDeploymentState.enabledProviderIds`. Counting
// inside the derived state instead would be counting inside a namespace the
// caller chose, which is the same hole with an extra step.
//
// This is the rule `ambiguityGuard` already enforces, moved to the point where
// the damage would be done instead of discovered afterwards.
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
//   provision  the namespace stays caller-chosen, and that is the feature: a
//              provider is registered under an operator-chosen stable slug (see
//              `consoleIssuerForSlug`'s note on why the slug and not the row
//              UUID). What the slug CANNOT do is choose a row: the derivation
//              reads the trusted issuer for that namespace and then only the
//              provider BOUND to it, so a slug this console does not already
//              own can only ever create, never rewrite — and
//              `scanAuthProviders` filters on exactly that binding.
//
//              What it also cannot do, since the lockout note above, is DECIDE
//              WHETHER A CREATE IS ALLOWED. The enabled-provider count that
//              decision reads is deployment-wide, so choosing an unowned slug
//              changes which namespace is written and nothing else. On a
//              deployment with no enabled provider a well-formed slug nobody
//              asked for still buys a new disabled-then-enabled row under a
//              namespace with no admins, inside a window already gated on
//              holding the bootstrap system key; on a deployment with one, it
//              buys a 409 and nothing is written.

import { consoleProviderIdFor, isInteractiveMethod, isProviderSlug } from "@/lib/auth-config";
import { invalidateAuthConfig } from "@/lib/auth-runtime";
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
  deriveSetupDeploymentState,
  runSetupProvisioning,
  type AuthProviderConfig,
  type SetupDeploymentState,
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

/**
 * The `?slug=` this read is scoped to, or a keyed refusal.
 *
 * An ABSENT or EMPTY parameter means the incumbent issuer, which is what every
 * ordinary first run reads. Empty is folded onto absent here (and not in
 * `readSlug`, whose caller is a JSON body where `""` is a distinct thing a
 * client chose to send): `?slug=` is a URL an operator can produce by hand or by
 * deleting a value, and answering it with "that is not a usable slug" would be
 * pedantry about a query string that named nothing.
 */
function readSlugQuery(request: Request | undefined): { readonly slug: string | null } | Response {
  if (request === undefined) return { slug: null };
  const raw = new URL(request.url).searchParams.get("slug");
  if (raw === null || raw.trim() === "") return { slug: null };
  return readSlug(raw);
}

/**
 * The wizard's view model, for one console-issuer namespace.
 *
 * `request` is optional because this handler is also called IN PROCESS by
 * `app/setup/page.tsx`; Next supplies one on the HTTP route. Absent means the
 * incumbent issuer, the same thing an absent `?slug=` means.
 */
export async function GET(request?: Request): Promise<Response> {
  const slugResult = readSlugQuery(request);
  if (slugResult instanceof Response) return slugResult;

  return withSetupWindow(async (context) => {
    const methods = await context.client.getSetupAuthMethods();
    // Rehydrate what has ALREADY been provisioned for the requested issuer, so
    // the wizard survives the OAuth round trip (sign-in is a full navigation
    // away from /setup and back) and any revisit of a provisioned-but-unclaimed
    // deployment. The state is display-safe by construction: ids, versions,
    // booleans and a domain COUNT — never the allow-list itself (D4).
    //
    // Scoped to the SLUG rather than always to the incumbent: an operator
    // registering an additional provider under its own slug has to be able to
    // reload into THAT namespace, and the OAuth round trip IS a reload — if
    // this always answered for the incumbent they would land on the incumbent's
    // state with the wrong `provider_id` behind the sign-in button.
    //
    // Note what this scoping does NOT do: it does not decide whether a write is
    // gated. That count is deployment-wide (`SetupDeploymentState`), so a slug
    // whose namespace reads empty here can still be refused on provision.
    const consoleConfig = consoleIssuerConfigFor(
      context.env,
      context.env.jwksUrl,
      slugResult.slug,
    );
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
      // Which namespace this view model describes, echoed so the wizard can put
      // it back on the provision body, on the claim body and on the OAuth
      // callback URL. `null` is the incumbent.
      slug: slugResult.slug,
      // Better Auth's providerId for the requested issuer — an identifier the
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
 * The proof of operatorship one provisioning run demands, and the two refusals
 * it is phrased with.
 *
 * `admissible` is the set of `auth_provider_settings` rows a session must have
 * been established through to count. Never empty — a run that needs no proof
 * carries `null` instead of this — and always server-derived. It holds exactly
 * one row today, the one being re-saved; it stays a set because the comparison
 * is a membership test either way and a one-element array is not a reason to
 * write the test twice.
 */
interface OperatorProofRequirement {
  readonly admissible: readonly string[];
  /** 401: nothing identifies the caller at all. */
  readonly absentCode: string;
  readonly absentKey: string;
  /** 403: a session resolved, but not through an admissible row. */
  readonly mismatchCode: string;
  readonly mismatchKey: string;
}

/**
 * Whether this provisioning run may run at all, and what it must prove first.
 *
 * A provisioning run ALWAYS ends with a provider enabled — step 4 of
 * `runSetupProvisioning` is the commit point, and the only run that skips it is
 * one whose row is enabled already. So the question is always "what does that do
 * to a deployment that already has an enabled provider?", and there are exactly
 * three answers:
 *
 *   `ungated`   Nothing is enabled deployment-wide. The run takes the count from
 *               0 to 1, which is the state the console supports. A first run,
 *               and the completion of an interrupted one, land here and are
 *               asked for nothing — there is no operator to prove yet, which is
 *               inherent to first-run bootstrap and documented rather than
 *               assumed.
 *   `operator`  The run RE-SAVES the row that is already enabled. The count does
 *               not change, but the row is a live authenticator and PATCHing it
 *               re-points sign-in at another IdP. An ESCALATION, so proof is a
 *               session established through that same row.
 *   `refused`   The run would ADD a second enabled provider. See below.
 *
 * ============================================================================
 * WHY THE THIRD IS A REFUSAL AND NOT A STRONGER PROOF
 * ============================================================================
 *
 * This console supports exactly ONE enabled auth provider. That is not a policy
 * choice made here, it is what `lib/auth-config.ts` does: `ambiguityGuard`
 * refuses EVERY resolution once more than one row is enabled and `active`, so
 * the next cold resolve renders no sign-in button for EITHER provider,
 * `consoleRuntime` is not ok, and session resolution answers "no session"
 * forever — which also makes the `operator` branch above permanently
 * unsatisfiable. There is no session, no operator and no configuration under
 * which two enabled providers work; wave 4B built the multi-provider resolution
 * but did not switch it on, and `resolveAuthConfigs` returning N is capability,
 * not permission.
 *
 * So proof of an operator is the wrong gate. It was the gate here until this
 * change, and it stopped an anonymous caller while waving through the person the
 * lockout actually harms: a legitimate operator signs in, satisfies the proof,
 * gets their second provider created and enabled, and is locked out of their own
 * console on the next reload. A gate that only the victim can pass is not a
 * gate. The honest answer is that the limit is one, said at the point the second
 * one would be created, with the remedy that actually exists — disable the
 * current provider through Moira's admin API with the bootstrap system key
 * (`docs/console-architecture.md` has the procedure), which takes the count back
 * to 0 and puts this run on the `ungated` branch.
 *
 * ============================================================================
 * THE COUNT IS DEPLOYMENT-WIDE, AND THAT IS LOAD-BEARING TWICE
 * ============================================================================
 *
 * `SetupDeploymentState.enabledProviderIds` is collected with `ambiguityGuard`'s
 * OWN predicate (`enabled && status === "active"`), deployment-wide, never out
 * of the derived state. Once for correctness: the number that decides whether
 * the console still resolves after this run is that number, so the two must not
 * be able to drift. Once for security: `derived` describes the namespace the
 * caller's `slug` selected, so a namespace-scoped count answers "nothing is
 * enabled here" for every slug this console does not already own — which is
 * exactly the slug an anonymous caller picks to reach an ungated create.
 *
 * The consequence, stated because an operator can meet it: a deployment with an
 * enabled NON-INTERACTIVE provider row (a `jwt_bearer` method serving API
 * clients, say) counts as having one, and the wizard refuses to add an
 * interactive one beside it. That is not over-refusal — `ambiguityGuard` counts
 * that row too, so the console would refuse to resolve sign-in afterwards. The
 * previous gate reached the same deployment as an unsatisfiable 401.
 */
type ProvisioningAdmission =
  | { readonly kind: "ungated" }
  | { readonly kind: "operator"; readonly requirement: OperatorProofRequirement }
  | { readonly kind: "refused" };

function provisioningAdmissionFor(deployment: SetupDeploymentState): ProvisioningAdmission {
  const derived = deployment.state;
  if (derived.providerId !== null && derived.providerEnabled) {
    return {
      kind: "operator",
      requirement: {
        admissible: [derived.providerId],
        absentCode: "setup_enabled_provider_requires_session",
        absentKey: CONSOLE_MESSAGE_KEYS.setup_enabled_provider_requires_session,
        mismatchCode: "setup_enabled_provider_session_mismatch",
        mismatchKey: CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
      },
    };
  }
  // The derived row is absent or disabled, so it is not itself one of these:
  // whatever this run enables is an ADDITION to them.
  if (deployment.enabledProviderIds.length === 0) return { kind: "ungated" };
  return { kind: "refused" };
}

/**
 * Does this caller meet the requirement, and which row is their session
 * established through?
 *
 * Returns the caller's authenticating row id on success, `null` when no proof
 * was required at all, and a keyed refusal otherwise. `requirement === null`
 * short-circuits before the session is resolved — see the header note on why
 * the resolution is lazy.
 *
 * # An allow-list refusal is not an absent operator
 *
 * The question this gate asks is "did you authenticate through one of these
 * rows", and `SessionCheck.ok` is a stricter question than that: `readSession`
 * runs `checkSession`, which also applies the provider row's own
 * `allowed_email_domains` (the same exact-match rule Moira's
 * `evaluate_claim_policy` applies). So the ONE operator the re-save path exists
 * to serve — the one whose own domain is missing from the list they are here to
 * widen — arrives holding `ok:false / email_domain_not_allowed`.
 *
 * Refusing that caller makes the documented remedy ("add the domain and save
 * again") unfollowable: the wizard sends them back to the auth-settings form
 * after a refused claim, and the save they are sent back to make would be
 * refused with "sign in through it first" — to somebody already signed in
 * through that very row. So the gate admits exactly that shape, and only when
 * the row that authenticated them is one the requirement names.
 *
 * What that widens, stated: inside the setup window, anybody the deployment's
 * IdP will authenticate — not only an allow-listed address — can re-save the
 * enabled row. It does NOT admit an
 * anonymous caller (the hole closed by the commit this gate came from), a
 * session through a row the requirement does not name, an unverified address,
 * or a session whose provider could not be resolved at all. Those keep their
 * refusals, and keep them under their OWN keys: the
 * `setup_enabled_provider_requires_session` copy says "sign in through it
 * first", which is the wrong instruction for an address the IdP never verified.
 */
async function operatorSessionFor(
  context: SetupWindowContext,
  request: Request,
  requirement: OperatorProofRequirement | null,
): Promise<{ readonly sessionProviderId: string | null } | Response> {
  if (requirement === null) return { sessionProviderId: null };

  const session = await context.readSession(request.headers);
  if (!session.ok) {
    if (
      session.rejection === "email_domain_not_allowed" &&
      session.resolvedProviderId !== null &&
      requirement.admissible.includes(session.resolvedProviderId)
    ) {
      // Refused ONLY on a list this request may change, by a row the
      // requirement names. That is the operator, and `resolvedProviderId` comes
      // from the configuration that resolved the cookie, never from the body —
      // see its note on `SessionCheck`.
      return { sessionProviderId: session.resolvedProviderId };
    }
    if (session.rejection === "no_session") {
      // Nothing identifies the caller at all: a 401, because signing in is the
      // one thing that fixes it.
      return setupError(401, requirement.absentCode, requirement.absentKey);
    }
    // A session exists and may not act. Answered with the reason `checkSession`
    // actually decided rather than re-keyed as "sign in through it first" —
    // re-keying it would tell an operator with an unverified address, or one
    // whose session predates the provider column, to repeat the one action that
    // cannot change the answer.
    return setupError(403, session.rejection, session.messageKey);
  }
  if (!requirement.admissible.includes(session.moiraProviderId)) {
    // A real session, through a provider the requirement does not name. On a
    // multi-provider deployment that is an operator who signed in through the
    // wrong one; on an unclaimed one it is the same escalation with one extra
    // step, since any enabled provider can mint a session. Either way the
    // answer is the same, and nothing is written.
    return setupError(403, requirement.mismatchCode, requirement.mismatchKey);
  }
  return { sessionProviderId: session.moiraProviderId };
}

/**
 * The in-module lock fired where the pre-check did not — now say WHY, honestly.
 *
 * There is exactly one way to reach this: the derivation reported the row
 * DISABLED (so `operatorProofFor` asked for no proof and no session was ever
 * resolved) and the fresh read inside `runSetupProvisioning` found it ENABLED.
 * `sessionProviderId` was therefore `null` for every caller alike, and the
 * module can only report `no_session` — which is a fact about what it was
 * HANDED, not a fact about the caller.
 *
 * Answering it with the `setup_enabled_provider_requires_session` copy ("sign in
 * through it first") was therefore wrong for most of the callers who see it: an
 * operator whose address the IdP never verified, one outside the allow-list, one
 * whose session carries a provider this configuration does not contain, and one
 * who is simply signed in correctly all did exactly what that sentence tells
 * them to do. So the session is resolved HERE, once, against the row the error
 * names, and the same `operatorSessionFor` that phrases the pre-check's
 * refusals phrases this one — anonymous callers keep the 401 and the
 * sign-in-first copy, every other refusal arrives under the key that names its
 * own remedy.
 *
 * The remaining case is not a session refusal at all: the caller CAN prove
 * operatorship of the row, and the only thing wrong was that the console's
 * derived state was stale. That is `setup_provider_enabled_mid_save`, a 409 for
 * the same reason `setup_resume_state_conflict` is one — a reload re-derives the
 * truth and the save then goes through the ordinary re-save path.
 */
async function enabledProviderRaceRefusal(
  context: SetupWindowContext,
  request: Request,
  error: SetupEnabledProviderSessionError,
): Promise<Response> {
  if (error.reason !== "no_session") {
    return setupError(
      403,
      "setup_enabled_provider_session_mismatch",
      CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
    );
  }
  const operator = await operatorSessionFor(context, request, {
    admissible: [error.providerId],
    absentCode: "setup_enabled_provider_requires_session",
    absentKey: CONSOLE_MESSAGE_KEYS.setup_enabled_provider_requires_session,
    mismatchCode: "setup_enabled_provider_session_mismatch",
    mismatchKey: CONSOLE_MESSAGE_KEYS.setup_enabled_provider_session_mismatch,
  });
  if (operator instanceof Response) return operator;
  return setupError(
    409,
    "setup_provider_enabled_mid_save",
    CONSOLE_MESSAGE_KEYS.setup_provider_enabled_mid_save,
  );
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
  // be a row bound to this console's trusted issuer — `scanAuthProviders`
  // filters on exactly that binding.
  //
  // `deployment.enabledProviderIds` is the second thing the same scan answers,
  // and it is DELIBERATELY not scoped to this console issuer: the slug chooses
  // the namespace `derived` describes, so a namespace-scoped count is a count
  // the caller picked. See `SetupDeploymentState`.
  const deployment = await deriveSetupDeploymentState(
    context.client,
    (providerId) => hasSealedSecret(context.store, providerId),
    consoleConfig.issuer,
  );
  const derived = deployment.state;

  // ---- and whether it may be spent at all, and by whom ---------------------
  //
  // Which row may be written is settled above; this settles whether the write
  // may happen and who may ask for it. `provisioningAdmissionFor` decides which
  // of the three shapes this run is: an ordinary run that takes the deployment
  // from no enabled provider to one (nothing asked), a re-save of the live
  // authenticator (an escalation — proved by a session through that same row),
  // or a run that would leave the deployment with TWO enabled providers, which
  // the console cannot resolve sign-in for at all. The last is refused outright
  // rather than gated: see that function's note on why proof of an operator is
  // the wrong question for a limit.
  //
  // The refusal precedes the session read, which is not merely an optimisation:
  // the answer does not depend on the caller, so resolving a session in order to
  // deliver the same 409 to everybody would be spending a Moira read to look
  // like a gate.
  //
  // WHERE THIS SITS, honestly: it is the first check that consults the
  // DEPLOYMENT, not the first check in the handler. Two refusals run in `POST`
  // before `withSetupWindow` is even entered (an unreadable body, an unknown
  // action), and seven pure-SHAPE refusals run in this function ahead of it —
  // slug, method, display name, client id, resume shape, endpoints, allow-list.
  // All nine are decided from the request body alone and disclose nothing about
  // what exists.
  //
  // What the ordering DOES guarantee, and the reason it must not be relaxed:
  // this gate precedes every refusal that depends on deployment state — the
  // empty-secret rule below, which would otherwise disclose whether the console
  // holds a sealed secret for the derived row, and the resume-conflict rule,
  // which would otherwise disclose the derived row's id and binding — and it
  // precedes every write. A new check that reads `derived`, `deployment` or
  // Moira belongs BELOW this line, not above it.
  const admission = provisioningAdmissionFor(deployment);
  if (admission.kind === "refused") {
    // A conflict with what the deployment currently IS, not a fact about the
    // caller — so a 409, and the same answer for every caller alike.
    return setupError(
      409,
      "setup_single_enabled_provider_only",
      CONSOLE_MESSAGE_KEYS.setup_single_enabled_provider_only,
    );
  }
  const operator = await operatorSessionFor(
    context,
    request,
    admission.kind === "operator" ? admission.requirement : null,
  );
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
      // read. Caught BEFORE `SetupOrderingError`, whose subclass this is; the
      // generic "correct the configuration" copy would send the operator
      // looking for a misconfiguration that is not there.
      return await enabledProviderRaceRefusal(context, request, error);
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
  } finally {
    // ----------------------------------------------------------------------
    // ISSUE #152 — the console's own half of `docs/runtime-cache-invalidation.md`.
    // ----------------------------------------------------------------------
    //
    // A provisioning run is the one auth-configuration write this deployment
    // makes THROUGH the console, so it is the one place the console can do what
    // Moira's `moira_runtime_config` NOTIFY trigger does for Moira's own caches:
    // invalidate at the write rather than wait for a TTL. Without it, an
    // operator who re-points the provider here keeps being served the previous
    // one — which is exactly how #152 was met, and the endpoint that
    // configuration named was refusing connections.
    //
    // IN `finally`, NOT ON THE SUCCESS PATH. `runSetupProvisioning` is a
    // four-step chain that can fail after it has written: the resume remedies
    // exist precisely because a run can leave a created trusted issuer, a
    // created or PATCHed provider row, or a sealed secret behind. A snapshot
    // held across a partial write is stale in the same way a snapshot held
    // across a complete one is.
    //
    // Cheap on purpose: it drops one object. The next `consoleRuntime()` pays
    // for the re-read, and only the requests that actually need a configuration
    // do.
    invalidateAuthConfig();
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
