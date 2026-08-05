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
  [K.auth_provider_update_failed]: {
    key: K.auth_provider_update_failed,
    message:
      "Saving the changes to the existing sign-in provider failed. Nothing was lost — the " +
      "provider is unchanged; retry the save.",
    description:
      "`SetupProvisioningError` at step `update_auth_provider` — a re-save of a provider row " +
      "that already exists (a resumed partial attempt, or the domain-refusal remedy's 'add the " +
      "domain and save again'). The row is PATCHED rather than re-created, so a retry replays " +
      "the same update safely and can never mint a duplicate row.",
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
  [K.setup_resume_state_conflict]: {
    key: K.setup_resume_state_conflict,
    message:
      "This attempt no longer matches what has actually been configured. Reload the page and " +
      "save again.",
    description:
      "The submitted `resume` hint named a provider row, a trusted issuer, or a stored-secret " +
      "state that disagrees with the one the console derived from Moira's own records. The hint " +
      "is never the authority for which row a privileged write may touch, so a disagreement is " +
      "refused rather than resolved in the caller's favour.",
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
  [K.setup_claim_issuer_mismatch]: {
    key: K.setup_claim_issuer_mismatch,
    message:
      "This claim names a different sign-in provider from the one you signed in through. Sign in " +
      "through that provider first.",
    description:
      "The claim body's `slug` resolved to a console issuer that is not the one the session was " +
      "established through (`SessionCheck.consoleIssuer`). The slug selects the " +
      "`admin_identities` namespace the grant is written into, so accepting a mismatch would " +
      "grant admin in a namespace this identity never authenticated against. Refused 403 with " +
      "nothing written.",
  },
  [K.setup_enabled_provider_requires_session]: {
    key: K.setup_enabled_provider_requires_session,
    message:
      "This sign-in provider is already enabled. Sign in through it first, then save your changes.",
    description:
      "Provisioning tried to re-save an ENABLED provider row with NO SESSION AT ALL behind the " +
      "request — a 401, and the one refusal for which 'sign in first' is the whole remedy. An " +
      "enabled row is a live authenticator, so rewriting its client id and endpoint URLs " +
      "re-points sign-in at another identity provider, and while the deployment is unclaimed " +
      "there is no admin grant yet to refuse that. A session established through that same " +
      "provider is the only proof of operatorship the setup window can ask for. Deliberately NOT " +
      "used for a caller who does hold a session and was refused for another reason: an " +
      "unverified address, a domain outside the allow-list and an unresolvable provider each " +
      "keep their own key, because each has already done what this sentence tells them to do.",
  },
  [K.setup_single_enabled_provider_only]: {
    key: K.setup_single_enabled_provider_only,
    message:
      "This console supports one enabled sign-in provider at a time, and this deployment already " +
      "has one. Disable the current provider through Moira's admin API using the bootstrap " +
      "system key, then save here again.",
    description:
      "Provisioning would have ENABLED a second provider on a deployment that already has one, " +
      "and the console cannot render sign-in for either of them afterwards: `ambiguityGuard` " +
      "(`lib/auth-config.ts`) refuses EVERY resolution once more than one provider is enabled, " +
      "so the next cold resolve produces no sign-in button, `consoleRuntime` is not ok, and " +
      "session resolution answers 'no session' forever. That is a lockout, not an escalation, " +
      "and it is refused for every caller alike — an operator holding a session through the " +
      "enabled provider satisfies no proof that makes the outcome survivable, so proof is not " +
      "what is asked for. A 409, because it is a conflict with the deployment's current state " +
      "rather than anything about the caller. The count is deployment-wide, taken with " +
      "`ambiguityGuard`'s own predicate, so naming a provider slug this console does not own " +
      "cannot shrink it. Refused with nothing written.",
  },
  [K.setup_provider_enabled_mid_save]: {
    key: K.setup_provider_enabled_mid_save,
    message:
      "This sign-in provider was enabled while your changes were being saved. Reload the page and " +
      "save again.",
    description:
      "The stale-derivation race, and the only outcome of it that is NOT a session refusal: the " +
      "console derived the row as disabled (so it asked for no proof of an operator), " +
      "`runSetupProvisioning` read it back ENABLED and refused the write, and re-resolving the " +
      "session afterwards shows the caller could have proved operatorship all along. Nothing is " +
      "wrong with them or with the configuration — the console's copy of the state was stale, so " +
      "this is a 409 for the same reason `setup_resume_state_conflict` is one, and a reload " +
      "re-derives the truth.",
  },
  [K.setup_enabled_provider_session_mismatch]: {
    key: K.setup_enabled_provider_session_mismatch,
    message:
      "You are signed in through a different sign-in provider. Sign in through the one you are " +
      "changing before saving it.",
    description:
      "Same refusal as the requires-session one, for a caller who DOES hold a valid session but " +
      "established it through another provider row (`SessionCheck.moiraProviderId` does not " +
      "match the derived row). Separated because the remedy differs: sign out and back in " +
      "through the provider being edited, rather than merely sign in.",
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

  /* --- the /setup wizard --------------------------------------------------- */
  [K.setup_page_title]: {
    key: K.setup_page_title,
    message: "Set up this deployment",
    description:
      "The `<h1>` of the public `/setup` route. Distinct from every step heading inside the " +
      "wizard, which name the step rather than the page.",
  },
  [K.setup_unavailable_heading]: {
    key: K.setup_unavailable_heading,
    message: "Setup is not available",
    description:
      "Heading over the refusal state on `/setup` when `GET /api/setup` answered with anything " +
      "other than an open setup window — no bootstrap credential, or Moira unreachable. The keyed " +
      "reason renders beside it through `t()`.",
  },
  [K.setup_steps_label]: {
    key: K.setup_steps_label,
    message: "Setup progress",
    description:
      "Accessible name of the wizard's step list `<nav>`. `no-hardcoded-copy` forbids a literal " +
      "`aria-label`, so the landmark name is a catalog key.",
  },
  [K.setup_step_welcome]: {
    key: K.setup_step_welcome,
    message: "Welcome",
    description: "Step-list label for the informational first step of the setup wizard.",
  },
  [K.setup_step_auth_settings]: {
    key: K.setup_step_auth_settings,
    message: "Sign-in settings",
    description:
      "Step-list label for the provider-configuration step, whose gate is " +
      "`isProvisioningComplete`.",
  },
  [K.setup_step_sign_in]: {
    key: K.setup_step_sign_in,
    message: "Operator sign-in",
    description:
      "Step-list label for the step where the operator authenticates through the provider they " +
      "just configured. Deliberately not the same English as `console.page.login_title`.",
  },
  [K.setup_step_claim]: {
    key: K.setup_step_claim,
    message: "Claim admin",
    description:
      "Step-list label for the once-only claim step. Unreachable while `reachableSetupStep` " +
      "says the provisioning gate is not complete.",
  },
  [K.setup_step_done]: {
    key: K.setup_step_done,
    message: "Finished",
    description: "Step-list label for the wizard's terminal confirmation step.",
  },
  [K.setup_welcome_heading]: {
    key: K.setup_welcome_heading,
    message: "Welcome to the Moira console",
    description: "Heading of the wizard's welcome step, shown before any configuration exists.",
  },
  [K.setup_welcome_claim_once]: {
    key: K.setup_welcome_claim_once,
    message:
      "The first administrator is claimed exactly once. After that this wizard closes for good, " +
      "and access is managed from inside the console.",
    description:
      "Welcome-step copy explaining the once-only nature of the claim: Moira's claim-status gate " +
      "answers 409 forever after the first successful claim.",
  },
  [K.setup_welcome_provider_first]: {
    key: K.setup_welcome_provider_first,
    message:
      "Configure a sign-in provider before claiming. This deployment denies every email domain " +
      "until you allow yours, so the claim step stays locked until the provider is enabled.",
    description:
      "Welcome-step copy explaining the provider-first ordering: the admission policy is " +
      "deny-by-default with no first-claim exemption, so claiming before provisioning is a " +
      "guaranteed 403.",
  },
  [K.setup_welcome_continue]: {
    key: K.setup_welcome_continue,
    message: "Start configuration",
    description: "The control that advances from the welcome step to the auth-settings step.",
  },
  [K.setup_auth_heading]: {
    key: K.setup_auth_heading,
    message: "Configure the sign-in provider",
    description: "Heading and accessible name of the auth-settings step's form region.",
  },
  [K.setup_auth_existing_heading]: {
    key: K.setup_auth_existing_heading,
    message: "Already configured in Moira",
    description:
      "Heading of the revisit block listing provider rows Moira already holds, rendered from the " +
      "display-safe `GET /api/setup` projection.",
  },
  [K.setup_auth_existing_configured]: {
    key: K.setup_auth_existing_configured,
    message: "Configured",
    description:
      "The masked value shown for an existing provider row's credential. Derived from the " +
      "PRESENCE of the row, never from any secret value — the console cannot read the secret " +
      "back, and must not try.",
  },
  [K.setup_auth_method_label]: {
    key: K.setup_auth_method_label,
    message: "Sign-in method",
    description: "Label of the method selector on the auth-settings form.",
  },
  [K.setup_auth_method_google]: {
    key: K.setup_auth_method_google,
    message: "Google OAuth",
    description: "Option label for `AuthMethod.google_oauth`.",
  },
  [K.setup_auth_method_generic]: {
    key: K.setup_auth_method_generic,
    message: "Generic OpenID Connect",
    description: "Option label for `AuthMethod.generic_oidc`.",
  },
  [K.setup_auth_slug_label]: {
    key: K.setup_auth_slug_label,
    message: "Provider slug",
    description:
      "Label of the provider-slug field. The slug picks the console-issuer namespace this " +
      "provider is registered under, and a new slug means a new trusted issuer and a new " +
      "provider row rather than a rewrite of the incumbent. What it does NOT pick is whether " +
      "the write is allowed: the enabled-provider count that decides that is deployment-wide.",
  },
  [K.setup_auth_slug_hint]: {
    key: K.setup_auth_slug_hint,
    message:
      "Leave empty for the default provider. Enter a short name — lower-case letters, digits and " +
      "hyphens — to register this provider under its own name instead. It becomes part of the " +
      "sign-in URL and cannot be changed afterwards. Only one sign-in provider can be enabled " +
      "at a time, so a new name here does not add a second one beside an enabled provider.",
    description:
      "Hint under the provider-slug field. Says what the slug is for (choosing the " +
      "console-issuer namespace this provider is registered under), what it costs (permanent — " +
      "it is a URL path segment and part of the issuer string Moira pins tokens to), and the " +
      "limit that bounds it. It deliberately does NOT offer the slug as a remedy for a provider " +
      "enabled with credentials nobody can sign in with: a second enabled provider is refused " +
      "outright (`setup_single_enabled_provider_only`), because the console cannot resolve " +
      "sign-in for either of them once two are enabled. That repair runs through Moira's admin " +
      "API with the bootstrap system key — see `docs/console-architecture.md`.",
  },
  [K.setup_auth_display_name_label]: {
    key: K.setup_auth_display_name_label,
    message: "Provider display name",
    description:
      "Label of the display-name field. Schema-required by Moira and rendered on every sign-in " +
      "button, so it is refused empty before any write.",
  },
  [K.setup_auth_client_id_label]: {
    key: K.setup_auth_client_id_label,
    message: "OAuth client ID",
    description: "Label of the client-id field on the auth-settings form.",
  },
  [K.setup_auth_client_secret_label]: {
    key: K.setup_auth_client_secret_label,
    message: "OAuth client secret",
    description:
      "Label of the client-secret field. The field is write-only: never pre-filled, never echoed " +
      "into any response, and stored encrypted in the console's own database (decision D7).",
  },
  [K.setup_auth_client_secret_hint]: {
    key: K.setup_auth_client_secret_hint,
    message: "Write-only. Stored encrypted by this console and never shown again.",
    description:
      "Hint under the client-secret field stating the D7 contract: Moira never stores the " +
      "secret, and the console has no read-back path for it.",
  },
  [K.setup_auth_discovery_url_label]: {
    key: K.setup_auth_discovery_url_label,
    message: "Discovery URL",
    description: "Label of the OIDC discovery-document field.",
  },
  [K.setup_auth_issuer_label]: {
    key: K.setup_auth_issuer_label,
    message: "Issuer URL",
    description:
      "Label of the IdP issuer field — the IDENTITY PROVIDER's issuer, never the console's own.",
  },
  [K.setup_auth_authorization_url_label]: {
    key: K.setup_auth_authorization_url_label,
    // Deliberately not the OAuth spec's own capitalised word for this endpoint:
    // this catalog is a CLIENT-SAFE module and `server-only-guards.test.ts`
    // forbids the credential-header literal in any client-safe module's code,
    // string literals included.
    message: "Authorize endpoint URL",
    description: "Label of the manual authorize-endpoint field, used when discovery is absent.",
  },
  [K.setup_auth_token_url_label]: {
    key: K.setup_auth_token_url_label,
    message: "Token endpoint",
    description: "Label of the manual token-endpoint field, used when discovery is absent.",
  },
  [K.setup_auth_allowed_domains_label]: {
    key: K.setup_auth_allowed_domains_label,
    message: "Allowed email domains",
    description:
      "Label of the allow-list field. The admission policy is deny-by-default (plan 07 decision " +
      "D3), so this list decides who can ever claim or hold admin access.",
  },
  [K.setup_auth_allowed_domains_hint]: {
    key: K.setup_auth_allowed_domains_hint,
    message:
      "Comma-separated. Only these domains may become administrators — an empty list would lock " +
      "everyone out, including you.",
    description:
      "Hint under the allow-list field. States the deny-by-default consequence because there is " +
      "no first-claim exemption: an empty list denies the operator's own claim on the next step.",
  },
  [K.setup_auth_form_incomplete]: {
    key: K.setup_auth_form_incomplete,
    message: "Fill in the required fields before saving.",
    description:
      "Client-side refusal announced when the auth-settings form is submitted with a required " +
      "field empty. Nothing is sent: the same shapes would be keyed 400s from the BFF one round " +
      "trip later.",
  },
  [K.setup_auth_submit]: {
    key: K.setup_auth_submit,
    message: "Save and enable provider",
    description:
      "Submit control of the auth-settings form. One submission drives the whole ordered " +
      "sequence: trusted issuer, provider, console-side secret, enable.",
  },
  [K.setup_auth_pending]: {
    key: K.setup_auth_pending,
    message: "Provisioning the sign-in provider",
    description: "Announced while the provision request is in flight.",
  },
  [K.setup_auth_retry]: {
    key: K.setup_auth_retry,
    message: "Retry",
    description:
      "The control that resumes a partial provisioning attempt. It re-sends the SAME submission " +
      "with the recorded `resume` state, so the retry replays rather than duplicates.",
  },
  [K.setup_auth_discard]: {
    key: K.setup_auth_discard,
    message: "Discard and start over",
    description:
      "Offered on the `retry_or_discard_provider` remedy: abandons the recorded partial state " +
      "and starts a fresh submission instead of resuming the failed one.",
  },
  [K.setup_auth_failure_region]: {
    key: K.setup_auth_failure_region,
    message: "Provisioning problem",
    description:
      "Accessible name of the region that renders a `SetupProvisioningError`'s keyed remedy, " +
      "its retry controls, and the recorded partial state's consequences.",
  },
  [K.setup_auth_not_complete]: {
    key: K.setup_auth_not_complete,
    message: "The provider is saved but not fully enabled yet. Retry to finish the remaining steps.",
    description:
      "Rendered when a provision response reports a state that fails `isProvisioningComplete` — " +
      "one of the four conditions (Moira row, console secret, enable, allow-list) is still " +
      "unconfirmed, so the wizard refuses to advance.",
  },
  [K.setup_request_unreachable]: {
    key: K.setup_request_unreachable,
    message: "This step did not reach the console. Check your connection and try again.",
    description:
      "The browser could not complete a call to the console's own `/api/setup` route. The thrown " +
      "cause is never echoed. Distinct copy from the admins and invite variants because two keys " +
      "may not share one English string.",
  },
  [K.setup_sign_in_heading]: {
    key: K.setup_sign_in_heading,
    message: "Sign in with the new provider",
    description: "Heading and accessible name of the wizard's combined sign-in-and-claim region.",
  },
  [K.setup_sign_in_intro]: {
    key: K.setup_sign_in_intro,
    message:
      "Use the provider you just configured to prove the identity that will become the first " +
      "administrator.",
    description:
      "Intro copy on the sign-in step. The buttons drive the same Better Auth flow as `/login`, " +
      "returning to `/setup` afterwards.",
  },
  [K.setup_sign_in_edit_settings]: {
    key: K.setup_sign_in_edit_settings,
    message: "Change the sign-in provider settings",
    description:
      "Returns the operator from the sign-in/claim step to the auth-settings form. Without it a " +
      "completed provision is a one-way door: a mistyped discovery URL, client id, or client " +
      "secret leaves the operator on a sign-in button that can never succeed. The re-save goes " +
      "back through the same server-derived provisioning path, so the control is navigation and " +
      "never a second way to choose which row is written.",
  },
  [K.setup_claim_heading]: {
    key: K.setup_claim_heading,
    message: "Claim administrator access",
    description:
      "Heading of the claim step. Rendered only when `reachableSetupStep` returns `claim` — " +
      "provisioning complete and a signed-in identity present.",
  },
  [K.setup_claim_button]: {
    key: K.setup_claim_button,
    message: "Claim admin access",
    description:
      "The control that sends `POST /api/setup {action: \"claim\"}`. Enabled only on the claim " +
      "step, so it can never fire a request the gate guarantees Moira will refuse.",
  },
  [K.setup_claim_pending]: {
    key: K.setup_claim_pending,
    message: "Claiming administrator access",
    description: "Announced while the claim request is in flight.",
  },
  [K.setup_claim_signed_in_as]: {
    key: K.setup_claim_signed_in_as,
    message: "Signed in as {email}.",
    description:
      "Shown above the claim control, naming the identity the claim will bind. `{email}` comes " +
      "from the console's own session probe, never from Moira.",
  },
  [K.setup_domain_not_allowed_title]: {
    key: K.setup_domain_not_allowed_title,
    message: "That email domain is not allowed yet",
    description:
      "Title of the actionable instruction rendered when the claim came back " +
      "`403 admin_claim_domain_not_allowed`. Never a generic error banner: the operator is sent " +
      "back to the auth-settings step where the allow-list can still be changed.",
  },
  [K.setup_domain_not_allowed_body]: {
    key: K.setup_domain_not_allowed_body,
    message:
      "Moira refused the claim because {domain} is not in the provider's allowed email domains. " +
      "Add {domain} below, save the provider again, then retry the claim.",
    description:
      "Body of the domain-refusal instruction. `{domain}` is the offending domain from the BFF's " +
      "re-keyed `message_args`; Moira's own envelope does not carry it.",
  },
  [K.setup_domain_not_allowed_action]: {
    key: K.setup_domain_not_allowed_action,
    message: "Add the domain and save",
    description:
      "Names the next action on the domain-refusal instruction, beside the focused allow-list " +
      "field on the auth-settings step.",
  },
  [K.setup_done_heading]: {
    key: K.setup_done_heading,
    message: "Setup is complete",
    description: "Heading of the wizard's terminal step, after a successful claim.",
  },
  [K.setup_done_admin_email]: {
    key: K.setup_done_admin_email,
    message: "Administrator access is granted to {email}.",
    description:
      "Confirmation line on the done step. `{email}` is the claimed identity's email from the " +
      "claim response.",
  },
  [K.setup_done_open_console]: {
    key: K.setup_done_open_console,
    message: "Open the console",
    description:
      "Link from the done step to the authenticated home route, where the new administrator's " +
      "session now has somewhere to go.",
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

  /* ------------------------------------------------------------------------ */
  /* The /settings/llm screen (issue #74)                                     */
  /* ------------------------------------------------------------------------ */
  [K.llm_page_title]: {
    key: K.llm_page_title,
    message: "Language model providers",
    description:
      "Heading of `/settings/llm`, the screen where an operator registers the endpoints " +
      "Moira sends prompts to.",
  },
  [K.llm_page_intro]: {
    key: K.llm_page_intro,
    message:
      "A provider needs four things before a prompt can reach it: the provider itself, at " +
      "least one model, a credential row, and routing pointed at it. Each one is listed " +
      "below with whatever is still missing.",
    description:
      "Rendered under the page heading. It states the chain because both of its failure " +
      "modes are reported by the backend in terms that name none of these four rows.",
  },
  [K.llm_load_failed]: {
    key: K.llm_load_failed,
    message: "The console could not read the provider configuration from the backend.",
    description:
      "Rendered instead of the whole screen when the server-side read throws. The page " +
      "still answers with a 2xx, because the accessibility walker asserts every route " +
      "answers below 400 and a backend outage must not take that gate red.",
  },
  [K.llm_request_failed]: {
    key: K.llm_request_failed,
    message: "That request did not complete. Nothing was changed.",
    description:
      "The browser-side fallback when a call to one of this screen's own route handlers " +
      "produced no readable keyed refusal - an offline browser, or a proxy that answered " +
      "with something that is not JSON.",
  },
  [K.llm_request_body_invalid]: {
    key: K.llm_request_body_invalid,
    message:
      "The console sent a request it could not build correctly. This is a fault in the " +
      "console, not in what you typed.",
    description:
      "A route handler could not read its own request body, or read one with no usable " +
      "fields. It is reachable only through a console bug or a hand-made request, so the " +
      "copy says so rather than asking the operator to correct an input.",
  },
  [K.llm_action_unknown]: {
    key: K.llm_action_unknown,
    message: "That is not an action this screen offers.",
    description:
      "The shortcut endpoint received a stage discriminator it does not implement. Distinct " +
      "from a malformed body: the body was readable and named something real-looking.",
  },
  [K.llm_general_route_missing]: {
    key: K.llm_general_route_missing,
    message:
      "This deployment has no default route, so routing cannot be pointed anywhere. Re-run " +
      "the database migrations and reload this page.",
    description:
      "The seeded default route could not be found. The console deliberately does not " +
      "create one: the create operation documents no conflict for a duplicate route key, so " +
      "a second one would leave routing with two candidates and no documented rule for " +
      "choosing.",
  },
  [K.llm_list_truncated]: {
    key: K.llm_list_truncated,
    message:
      "There are more rows than one page can show, so the console cannot tell whether this " +
      "already exists. Remove some rows before trying again.",
    description:
      "A reuse-first lookup ran out of page before it found a match. Refusing is " +
      "deliberate: creating the row anyway is how a duplicate provider or a second eligible " +
      "routing policy gets made.",
  },
  [K.llm_providers_heading]: {
    key: K.llm_providers_heading,
    message: "Configured providers",
    description:
      "Heading of the section listing every provider row, with its models, credential rows " +
      "and routing.",
  },
  [K.llm_providers_empty]: {
    key: K.llm_providers_empty,
    message: "No provider is configured yet.",
    description:
      "The empty state for the provider list. Rendered on a freshly migrated deployment, " +
      "where it is the expected state rather than a problem.",
  },
  [K.llm_status_active]: {
    key: K.llm_status_active,
    message: "Enabled",
    description:
      "Badge text for a row the backend reports as active. Paired with a tone, never colour " +
      "alone.",
  },
  [K.llm_status_disabled]: {
    key: K.llm_status_disabled,
    message: "Disabled",
    description:
      "Badge text for any row that is not active. Covers disabled and deleted alike, " +
      "because the difference does not change what an operator can do next from this " +
      "screen.",
  },
  [K.llm_models_heading]: {
    key: K.llm_models_heading,
    message: "Models",
    description: "Sub-heading above the models registered against one provider.",
  },
  [K.llm_models_empty]: {
    key: K.llm_models_empty,
    message: "No model is registered for this provider.",
    description:
      "Empty state for one provider's model list. A provider with no model is never " +
      "selected by routing, and nothing reports that at request time.",
  },
  [K.llm_key_rows_heading]: {
    key: K.llm_key_rows_heading,
    message: "Credential rows",
    description:
      "Sub-heading above the credential rows attached to one provider. Rows, not keys: no " +
      "key value is ever sent to the browser.",
  },
  [K.llm_key_row_present]: {
    key: K.llm_key_row_present,
    message: "A stored credential",
    description:
      "Label for one credential row. It deliberately describes the row and not its contents " +
      "- the value, its mask and its fingerprint are all withheld by the server.",
  },
  [K.llm_key_row_missing]: {
    key: K.llm_key_row_missing,
    message:
      "No credential row exists. A prompt is refused before it reaches the endpoint, even " +
      "when the endpoint needs no key.",
    description:
      "Both the empty state for one provider's credential rows and the missing-step line in " +
      "the readiness list. It states the surprising half of the rule, because the backend " +
      "reports this as a missing-credential error that reads as though a key were wrong.",
  },
  [K.llm_routing_heading]: {
    key: K.llm_routing_heading,
    message: "Routing entries",
    description: "Sub-heading above the routing policies pointing at one provider.",
  },
  [K.llm_policy_present]: {
    key: K.llm_policy_present,
    message: "Bound to a route",
    description:
      "Fallback label for a routing policy whose route key the console could not resolve - " +
      "the policy exists and points somewhere, and saying so beats rendering an opaque " +
      "identifier.",
  },
  [K.llm_policy_missing]: {
    key: K.llm_policy_missing,
    message: "Routing does not point at this provider yet.",
    description:
      "Both the empty state for one provider's routing policies and the missing-step line " +
      "in the readiness list. A provider with no policy is simply never selected, with no " +
      "error at all until a completion picks something else.",
  },
  [K.llm_disable_provider]: {
    key: K.llm_disable_provider,
    message: "Disable this provider",
    description:
      "The undo for having created a provider. It disables rather than deletes: nothing on " +
      "this surface is destroyed, and a disabled row stays readable.",
  },
  [K.llm_disable_model]: {
    key: K.llm_disable_model,
    message: "Disable this model",
    description: "The undo for having registered a model.",
  },
  [K.llm_disable_key_row]: {
    key: K.llm_disable_key_row,
    message: "Disable this credential",
    description:
      "The undo for having created a credential row. Disabling it makes the provider " +
      "ineligible again, which is the same state as never having created it.",
  },
  [K.llm_disable_policy]: {
    key: K.llm_disable_policy,
    message: "Stop routing here",
    description:
      "The undo for having pointed routing at this provider. It is the step that moves live " +
      "traffic, so its label says what stops rather than which row is edited.",
  },
  [K.llm_enable_model]: {
    key: K.llm_enable_model,
    message: "Enable this model",
    description:
      "Shown in place of the disable control on a model that is not active. Routing accepts " +
      "only active models, and re-adding the same identifier collides with the row that is " +
      "already there, so this is the only way back.",
  },
  [K.llm_enable_key_row]: {
    key: K.llm_enable_key_row,
    message: "Enable this credential",
    description:
      "Shown in place of the disable control on a credential row that is not active. A " +
      "disabled row fails a completion with the same error a missing one does.",
  },
  [K.llm_enable_policy]: {
    key: K.llm_enable_policy,
    message: "Route here again",
    description:
      "Shown in place of the stop-routing control on a policy that is not active. It moves " +
      "live traffic back, so its label says what resumes rather than which row is edited.",
  },
  [K.llm_add_provider_heading]: {
    key: K.llm_add_provider_heading,
    message: "Add a provider by hand",
    description:
      "Heading of the manual provider form - the long way round the shortcut, for an " +
      "endpoint that is not reachable from this deployment right now.",
  },
  [K.llm_provider_name_label]: {
    key: K.llm_provider_name_label,
    message: "Display name",
    description: "Label of the provider's name field.",
  },
  [K.llm_provider_name_hint]: {
    key: K.llm_provider_name_hint,
    message: "How this provider is named on this screen. It is never sent to the endpoint.",
    description:
      "Hint under the name field, so the operator does not try to make it match something " +
      "the endpoint expects.",
  },
  [K.llm_provider_base_url_label]: {
    key: K.llm_provider_base_url_label,
    message: "Endpoint address",
    description:
      "Label of the field holding the OpenAI-compatible base address of a provider.",
  },
  [K.llm_provider_base_url_hint]: {
    key: K.llm_provider_base_url_hint,
    message:
      "The base address of an OpenAI-compatible server. The version segment is added for " +
      "you when it is missing.",
    description:
      "Hint under every endpoint field on this screen. It states the canonicalisation, " +
      "because a provider row created from a bare origin fails much later, at request time, " +
      "with a message that names none of this.",
  },
  [K.llm_add_provider_submit]: {
    key: K.llm_add_provider_submit,
    message: "Add provider",
    description: "Submit control of the manual provider form.",
  },
  [K.llm_provider_created]: {
    key: K.llm_provider_created,
    message: "Provider added. Finish the remaining steps below.",
    description:
      "Confirmation after the manual form succeeded. It points at the rest of the chain, " +
      "because creating the provider alone leaves the deployment no closer to running a " +
      "prompt.",
  },
  [K.llm_display_name_required]: {
    key: K.llm_display_name_required,
    message: "Enter a display name.",
    description:
      "The console refused a provider create or patch with a blank name, before any request " +
      "left.",
  },
  [K.llm_base_url_required]: {
    key: K.llm_base_url_required,
    message: "Enter the address of the endpoint.",
    description: "The console refused an endpoint field that was empty.",
  },
  [K.llm_base_url_invalid]: {
    key: K.llm_base_url_invalid,
    message: "That is not an address the console can read.",
    description: "The endpoint field did not parse as an address at all.",
  },
  [K.llm_base_url_scheme_unsupported]: {
    key: K.llm_base_url_scheme_unsupported,
    message: "Only web addresses are accepted here.",
    description:
      "The endpoint address parsed but named a scheme this console will not fetch. The " +
      "console makes this call itself, so the set of schemes it will follow is narrowed " +
      "deliberately.",
  },
  [K.llm_base_url_userinfo_rejected]: {
    key: K.llm_base_url_userinfo_rejected,
    message:
      "Remove the sign-in details from the address, and store a key as a credential " +
      "instead.",
    description:
      "The endpoint address carried a user name or password. Accepting it would write a " +
      "secret into a provider row, and from there into every list response this screen " +
      "renders.",
  },
  [K.llm_chain_heading]: {
    key: K.llm_chain_heading,
    message: "Finish setting this provider up",
    description:
      "Heading of the panel holding the three steps that come after a provider row exists.",
  },
  [K.llm_chain_complete]: {
    key: K.llm_chain_complete,
    message: "Ready: a prompt can reach this provider.",
    description:
      "Rendered when all four parts of the chain are present and active. It is derived from " +
      "the server-rendered data rather than from what the panel believes it just did.",
  },
  [K.llm_chain_incomplete]: {
    key: K.llm_chain_incomplete,
    message: "Not ready yet.",
    description:
      "Rendered when any part of the chain is missing. The missing parts are listed under " +
      "it.",
  },
  [K.llm_step_model_missing]: {
    key: K.llm_step_model_missing,
    message: "Register a model, or enable one that is disabled.",
    description:
      "Readiness line for a provider with no model routing would accept. Names both causes " +
      "because they are indistinguishable from the failure: routing joins on an active " +
      "model, so a provider whose only model is disabled has none as far as a prompt is " +
      "concerned.",
  },
  [K.llm_step_enable_missing]: {
    key: K.llm_step_enable_missing,
    message: "Enable the provider.",
    description:
      "Readiness line for a provider row that is not active. Reachable after an operator " +
      "disables one and then wants it back.",
  },
  [K.llm_add_model_label]: {
    key: K.llm_add_model_label,
    message: "Model identifier",
    description:
      "Label of the field holding the identifier the endpoint itself uses for a model.",
  },
  [K.llm_add_model_hint]: {
    key: K.llm_add_model_hint,
    message: "Exactly as the endpoint reports it. This is the value sent on every request.",
    description:
      "Hint under the model field. A near-miss here is answered by the endpoint rather than " +
      "by the backend, which makes it hard to attribute.",
  },
  [K.llm_add_model_submit]: {
    key: K.llm_add_model_submit,
    message: "Add model",
    description: "Submit control of the add-model field.",
  },
  [K.llm_model_key_required]: {
    key: K.llm_model_key_required,
    message: "Enter the identifier the endpoint uses for this model.",
    description:
      "The console refused a model create with a blank identifier, before any request left.",
  },
  [K.llm_model_required]: {
    key: K.llm_model_required,
    message: "Select at least one model.",
    description:
      "The shortcut was asked to register a provider with no model selected. A provider " +
      "with no model is never selected by routing.",
  },
  [K.llm_model_not_found]: {
    key: K.llm_model_not_found,
    message: "That model does not belong to this provider.",
    description:
      "The console refused to act on a model identifier that does not appear among the " +
      "named provider's models. The backend's own disable operation takes no provider, so " +
      "this check exists only here.",
  },
  [K.llm_model_not_selectable]: {
    key: K.llm_model_not_selectable,
    message:
      "That model is not active, so routing would never select it. Enable the model first, " +
      "then point routing at it.",
    description:
      "Routing was asked to bind a policy to a model whose status is not active. The " +
      "backend stores such a policy and then never selects it, because routing joins the " +
      "model table on an active status — so the deployment would read as configured and " +
      "every completion would still fail.",
  },
  [K.llm_key_label]: {
    key: K.llm_key_label,
    message: "Key",
    description:
      "Label of the write-only field holding a provider key. Nothing populates it and " +
      "nothing reads it back.",
  },
  [K.llm_add_key_row_hint]: {
    key: K.llm_add_key_row_hint,
    message:
      "Leave this blank for an endpoint that needs no key. The row itself is what the " +
      "backend requires, not its contents.",
    description:
      "Hint under the key field. Blank is the ordinary case for an endpoint on the " +
      "operator's own network, and the console generates the stored placeholder itself.",
  },
  [K.llm_add_key_row_submit]: {
    key: K.llm_add_key_row_submit,
    message: "Create credential row",
    description: "Submit control of the credential form.",
  },
  [K.llm_key_row_not_found]: {
    key: K.llm_key_row_not_found,
    message: "That credential does not belong to this provider.",
    description:
      "The console refused to act on a credential identifier that does not appear among the " +
      "named provider's rows.",
  },
  [K.llm_bind_routing_model_label]: {
    key: K.llm_bind_routing_model_label,
    message: "Model to route to",
    description:
      "Label of the selector choosing which of a provider's models the default route should " +
      "send prompts to.",
  },
  [K.llm_bind_routing_no_model]: {
    key: K.llm_bind_routing_no_model,
    message: "Choose a model",
    description:
      "The unselected option of that selector. Chosen over a blank entry so the control " +
      "announces what it is for.",
  },
  [K.llm_bind_routing_submit]: {
    key: K.llm_bind_routing_submit,
    message: "Point routing here",
    description:
      "Submit control that binds the default route to the selected provider and model.",
  },
  [K.llm_policy_not_found]: {
    key: K.llm_policy_not_found,
    message: "That routing entry does not belong to this provider.",
    description:
      "The console refused to act on a routing identifier that does not point at the named " +
      "provider.",
  },
  [K.llm_connect_heading]: {
    key: K.llm_connect_heading,
    message: "Connect a local endpoint",
    description:
      "Heading of the shortcut panel, which asks an endpoint what it serves and then " +
      "registers everything a prompt needs.",
  },
  [K.llm_connect_intro]: {
    key: K.llm_connect_intro,
    message:
      "Ask the endpoint what it serves, then register it in one step. The console makes " +
      "that call itself; your browser never contacts the endpoint.",
    description:
      "Rendered under the shortcut heading. It states where the outbound call is made from, " +
      "because that is a deliberate boundary and not an implementation detail: the endpoint " +
      "is on the operator's own network.",
  },
  [K.llm_connect_endpoint_label]: {
    key: K.llm_connect_endpoint_label,
    message: "Local endpoint address",
    description:
      "Label of the shortcut's address field, pre-filled with this deployment's usual local " +
      "endpoint. The address itself is a constant in the code, never catalogue copy.",
  },
  [K.llm_connect_discover_submit]: {
    key: K.llm_connect_discover_submit,
    message: "Ask the endpoint",
    description:
      "The first of the shortcut's two controls. It writes nothing - a mistyped address " +
      "must not leave a provider row behind.",
  },
  [K.llm_connect_discovered_heading]: {
    key: K.llm_connect_discovered_heading,
    message: "Models this endpoint reports",
    description:
      "Legend above the list of model identifiers the endpoint returned, offered for " +
      "selection so nobody has to type one.",
  },
  [K.llm_connect_submit]: {
    key: K.llm_connect_submit,
    message: "Register the selected models",
    description: "The second of the shortcut's two controls. This is the one that writes.",
  },
  [K.llm_connect_pending]: {
    key: K.llm_connect_pending,
    message: "Working...",
    description: "Announced politely while either of the shortcut's two calls is in flight.",
  },
  [K.llm_connect_done]: {
    key: K.llm_connect_done,
    message: "Done. The provider, its models, a credential row and routing all exist.",
    description:
      "Announced after the whole chain completed. It names all four rows, because that " +
      "conjunction is the thing the operator came to this screen to achieve.",
  },
  [K.llm_connect_step_failed]: {
    key: K.llm_connect_step_failed,
    message:
      "Registration stopped part-way. What was already created is listed below, and trying " +
      "again continues from there rather than duplicating it.",
    description:
      "The chain failed at a step. Everything written up to that point is reported with it: " +
      "a retry made blind is how a second eligible routing policy gets created, since none " +
      "of these operations reports a conflict for a duplicate.",
  },
  [K.llm_discovery_unreachable]: {
    key: K.llm_discovery_unreachable,
    message:
      "The console could not reach that endpoint. Check that it is running, and that this " +
      "deployment can route to it.",
    description:
      "The outbound probe never produced a response - name resolution, a refused " +
      "connection, a certificate, or the timeout. Ordinary rather than exceptional: a " +
      "laptop with its tunnel down reaches this every time.",
  },
  [K.llm_discovery_refused]: {
    key: K.llm_discovery_refused,
    message: "The endpoint answered, but would not list its models.",
    description:
      "The probe got an HTTP response with a failure status. Distinct from unreachable, " +
      "because the remedy is different: something is listening and it said no.",
  },
  [K.llm_discovery_response_too_large]: {
    key: K.llm_discovery_response_too_large,
    message: "The endpoint's answer was too large for the console to read.",
    description:
      "The probe's read cap was passed. The read is bounded so that a hostile or hung " +
      "endpoint cannot hold a request handler open.",
  },
  [K.llm_discovery_invalid_response]: {
    key: K.llm_discovery_invalid_response,
    message: "The endpoint's answer was not a model listing the console recognises.",
    description:
      "The probe's response parsed but did not match the shape a model listing must have. " +
      "Nothing from an unvalidated response is rendered.",
  },
  [K.llm_trace_heading]: {
    key: K.llm_trace_heading,
    message: "What was written",
    description:
      "Heading of the per-step record the shortcut returns, shown after a success and after " +
      "a partial failure alike.",
  },
  [K.llm_step_provider]: {
    key: K.llm_step_provider,
    message: "Provider",
    description: "Names the first step of the registration chain in the trace.",
  },
  [K.llm_step_provider_model]: {
    key: K.llm_step_provider_model,
    message: "Model",
    description: "Names the second step of the registration chain in the trace.",
  },
  [K.llm_step_provider_credential]: {
    key: K.llm_step_provider_credential,
    message: "Credential row",
    description:
      "Names the third step of the registration chain in the trace - the one a keyless " +
      "endpoint still needs.",
  },
  [K.llm_step_provider_enable]: {
    key: K.llm_step_provider_enable,
    message: "Enable",
    description:
      "Names the fourth step of the registration chain in the trace. It is skipped when the " +
      "provider is already active, which is the ordinary case.",
  },
  [K.llm_step_routing_policy]: {
    key: K.llm_step_routing_policy,
    message: "Routing",
    description: "Names the last step of the registration chain in the trace.",
  },
  [K.llm_step_unknown]: {
    key: K.llm_step_unknown,
    message: "Step",
    description:
      "Fallback name for a trace step the console does not have a label for, so an added " +
      "step degrades to something readable instead of rendering its own identifier.",
  },
  [K.llm_outcome_created]: {
    key: K.llm_outcome_created,
    message: "created",
    description: "Trace outcome for a row this run wrote.",
  },
  [K.llm_outcome_reused]: {
    key: K.llm_outcome_reused,
    message: "reused",
    description:
      "Trace outcome for a row that already existed and was matched rather than duplicated.",
  },
  [K.llm_outcome_enabled]: {
    key: K.llm_outcome_enabled,
    message: "enabled",
    description:
      "Trace outcome for a row that already existed but was disabled, and was turned back " +
      "on. Kept apart from 'reused' because routing accepts only active rows: a step that " +
      "reported 'reused' for a disabled row would be announcing a working deployment no " +
      "prompt could reach.",
  },
  [K.llm_outcome_skipped]: {
    key: K.llm_outcome_skipped,
    message: "already done",
    description:
      "Trace outcome for a step that had nothing to do - in practice, enabling a provider " +
      "that was already active.",
  },
};

/** Every entry, as a plain array. */
export const CONSOLE_CATALOG_ENTRIES: readonly CatalogEntry[] = Object.values(CONSOLE_CATALOG);
