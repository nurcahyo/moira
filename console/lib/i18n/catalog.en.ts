// The console's English message catalog.
//
// ============================================================================
// THE GATE IS `tsc`, NOT A TEST
// ============================================================================
//
// `CONSOLE_CATALOG` is declared `Record<ConsoleMessageKey, CatalogEntry>`. A key
// in `keys.ts` with no entry here is a **missing property** error; an entry here
// with no key is an **excess property** error. Both fire at `bun run typecheck`,
// before any test runs.
//
// That is deliberately the primary layer rather than a source-scanning test.
// Moira learned this the expensive way (`src/i18n/catalog/mod.rs:53-58`): a
// source-text walker only ever sees *literal* arguments, which is how 23 of 28
// execution-failure classes shipped as bare keys with no English at all. The
// repair there was a `const` block that refuses to compile
// (`src/i18n/catalog/mod.rs:107-121`); this is the TypeScript spelling of it,
// and `lib/types.ts:20-52` already uses the same idiom for the DTO descriptors.
//
// The test layer (`tests/unit/lib/i18n-catalog-coverage.test.ts`) is still
// required, because `tsc` cannot see emission SITES: it cannot tell that a key
// is referenced from somewhere, nor that a bare `console.error.…` literal
// appeared in a module that never imported `keys.ts`.
//
// ============================================================================
// ENTRY SHAPE
// ============================================================================
//
// Mirrors Moira's `I18nEntry` (`src/i18n/catalog/mod.rs:24-29`): `key`,
// `message`, `description` — all three mandatory. `description` says WHEN the
// key is used, not what the English says; a description that paraphrases the
// message tells a translator nothing, and the coverage test rejects
// `message === description` for exactly that reason.
//
// ============================================================================
// WHAT IS DELIBERATELY *NOT* IN HERE
// ============================================================================
//
// 1. `lib/env.ts` boot diagnostics (the `problems.push(...)` strings). They are
//    raised as `ConsoleConfigError` and printed to a LOG at process start —
//    there is no browser, and there may not even be an HTTP server yet.
//    Cataloguing them would also break five substring assertions in
//    `tests/unit/lib/env.test.ts` for zero operator benefit.
//
// 2. Pure-developer `throw new Error` diagnostics: `setup-flow.ts:179-180,
//    351-353,414-416,421,451-452,670-671`; `moira-session.ts:147,199-201`;
//    `auth.ts:136`; `moira-client.ts:495`. Every one of them describes a
//    programming mistake ("the console built a request it is forbidden to
//    build"), is unreachable from operator input, and is read by whoever is
//    holding the stack trace.
//
// 3. The English inside `SetupProvisioningError` (`setup-flow.ts:515,543,
//    563-564,581-582,603`) and inside `lib/errors.ts:244,272`. Those are the
//    **fallback slot**, not the render path. `SetupProvisioningError` already
//    carries a `messageKey` (`setup-flow.ts:258`) and every one of those keys is
//    in `keys.ts`; `toMoiraError`/`toTransportError` already carry
//    `CONSOLE_MALFORMED_ERROR_KEY`/`CONSOLE_TRANSPORT_ERROR_KEY` plus structured
//    `messageArgs`. Adding the entries below makes `t()` win at render time.
//    DO NOT "fix" those call sites by deleting their English: `errors.test.ts:226`
//    asserts `typeof text.message === "string"`, and the deliberate no-echo
//    guarantee at `errors.test.ts:208` lives in that same literal.
//
// 4. `docs/i18n-response-catalog.json` gets NO console entries. It is generated
//    from the Rust catalog and `src/i18n/catalog/mod.rs:591-648` compares all
//    three fields in BOTH directions — a console entry fails that gate.
//    `lib/moira-keys.ts` stays where it is and stays English-free, which is what
//    keeps `tests/unit/lib/moira-keys.test.ts:67-75` meaningful.

import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "./keys";

/** One catalog entry. Mirrors Moira's `I18nEntry`. */
export interface CatalogEntry {
  readonly key: ConsoleMessageKey;
  /** English default. May contain `{placeholder}` tokens. */
  readonly message: string;
  /** WHEN this key is emitted. Never a paraphrase of `message`. */
  readonly description: string;
}

const K = CONSOLE_MESSAGE_KEYS;

/**
 * Every console-originated string.
 *
 * The annotation is the gate — see the header. Do not replace it with
 * `as const` alone, and do not widen the key type to `string`.
 */
export const CONSOLE_CATALOG: Readonly<Record<ConsoleMessageKey, CatalogEntry>> = {
  /* --- lib/errors.ts ------------------------------------------------------ */
  [K.moira_unreachable]: {
    key: K.moira_unreachable,
    message: "The console could not reach Moira. Check that the backend is running, then retry.",
    description:
      "The fetch to Moira produced no HTTP response at all — DNS, TLS, connection reset, or an " +
      "abort. Emitted by `toTransportError` in lib/errors.ts. The thrown cause is never echoed.",
  },
  [K.moira_response_unreadable]: {
    key: K.moira_response_unreadable,
    message: "Moira returned HTTP {status} with a response the console could not read.",
    description:
      "An HTTP error whose body was not a Moira ErrorResponse — proxy HTML, an empty body, a " +
      "gateway page. Emitted by `toMoiraError` in lib/errors.ts. `{status}` comes from " +
      "`messageArgs`; the body itself is deliberately never included.",
  },

  /* --- lib/auth-config.ts ------------------------------------------------- */
  [K.no_enabled_auth_provider]: {
    key: K.no_enabled_auth_provider,
    message: "No sign-in provider is enabled yet. Finish setting up this deployment first.",
    description:
      "`resolveAuthConfigs` found no active, enabled `auth_provider_settings` row. This is the " +
      "normal first-run state, not a failure — it is the setup wizard's whole reason to exist.",
  },
  [K.ambiguous_enabled_auth_providers]: {
    key: K.ambiguous_enabled_auth_providers,
    message:
      "More than one sign-in provider is enabled. The console will not guess which one governs — " +
      "disable all but one in Moira.",
    description:
      "`ambiguityGuard` saw more than one enabled row. Wave 4A replaced Moira's row-ordered " +
      "policy lookup with a deterministic two-stage one plus a partial unique index, which is " +
      "what makes this refusal redundant — but only on a deployment that has actually RUN that " +
      "migration, and the console cannot tell. So the guard stays until 4A is deployed rather " +
      "than merely merged. The resolution underneath it is already N-capable.",
  },
  [K.auth_method_not_interactive]: {
    key: K.auth_method_not_interactive,
    message:
      "The enabled provider uses a machine trust method, not an interactive sign-in method. " +
      "Nobody can sign in through it.",
    description:
      "The enabled row's `method` is `jwks` — a bearer-token trust method for services, not a " +
      "browser sign-in method. Emitted by `isInteractiveMethod` failing in lib/auth-config.ts.",
  },
  [K.auth_provider_endpoints_incomplete]: {
    key: K.auth_provider_endpoints_incomplete,
    message:
      "The enabled provider is missing the endpoints an OAuth sign-in needs: either a discovery " +
      "URL, or both an authorization URL and a token URL.",
    description:
      "`hasUsableEndpoints` returned false. Raised before a sign-in button is offered, so the " +
      "operator is not sent into a flow that cannot complete.",
  },
  [K.allowed_email_domains_empty]: {
    key: K.allowed_email_domains_empty,
    message:
      "The enabled provider allows no email domains, so every sign-in would be denied. Set " +
      "`allowed_email_domains` on the provider in Moira.",
    description:
      "The enabled row's `allowed_email_domains` is empty. The policy is deny-by-default (plan " +
      "07 decision D3), so an empty list denies everyone rather than allowing everyone.",
  },
  [K.provider_not_bound_to_trusted_jwt_issuer]: {
    key: K.provider_not_bound_to_trusted_jwt_issuer,
    message:
      "The enabled provider is not bound to a trusted JWT issuer, so no sign-in through it can " +
      "ever be granted admin authority.",
    description:
      "The enabled row carries no `trusted_jwt_issuer_id` (the B1 defect). Such a row can be " +
      "signed into and can never produce a successful claim; caught on the read path so the " +
      "failure does not land as a 403 on the very last step of setup.",
  },
  [K.trusted_jwt_issuer_not_resolvable]: {
    key: K.trusted_jwt_issuer_not_resolvable,
    message:
      "The trusted JWT issuer this provider is bound to cannot supply an issuer string for this " +
      "console. Re-bind the provider to an issuer this console registered.",
    description:
      "Wave 4B: the console's minted `iss` is the `issuer` of the bound `trusted_jwt_issuers` " +
      "row. Two shapes reach this key — the id names no active issuer row the console can " +
      "read, or the row's `issuer` is outside the namespace this console owns so no stable " +
      "Better Auth `providerId` can be derived from it. Refusing beats inventing an id: " +
      "`account.providerId` cannot be migrated once a human has signed in.",
  },

  /* --- lib/console-secrets.ts / lib/auth-config.ts ------------------------ */
  [K.oauth_client_secret_missing]: {
    key: K.oauth_client_secret_missing,
    message:
      "The console holds no OAuth client secret for the enabled provider. Re-enter it to finish " +
      "the sign-in configuration.",
    description:
      "D7 splits the provider configuration in two: Moira holds the non-secret half, the console " +
      "holds the client secret. This is the drift state where Moira has the provider and the " +
      "console has nothing sealed for it. Emitted from both lib/auth-config.ts and " +
      "lib/console-secrets.ts — one key, two emitters, deliberately.",
  },
  [K.oauth_client_id_drifted]: {
    key: K.oauth_client_id_drifted,
    message:
      "The console's stored OAuth client secret was sealed against a different client ID than " +
      "the one Moira now has. Re-enter the secret for the current client ID.",
    description:
      "`classifySecretDrift` returned `client_id_mismatch`: both halves exist but disagree. The " +
      "sealed secret is bound to its `client_id` as AAD, so using it would fail to decrypt " +
      "rather than authenticate as the wrong client.",
  },
  [K.moira_provider_client_id_missing]: {
    key: K.moira_provider_client_id_missing,
    message:
      "The provider in Moira carries no client ID, so there is nothing for the console's secret " +
      "to be bound to.",
    description:
      "`classifySecretDrift` returned `moira_client_id_missing`. Distinct from the missing-secret " +
      "state: here the console has nothing to bind TO, so re-entering a secret would not help " +
      "until the Moira row is fixed.",
  },

  /* --- lib/auth-runtime.ts ------------------------------------------------ */
  [K.auth_config_unavailable]: {
    key: K.auth_config_unavailable,
    message:
      "The console cannot read its sign-in configuration: it has no snapshot and no bootstrap " +
      "credential to fetch one with.",
    description:
      "The bootstrap deadlock in lib/auth-runtime.ts's header, reported as itself rather than as " +
      "an opaque 401 from Moira. Reached when `MOIRA_SYSTEM_KEY` has been removed and the " +
      "process has not yet snapshotted a configuration — typically a fresh replica after a " +
      "restart.",
  },

  /* --- lib/moira-session.ts ----------------------------------------------- */
  [K.session_required]: {
    key: K.session_required,
    message: "You need to be signed in to do that.",
    description:
      "`checkSession` found no console session, or one with no email address. Not an error " +
      "condition on the sign-in page itself — it is what gates every authenticated surface.",
  },
  [K.email_not_verified]: {
    key: K.email_not_verified,
    message:
      "Your identity provider has not verified this email address. Moira refuses admin claims " +
      "from unverified addresses.",
    description:
      "The session's `emailVerified` is not `true`. The console enforces this before calling " +
      "Moira, which would refuse the claim anyway with 403 admin_claim_email_not_verified.",
  },
  [K.email_domain_not_allowed]: {
    key: K.email_domain_not_allowed,
    message: "This email domain is not allowed to administer this deployment.",
    description:
      "The session's email domain is outside the provider's `allowed_email_domains`. The same " +
      "allow-list Moira applies at claim time, applied again at the console session boundary so " +
      "the two answers cannot disagree.",
  },
  [K.idp_subject_missing]: {
    key: K.idp_subject_missing,
    message:
      "Your identity provider supplied no stable subject, so the console cannot identify you to " +
      "Moira.",
    description:
      "No `account.accountId` was recorded for the session. Minting a token anyway would produce " +
      "one that verifies against the console's JWKS and then matches no admin_identities grant.",
  },
  [K.session_provider_unknown]: {
    key: K.session_provider_unknown,
    message: "This session predates multi-provider sign-in. Sign out and sign in again.",
    description:
      "Wave 4B stamps the authenticating provider onto the session row, and the minted `iss` " +
      "selects which trusted_jwt_issuers row — and therefore which admin_identities grant " +
      "namespace — the token is redeemed against. The column is nullable, so every session " +
      "live at deploy time reaches the minter without one. Those sessions REFUSE rather than " +
      "default: defaulting would authorise the session against a provider that did not " +
      "authenticate it.",
  },

  /* --- lib/setup-flow.ts -------------------------------------------------- */
  [K.trusted_jwt_issuer_registration_failed]: {
    key: K.trusted_jwt_issuer_registration_failed,
    message: "Registering the console's trusted JWT issuer with Moira failed. Retry is safe.",
    description:
      "`SetupProvisioningError` at step `ensure_trusted_jwt_issuer`. Nothing was written, so the " +
      "remedy is a plain retry.",
  },
  [K.auth_provider_create_failed]: {
    key: K.auth_provider_create_failed,
    message:
      "Creating the sign-in provider in Moira failed. Retrying reuses the trusted JWT issuer " +
      "that was already registered.",
    description:
      "`SetupProvisioningError` at step `create_auth_provider` — the §0 partial state. The " +
      "issuer row exists and is inert; a naive retry that re-POSTs it hits " +
      "`trusted_jwt_issuers_issuer_active_unique`, which is not mapped to a 409 and surfaces as " +
      "an opaque 500 database_error. Reuse-first provisioning is what avoids it.",
  },
  [K.auth_provider_secret_write_failed]: {
    key: K.auth_provider_secret_write_failed,
    message:
      "Storing the OAuth client secret in the console failed. The provider exists in Moira but " +
      "is still disabled; retry, or discard it and start again.",
    description:
      "`SetupProvisioningError` at step `store_console_secret`. The Moira half of the dual write " +
      "landed and the console half did not — the D7 drift state, created deliberately rather " +
      "than discovered later.",
  },
  [K.auth_provider_enable_failed]: {
    key: K.auth_provider_enable_failed,
    message:
      "Enabling the sign-in provider failed. Everything else is already stored, so retrying does " +
      "not ask for the secret again.",
    description:
      "`SetupProvisioningError` at step `enable_auth_provider` — the dual write's commit point. " +
      "Retry safety comes from `If-Match` plus `enable` being naturally idempotent; the " +
      "operation declares no `Idempotency-Key`.",
  },

  /* --- the BFF setup door (lib/setup-window.ts, app/api/setup/route.ts) ----
   *
   * Every entry here is a refusal the CONSOLE decided. None of them is a Moira
   * error passed through: those already carry their own `message_key`, and
   * `lib/errors.ts` maps them to a remedy. The one apparent exception,
   * `setup_claim_domain_not_allowed`, is deliberately the console's own key —
   * Moira's envelope for that code does not name the offending domain, and this
   * is the one screen on which the operator can still change it. */
  [K.setup_system_key_absent]: {
    key: K.setup_system_key_absent,
    message:
      "First-run setup is not available on this deployment: it holds no bootstrap credential.",
    description:
      "`withSetupWindow` refused with 404 because the console has no bootstrap system key. Either " +
      "it was never configured, or the operator removed it after finishing setup — which is what " +
      "they are told to do, so this is the normal steady state rather than a fault.",
  },
  [K.setup_already_claimed]: {
    key: K.setup_already_claimed,
    message: "Setup is already complete for this deployment. Sign in instead.",
    description:
      "`withSetupWindow` refused with 409: Moira's claim-status says an admin identity already " +
      "exists. Read from Moira on every request, never cached — retrying will not change it.",
  },
  [K.setup_request_body_invalid]: {
    key: K.setup_request_body_invalid,
    message: "The setup request could not be read. Send it again.",
    description:
      "`POST /api/setup` received a body that was absent, not JSON, or not a JSON object. Kept " +
      "distinct from a rejected FIELD so the wizard can tell a transport problem from a " +
      "validation one.",
  },
  [K.setup_action_unknown]: {
    key: K.setup_action_unknown,
    message: "That setup step is not one this console performs.",
    description:
      "`POST /api/setup` was sent an `action` other than `provision` or `claim`. Reachable only " +
      "from a client this console did not ship, so it is refused rather than guessed at.",
  },
  [K.setup_method_unsupported]: {
    key: K.setup_method_unsupported,
    message: "Choose a sign-in method the console can offer a button for.",
    description:
      "The submitted `method` is absent, unknown, or non-interactive (`jwks` is a bearer-token " +
      "trust method with no OAuth client). Provisioning one would create a provider row that can " +
      "never be offered at sign-in.",
  },
  [K.setup_display_name_required]: {
    key: K.setup_display_name_required,
    message: "Give the sign-in provider a name to show on the sign-in button.",
    description:
      "`display_name` was empty. Schema-required by Moira — omitting it is a 400 there — and it " +
      "is the string operators actually see, so it is refused here before any write.",
  },
  [K.setup_client_id_required]: {
    key: K.setup_client_id_required,
    message: "Enter the OAuth client ID issued by your identity provider.",
    description:
      "`client_id` was empty. Without it there is nothing for the console to seal its client " +
      "secret against: the encryption binds `(provider id, client id)` as additional data.",
  },
  [K.setup_client_secret_required]: {
    key: K.setup_client_secret_required,
    message: "Enter the OAuth client secret issued by your identity provider.",
    description:
      "`client_secret` was empty. The console stores it encrypted in its own database and never " +
      "sends it to Moira, so an empty value is a sign-in that cannot complete its code exchange.",
  },
  [K.setup_issuer_or_discovery_required]: {
    key: K.setup_issuer_or_discovery_required,
    message:
      "Supply a discovery document, or the issuer with its authorization and token endpoints.",
    description:
      "Neither a discovery URL nor a complete manual endpoint set was submitted. Moira refuses the " +
      "same shape as `auth_provider_method_config_incomplete` one write later; refusing here leaves " +
      "no orphan trusted-issuer row behind.",
  },
  [K.setup_allowed_email_domains_required]: {
    key: K.setup_allowed_email_domains_required,
    message: "List at least one email domain that may become an administrator.",
    description:
      "`allowed_email_domains` was empty. The policy is deny-by-default with no first-claim " +
      "exemption, so an empty list would refuse every claim — including the operator's own, on the " +
      "very next step.",
  },
  [K.setup_provider_slug_invalid]: {
    key: K.setup_provider_slug_invalid,
    message:
      "Use a short lower-case name, letters and digits separated by hyphens, for this provider.",
    description:
      "The submitted `slug` is not a usable provider slug. It becomes a URL path segment in the " +
      "OAuth redirect and part of the issuer string Moira pins tokens to, neither of which can be " +
      "changed after the first sign-in.",
  },
  [K.setup_resume_state_invalid]: {
    key: K.setup_resume_state_invalid,
    message:
      "The console could not read what the previous attempt completed. Start this step again.",
    description:
      "A `resume`/`state` payload did not narrow back to a provisioning state. Refused rather than " +
      "treated as a fresh start: restarting re-registers the trusted JWT issuer and hits a unique " +
      "index Moira reports as an opaque server error.",
  },
  [K.setup_ordering_violated]: {
    key: K.setup_ordering_violated,
    message:
      "This deployment's identity configuration must be corrected before setup can continue.",
    description:
      "`SetupOrderingError` escaped provisioning — a trusted issuer that asserts scopes, a deleted " +
      "one being reused, or a provider row that came back without its issuer binding. A retry " +
      "cannot differ until the configuration changes.",
  },
  [K.setup_claim_step_unreachable]: {
    key: K.setup_claim_step_unreachable,
    message: "Finish configuring the sign-in provider before claiming administrator access.",
    description:
      "`assertClaimStepIsReachable` refused: the provisioning gate is not complete, so the claim " +
      "would be a request Moira is guaranteed to deny. Navigation state, not advice.",
  },
  [K.setup_email_not_verified]: {
    key: K.setup_email_not_verified,
    message:
      "Your identity provider has not verified this address. Sign in with a verified account.",
    description:
      "`claimAdminIdentity` refused before the request left the process because the session " +
      "reported an unverified address. Moira refuses the same claim with " +
      "`admin_claim_email_not_verified`; this is the defence in depth in front of it.",
  },
  [K.setup_claim_domain_not_allowed]: {
    key: K.setup_claim_domain_not_allowed,
    message:
      "Moira refused this claim: the domain {domain} is not on this deployment's allow-list.",
    description:
      "Moira answered `403 admin_claim_domain_not_allowed`. Re-keyed by the console so the offending " +
      "domain is named — Moira's own envelope does not carry it, and this is the last screen on " +
      "which the allow-list can still be changed.",
  },

  /* --- accessibility ------------------------------------------------------ */
  //
  // These two are pinned CHARACTER FOR CHARACTER by shipped tests:
  // `tests/unit/atoms/Spinner.test.tsx:6,8` asserts the accessible name is
  // exactly "Loading"; `tests/unit/atoms/Label.test.tsx:16,25` and
  // `tests/unit/molecules/FormField.test.tsx:14,18-19,51` assert on
  // "Email * (required)", which requires the LEADING SPACE below. If either of
  // those goes red, this English changed — that is the signal, not a reason to
  // edit the test.
  [K.a11y_loading]: {
    key: K.a11y_loading,
    message: "Loading",
    description:
      'Default accessible name for the Spinner atom\'s `role="status"` region. `status` is ' +
      '"name from author" per ARIA, so a visually-hidden text node alone would not name it.',
  },
  [K.a11y_required]: {
    key: K.a11y_required,
    message: " (required)",
    description:
      "Screen-reader-only suffix appended by the Label atom when `required` is set, so the " +
      "requirement is not conveyed by the bare `*` glyph alone. The leading space is load-bearing: " +
      'it separates the suffix from the label text in the computed accessible name ("Email * (required)").',
  },

  /* --- document metadata -------------------------------------------------- */
  [K.meta_title]: {
    key: K.meta_title,
    message: "Moira Console",
    description: "The `<title>` of every console page, via `generateMetadata()` in app/layout.tsx.",
  },
  [K.meta_description]: {
    key: K.meta_description,
    message: "Administer a Moira deployment: identities, sign-in providers, and credentials.",
    description: 'The `<meta name="description">` served with every console page.',
  },

  /* --- pages -------------------------------------------------------------- */
  [K.page_home_title]: {
    key: K.page_home_title,
    message: "Overview",
    description:
      "The `<h1>` of the home route `/`. Deliberately NOT the same string as `console.meta.title`: " +
      "the document title names the product, the heading names the page inside it, and the " +
      "coverage guard rejects two keys sharing one English string because a copy edit would " +
      "then silently apply to only one of the two places.",
  },
  [K.page_home_body]: {
    key: K.page_home_body,
    message:
      "Administration surfaces arrive in later waves. This page exists so the console chrome has " +
      "a home to render into.",
    description:
      "Body copy on the home route. Replaced when the dashboard lands; kept keyed so the " +
      "replacement is a catalog edit rather than a component edit.",
  },
  [K.page_login_title]: {
    key: K.page_login_title,
    message: "Sign in",
    description:
      "The `<h1>` of `/login`. Deliberately distinct from `console.signIn.heading`, which names " +
      "the panel INSIDE the page — the page can host other content around it.",
  },
  [K.page_admins_title]: {
    key: K.page_admins_title,
    message: "Admin access",
    description:
      "The `<h1>` of `/admins`. Distinct from `console.admins.heading`, which names the grants " +
      "region inside the page; the page also hosts the invitation form and the invitation list.",
  },
  [K.page_invite_title]: {
    key: K.page_invite_title,
    message: "Admin invitation",
    description:
      "The `<h1>` of the public `/invite/[token]` page. Deliberately says nothing about who sent " +
      "it or which deployment it is for — the page is reachable by anyone holding the link.",
  },

  /* --- sign-in ------------------------------------------------------------ */
  [K.sign_in_heading]: {
    key: K.sign_in_heading,
    message: "Sign in to this deployment",
    description: "Accessible name of the SignInPanel organism's region on /login.",
  },
  [K.sign_in_button]: {
    key: K.sign_in_button,
    message: "Continue with {provider}",
    description:
      "One sign-in button per RESOLVED provider, when that provider's display name is known. " +
      "`{provider}` is the `display_name` from the anonymous " +
      "`GET /api/v1/admin/setup/sign-in-methods` projection, or — when several providers are " +
      "offered and Moira gave no name for one — its Better Auth provider id, because two buttons " +
      "sharing one accessible name is worse than an ugly one. Wave 4B made the panel N-capable; " +
      "`ambiguityGuard` still refuses a deployment with more than one enabled row until wave 4A " +
      "is deployed, so today it renders one.",
  },
  [K.sign_in_button_generic]: {
    key: K.sign_in_button_generic,
    message: "Continue with your identity provider",
    description:
      "The sign-in button when the anonymous sign-in-methods call yielded no display name for the " +
      "resolved provider — Moira unreachable, or the row absent from the projection. The " +
      "configuration is already resolved at this point, so the button still works. Used only when " +
      "there is exactly ONE provider: with several, an unnamed provider falls back to its id " +
      "instead, so the buttons stay distinguishable.",
  },
  [K.sign_in_pending]: {
    key: K.sign_in_pending,
    message: "Signing in",
    description:
      "Accessible name of the Spinner shown while `POST /api/auth/sign-in/oauth2` is in flight and " +
      "the browser has not yet been redirected to the identity provider.",
  },
  [K.sign_in_unavailable_heading]: {
    key: K.sign_in_unavailable_heading,
    message: "Sign-in is not available",
    description:
      "Heading above any refusal state. Rendered INSTEAD of a button, never alongside one — a " +
      "button that 503s on click is the failure this surface exists to avoid.",
  },
  [K.sign_in_request_failed]: {
    key: K.sign_in_request_failed,
    message: "The console could not start the sign-in. Try again in a moment.",
    description:
      "`POST /api/auth/sign-in/oauth2` returned a non-2xx other than 429, or the request threw. " +
      "The response body is deliberately not echoed: it is a Better Auth error object and can " +
      "name internal configuration.",
  },
  [K.sign_in_rate_limited]: {
    key: K.sign_in_rate_limited,
    message: "Too many sign-in attempts. Wait a moment, then try again.",
    description:
      "HTTP 429 from `POST /api/auth/sign-in/oauth2`. Better Auth's rate limiter is on in " +
      "production with database storage, so the limit is SHARED ACROSS REPLICAS and a user can " +
      "hit it without having clicked many times themselves.",
  },
  [K.sign_in_no_redirect_url]: {
    key: K.sign_in_no_redirect_url,
    message: "The sign-in did not return a destination to continue to.",
    description:
      "`POST /api/auth/sign-in/oauth2` answered 200 with no `url` field. Distinguished from a " +
      "plain failure because it means the configuration resolved but produced no authorization URL.",
  },

  /* --- generic actions ---------------------------------------------------- */
  //
  // `console.action.*` rather than `console.secret.*` on purpose: `CopyButton` is
  // a presentational atom with no idea what it is copying, and a key namespaced
  // to the secret surface would make it look like one. It is reused by the next
  // thing that needs a copy control.
  [K.action_copy]: {
    key: K.action_copy,
    message: "Copy",
    description: "The CopyButton atom's idle label.",
  },
  [K.action_copied]: {
    key: K.action_copied,
    message: "Copied",
    description:
      "The CopyButton atom's label after a successful clipboard write, announced through a polite " +
      "live region so the change is not silent for a screen-reader user.",
  },
  [K.action_copy_failed]: {
    key: K.action_copy_failed,
    message: "Could not copy. Select the value and copy it manually.",
    description:
      "`navigator.clipboard.writeText` rejected or is unavailable — it requires a secure context " +
      "and can be blocked by permissions policy. The value is still on screen, so this is a " +
      "degradation, not a failure.",
  },

  /* --- the once-only secret surface --------------------------------------- */
  [K.secret_modal_heading]: {
    key: K.secret_modal_heading,
    message: "Invitation created",
    description: "Accessible name of the OnceOnlySecretModal dialog.",
  },
  [K.secret_shown_once]: {
    key: K.secret_shown_once,
    message: "This is shown once. Copy it now — the console cannot display it again.",
    description:
      "The warning above the value. Moira returns the raw token exactly once, at creation; every " +
      "later read of the record returns the sanitized shape, which has no token field at all.",
  },
  [K.secret_token_label]: {
    key: K.secret_token_label,
    message: "Invitation token",
    description:
      "Labels the raw value, as distinct from the shareable link built from it. Both are shown " +
      "because an operator pasting into a chat wants the link and one automating a setup wants " +
      "the token.",
  },
  [K.secret_link_label]: {
    key: K.secret_link_label,
    message: "Invitation link",
    description:
      "Labels the link. Moira's envelope carries the raw token and never a URL — only the console " +
      "knows its own public origin — so the link is composed here.",
  },
  [K.secret_dismiss]: {
    key: K.secret_dismiss,
    message: "I have copied it",
    description:
      'The dialog\'s only close control. Worded as a confirmation rather than "Close" because ' +
      "dismissing it is irreversible.",
  },
  [K.secret_already_shown]: {
    key: K.secret_already_shown,
    message:
      "This invitation already exists and its token was shown when it was created. It cannot be " +
      "shown again — revoke it and create a new one if you no longer have it.",
    description:
      "`secret === null` in the envelope. THE NORMAL IDEMPOTENT-REPLAY CASE, not an error: " +
      "`AdminInviteSecretResponse.secret` is nullable and not required, and the stored replay body " +
      "is the sanitized record. A UI that treats null as a failure reports a successful, correct " +
      "operation as broken.",
  },
  [K.secret_expires_at]: {
    key: K.secret_expires_at,
    message: "Expires {expires_at}.",
    description:
      "Rendered under the value. `{expires_at}` is `AdminInviteRecord.expires_at`, an RFC 3339 " +
      "timestamp. The record's `expired` flag is derived server-side and is not stored — nothing " +
      "sweeps for expiry, so `status` never reads `expired`.",
  },
  [K.action_cancel]: {
    key: K.action_cancel,
    message: "Cancel",
    description:
      "The dismissing control on every DangerConfirmDialog. `console.action.*` rather than a " +
      "per-screen key because the dialog is a molecule with no idea what it is confirming.",
  },

  /* --- the authenticated chrome (plan 09 wave 5) -------------------------- */
  [K.chrome_nav_label]: {
    key: K.chrome_nav_label,
    message: "Console sections",
    description:
      "Accessible name of the `<nav>` in the (console) layout. `no-hardcoded-copy.test.tsx` " +
      "forbids a literal `aria-label`, so every landmark name is a catalog key.",
  },
  [K.chrome_nav_home]: {
    key: K.chrome_nav_home,
    message: "Home",
    description:
      "Navigation link to `/`, the authenticated home route. Deliberately not the same English " +
      "as `console.page.home_title` — two keys sharing one message fail the catalog gate, and a " +
      "nav item and a page heading are edited by different people for different reasons.",
  },
  [K.chrome_nav_admins]: {
    key: K.chrome_nav_admins,
    message: "Admins",
    description:
      "Navigation link to `/admins`. Without it that route is reachable only by typing the URL, " +
      "which is why the (console) layout's own header scheduled the chrome for this wave.",
  },
  [K.chrome_sign_out]: {
    key: K.chrome_sign_out,
    message: "Sign out",
    description: "The sign-out control in the console header.",
  },
  [K.chrome_sign_out_pending]: {
    key: K.chrome_sign_out_pending,
    message: "Signing out",
    description:
      "Announced while the sign-out request is in flight. The control is `aria-busy` for the " +
      "same interval.",
  },
  [K.chrome_sign_out_failed]: {
    key: K.chrome_sign_out_failed,
    message: "Could not sign out. Close this browser or clear its cookies.",
    description:
      "Better Auth's sign-out endpoint refused or was unreachable. The remedy is deliberately " +
      "client-side: the session cookie is the only thing that needs to stop existing, and the " +
      "console cannot promise a server round trip it just failed to make.",
  },

  /* --- invitation lifetimes ----------------------------------------------- */
  [K.expiry_label]: {
    key: K.expiry_label,
    message: "Valid for",
    description: "Label of the ExpiryPicker select in the invitation form.",
  },
  [K.expiry_hint]: {
    key: K.expiry_hint,
    message: "Moira refuses anything longer than 72 hours rather than shortening it silently.",
    description:
      "Hint under the ExpiryPicker. States the cap as a REFUSAL because that is what " +
      "`validated_invite_lifetime` does — an operator who believed they issued a 30-day " +
      "invitation and silently received a 3-day one would find out at the worst moment.",
  },
  [K.expiry_option_one_hour]: {
    key: K.expiry_option_one_hour,
    message: "1 hour",
    description:
      "The shortest offered lifetime. Separate from the plural key because English has no " +
      "plural-rule machinery in this catalog and `1 hours` reads as a bug.",
  },
  [K.expiry_option_hours]: {
    key: K.expiry_option_hours,
    message: "{hours} hours",
    description:
      "Every offered lifetime above one hour. `{hours}` is an integer supplied by ExpiryPicker.",
  },

  /* --- the /admins screen -------------------------------------------------- */
  [K.admins_heading]: {
    key: K.admins_heading,
    message: "Admin grants",
    description: "Heading and accessible name of the grants region on /admins.",
  },
  [K.admins_intro]: {
    key: K.admins_intro,
    message:
      "Everyone listed here can use this console. Only the owner can transfer ownership or " +
      "revoke another admin.",
    description:
      "Intro copy on /admins. States the ownership rule in the terms Moira enforces it in — " +
      "`require_primary_actor` reads the caller's own row, so this is row state, not a scope.",
  },
  [K.admins_per_grant_note]: {
    key: K.admins_per_grant_note,
    message:
      "Each row is one sign-in identity. Somebody who signs in through two different providers " +
      "appears twice, and revoking one row leaves the other active.",
    description:
      "Finding F24, stated to the operator instead of papered over. `admin_identities` is keyed " +
      "on (issuer, subject) and there is no column linking two grants to one human, so the " +
      "screen must not imply person-level identity it does not have.",
  },
  [K.admins_table_label]: {
    key: K.admins_table_label,
    message: "Admin sign-in identities",
    description: "Accessible name of the grants table.",
  },
  [K.admins_column_email]: {
    key: K.admins_column_email,
    message: "Email",
    description:
      "First column, deliberately. `issuer` is this console's own string on every row and " +
      "disambiguates nothing; `subject` is an opaque IdP identifier. Email is the only " +
      "human-identifiable attribute on the record, which is why decision D5 makes it required.",
  },
  [K.admins_column_status]: {
    key: K.admins_column_status,
    message: "Status",
    description: "Grant status column header.",
  },
  [K.admins_column_created]: {
    key: K.admins_column_created,
    message: "Granted",
    description: "Column header for `created_at` on a grant.",
  },
  [K.admins_column_actions]: {
    key: K.admins_column_actions,
    message: "Actions",
    description: "Column header for the per-row controls.",
  },
  [K.admins_owner_badge]: {
    key: K.admins_owner_badge,
    message: "Owner",
    description:
      "Rendered for `is_primary`. A property of the ROW, never of the signed-in reader — " +
      "`lib/types.ts` carries the same warning, because a console that treated it as a " +
      "permission would disagree with Moira the moment a non-primary admin opened the page.",
  },
  [K.admins_status_active]: {
    key: K.admins_status_active,
    message: "Active",
    description: "`AdminIdentityStatus.active`.",
  },
  [K.admins_status_revoked]: {
    key: K.admins_status_revoked,
    message: "Revoked",
    description: "`AdminIdentityStatus.revoked` — a soft revoke; the row is retained.",
  },
  [K.admins_empty]: {
    key: K.admins_empty,
    message: "No admin sign-in identities have been granted yet.",
    description:
      "Empty state for the grants table. Reachable on a deployment whose only admin was created " +
      "through the bootstrap system key and then revoked.",
  },
  [K.admins_activity_label]: {
    key: K.admins_activity_label,
    message: "Admin management activity",
    description:
      "Accessible name of the polite live region that reports the outcome of a transfer or a " +
      "revocation. Present before it is populated, because a live region created and filled in " +
      "the same tick is frequently missed.",
  },
  [K.admins_working]: {
    key: K.admins_working,
    message: "Applying the change",
    description: "Announced while a transfer or revocation is in flight.",
  },
  [K.admins_request_failed]: {
    key: K.admins_request_failed,
    message: "The request did not reach this deployment. Check your connection and try again.",
    description:
      "The browser could not complete the call to the console's own route handler. Distinct " +
      "from a refusal by Moira, which arrives with its own key and is rendered through `t()`.",
  },
  [K.admins_transfer]: {
    key: K.admins_transfer,
    message: "Make owner",
    description:
      "Per-row transfer control. ONE request: `set_primary` demotes every other active primary " +
      "in the same transaction, so there is no second demote-the-actor call to make.",
  },
  [K.admins_transfer_confirm_title]: {
    key: K.admins_transfer_confirm_title,
    message: "Transfer ownership?",
    description: "Accessible name and heading of the transfer confirmation dialog.",
  },
  [K.admins_transfer_confirm_body]: {
    key: K.admins_transfer_confirm_body,
    message:
      "{email} becomes the owner and you stop being it. Only they will be able to transfer it " +
      "back.",
    description:
      "Transfer confirmation body. Says the actor loses ownership, because they do: exactly one " +
      "grant can be primary at a time, enforced by a unique index.",
  },
  [K.admins_transfer_confirm_action]: {
    key: K.admins_transfer_confirm_action,
    message: "Transfer ownership",
    description: "The confirming control in the transfer dialog.",
  },
  [K.admins_revoke]: {
    key: K.admins_revoke,
    message: "Revoke",
    description: "Per-row control that soft-revokes a grant.",
  },
  [K.admins_revoke_confirm_title]: {
    key: K.admins_revoke_confirm_title,
    message: "Revoke admin access?",
    description: "Accessible name and heading of the grant revocation dialog.",
  },
  [K.admins_revoke_confirm_body]: {
    key: K.admins_revoke_confirm_body,
    message: "{email} loses access to this console immediately. An invitation can restore it.",
    description:
      "Revocation confirmation body. Names the remedy, because revocation and re-invitation are " +
      "the two ordinary operations that together do what a recovery flow would.",
  },
  [K.admins_revoke_confirm_action]: {
    key: K.admins_revoke_confirm_action,
    message: "Revoke access",
    description: "The confirming control in the revocation dialog.",
  },
  [K.admins_owner_not_revocable]: {
    key: K.admins_owner_not_revocable,
    message: "The owner cannot be revoked. Transfer ownership to somebody else first.",
    description:
      "Decision D-F20's operator-visible consequence, stated as a RULE rather than surfaced as a " +
      "failed request: `revoke_grant` clears `is_primary` and the last-primary guard refuses " +
      "that, so on a deployment with one admin the operation is permanently unavailable for " +
      "that row. Rendered beside the disabled control, never as an error banner.",
  },

  /* --- the invitation form ------------------------------------------------- */
  [K.admins_invite_heading]: {
    key: K.admins_invite_heading,
    message: "Invite an admin",
    description: "Heading and accessible name of the invitation form region.",
  },
  [K.admins_invite_constraint_label]: {
    key: K.admins_invite_constraint_label,
    message: "Bind this invitation to",
    description:
      "Label of the constraint selector. `constraint` is required and there is no " +
      "anyone-with-the-link option, because an unbound invitation would make a leaked URL " +
      "equivalent to handing out admin.",
  },
  [K.admins_invite_constraint_email]: {
    key: K.admins_invite_constraint_email,
    message: "One email address",
    description: "`AdminInviteConstraint.email`.",
  },
  [K.admins_invite_constraint_domain]: {
    key: K.admins_invite_constraint_domain,
    message: "Any address at one domain",
    description:
      "`AdminInviteConstraint.domain`. Exact match on the domain — `sub.example.com` is not " +
      "admitted by `example.com`, mirroring `evaluate_claim_policy`.",
  },
  [K.admins_invite_value_label_email]: {
    key: K.admins_invite_value_label_email,
    message: "Email address",
    description: "Field label when the constraint is a single address.",
  },
  [K.admins_invite_value_label_domain]: {
    key: K.admins_invite_value_label_domain,
    message: "Email domain",
    description: "Field label when the constraint is a domain.",
  },
  [K.admins_invite_value_hint_email]: {
    key: K.admins_invite_value_hint_email,
    message: "Only this address can redeem the invitation.",
    description: "Hint under the value field in email mode.",
  },
  [K.admins_invite_value_hint_domain]: {
    key: K.admins_invite_value_hint_domain,
    message: "Any address at exactly this domain can redeem the invitation, once.",
    description:
      "Hint under the value field in domain mode. Says `once`: the invitation is single-use " +
      "whichever constraint it carries.",
  },
  [K.admins_invite_value_required]: {
    key: K.admins_invite_value_required,
    message: "Enter the address or domain to invite.",
    description: "Client-side required-field refusal on the invitation form.",
  },
  [K.admins_invite_submit]: {
    key: K.admins_invite_submit,
    message: "Create invitation",
    description: "Submit control of the invitation form.",
  },
  [K.admins_invite_pending]: {
    key: K.admins_invite_pending,
    message: "Creating the invitation",
    description: "Announced while the create request is in flight.",
  },
  [K.admins_invite_domain_not_in_allow_list]: {
    key: K.admins_invite_domain_not_in_allow_list,
    message:
      "No enabled sign-in provider admits this domain, so the invitation could not be redeemed. " +
      "Add the domain to that provider's allowed email domains first.",
    description:
      "The pre-submit gate's HARD refusal, used only when exactly one provider is enabled — the " +
      "one case in which the console's union provably equals the row Moira will resolve. UI " +
      "gating only; Moira's redeem-time check remains the authority.",
  },
  [K.admins_invite_no_enabled_provider]: {
    key: K.admins_invite_no_enabled_provider,
    message: "No sign-in provider is enabled, so nobody could redeem an invitation yet.",
    description:
      "The unambiguous refusal: with zero enabled providers there is no way to sign in at all, " +
      "so an invitation would strand its holder however it was written.",
  },
  [K.admins_invite_multi_provider_warning]: {
    key: K.admins_invite_multi_provider_warning,
    message:
      "Several sign-in providers are enabled and this console cannot tell which one will govern " +
      "the redemption, so this check is a hint rather than a guarantee. If it is refused, the " +
      "invitation stays usable and the same link works once the domain is allowed.",
    description:
      "Blocker W5-B11, decision W5-D11. Redemption applies exactly ONE provider row; the " +
      "anonymous projection carries neither `trusted_jwt_issuer_id` nor `created_at`, so the " +
      "console can only compute a union and cannot tell which row wins. Warning rather than " +
      "block, and it says which of the two it is doing. The real safety net is that a " +
      "policy-denied redemption does not consume the invitation.",
  },

  /* --- the invitation list ------------------------------------------------- */
  [K.admins_invites_heading]: {
    key: K.admins_invites_heading,
    message: "Invitations",
    description: "Heading and accessible name of the invitation list region.",
  },
  [K.admins_invites_table_label]: {
    key: K.admins_invites_table_label,
    message: "Issued invitations",
    description: "Accessible name of the invitation table.",
  },
  [K.admins_invites_empty]: {
    key: K.admins_invites_empty,
    message: "No invitations have been issued.",
    description: "Empty state for the invitation list.",
  },
  [K.admins_invites_privacy_note]: {
    key: K.admins_invites_privacy_note,
    message: "This list names the people who were invited. Treat it as personal data.",
    description:
      "`AdminInviteRecord.value` is the invited address or domain and `consumed_subject` is the " +
      "redeemer's IdP subject; both are returned to any holder of `moira:admins:read`. That is " +
      "the right audience, and worth stating rather than discovering.",
  },
  [K.admins_invite_column_value]: {
    key: K.admins_invite_column_value,
    message: "Invited",
    description: "Column header for `AdminInviteRecord.value`.",
  },
  [K.admins_invite_column_status]: {
    key: K.admins_invite_column_status,
    message: "State",
    description:
      "Column header for the invitation's state. Deliberately not the same English as the grant " +
      "table's Status column — two keys with identical copy fail the catalog gate, and these " +
      "genuinely name different vocabularies.",
  },
  [K.admins_invite_column_expires]: {
    key: K.admins_invite_column_expires,
    message: "Expires",
    description: "Column header for `AdminInviteRecord.expires_at`.",
  },
  [K.admins_invite_status_pending]: {
    key: K.admins_invite_status_pending,
    message: "Waiting to be redeemed",
    description: "`AdminInviteStatus.pending` and not past `expires_at`.",
  },
  [K.admins_invite_status_consumed]: {
    key: K.admins_invite_status_consumed,
    message: "Redeemed",
    description: "`AdminInviteStatus.consumed`. Single-use, so this is terminal.",
  },
  [K.admins_invite_status_revoked]: {
    key: K.admins_invite_status_revoked,
    message: "Withdrawn",
    description: "`AdminInviteStatus.revoked`.",
  },
  [K.admins_invite_status_expired]: {
    key: K.admins_invite_status_expired,
    message: "Expired",
    description:
      "DERIVED from `AdminInviteRecord.expired`, not a `status` value: nothing sweeps for " +
      "expiry, so `status` never reads `expired` and a UI keying off `status` alone would show " +
      "a dead invitation as pending.",
  },
  [K.admins_invite_revoke]: {
    key: K.admins_invite_revoke,
    message: "Withdraw",
    description: "Per-row control that revokes an invitation.",
  },
  [K.admins_invite_revoke_confirm_title]: {
    key: K.admins_invite_revoke_confirm_title,
    message: "Withdraw this invitation?",
    description: "Accessible name and heading of the invitation revocation dialog.",
  },
  [K.admins_invite_revoke_confirm_body]: {
    key: K.admins_invite_revoke_confirm_body,
    message: "The link sent to {value} stops working. Issue a new invitation to replace it.",
    description:
      "Invitation revocation body. Names the remedy, and names the invitee so the operator can " +
      "see which link they are about to break.",
  },
  [K.admins_invite_revoke_confirm_action]: {
    key: K.admins_invite_revoke_confirm_action,
    message: "Withdraw invitation",
    description: "The confirming control in the invitation revocation dialog.",
  },

  /* --- the public /invite/[token] page ------------------------------------- */
  [K.invite_panel_label]: {
    key: K.invite_panel_label,
    message: "Invitation",
    description: "Accessible name of the InviteAcceptPanel region.",
  },
  [K.invite_heading_email]: {
    key: K.invite_heading_email,
    message: "This invitation is for {value}.",
    description:
      "Rendered for `AdminInviteConstraint.email`. `{value}` comes from the anonymous preview, " +
      "which carries the constraint, the value and the expiry and nothing else — no inviter, no " +
      "deployment detail, no policy.",
  },
  [K.invite_heading_domain]: {
    key: K.invite_heading_domain,
    message: "This invitation is for anyone with a {value} address.",
    description: "Rendered for `AdminInviteConstraint.domain`.",
  },
  [K.invite_expires_at]: {
    key: K.invite_expires_at,
    message: "It stops working after {expires_at}.",
    description:
      "`{expires_at}` is the preview's RFC 3339 timestamp. Separate from " +
      "`console.secret.expires_at`, which the inviter sees: two audiences, two sentences.",
  },
  [K.invite_sign_in_first]: {
    key: K.invite_sign_in_first,
    message: "Sign in to accept it.",
    description:
      "Shown above the sign-in panel when the visitor has no session. Redemption needs a " +
      "verified identity — the token proves the invitation, never the person.",
  },
  [K.invite_accept]: {
    key: K.invite_accept,
    message: "Accept invitation",
    description: "The control that redeems the invitation for the signed-in visitor.",
  },
  [K.invite_accept_pending]: {
    key: K.invite_accept_pending,
    message: "Accepting the invitation",
    description: "Announced while the redemption request is in flight.",
  },
  [K.invite_accepted]: {
    key: K.invite_accepted,
    message: "Done. You can open the console now.",
    description:
      "Redemption succeeded. Moira's own `admin_invite_redeemed` notice is rendered beside this " +
      "through `t()`; this key is the console's own next-step instruction.",
  },
  [K.invite_request_failed]: {
    key: K.invite_request_failed,
    message: "The request did not reach this deployment. Try the link again.",
    description:
      "The browser could not complete the call to the console's redemption route handler. " +
      "Deliberately distinct copy from `console.admins.request_failed`: identical English on two " +
      "keys fails the catalog gate, and these are read by different people.",
  },
  [K.invite_unusable_heading]: {
    key: K.invite_unusable_heading,
    message: "This invitation cannot be used",
    description:
      "Heading of the error STATE, which is rendered as a page with a 200 status rather than as " +
      "a 404: the a11y walker asserts every discovered route answers below 400, and an " +
      "unreadable invitation is a condition the holder needs explained, not a missing document.",
  },
  [K.invite_domain_not_allowed]: {
    key: K.invite_domain_not_allowed,
    message:
      "This deployment does not accept admins at your email domain. Ask whoever invited you to " +
      "add it to the sign-in provider's allowed domains, then use this link again.",
    description:
      "`moira.error.admin_claim_domain_not_allowed` on the redemption path, rendered as an " +
      "actionable instruction and never as a generic error banner. It is NOT the same condition " +
      "as `invite_email_mismatch`/`invite_domain_mismatch`, whose remedy is a new invitation. " +
      "Decision D3: an invitation is a scoping token, never a policy exemption — and a " +
      "policy-denied redemption does not consume it, so the same link works afterwards.",
  },
  [K.invite_already_claimed]: {
    key: K.invite_already_claimed,
    message:
      "An admin identity already exists for this sign-in. Ask an existing admin to check the " +
      "admin list.",
    description:
      "`moira.error.admin_identity_already_claimed`, worded for finding F24. It must NOT say " +
      '"you already have admin": `admin_identities` is keyed on (issuer, subject) with the ' +
      "console's own issuer on every row, so under two providers minting one issuer the holder " +
      "of that grant may be somebody else entirely.",
  },
};

/** Every entry, as a plain array. */
export const CONSOLE_CATALOG_ENTRIES: readonly CatalogEntry[] = Object.values(CONSOLE_CATALOG);
