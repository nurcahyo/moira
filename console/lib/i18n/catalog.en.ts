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
  [K.auth_config_stale]: {
    key: K.auth_config_stale,
    message:
      "The console is serving a sign-in configuration it can no longer re-read from Moira, so a " +
      "provider changed since it was read will not take effect until Moira is reachable again " +
      "or this console is restarted.",
    description:
      "The snapshot in lib/auth-runtime.ts is past AUTH_CONFIG_SNAPSHOT_TTL_MS and the refresh " +
      "could not run: no MOIRA_SYSTEM_KEY to re-read with, or Moira is unreachable. The " +
      "configuration is still served — it is the only one anybody could sign in with — but " +
      "issue #152 was about the SILENCE, so this is what /login renders instead of nothing.",
  },

  /* --- app/api/auth/[...all]/route.ts -------------------------------------- */
  [K.auth_provider_unreachable]: {
    key: K.auth_provider_unreachable,
    message:
      "The console could not reach the identity provider named in its sign-in configuration. " +
      "Check that the provider's endpoints are correct and reachable from the console.",
    description:
      "A network-level failure escaping Better Auth's own handler — the console dialled the " +
      "configured discovery, authorization, token or userinfo URL and got no HTTP response at " +
      "all (ECONNREFUSED, DNS failure, connect timeout). Emitted by app/api/auth/[...all]/" +
      "route.ts. Before issue #152 this reached the operator as a bare `TypeError: fetch " +
      "failed` 500, which names neither configuration nor the provider as the cause.",
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
  [K.sign_in_go_to_setup]: {
    key: K.sign_in_go_to_setup,
    message: "Start the first-run setup",
    description:
      "Link to `/setup`, rendered under the refusal ONLY when the setup window is actually open " +
      'so it can never be a dead end. Without it "Finish setting up this deployment first" was ' +
      "advice with no way to act on it: `/` sends a signed-out visitor to `/login`, and nothing " +
      "on that page named the route that fixes it.",
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
    message:
      "The provider is saved but not fully enabled yet. Retry to finish the remaining steps.",
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
      'The control that sends `POST /api/setup {action: "claim"}`. Enabled only on the claim ' +
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

  /* ------------------------------------------------------------------------ */
  /* The /settings/keys screen (issue #180)                                   */
  /* ------------------------------------------------------------------------ */
  [K.chrome_nav_keys]: {
    key: K.chrome_nav_keys,
    message: "Application keys",
    description:
      "Navigation link to `/settings/keys`. The screen that mints the credential an " +
      "application presents to Moira, which is the one thing a finished setup still leaves an " +
      "operator reaching for a terminal to do.",
  },
  [K.keys_page_title]: {
    key: K.keys_page_title,
    message: "Keys your applications use",
    description:
      "The `<h1>` of `/settings/keys`. Names the SUBJECT rather than the object — an operator " +
      'arrives here asking "how does my app authenticate", not "show me a key table".',
  },
  [K.keys_page_intro]: {
    key: K.keys_page_intro,
    message:
      "An application sends one of these keys on every request. Finishing setup gives you an " +
      "operator who can configure this deployment; a key is what lets your own software call " +
      "it.",
    description:
      "Rendered under the page heading. States the boundary the screen exists to close: " +
      "operator access and application access are different credentials, and setup only " +
      "issues the first.",
  },
  [K.keys_applications_heading]: {
    key: K.keys_applications_heading,
    message: "Applications",
    description:
      "Heading of the list of applications and their keys. Emitted once per render of " +
      "`/settings/keys`.",
  },
  [K.keys_applications_empty]: {
    key: K.keys_applications_empty,
    message:
      "No application exists yet. Create one above — a key always belongs to an application.",
    description:
      "Shown when Moira reports no application at all, which is the state of a freshly " +
      "claimed deployment. Says WHY the empty state blocks the mint form rather than leaving " +
      "the operator to infer it from a disabled control.",
  },
  [K.keys_add_application_heading]: {
    key: K.keys_add_application_heading,
    message: "Add an application",
    description: "Heading of the create-application form on `/settings/keys`.",
  },
  [K.keys_application_name_label]: {
    key: K.keys_application_name_label,
    message: "Application name",
    description: "Label of the display-name field on the create-application form.",
  },
  [K.keys_application_name_hint]: {
    key: K.keys_application_name_hint,
    message: "How this application is named on this screen. It is never sent anywhere.",
    description:
      "Hint under the application-name field. Answers the question the field otherwise raises " +
      "— whether the name is operational — before an operator picks a cautious one.",
  },
  [K.keys_application_slug_label]: {
    key: K.keys_application_slug_label,
    message: "Short name",
    description: "Label of the optional slug field on the create-application form.",
  },
  [K.keys_application_slug_hint]: {
    key: K.keys_application_slug_hint,
    message:
      "Optional. Lower-case letters, digits and hyphens. Left blank, the application is " +
      "identified by its id alone.",
    description:
      "Hint under the slug field. States the character rule the console applies before " +
      "submitting, so a rejected slug is not the first the operator hears of it.",
  },
  [K.keys_create_application_button]: {
    key: K.keys_create_application_button,
    message: "Create application",
    description: "The submit control of the create-application form.",
  },
  [K.keys_issued_heading]: {
    key: K.keys_issued_heading,
    message: "Issued keys",
    description:
      "Heading above the keys belonging to one application. Rendered once per application " +
      "row.",
  },
  [K.keys_issued_empty]: {
    key: K.keys_issued_empty,
    message: "No key has been issued for this application yet.",
    description:
      "Shown for an application that exists but has no key — the normal state immediately " +
      "after creating one.",
  },
  [K.keys_prefix_label]: {
    key: K.keys_prefix_label,
    message: "Prefix",
    description:
      "Labels the `key_prefix` shown on a key row. That prefix identifies the key to a human " +
      "and cannot authenticate; it is the only part of the credential this screen ever " +
      "displays.",
  },
  [K.keys_scopes_label]: {
    key: K.keys_scopes_label,
    message: "Permissions",
    description: "Labels the scope list on a key row and the checkbox group on the mint form.",
  },
  [K.keys_last_used_label]: {
    key: K.keys_last_used_label,
    message: "Last used",
    description: "Labels the `last_used_at` timestamp on a key row.",
  },
  [K.keys_never_used]: {
    key: K.keys_never_used,
    message: "Never used",
    description:
      "Rendered in place of a timestamp when Moira reports no `last_used_at`. A key nobody " +
      "has ever presented is the safest one to revoke, so the state is named rather than left " +
      "blank.",
  },
  [K.keys_expires_label]: {
    key: K.keys_expires_label,
    message: "Stops working",
    description: "Labels the `expires_at` timestamp on a key row.",
  },
  [K.keys_expires_never]: {
    key: K.keys_expires_never,
    message: "Does not expire",
    description:
      "Rendered in place of a timestamp when a key has no `expires_at`. Stated positively: a " +
      "blank cell reads as missing data rather than as an unbounded credential.",
  },
  [K.keys_status_active]: {
    key: K.keys_status_active,
    message: "Accepting requests",
    description:
      "The `active` key status. Says what the status MEANS for traffic rather than repeating " +
      "the enum, because that is the question an operator is asking when they look at this " +
      "column.",
  },
  [K.keys_status_revoked]: {
    key: K.keys_status_revoked,
    message: "Revoked, no longer accepted",
    description: "The `revoked` key status.",
  },
  [K.keys_status_expired]: {
    key: K.keys_status_expired,
    message: "Past its expiry date",
    description: "The `expired` key status.",
  },
  [K.keys_status_deleted]: {
    key: K.keys_status_deleted,
    message: "Deleted from this deployment",
    description:
      "The `deleted` key status. Reachable only through the admin API — this console revokes " +
      "and never deletes — and rendered so a row created elsewhere is not a blank badge.",
  },
  [K.keys_mint_heading]: {
    key: K.keys_mint_heading,
    message: "Issue a new key",
    description: "Heading of the mint form inside an application's row.",
  },
  [K.keys_key_name_label]: {
    key: K.keys_key_name_label,
    message: "What is this key for",
    description:
      "Label of the key display-name field. Phrased as the question the field answers, " +
      'because "name" invites a restatement of the application name and the value of this ' +
      "field is telling two of an application's keys apart later.",
  },
  [K.keys_key_name_hint]: {
    key: K.keys_key_name_hint,
    message: "It appears in this list and in the audit trail. The key itself is never named.",
    description: "Hint under the key-name field.",
  },
  [K.keys_scopes_hint]: {
    key: K.keys_scopes_hint,
    message:
      "What this key may do. Administrative permissions are deliberately not offered here — a " +
      "key that can reconfigure the deployment is not an application credential.",
    description:
      "Hint under the scope checkboxes on the mint form. States the omission as a decision, " +
      "so an operator hunting for an admin scope stops looking rather than assuming the list " +
      "is broken.",
  },
  [K.keys_mint_button]: {
    key: K.keys_mint_button,
    message: "Issue key",
    description: "The submit control of the mint form.",
  },
  [K.keys_revoke_button]: {
    key: K.keys_revoke_button,
    message: "Revoke this key",
    description: "The per-row control that revokes one key.",
  },
  [K.keys_revoke_pending]: {
    key: K.keys_revoke_pending,
    message: "Revoking",
    description: "Announced while a revocation request is in flight.",
  },
  [K.keys_revoke_confirm_body]: {
    key: K.keys_revoke_confirm_body,
    message:
      "Anything using this key stops working immediately, and the key cannot be brought back. " +
      "The row stays here so you can still see when it was last used.",
    description:
      "The consequence sentence in the revoke confirmation. Names BOTH halves — the traffic that " +
      "breaks and the row that survives — because an operator hesitating over this control is " +
      "usually weighing exactly those two.",
  },
  [K.keys_unattached_heading]: {
    key: K.keys_unattached_heading,
    message: "Keys with no application on this page",
    description:
      "Heading of the list of keys whose `application_id` matches no listed application.",
  },
  [K.keys_unattached_intro]: {
    key: K.keys_unattached_intro,
    message:
      "These keys still authenticate. Their application was deleted, or it sits beyond the " +
      "list above.",
    description:
      "Rendered under that heading. The list exists because a key that works and appears on " +
      "no screen is the credential nobody revokes; this copy is why it is not simply filtered " +
      "out.",
  },
  [K.keys_truncated_notice]: {
    key: K.keys_truncated_notice,
    message:
      "This deployment has more applications or keys than this screen lists. Use the admin " +
      "API to see the rest.",
    description:
      "Shown when either Moira list reported more pages. Says the list is a prefix rather " +
      "than letting it read as complete.",
  },
  [K.keys_request_body_invalid]: {
    key: K.keys_request_body_invalid,
    message: "The console could not read that request.",
    description: "Emitted by the `/api/keys` handlers for a body that is not an object.",
  },
  [K.keys_application_required]: {
    key: K.keys_application_required,
    message: "Choose the application this key belongs to.",
    description:
      "Emitted when a mint request carries no `application_id`. Moira requires one and there " +
      "is no such thing as a key belonging to no application.",
  },
  [K.keys_display_name_required]: {
    key: K.keys_display_name_required,
    message: "Give this a name first.",
    description: "Emitted when a create-application or mint request carries an empty display name.",
  },
  [K.keys_request_failed]: {
    key: K.keys_request_failed,
    message: "The console could not complete that. Try again in a moment.",
    description:
      "Rendered by the keys panels when a request never reached a keyed refusal — a transport " +
      "failure rather than an answer.",
  },
  [K.keys_load_failed]: {
    key: K.keys_load_failed,
    message: "The console could not read the applications and keys from Moira.",
    description:
      "Rendered by `/settings/keys` when the server-side read threw. The page still answers " +
      "below 400 — the a11y walker fails the gate on any status >= 400, and a backend outage " +
      "must not take that red.",
  },
  [K.keys_secret_heading]: {
    key: K.keys_secret_heading,
    message: "Key issued",
    description:
      "Heading and accessible name of the once-only modal when what was minted is a consumer " +
      "key. The modal's default heading names an INVITATION, which is the other credential it " +
      "shows and the wrong word here.",
  },
  [K.keys_secret_notice]: {
    key: K.keys_secret_notice,
    message:
      "This is the credential your application presents to Moira. Store it where that " +
      "application reads its configuration.",
    description:
      "The notice above the plaintext in the once-only modal on the keys screen. Moira's key " +
      "envelope carries no `notice` of its own — unlike the invitation one — so the console " +
      "supplies its own rather than fabricating a server-shaped message.",
  },
  [K.keys_scope_responses_create]: {
    key: K.keys_scope_responses_create,
    message: "Send prompts",
    description:
      "Copy for the `moira:responses:create` scope on the mint form. Without it a key " +
      "authenticates and can do nothing, which is why it is the default.",
  },
  [K.keys_scope_responses_stream]: {
    key: K.keys_scope_responses_stream,
    message: "Stream answers as they are generated",
    description: "Copy for the `moira:responses:stream` scope.",
  },
  [K.keys_scope_responses_read]: {
    key: K.keys_scope_responses_read,
    message: "Read answers it created earlier",
    description: "Copy for the `moira:responses:read` scope.",
  },
  [K.keys_scope_conversations_create]: {
    key: K.keys_scope_conversations_create,
    message: "Start conversations",
    description: "Copy for the `moira:conversations:create` scope.",
  },
  [K.keys_scope_conversations_read]: {
    key: K.keys_scope_conversations_read,
    message: "Read its conversations",
    description: "Copy for the `moira:conversations:read` scope.",
  },
  [K.keys_scope_conversations_write]: {
    key: K.keys_scope_conversations_write,
    message: "Add to its conversations",
    description: "Copy for the `moira:conversations:write` scope.",
  },
  [K.keys_scope_memories_create]: {
    key: K.keys_scope_memories_create,
    message: "Record memories",
    description: "Copy for the `moira:memories:create` scope.",
  },
  [K.keys_scope_memories_read]: {
    key: K.keys_scope_memories_read,
    message: "Read memories",
    description: "Copy for the `moira:memories:read` scope.",
  },
  [K.keys_scope_rag_collections_read]: {
    key: K.keys_scope_rag_collections_read,
    message: "List document collections",
    description: "Copy for the `moira:rag-collections:read` scope.",
  },
  [K.keys_scope_rag_documents_read]: {
    key: K.keys_scope_rag_documents_read,
    message: "Read documents",
    description: "Copy for the `moira:rag-documents:read` scope.",
  },
  [K.keys_scope_usage_read]: {
    key: K.keys_scope_usage_read,
    message: "Read its own usage figures",
    description: "Copy for the `moira:usage:read` scope.",
  },
  [K.secret_key_label]: {
    key: K.secret_key_label,
    message: "Consumer key",
    description:
      "Labels the plaintext field in the once-only modal when what was minted is a consumer " +
      "key rather than an invitation token.",
  },
  [K.secret_no_expiry]: {
    key: K.secret_no_expiry,
    message: "This credential does not expire. Revoke it when it is no longer needed.",
    description:
      "Rendered in the once-only modal in place of the expiry line when the minted credential " +
      "has no `expires_at`. An invitation always has one; a consumer key need not.",
  },

  /* ------------------------------------------------------------------------ */
  /* The /settings/auth screen (issue #185)                                   */
  /* ------------------------------------------------------------------------ */
  [K.authsettings_page_title]: {
    key: K.authsettings_page_title,
    message: "How operators sign in",
    description:
      "The h1 of /settings/auth. Names what the screen governs — how a human signs in to this console — rather than the row it edits.",
  },
  [K.authsettings_page_intro]: {
    key: K.authsettings_page_intro,
    message:
      "This is how operators sign in to the console. Only the owner can change it, because a wrong value here locks everybody out, including you.",
    description:
      "Rendered under the page heading. States the ownership rule and the reason for it in the same breath, so the restriction reads as a consequence rather than as bureaucracy.",
  },
  [K.authsettings_not_owner]: {
    key: K.authsettings_not_owner,
    message:
      "Only the owner can change how operators sign in. Ask them, or transfer ownership first.",
    description:
      "Emitted when a signed-in admin who is not the primary identity reaches the screen or its endpoint. Names the two ways forward, because the refusal is otherwise a dead end.",
  },
  [K.authsettings_owner_grant_absent]: {
    key: K.authsettings_owner_grant_absent,
    message:
      "This console has no admin grant for your account in this namespace, so it cannot tell whether you are the owner.",
    description:
      "Emitted when the ownership lookup finds no grant for the caller's (issuer, subject). Distinct from the not-owner refusal: the answer is unknown rather than no.",
  },
  [K.authsettings_owner_lookup_truncated]: {
    key: K.authsettings_owner_lookup_truncated,
    message:
      "There are more admins than this console can read in one page, so it cannot confirm who the owner is.",
    description:
      "Emitted when the ownership lookup's page reported more results. Issue #117's lesson: an owner whose grant sits on page two must not be told they are not the owner.",
  },
  [K.authsettings_current_heading]: {
    key: K.authsettings_current_heading,
    message: "What is configured now",
    description: "Heading of the read-only summary of the live provider row.",
  },
  [K.authsettings_secret_hint]: {
    key: K.authsettings_secret_hint,
    message:
      "Write-only. Stored encrypted by this console and never shown again. Leave it blank to keep the one already stored.",
    description:
      "Hint under the secret field on the update form. The blank-means-keep rule is the whole reason an operator can correct a display name without retyping a secret they may not have.",
  },
  [K.authsettings_secret_required_for_new_client_id]: {
    key: K.authsettings_secret_required_for_new_client_id,
    message:
      "Changing the client ID needs its client secret in the same save. The stored one is sealed against the old ID and would stop working.",
    description:
      "Emitted, BEFORE any request to Moira, when the submitted client id differs from the sealed one and no secret was supplied. This is the refusal that prevents the most likely lockout.",
  },
  [K.authsettings_sealed_against]: {
    key: K.authsettings_sealed_against,
    message: "A secret is stored, sealed against client ID {client_id}.",
    description:
      "Rendered in the summary when the console holds a sealed secret. The sealed client id is stored in the clear precisely so drift can be shown without decrypting anything.",
  },
  [K.authsettings_sealed_absent]: {
    key: K.authsettings_sealed_absent,
    message: "No client secret is stored in this console for this provider.",
    description:
      "Rendered in the summary when the console holds no sealed secret — the state in which sign-in cannot complete a code exchange.",
  },
  [K.authsettings_update_heading]: {
    key: K.authsettings_update_heading,
    message: "Change the sign-in provider",
    description:
      "Heading of the form that writes both Moira's row and, when supplied, this console's sealed secret.",
  },
  [K.authsettings_update_button]: {
    key: K.authsettings_update_button,
    message: "Save sign-in settings",
    description: "The submit control of the update form.",
  },
  [K.authsettings_rotate_heading]: {
    key: K.authsettings_rotate_heading,
    message: "Replace the stored client secret",
    description:
      "Heading of the secret-only form. Separate from the update form because it makes no request to Moira at all.",
  },
  [K.authsettings_rotate_intro]: {
    key: K.authsettings_rotate_intro,
    message:
      "Use this when the secret changed at the identity provider and nothing else did. It writes only to this console.",
    description:
      "Rendered under that heading. Says where the write lands, because an operator reasonably expects a settings save to reach the backend.",
  },
  [K.authsettings_rotate_button]: {
    key: K.authsettings_rotate_button,
    message: "Replace stored secret",
    description: "The submit control of the secret-only form.",
  },
  [K.authsettings_saved]: {
    key: K.authsettings_saved,
    message: "Saved. Sign-in now uses the settings above.",
    description:
      "Announced after a successful update, once the drift re-check has confirmed the console and Moira agree.",
  },
  [K.authsettings_drift_after_write]: {
    key: K.authsettings_drift_after_write,
    message:
      "The settings were saved, but this console's stored secret no longer matches them. Enter the client secret again.",
    description:
      "Emitted when the post-write drift re-check is not in_sync. The write is NOT reported as success: the deployment is in the state that stops sign-in working, and saying 'saved' would send the operator away from the one screen that can fix it.",
  },
  [K.authsettings_request_body_invalid]: {
    key: K.authsettings_request_body_invalid,
    message: "The console could not read that save request.",
    description: "Emitted by the /settings/auth handler for a body that is not an object.",
  },
  [K.authsettings_client_id_required]: {
    key: K.authsettings_client_id_required,
    message: "The OAuth client ID cannot be empty.",
    description: "Emitted when the update form submits a blank client id.",
  },
  [K.authsettings_domains_required]: {
    key: K.authsettings_domains_required,
    message:
      "Keep at least one allowed email domain. An empty list denies every operator, including you.",
    description:
      "Emitted when the update form submits an empty allow-list. Moira would accept it; the deployment it produces has no way back in.",
  },
  [K.authsettings_secret_required]: {
    key: K.authsettings_secret_required,
    message: "Enter the client secret.",
    description: "Emitted when the secret-only form is submitted with an empty field.",
  },
  [K.authsettings_no_provider]: {
    key: K.authsettings_no_provider,
    message: "No sign-in provider is configured for this console's namespace yet.",
    description:
      "Rendered when the row derivation finds nothing to edit — a deployment claimed through a different namespace, or one whose provider row was deleted.",
  },
  [K.authsettings_load_failed]: {
    key: K.authsettings_load_failed,
    message: "The console could not read the sign-in settings from Moira.",
    description:
      "Rendered by /settings/auth when the server-side read threw. The page still answers below 400, because the a11y walker fails the gate on any status >= 400.",
  },
  [K.authsettings_request_failed]: {
    key: K.authsettings_request_failed,
    message: "The console could not save that. Try again in a moment.",
    description:
      "Rendered by the panels when a request never reached a keyed refusal — a transport failure rather than an answer.",
  },
  [K.chrome_nav_auth_settings]: {
    key: K.chrome_nav_auth_settings,
    message: "Sign-in",
    description:
      "Navigation link to /settings/auth. Shown to every admin; the screen itself explains that only the owner may change anything, which is more useful than a menu entry that silently disappears.",
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
  [K.chrome_nav_llm_settings]: {
    key: K.chrome_nav_llm_settings,
    message: "Language models",
    description:
      "Navigation link to `/settings/llm`. That page shipped after this header did, and until " +
      "this key existed it was reachable only by typing the URL — so an operator who had just " +
      "finished the setup wizard had no way from the UI to the screen that points Moira at a " +
      "provider. Deliberately not the page's own heading (`console.llm.page_title`): two keys " +
      "sharing one message fail the catalog gate.",
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
    description: "Label of the field holding the OpenAI-compatible base address of a provider.",
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
      "Remove the sign-in details from the address, and store a key as a credential " + "instead.",
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
      "Rendered when any part of the chain is missing. The missing parts are listed under " + "it.",
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
    description: "Label of the field holding the identifier the endpoint itself uses for a model.",
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
    description: "Submit control that binds the default route to the selected provider and model.",
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

  /* --- the "Connect Claude subscription" panel (issue #211) --------------- */
  [K.claude_subscription_heading]: {
    key: K.claude_subscription_heading,
    message: "Connect Claude subscription",
    description:
      "Heading of the panel that stores a long-lived Claude subscription token as an oauth2 " +
      "provider credential.",
  },
  [K.claude_subscription_intro]: {
    key: K.claude_subscription_intro,
    message:
      "Paste the output of `claude setup-token`, run on a machine where you are signed in to " +
      "your Claude subscription. The console stores it encrypted and never shows it again.",
    description:
      "Rendered under the panel heading. Names the exact command an operator needs to run " +
      "locally to produce the value this field wants.",
  },
  [K.claude_subscription_token_label]: {
    key: K.claude_subscription_token_label,
    message: "Subscription token",
    description: "Label of the panel's single field.",
  },
  [K.claude_subscription_token_hint]: {
    key: K.claude_subscription_token_hint,
    message: "Never shown again after this is saved.",
    description: "Hint under the token field, stating the write-once nature of the value.",
  },
  [K.claude_subscription_submit]: {
    key: K.claude_subscription_submit,
    message: "Save subscription token",
    description: "Submit control for the panel's single field.",
  },
  [K.claude_subscription_pending]: {
    key: K.claude_subscription_pending,
    message: "Saving...",
    description: "Announced politely while the save request is in flight.",
  },
  [K.claude_subscription_created]: {
    key: K.claude_subscription_created,
    message: "Saved. A new subscription credential was created.",
    description:
      "Announced after a successful save when no matching credential existed yet, so this " +
      "run created one.",
  },
  [K.claude_subscription_rotated]: {
    key: K.claude_subscription_rotated,
    message: "Saved. The existing subscription credential was updated with this token.",
    description:
      "Announced after a successful save when a matching credential already existed, so this " +
      "run replaced its value in place rather than creating a second row.",
  },
  [K.claude_subscription_token_required]: {
    key: K.claude_subscription_token_required,
    message: "Enter the subscription token before saving.",
    description: "The field was submitted empty.",
  },
  [K.claude_subscription_token_too_long]: {
    key: K.claude_subscription_token_too_long,
    message: "That token is longer than the console will accept.",
    description: "The pasted value exceeded the console's bound on a subscription token.",
  },
  [K.claude_subscription_token_invalid]: {
    key: K.claude_subscription_token_invalid,
    message: "That does not look like a single token - check for an extra line in the paste.",
    description:
      "The pasted value contained a control character, most likely a newline from a copy " +
      "that grabbed more than one line.",
  },
  [K.claude_subscription_request_body_invalid]: {
    key: K.claude_subscription_request_body_invalid,
    message: "The console could not read that subscription-token request.",
    description:
      "Emitted by the /api/settings/llm/claude-subscription handler for a body that is not " +
      "an object.",
  },
  [K.claude_subscription_list_truncated]: {
    key: K.claude_subscription_list_truncated,
    message: "Too many providers exist for the console to check safely. Contact an administrator.",
    description:
      "The provider list was truncated before a match could be confirmed absent, so the " +
      "console refused to guess rather than risk creating a duplicate provider row.",
  },

  /* --- the /graph screen (plan 12 §4, issue #234) --------------------------- */
  [K.page_graph_title]: {
    key: K.page_graph_title,
    message: "Relationship graph",
    description: "Heading of the /graph page.",
  },
  [K.chrome_nav_graph]: {
    key: K.chrome_nav_graph,
    message: "Graph",
    description: "Chrome nav link to /graph.",
  },
  [K.graph_intro]: {
    key: K.graph_intro,
    message:
      "How agents, skills, evaluation suites, flows, providers and models reference each " +
      "other, read straight off their configuration — nothing here is a separate record kept " +
      "in sync by hand.",
    description: "Introductory copy on the /graph page, stating the derived-not-stored design.",
  },
  [K.graph_request_failed]: {
    key: K.graph_request_failed,
    message: "Could not reach Moira to build the graph. Try again shortly.",
    description:
      "Shown when GET /api/v1/admin/graph could not be reached, mirroring " +
      "admins_request_failed's fail-as-a-page-not-a-500 posture.",
  },
  [K.graph_empty]: {
    key: K.graph_empty,
    message:
      "Nothing to show yet — no agents, skills, evaluation suites, flows, providers or models are configured.",
    description: "Shown when the graph has no nodes at all.",
  },
  [K.graph_legend_label]: {
    key: K.graph_legend_label,
    message: "Node types",
    description: "Accessible label for the node-type legend beside the canvas.",
  },
  [K.graph_node_type_agent]: {
    key: K.graph_node_type_agent,
    message: "Agent",
    description: "Legend label for the agent node type.",
  },
  [K.graph_node_type_skill]: {
    key: K.graph_node_type_skill,
    message: "Skill",
    description: "Legend label for the skill node type.",
  },
  [K.graph_node_type_eval_suite]: {
    key: K.graph_node_type_eval_suite,
    message: "Evaluation suite",
    description: "Legend label for the eval_suite node type.",
  },
  [K.graph_node_type_flow]: {
    key: K.graph_node_type_flow,
    message: "Flow",
    description: "Legend label for the flow node type.",
  },
  [K.graph_node_type_provider]: {
    key: K.graph_node_type_provider,
    message: "LLM provider",
    description:
      'Legend label for the provider node type. "LLM provider", not the bare "Provider" ' +
      "console.llm.step_provider already uses, so the two entries read distinctly and the " +
      "no-duplicate-English catalog test can tell them apart.",
  },
  [K.graph_node_type_model]: {
    key: K.graph_node_type_model,
    message: "LLM model",
    description:
      'Legend label for the model node type. "LLM model", not the bare "Model" ' +
      "console.llm.step_provider_model already uses, for the same reason as the provider " +
      "entry above.",
  },
  [K.graph_node_type_memory_scope]: {
    key: K.graph_node_type_memory_scope,
    message: "Memory scope",
    description:
      "Legend label for the synthetic memory_scope node type — one per distinct scope in " +
      "use, not one per memory record.",
  },
  [K.graph_canvas_label]: {
    key: K.graph_canvas_label,
    message: "Relationship graph canvas",
    description: "Accessible label for the react-flow canvas region.",
  },

  /* ------------------------------------------------------------------------ */
  /* The authenticated chrome — plan 12 §5/§3/§6, issue #83 nav links         */
  /* ------------------------------------------------------------------------ */
  [K.chrome_nav_skills]: {
    key: K.chrome_nav_skills,
    message: "Skills",
    description: "Navigation link to `/skills`, the tool/guard skill registry and OpenAPI import screen.",
  },
  [K.chrome_nav_provider_health]: {
    key: K.chrome_nav_provider_health,
    message: "Provider health",
    description: "Navigation link to `/providers/health`, the rolling reachability dashboard (issue #83).",
  },
  [K.chrome_nav_evals]: {
    key: K.chrome_nav_evals,
    message: "Evals",
    description: "Navigation link to `/evals`, the eval suite/case/run screen (plan 12 §3).",
  },
  [K.chrome_nav_playground]: {
    key: K.chrome_nav_playground,
    message: "Playground",
    description: "Navigation link to `/playground`, the test-chat screen (issue #261).",
  },
  [K.chrome_nav_flows]: {
    key: K.chrome_nav_flows,
    message: "Flows",
    description: "Navigation link to `/flows`, the multi-agent flow authoring screen (plan 12 §6).",
  },

  /* ------------------------------------------------------------------------ */
  /* The /skills screen (plan 12 §5, issue #237)                              */
  /* ------------------------------------------------------------------------ */
  [K.page_skills_title]: {
    key: K.page_skills_title,
    message: "Skill registry",
    description: "Heading of `/skills`. Deliberately not the same English as the nav link's `console.chrome.nav_skills`.",
  },
  [K.skills_page_intro]: {
    key: K.skills_page_intro,
    message:
      "A tool skill is offered to a model as a callable action; a guard skill is a deterministic " +
      "policy check that may only narrow access. Freshly created or imported skills start in " +
      "draft and take no effect until enabled.",
    description: "Rendered under the `/skills` heading, explaining the tool/guard split and the draft lifecycle before the list below it.",
  },
  [K.skills_load_failed]: {
    key: K.skills_load_failed,
    message: "The console could not read the skill registry from Moira.",
    description: "Rendered instead of the whole screen when the server-side read throws. The page still answers below 400 so the a11y walker's status gate stays green through a backend outage.",
  },
  [K.skills_request_failed]: {
    key: K.skills_request_failed,
    message: "The skills screen could not complete that request. Nothing was changed.",
    description: "Fallback failure text for a browser-side call to this screen's own route handlers that produced no readable keyed refusal.",
  },
  [K.skills_request_body_invalid]: {
    key: K.skills_request_body_invalid,
    message: "This screen's own request was malformed before it reached Moira — a fault in the console, not in what you typed.",
    description: "A skills route handler could not read its own request body, or read one with no usable fields. Reachable only through a console bug or a hand-made request.",
  },
  [K.skills_skill_key_required]: {
    key: K.skills_skill_key_required,
    message: "Give this skill a key before creating it.",
    description: "The console's own 400 when `skill_key` is missing or blank on `POST /api/skills`.",
  },
  [K.skills_display_name_required]: {
    key: K.skills_display_name_required,
    message: "Give this skill a display name.",
    description: "The console's own 400 when `display_name` is missing or blank, on create or on the edit form's save.",
  },
  [K.skills_kind_required]: {
    key: K.skills_kind_required,
    message: "Choose whether this skill is a tool or a guard.",
    description: "The console's own 400 when `kind` is absent or not one of the two values `SkillKind` allows.",
  },
  [K.skills_bulk_enable_empty]: {
    key: K.skills_bulk_enable_empty,
    message: "Select at least one skill before enabling in bulk.",
    description: "The console's own 400 when `POST /api/skills/bulk-enable` is called with an empty `skill_ids` list.",
  },
  [K.skills_import_document_required]: {
    key: K.skills_import_document_required,
    message: "Paste an OpenAPI document before importing.",
    description: "The console's own 400 when the import request carries no `document` field at all.",
  },
  [K.skills_executor_method_invalid]: {
    key: K.skills_executor_method_invalid,
    message: "Choose a valid HTTP method for this executor.",
    description: "The console's own 400 when the executor edit form's `method` field is missing or not one of GET/POST/PUT/PATCH/DELETE.",
  },
  [K.skills_executor_url_required]: {
    key: K.skills_executor_url_required,
    message: "Give this executor a URL template.",
    description: "The console's own 400 when the executor edit form's `url_template` field is blank.",
  },
  [K.skills_executor_timeout_invalid]: {
    key: K.skills_executor_timeout_invalid,
    message: "The executor's timeout must be a positive number of milliseconds.",
    description: "The console's own 400 when the executor edit form's `timeout_ms` field is not a positive number.",
  },
  [K.skills_list_heading]: {
    key: K.skills_list_heading,
    message: "Registered skills",
    description: "Heading of the section listing every skill row.",
  },
  [K.skills_list_empty]: {
    key: K.skills_list_empty,
    message: "No skill is registered yet.",
    description: "Empty state for the skill list, on a deployment with no skills authored or imported.",
  },
  [K.skills_kind_tool]: {
    key: K.skills_kind_tool,
    message: "Tool",
    description: "Badge text for a skill whose `kind` is `tool`.",
  },
  [K.skills_kind_guard]: {
    key: K.skills_kind_guard,
    message: "Guard",
    description: "Badge text for a skill whose `kind` is `guard`.",
  },
  [K.skills_status_draft]: {
    key: K.skills_status_draft,
    message: "Draft",
    description: "Badge text for a skill whose `status` is `draft` — freshly authored or imported, not yet reviewed.",
  },
  [K.skills_status_enabled]: {
    key: K.skills_status_enabled,
    message: "Skill enabled",
    description: "Badge text for a skill whose `status` is `enabled` — callable by an agent.",
  },
  [K.skills_status_disabled]: {
    key: K.skills_status_disabled,
    message: "Skill disabled",
    description: "Badge text for a skill whose `status` is `disabled`.",
  },
  [K.skills_no_description]: {
    key: K.skills_no_description,
    message: "No description.",
    description: "Rendered in place of a skill's description when it is null or blank.",
  },
  [K.skills_tags_none]: {
    key: K.skills_tags_none,
    message: "No tags.",
    description: "Rendered in place of a skill's tag list when it is empty.",
  },
  [K.skills_select_for_bulk_enable]: {
    key: K.skills_select_for_bulk_enable,
    message: "Select for bulk-enable",
    description: "Accessible name of the per-row checkbox that adds a skill to the bulk-enable selection.",
  },
  [K.skills_enable]: {
    key: K.skills_enable,
    message: "Enable skill",
    description: "The per-row control that appears when a skill is draft or disabled.",
  },
  [K.skills_disable]: {
    key: K.skills_disable,
    message: "Disable skill",
    description: "The per-row control that appears when a skill is enabled.",
  },
  [K.skills_edit]: {
    key: K.skills_edit,
    message: "Edit skill",
    description: "Opens the inline edit form for a skill's display name, description and tags.",
  },
  [K.skills_edit_cancel]: {
    key: K.skills_edit_cancel,
    message: "Discard skill edits",
    description: "Closes the inline edit form without saving.",
  },
  [K.skills_edit_save]: {
    key: K.skills_edit_save,
    message: "Save skill",
    description: "Submits the inline edit form's `PATCH`.",
  },
  [K.skills_edit_saved]: {
    key: K.skills_edit_saved,
    message: "Skill updated.",
    description: "Announced after a successful edit, before the page reloads the server-rendered list.",
  },
  [K.skills_delete]: {
    key: K.skills_delete,
    message: "Delete skill",
    description: "Opens the delete confirmation dialog for one skill row.",
  },
  [K.skills_delete_confirm_title]: {
    key: K.skills_delete_confirm_title,
    message: "Delete this skill?",
    description: "Title of `DangerConfirmDialog` for a skill deletion.",
  },
  [K.skills_delete_confirm_body]: {
    key: K.skills_delete_confirm_body,
    message: "This removes the skill and its HTTP executor, if it has one. This cannot be undone from this screen.",
    description: "Body of `DangerConfirmDialog` for a skill deletion, announced via `role=\"alert\"`.",
  },
  [K.skills_delete_confirm_action]: {
    key: K.skills_delete_confirm_action,
    message: "Delete permanently",
    description: "Label of the destructive confirm button in the skill deletion dialog.",
  },
  [K.skills_bulk_enable_button]: {
    key: K.skills_bulk_enable_button,
    message: "Enable selected skills",
    description: "Submits `POST /api/skills/bulk-enable` for every checked row.",
  },
  [K.skills_bulk_enable_none_selected]: {
    key: K.skills_bulk_enable_none_selected,
    message: "Select at least one skill above before enabling in bulk.",
    description: "Rendered as a hint under a disabled bulk-enable button when nothing is checked.",
  },
  [K.skills_bulk_enable_done]: {
    key: K.skills_bulk_enable_done,
    message: "Selected skills were enabled.",
    description: "Announced after a successful bulk-enable, before the page reloads the server-rendered list.",
  },
  [K.skills_field_skill_key_label]: {
    key: K.skills_field_skill_key_label,
    message: "Skill key",
    description: "Label of the `skill_key` field on the create-skill form.",
  },
  [K.skills_field_skill_key_hint]: {
    key: K.skills_field_skill_key_hint,
    message: "A stable identifier for this skill. It cannot be changed after creation.",
    description: "Hint under the `skill_key` field.",
  },
  [K.skills_field_display_name_label]: {
    key: K.skills_field_display_name_label,
    message: "Skill display name",
    description: "Label of the `display_name` field, shared by the create-skill form and the edit form.",
  },
  [K.skills_field_kind_label]: {
    key: K.skills_field_kind_label,
    message: "Kind",
    description: "Label of the tool/guard `kind` select on the create-skill form.",
  },
  [K.skills_field_description_label]: {
    key: K.skills_field_description_label,
    message: "Description",
    description: "Label of the `description` field, shared by the create-skill form and the edit form.",
  },
  [K.skills_field_tags_label]: {
    key: K.skills_field_tags_label,
    message: "Tags",
    description: "Label of the `tags` field, shared by the create-skill form and the edit form.",
  },
  [K.skills_field_tags_hint]: {
    key: K.skills_field_tags_hint,
    message: "Comma-separated. Used for filtering and grouping only.",
    description: "Hint under the `tags` field, explaining the comma-separated input format.",
  },
  [K.skills_create_heading]: {
    key: K.skills_create_heading,
    message: "Add a skill",
    description: "Heading of the create-skill form.",
  },
  [K.skills_create_submit]: {
    key: K.skills_create_submit,
    message: "Create skill",
    description: "Submit button of the create-skill form.",
  },
  [K.skills_create_success]: {
    key: K.skills_create_success,
    message: "Skill created as a draft. Enable it below once it is ready.",
    description: "Announced after `POST /api/skills` succeeds, before the page reloads the server-rendered list.",
  },
  [K.skills_import_heading]: {
    key: K.skills_import_heading,
    message: "Import from an OpenAPI document",
    description: "Heading of the OpenAPI import panel (plan 12 §5, issue #237).",
  },
  [K.skills_import_intro]: {
    key: K.skills_import_intro,
    message:
      "Paste an OpenAPI 3.x document. Each operation becomes one draft skill with an HTTP " +
      "executor, capped at 300 operations per import. Nothing is enabled automatically — review " +
      "the results below and enable what you want an agent to call.",
    description: "Explains the import pipeline's cap and draft-first behaviour above the paste field (plan 12 §5 decision 22/23).",
  },
  [K.skills_import_field_label]: {
    key: K.skills_import_field_label,
    message: "OpenAPI document (JSON)",
    description: "Label of the textarea that accepts the pasted OpenAPI spec.",
  },
  [K.skills_import_field_hint]: {
    key: K.skills_import_field_hint,
    message: "Paste the full JSON document, or choose a file to load it from.",
    description: "Hint under the import textarea, naming both the paste and file-picker paths.",
  },
  [K.skills_import_submit]: {
    key: K.skills_import_submit,
    message: "Import skills",
    description: "Submit button of the OpenAPI import panel.",
  },
  [K.skills_import_invalid_json]: {
    key: K.skills_import_invalid_json,
    message: "That is not valid JSON. Check the pasted document and try again.",
    description: "Rendered client-side when the pasted text fails to parse as JSON, before anything is sent to the console's own route handler.",
  },
  [K.skills_import_success]: {
    key: K.skills_import_success,
    message: "Imported {count} skill(s) as drafts.",
    description: "Announced after a successful import, with `{count}` filled from `imported_count`.",
  },
  [K.skills_import_results_heading]: {
    key: K.skills_import_results_heading,
    message: "Imported drafts",
    description: "Heading over the list of skills the last import created, rendered above the main skill list.",
  },
  [K.skills_executor_heading]: {
    key: K.skills_executor_heading,
    message: "HTTP executor",
    description: "Heading of the per-skill executor panel.",
  },
  [K.skills_executor_show]: {
    key: K.skills_executor_show,
    message: "Show executor",
    description: "Expands a skill row's executor panel, triggering the on-demand `GET .../executor` fetch.",
  },
  [K.skills_executor_hide]: {
    key: K.skills_executor_hide,
    message: "Hide executor",
    description: "Collapses a skill row's executor panel.",
  },
  [K.skills_executor_loading]: {
    key: K.skills_executor_loading,
    message: "Loading the executor…",
    description: "Shown while the on-demand executor fetch is in flight.",
  },
  [K.skills_executor_none]: {
    key: K.skills_executor_none,
    message: "This skill has no HTTP executor. It cannot be called until one is imported.",
    description: "Rendered when `GET /api/skills/{id}/executor` answers 404 — the normal state for a hand-authored skill, since there is no create-executor endpoint on this surface.",
  },
  [K.skills_executor_load_failed]: {
    key: K.skills_executor_load_failed,
    message: "The console could not read this skill's executor.",
    description: "Rendered when the on-demand executor fetch fails for a reason other than 404.",
  },
  [K.skills_executor_allowed_host_label]: {
    key: K.skills_executor_allowed_host_label,
    message: "Allowed host",
    description: "Read-only label for `allowed_host`, derived server-side from `url_template` and never editable directly.",
  },
  [K.skills_executor_method_label]: {
    key: K.skills_executor_method_label,
    message: "HTTP method",
    description: "Label of the executor edit form's method select.",
  },
  [K.skills_executor_url_label]: {
    key: K.skills_executor_url_label,
    message: "URL template",
    description: "Label of the executor edit form's `url_template` field.",
  },
  [K.skills_executor_timeout_label]: {
    key: K.skills_executor_timeout_label,
    message: "Timeout (ms)",
    description: "Label of the executor edit form's `timeout_ms` field.",
  },
  [K.skills_executor_key_row_label]: {
    key: K.skills_executor_key_row_label,
    message: "Executor credential row",
    description: "Label of the executor edit form's `credential_id` field — a row identifier only, never a secret value.",
  },
  [K.skills_executor_key_row_hint]: {
    key: K.skills_executor_key_row_hint,
    message: "The id of a provider credential row this executor authenticates with. Leave blank for none.",
    description: "Hint under the executor's `credential_id` field, stating plainly that it names a row rather than holding a value.",
  },
  [K.skills_executor_save]: {
    key: K.skills_executor_save,
    message: "Save executor",
    description: "Submit button of the executor edit form.",
  },
  [K.skills_executor_saved]: {
    key: K.skills_executor_saved,
    message: "Executor updated.",
    description: "Announced after a successful executor `PATCH`.",
  },
  [K.skills_executor_delete]: {
    key: K.skills_executor_delete,
    message: "Delete executor",
    description: "Opens the delete confirmation dialog for a skill's HTTP executor.",
  },
  [K.skills_executor_delete_confirm_title]: {
    key: K.skills_executor_delete_confirm_title,
    message: "Delete this executor?",
    description: "Title of `DangerConfirmDialog` for an executor deletion.",
  },
  [K.skills_executor_delete_confirm_body]: {
    key: K.skills_executor_delete_confirm_body,
    message: "The skill stays, but it can no longer be called until a new executor exists.",
    description: "Body of `DangerConfirmDialog` for an executor deletion, announced via `role=\"alert\"`.",
  },
  [K.skills_executor_delete_confirm_action]: {
    key: K.skills_executor_delete_confirm_action,
    message: "Delete executor permanently",
    description: "Label of the destructive confirm button in the executor deletion dialog.",
  },
  [K.skills_executor_deleted]: {
    key: K.skills_executor_deleted,
    message: "Executor deleted.",
    description: "Announced after a successful executor deletion.",
  },

  /* ------------------------------------------------------------------------ */
  /* The /providers/health screen (issue #83)                                 */
  /* ------------------------------------------------------------------------ */
  [K.page_provider_health_title]: {
    key: K.page_provider_health_title,
    message: "Provider health dashboard",
    description: "Heading of `/providers/health`. Deliberately not the same English as `console.chrome.nav_provider_health`.",
  },
  [K.providerhealth_page_intro]: {
    key: K.providerhealth_page_intro,
    message: "A rolling reachability window for every enabled provider, refreshed on each visit to this page.",
    description: "Rendered under the `/providers/health` heading.",
  },
  [K.providerhealth_load_failed]: {
    key: K.providerhealth_load_failed,
    message: "The console could not read provider health from Moira.",
    description: "Rendered instead of the table when the server-side read throws. The page still answers below 400 so the a11y walker's status gate stays green through a backend outage.",
  },
  [K.providerhealth_table_label]: {
    key: K.providerhealth_table_label,
    message: "Provider reachability",
    description: "Accessible name of the provider health table.",
  },
  [K.providerhealth_column_provider]: {
    key: K.providerhealth_column_provider,
    message: "Provider name",
    description: "Column header for `display_name`.",
  },
  [K.providerhealth_column_type]: {
    key: K.providerhealth_column_type,
    message: "Provider kind",
    description: "Column header for `provider_type`.",
  },
  [K.providerhealth_column_status]: {
    key: K.providerhealth_column_status,
    message: "Health status",
    description: "Column header for the healthy/degraded/unhealthy/unknown badge.",
  },
  [K.providerhealth_column_probes]: {
    key: K.providerhealth_column_probes,
    message: "Probes (successful / total)",
    description: "Column header for `probes_successful`/`probes_total`.",
  },
  [K.providerhealth_column_latency]: {
    key: K.providerhealth_column_latency,
    message: "Average latency",
    description: "Column header for `average_latency_ms`.",
  },
  [K.providerhealth_column_last_probe]: {
    key: K.providerhealth_column_last_probe,
    message: "Last probed",
    description: "Column header for `last_probe_at`.",
  },
  [K.providerhealth_column_last_success]: {
    key: K.providerhealth_column_last_success,
    message: "Last succeeded",
    description: "Column header for `last_success_at`.",
  },
  [K.providerhealth_column_last_failure]: {
    key: K.providerhealth_column_last_failure,
    message: "Last failed",
    description: "Column header for `last_failure_at`.",
  },
  [K.providerhealth_status_healthy]: {
    key: K.providerhealth_status_healthy,
    message: "Healthy",
    description: "Badge text for `ProviderHealthStatus.healthy`.",
  },
  [K.providerhealth_status_degraded]: {
    key: K.providerhealth_status_degraded,
    message: "Degraded",
    description: "Badge text for `ProviderHealthStatus.degraded`.",
  },
  [K.providerhealth_status_unhealthy]: {
    key: K.providerhealth_status_unhealthy,
    message: "Unhealthy",
    description: "Badge text for `ProviderHealthStatus.unhealthy`.",
  },
  [K.providerhealth_status_unknown]: {
    key: K.providerhealth_status_unknown,
    message: "Not yet probed",
    description: "Badge text for `ProviderHealthStatus.unknown` — a provider with no snapshot in the rolling window, never probed or not probed recently enough.",
  },
  [K.providerhealth_empty]: {
    key: K.providerhealth_empty,
    message: "No provider is enabled yet.",
    description: "Empty state for the health table on a deployment with no enabled providers.",
  },
  [K.providerhealth_never]: {
    key: K.providerhealth_never,
    message: "Never",
    description: "Rendered in place of a null timestamp column (last probed/succeeded/failed).",
  },
  [K.providerhealth_latency_unknown]: {
    key: K.providerhealth_latency_unknown,
    message: "No data",
    description: "Rendered in place of a null `average_latency_ms`.",
  },

  /* ------------------------------------------------------------------------ */
  /* The /evals screen (plan 12 §3)                                           */
  /* ------------------------------------------------------------------------ */
  [K.page_evals_title]: {
    key: K.page_evals_title,
    message: "Eval suites",
    description: "Heading of `/evals`.",
  },
  [K.evals_page_intro]: {
    key: K.evals_page_intro,
    message: "Author suites of graded cases, then trigger and review runs against them.",
    description: "Rendered under the `/evals` heading.",
  },
  [K.evals_load_failed]: {
    key: K.evals_load_failed,
    message: "The console could not read eval suites from Moira.",
    description: "Rendered instead of the whole screen when the server-side read throws. The page still answers below 400 so the a11y walker's status gate stays green through a backend outage.",
  },
  [K.evals_request_failed]: {
    key: K.evals_request_failed,
    message: "The evals screen could not complete that request. Nothing was changed.",
    description: "Fallback failure text for a browser-side call to this screen's own route handlers that produced no readable keyed refusal.",
  },
  [K.evals_request_body_invalid]: {
    key: K.evals_request_body_invalid,
    message: "This eval request was malformed before it reached Moira — a fault in the console, not in what you typed.",
    description: "An evals route handler could not read its own request body, or read one with no usable fields.",
  },
  [K.evals_suite_key_required]: {
    key: K.evals_suite_key_required,
    message: "Give this suite a key before creating it.",
    description: "The console's own 400 when `suite_key` is missing or blank on `POST /api/evals/suites`.",
  },
  [K.evals_display_name_required]: {
    key: K.evals_display_name_required,
    message: "Give this suite a display name.",
    description: "The console's own 400 when `display_name` is missing or blank, on create or on the edit form's save.",
  },
  [K.evals_grading_kind_required]: {
    key: K.evals_grading_kind_required,
    message: "Choose a grading kind for this case.",
    description: "The console's own 400 when `grading_kind` is absent or not one of the three `GradingKind` values.",
  },
  [K.evals_case_input_required]: {
    key: K.evals_case_input_required,
    message: "Give this case an input.",
    description: "The console's own 400 when the case form's `input` field is empty.",
  },
  [K.evals_case_expected_required]: {
    key: K.evals_case_expected_required,
    message: "Give this case an expected value.",
    description: "The console's own 400 when the case form's `expected` field is empty.",
  },
  [K.evals_run_not_available]: {
    key: K.evals_run_not_available,
    message: "Triggering a run isn't available on this deployment yet. This backend endpoint is landing in a follow-up release.",
    description: "Rendered when `POST /api/evals/suites/{id}/run` answers its deliberate 501 stub — the Moira operation is not in the committed spec yet (see that route's header).",
  },
  [K.evals_suites_heading]: {
    key: K.evals_suites_heading,
    message: "Suites",
    description: "Heading of the section listing every eval suite row.",
  },
  [K.evals_suites_empty]: {
    key: K.evals_suites_empty,
    message: "No eval suite is authored yet.",
    description: "Empty state for the suite list.",
  },
  [K.evals_status_active]: {
    key: K.evals_status_active,
    message: "Suite active",
    description: "Badge text for an eval suite whose `status` is `active`.",
  },
  [K.evals_status_inactive]: {
    key: K.evals_status_inactive,
    message: "Suite not active",
    description: "Badge text for an eval suite whose `status` is not `active` — `disabled` or `deleted`.",
  },
  [K.evals_expand]: {
    key: K.evals_expand,
    message: "Show cases and runs",
    description: "Expands a suite row's cases and runs panels.",
  },
  [K.evals_collapse]: {
    key: K.evals_collapse,
    message: "Hide cases and runs",
    description: "Collapses a suite row's cases and runs panels.",
  },
  [K.evals_edit]: {
    key: K.evals_edit,
    message: "Edit suite",
    description: "Opens the inline edit form for a suite's display name and description.",
  },
  [K.evals_edit_cancel]: {
    key: K.evals_edit_cancel,
    message: "Discard suite edits",
    description: "Closes the suite's inline edit form without saving.",
  },
  [K.evals_edit_save]: {
    key: K.evals_edit_save,
    message: "Save suite",
    description: "Submits the suite's inline edit form's `PATCH`.",
  },
  [K.evals_edit_saved]: {
    key: K.evals_edit_saved,
    message: "Suite updated.",
    description: "Announced after a successful suite edit.",
  },
  [K.evals_delete]: {
    key: K.evals_delete,
    message: "Delete suite",
    description: "Opens the delete confirmation dialog for one suite row.",
  },
  [K.evals_delete_confirm_title]: {
    key: K.evals_delete_confirm_title,
    message: "Delete this suite?",
    description: "Title of `DangerConfirmDialog` for a suite deletion.",
  },
  [K.evals_delete_confirm_body]: {
    key: K.evals_delete_confirm_body,
    message: "This removes the suite, its cases and its run history. There is no restore operation on this surface.",
    description: "Body of `DangerConfirmDialog` for a suite deletion, announced via `role=\"alert\"`. Unlike skills, eval suites have no enable/disable — this really is the one-way door.",
  },
  [K.evals_delete_confirm_action]: {
    key: K.evals_delete_confirm_action,
    message: "Delete suite permanently",
    description: "Label of the destructive confirm button in the suite deletion dialog.",
  },
  [K.evals_field_suite_key_label]: {
    key: K.evals_field_suite_key_label,
    message: "Suite key",
    description: "Label of the `suite_key` field on the create-suite form.",
  },
  [K.evals_field_suite_key_hint]: {
    key: K.evals_field_suite_key_hint,
    message: "A stable identifier for this suite. It cannot be changed after creation.",
    description: "Hint under the `suite_key` field.",
  },
  [K.evals_field_display_name_label]: {
    key: K.evals_field_display_name_label,
    message: "Suite display name",
    description: "Label of the `display_name` field, shared by the create-suite form and the edit form.",
  },
  [K.evals_field_description_label]: {
    key: K.evals_field_description_label,
    message: "Suite description",
    description: "Label of the `description` field, shared by the create-suite form and the edit form.",
  },
  [K.evals_create_heading]: {
    key: K.evals_create_heading,
    message: "Add an eval suite",
    description: "Heading of the create-suite form.",
  },
  [K.evals_create_submit]: {
    key: K.evals_create_submit,
    message: "Create suite",
    description: "Submit button of the create-suite form.",
  },
  [K.evals_create_success]: {
    key: K.evals_create_success,
    message: "Suite created. Add cases below, then trigger a run.",
    description: "Announced after `POST /api/evals/suites` succeeds.",
  },
  [K.evals_cases_heading]: {
    key: K.evals_cases_heading,
    message: "Cases",
    description: "Heading of the section listing one suite's cases.",
  },
  [K.evals_cases_empty]: {
    key: K.evals_cases_empty,
    message: "No case is authored for this suite yet.",
    description: "Empty state for the case list.",
  },
  [K.evals_case_grading_label]: {
    key: K.evals_case_grading_label,
    message: "Grading kind",
    description: "Label of the case form's `grading_kind` select.",
  },
  [K.evals_case_input_label]: {
    key: K.evals_case_input_label,
    message: "Input (JSON)",
    description: "Label of the case form's `input` field.",
  },
  [K.evals_case_input_hint]: {
    key: K.evals_case_input_hint,
    message: "The input passed to the agent under test, as JSON.",
    description: "Hint under the case form's `input` field.",
  },
  [K.evals_case_expected_label]: {
    key: K.evals_case_expected_label,
    message: "Expected (JSON)",
    description: "Label of the case form's `expected` field.",
  },
  [K.evals_case_expected_hint]: {
    key: K.evals_case_expected_hint,
    message: "What the grader compares the result against, as JSON.",
    description: "Hint under the case form's `expected` field.",
  },
  [K.evals_case_add_submit]: {
    key: K.evals_case_add_submit,
    message: "Add case",
    description: "Submit button of the case form.",
  },
  [K.evals_case_added]: {
    key: K.evals_case_added,
    message: "Case added.",
    description: "Announced after `POST /api/evals/suites/{id}/cases` succeeds.",
  },
  [K.evals_case_invalid_json]: {
    key: K.evals_case_invalid_json,
    message: "Input and expected must both be valid JSON.",
    description: "Rendered client-side when either JSON field fails to parse, before anything is sent to the console's own route handler.",
  },
  [K.evals_case_delete]: {
    key: K.evals_case_delete,
    message: "Delete case",
    description: "Removes one case. `EvalCaseRecord` carries no version, so this is a direct action with no confirmation dialog — a case is a small, easily re-added row.",
  },
  [K.evals_grading_exact_match]: {
    key: K.evals_grading_exact_match,
    message: "Exact match",
    description: "Option label for `GradingKind.exact_match`.",
  },
  [K.evals_grading_contains]: {
    key: K.evals_grading_contains,
    message: "Contains",
    description: "Option label for `GradingKind.contains`.",
  },
  [K.evals_grading_schema_valid]: {
    key: K.evals_grading_schema_valid,
    message: "Schema valid",
    description: "Option label for `GradingKind.schema_valid`.",
  },
  [K.evals_runs_heading]: {
    key: K.evals_runs_heading,
    message: "Eval runs",
    description: "Heading of the section listing one suite's eval runs.",
  },
  [K.evals_runs_empty]: {
    key: K.evals_runs_empty,
    message: "No run has been triggered for this suite yet.",
    description: "Empty state for the run list.",
  },
  [K.evals_run_trigger]: {
    key: K.evals_run_trigger,
    message: "Run this suite",
    description: "Triggers `POST /api/evals/suites/{id}/run` — currently the deferred stub, see `console.evals.run_not_available`.",
  },
  [K.evals_run_status_pending]: {
    key: K.evals_run_status_pending,
    message: "Run pending",
    description: "Badge text for `EvalRunStatus.pending`.",
  },
  [K.evals_run_status_running]: {
    key: K.evals_run_status_running,
    message: "Run in progress",
    description: "Badge text for `EvalRunStatus.running`.",
  },
  [K.evals_run_status_completed]: {
    key: K.evals_run_status_completed,
    message: "Run completed",
    description: "Badge text for `EvalRunStatus.completed`.",
  },
  [K.evals_run_status_failed]: {
    key: K.evals_run_status_failed,
    message: "Run failed",
    description: "Badge text for `EvalRunStatus.failed`.",
  },
  [K.evals_run_score_label]: {
    key: K.evals_run_score_label,
    message: "Score",
    description: "Label for a run's `score` field.",
  },
  [K.evals_run_score_none]: {
    key: K.evals_run_score_none,
    message: "Not scored",
    description: "Rendered in place of a null `score`.",
  },
  [K.evals_run_created_label]: {
    key: K.evals_run_created_label,
    message: "Triggered",
    description: "Label for a run row's `created_at` timestamp.",
  },

  /* ------------------------------------------------------------------------ */
  /* The /flows screen (plan 12 §6)                                           */
  /* ------------------------------------------------------------------------ */
  [K.page_flows_title]: {
    key: K.page_flows_title,
    message: "Multi-agent flows",
    description: "Heading of `/flows`. Deliberately not the same English as `console.chrome.nav_flows`.",
  },
  [K.flows_page_intro]: {
    key: K.flows_page_intro,
    message:
      "A flow is an ordered, sequential list of steps, each running one agent profile. Author the " +
      "steps below, then trigger and review runs.",
    description: "Rendered under the `/flows` heading, naming the sequential-only MVP shape (decision 13).",
  },
  [K.flows_load_failed]: {
    key: K.flows_load_failed,
    message: "The console could not read flows from Moira.",
    description: "Rendered instead of the whole screen when the server-side read throws. The page still answers below 400 so the a11y walker's status gate stays green through a backend outage.",
  },
  [K.flows_request_failed]: {
    key: K.flows_request_failed,
    message: "The flows screen could not complete that request. Nothing was changed.",
    description: "Fallback failure text for a browser-side call to this screen's own route handlers that produced no readable keyed refusal.",
  },
  [K.flows_request_body_invalid]: {
    key: K.flows_request_body_invalid,
    message: "This flow request was malformed before it reached Moira — a fault in the console, not in what you typed.",
    description: "A flows route handler could not read its own request body, or read one with no usable fields.",
  },
  [K.flows_flow_key_required]: {
    key: K.flows_flow_key_required,
    message: "Give this flow a key before creating it.",
    description: "The console's own 400 when `flow_key` is missing or blank on `POST /api/flows`.",
  },
  [K.flows_display_name_required]: {
    key: K.flows_display_name_required,
    message: "Give this flow a display name.",
    description: "The console's own 400 when `display_name` is missing or blank, on create or on the edit form's save.",
  },
  [K.flows_steps_invalid]: {
    key: K.flows_steps_invalid,
    message: "Every step needs a step key, an order, and an agent profile.",
    description: "The console's own 400 when the step list carries an entry missing one of its three required fields.",
  },
  [K.flows_run_not_available]: {
    key: K.flows_run_not_available,
    message: "Triggering a flow run isn't available on this deployment yet. This backend endpoint is landing in a follow-up release.",
    description: "Rendered when `POST /api/flows/{id}/run` answers its deliberate 501 stub — the Moira operation is not in the committed spec yet (see that route's header).",
  },
  [K.flows_list_heading]: {
    key: K.flows_list_heading,
    message: "Authored flows",
    description: "Heading of the section listing every flow row.",
  },
  [K.flows_list_empty]: {
    key: K.flows_list_empty,
    message: "No flow is authored yet.",
    description: "Empty state for the flow list.",
  },
  [K.flows_status_active]: {
    key: K.flows_status_active,
    message: "Flow active",
    description: "Badge text for a flow whose `status` is `active`.",
  },
  [K.flows_status_inactive]: {
    key: K.flows_status_inactive,
    message: "Flow not active",
    description: "Badge text for a flow whose `status` is not `active` — `disabled` or `deleted`.",
  },
  [K.flows_expand]: {
    key: K.flows_expand,
    message: "Show steps and runs",
    description: "Expands a flow row's steps and runs panels.",
  },
  [K.flows_collapse]: {
    key: K.flows_collapse,
    message: "Hide steps and runs",
    description: "Collapses a flow row's steps and runs panels.",
  },
  [K.flows_edit]: {
    key: K.flows_edit,
    message: "Edit flow",
    description: "Opens the inline edit form for a flow's display name, description and step list.",
  },
  [K.flows_edit_cancel]: {
    key: K.flows_edit_cancel,
    message: "Discard flow edits",
    description: "Closes the flow's inline edit form without saving.",
  },
  [K.flows_edit_save]: {
    key: K.flows_edit_save,
    message: "Save flow",
    description: "Submits the flow's inline edit form's `PATCH`.",
  },
  [K.flows_edit_saved]: {
    key: K.flows_edit_saved,
    message: "Flow updated.",
    description: "Announced after a successful flow edit.",
  },
  [K.flows_delete]: {
    key: K.flows_delete,
    message: "Delete flow",
    description: "Opens the delete confirmation dialog for one flow row.",
  },
  [K.flows_delete_confirm_title]: {
    key: K.flows_delete_confirm_title,
    message: "Delete this flow?",
    description: "Title of `DangerConfirmDialog` for a flow deletion.",
  },
  [K.flows_delete_confirm_body]: {
    key: K.flows_delete_confirm_body,
    message: "This removes the flow, its steps and its run history. There is no restore operation on this surface.",
    description: "Body of `DangerConfirmDialog` for a flow deletion, announced via `role=\"alert\"`.",
  },
  [K.flows_delete_confirm_action]: {
    key: K.flows_delete_confirm_action,
    message: "Delete flow permanently",
    description: "Label of the destructive confirm button in the flow deletion dialog.",
  },
  [K.flows_field_flow_key_label]: {
    key: K.flows_field_flow_key_label,
    message: "Flow key",
    description: "Label of the `flow_key` field on the create-flow form.",
  },
  [K.flows_field_flow_key_hint]: {
    key: K.flows_field_flow_key_hint,
    message: "A stable identifier for this flow. It cannot be changed after creation.",
    description: "Hint under the `flow_key` field.",
  },
  [K.flows_field_display_name_label]: {
    key: K.flows_field_display_name_label,
    message: "Flow display name",
    description: "Label of the `display_name` field, shared by the create-flow form and the edit form.",
  },
  [K.flows_field_description_label]: {
    key: K.flows_field_description_label,
    message: "Flow description",
    description: "Label of the `description` field, shared by the create-flow form and the edit form.",
  },
  [K.flows_create_heading]: {
    key: K.flows_create_heading,
    message: "Add a flow",
    description: "Heading of the create-flow form.",
  },
  [K.flows_create_submit]: {
    key: K.flows_create_submit,
    message: "Create flow",
    description: "Submit button of the create-flow form.",
  },
  [K.flows_create_success]: {
    key: K.flows_create_success,
    message: "Flow created.",
    description: "Announced after `POST /api/flows` succeeds.",
  },
  [K.flows_steps_heading]: {
    key: K.flows_steps_heading,
    message: "Steps, in order",
    description: "Heading of the step builder, shared by the create-flow form and the edit form.",
  },
  [K.flows_steps_empty]: {
    key: K.flows_steps_empty,
    message: "No step is added yet. A flow with no steps can be authored but cannot run.",
    description: "Empty state for the step builder's current list.",
  },
  [K.flows_step_key_label]: {
    key: K.flows_step_key_label,
    message: "Step key",
    description: "Label of one step row's `step_key` field in the step builder.",
  },
  [K.flows_step_order_label]: {
    key: K.flows_step_order_label,
    message: "Step order",
    description: "Label of one step row's `step_order` field in the step builder.",
  },
  [K.flows_step_agent_profile_label]: {
    key: K.flows_step_agent_profile_label,
    message: "Agent profile",
    description: "Label of one step row's `agent_profile_id` select in the step builder.",
  },
  [K.flows_step_agent_profile_none]: {
    key: K.flows_step_agent_profile_none,
    message: "Choose an agent profile",
    description: "Placeholder option of the agent-profile select before anything is chosen.",
  },
  [K.flows_step_on_failure_label]: {
    key: K.flows_step_on_failure_label,
    message: "On failure",
    description: "Label of one step row's `on_failure` select in the step builder.",
  },
  [K.flows_on_failure_abort]: {
    key: K.flows_on_failure_abort,
    message: "Abort the flow",
    description: "Option label for `FlowStepOnFailure.abort`.",
  },
  [K.flows_on_failure_continue]: {
    key: K.flows_on_failure_continue,
    message: "Continue to the next step",
    description: "Option label for `FlowStepOnFailure.continue`.",
  },
  [K.flows_step_add]: {
    key: K.flows_step_add,
    message: "Add step",
    description: "Adds a new blank row to the step builder's local list.",
  },
  [K.flows_step_remove]: {
    key: K.flows_step_remove,
    message: "Remove step",
    description: "Removes one row from the step builder's local list.",
  },
  [K.flows_step_agent_profiles_load_failed]: {
    key: K.flows_step_agent_profiles_load_failed,
    message: "The console could not read the agent profile list. Steps can still be authored by pasting a profile id.",
    description: "Rendered when the server-side agent-profile read fails; the step builder degrades to a free-text id field rather than blocking flow authoring entirely.",
  },
  [K.flows_runs_heading]: {
    key: K.flows_runs_heading,
    message: "Flow runs",
    description: "Heading of the section listing one flow's runs.",
  },
  [K.flows_runs_empty]: {
    key: K.flows_runs_empty,
    message: "No run has been triggered for this flow yet.",
    description: "Empty state for the run list.",
  },
  [K.flows_run_trigger]: {
    key: K.flows_run_trigger,
    message: "Run this flow",
    description: "Triggers `POST /api/flows/{id}/run` — currently the deferred stub, see `console.flows.run_not_available`.",
  },
  [K.flows_run_status_running]: {
    key: K.flows_run_status_running,
    message: "Flow run in progress",
    description: "Badge text for `FlowRunStatus.running`.",
  },
  [K.flows_run_status_completed]: {
    key: K.flows_run_status_completed,
    message: "Flow run completed",
    description: "Badge text for `FlowRunStatus.completed`.",
  },
  [K.flows_run_status_failed]: {
    key: K.flows_run_status_failed,
    message: "Flow run failed",
    description: "Badge text for `FlowRunStatus.failed`.",
  },
  [K.flows_run_status_cancelled]: {
    key: K.flows_run_status_cancelled,
    message: "Flow run cancelled",
    description: "Badge text for `FlowRunStatus.cancelled`.",
  },
  [K.flows_run_created_label]: {
    key: K.flows_run_created_label,
    message: "Flow run triggered",
    description: "Label for a flow run row's `created_at` timestamp.",
  },

  /* --- the /playground screen (issue #261) --------------------------------- */
  [K.page_playground_title]: {
    key: K.page_playground_title,
    message: "Test playground",
    description:
      "Page `<h1>` for `/playground`. Deliberately not the same English as the nav link's " +
      "`console.chrome.nav_playground` — same convention as `console.page.skills_title`.",
  },
  [K.playground_page_intro]: {
    key: K.playground_page_intro,
    message:
      "Send a prompt through Moira's real execution path and see how it was routed — which candidate served, retries and failovers, latency and token usage. This is a testing tool, not a chat product: nothing sent here is saved as conversation history.",
    description: "Intro paragraph under the page title.",
  },
  [K.playground_request_body_invalid]: {
    key: K.playground_request_body_invalid,
    message: "The console could not read that playground request.",
    description: "400 when a playground BFF route's JSON body fails to parse at all.",
  },
  [K.playground_prompt_required]: {
    key: K.playground_prompt_required,
    message: "Enter a prompt before sending.",
    description: "400 when the prompt field is missing or blank, and the client-side disabled-Send state's reason.",
  },
  [K.playground_run_failed]: {
    key: K.playground_run_failed,
    message: "The console could not reach the non-streaming execution endpoint.",
    description:
      "Fallback text for a transport failure calling `POST /api/playground/run`, before any Moira-supplied message is available.",
  },
  [K.playground_stream_failed]: {
    key: K.playground_stream_failed,
    message: "The console could not reach the streaming execution endpoint.",
    description:
      "Fallback text for a transport failure calling `POST /api/playground/stream`, before any Moira-supplied message is available.",
  },
  [K.playground_diagnose_failed]: {
    key: K.playground_diagnose_failed,
    message: "The console could not reach the diagnostics endpoint.",
    description:
      "Fallback text for a transport failure calling `POST /api/playground/diagnose`, before any Moira-supplied message is available.",
  },
  [K.playground_execution_summary_failed]: {
    key: K.playground_execution_summary_failed,
    message: "The routing summary for this run could not be loaded.",
    description:
      "Shown when the post-run `GET /api/playground/executions/{id}` follow-up fails; the response text itself is still shown.",
  },
  [K.playground_models_load_failed]: {
    key: K.playground_models_load_failed,
    message: "The model list for this provider could not be loaded.",
    description: "Shown when `GET /api/playground/providers/{id}/models` fails after choosing a provider.",
  },
  [K.playground_pickers_load_failed]: {
    key: K.playground_pickers_load_failed,
    message: "Routes, agent profiles or providers could not be loaded. Controls are shown without their pickers.",
    description:
      "Non-fatal notice when the page's server-side route/agent-profile/provider reads fail — the page still renders, same posture as `/flows`'s agent-profile read.",
  },

  [K.playground_controls_heading]: {
    key: K.playground_controls_heading,
    message: "Controls",
    description: "Heading of the collapsed-by-default `<details>` controls panel.",
  },
  [K.playground_field_route_label]: {
    key: K.playground_field_route_label,
    message: "Route override",
    description: "Label for the route picker.",
  },
  [K.playground_field_route_none]: {
    key: K.playground_field_route_none,
    message: "Default routing (no override)",
    description: "The route picker's empty option.",
  },
  [K.playground_field_agent_profile_label]: {
    key: K.playground_field_agent_profile_label,
    message: "Agent profile filter",
    description: "Label for the agent-profile picker — named as a filter, because that is all it does; see the hint below.",
  },
  [K.playground_field_agent_profile_hint]: {
    key: K.playground_field_agent_profile_hint,
    message:
      "There is no agent-profile field on the execution request — Moira resolves it from the selected route's own configuration. Choosing a profile here narrows the Route list below to routes wired to it, so the tool loop actually runs.",
    description:
      "Explains why this control filters Route rather than being sent on the wire — `agent_profile_hint` is hardcoded to `None` on `POST /api/v1/responses` (`src/application/public.rs`).",
  },
  [K.playground_field_agent_profile_none]: {
    key: K.playground_field_agent_profile_none,
    message: "Any route",
    description: "The agent-profile filter's empty option — every route is shown.",
  },
  [K.playground_field_provider_label]: {
    key: K.playground_field_provider_label,
    message: "Provider override",
    description: "Label for the provider picker.",
  },
  [K.playground_field_provider_none]: {
    key: K.playground_field_provider_none,
    message: "No provider override",
    description: "The provider picker's empty option.",
  },
  [K.playground_field_model_label]: {
    key: K.playground_field_model_label,
    message: "Model override",
    description: "Label for the model picker.",
  },
  [K.playground_field_model_none]: {
    key: K.playground_field_model_none,
    message: "No model override",
    description: "The model picker's empty option.",
  },
  [K.playground_field_model_needs_provider]: {
    key: K.playground_field_model_needs_provider,
    message: "Choose a provider to list its models.",
    description: "Shown in place of the model picker until a provider is selected.",
  },
  [K.playground_field_temperature_label]: {
    key: K.playground_field_temperature_label,
    message: "Temperature",
    description: "Label for the temperature number input.",
  },
  [K.playground_field_max_tokens_label]: {
    key: K.playground_field_max_tokens_label,
    message: "Max output tokens",
    description: "Label for the max-output-tokens number input.",
  },
  [K.playground_field_priority_label]: {
    key: K.playground_field_priority_label,
    message: "Priority",
    description: "Label for the priority number input.",
  },
  [K.playground_field_priority_hint]: {
    key: K.playground_field_priority_hint,
    message:
      "Only takes effect with Detailed diagnostics enabled — the streaming and non-streaming chat endpoints have no priority field at all. Scope-gated server-side (`moira:execution:override-priority`); a caller without that scope sees the refusal after sending, not a silently ignored value.",
    description:
      "Explains the field's real scope: `ExecutionOptions.priority` is reachable only through `POST /api/v1/admin/runtime/diagnose`.",
  },
  [K.playground_field_complexity_hint_label]: {
    key: K.playground_field_complexity_hint_label,
    message: "Complexity hint",
    description: "Label for the complexity-tier picker. Same diagnostics-only scope as priority.",
  },
  [K.playground_complexity_none]: {
    key: K.playground_complexity_none,
    message: "Unset",
    description: "The complexity-hint picker's empty option.",
  },
  [K.playground_complexity_trivial]: {
    key: K.playground_complexity_trivial,
    message: "Trivial",
    description: "`ComplexityTier.trivial`.",
  },
  [K.playground_complexity_standard]: {
    key: K.playground_complexity_standard,
    message: "Standard",
    description: "`ComplexityTier.standard`.",
  },
  [K.playground_complexity_heavy]: {
    key: K.playground_complexity_heavy,
    message: "Heavy",
    description: "`ComplexityTier.heavy`.",
  },
  [K.playground_field_stream_toggle_label]: {
    key: K.playground_field_stream_toggle_label,
    message: "Stream the response",
    description: "Label for the streaming/non-streaming fallback toggle. On by default.",
  },
  [K.playground_field_diagnostics_toggle_label]: {
    key: K.playground_field_diagnostics_toggle_label,
    message: "Detailed diagnostics (routing candidates + tool calls)",
    description: "Label for the toggle that switches Send to `POST /api/playground/diagnose`.",
  },
  [K.playground_field_diagnostics_toggle_hint]: {
    key: K.playground_field_diagnostics_toggle_hint,
    message:
      "Runs a separate, non-streaming diagnostic execution instead of the normal chat call — the only way to see per-candidate ranking, retries/failovers and tool invocations, because the streaming and non-streaming chat endpoints never emit them. Disabled on this deployment, or without the `moira:runtime:diagnose` scope, this will fail cleanly after sending rather than silently doing nothing.",
    description:
      "The core honesty note for issue #261's biggest gap: tool calls and candidate ranking are dropped from the public SSE stream by `map_runtime_event` and exist only via the diagnose endpoint.",
  },

  [K.playground_prompt_label]: {
    key: K.playground_prompt_label,
    message: "Prompt",
    description: "Label for the prompt textarea.",
  },
  [K.playground_prompt_placeholder]: {
    key: K.playground_prompt_placeholder,
    message: "Ask the model something…",
    description: "Placeholder text for the prompt textarea.",
  },
  [K.playground_send]: {
    key: K.playground_send,
    message: "Send",
    description: "The submit button.",
  },
  [K.playground_stop]: {
    key: K.playground_stop,
    message: "Stop",
    description: "The cancel button, shown only while a streaming request is in flight.",
  },
  [K.playground_status_idle]: {
    key: K.playground_status_idle,
    message: "Idle",
    description: "Status line before the first send.",
  },
  [K.playground_status_sending]: {
    key: K.playground_status_sending,
    message: "Sending…",
    description: "Status line for the non-streaming request while it is in flight.",
  },
  [K.playground_status_streaming]: {
    key: K.playground_status_streaming,
    message: "Streaming…",
    description: "Status line while SSE frames are arriving.",
  },
  [K.playground_status_diagnosing]: {
    key: K.playground_status_diagnosing,
    message: "Running detailed diagnostics…",
    description: "Status line while the non-streaming diagnostic call is in flight.",
  },
  [K.playground_status_cancelled]: {
    key: K.playground_status_cancelled,
    message: "Cancelled",
    description: "Status line after the Stop button aborts an in-flight stream.",
  },

  [K.playground_response_heading]: {
    key: K.playground_response_heading,
    message: "Response",
    description: "Heading of the response panel.",
  },
  [K.playground_response_empty]: {
    key: K.playground_response_empty,
    message: "Send a prompt to see the response here.",
    description: "The response panel's empty state.",
  },

  [K.playground_routing_heading]: {
    key: K.playground_routing_heading,
    message: "Routing outcome",
    description: "Heading of the routing-transparency panel.",
  },
  [K.playground_routing_route_label]: {
    key: K.playground_routing_route_label,
    message: "Route used",
    description: "Label for the route that actually served the request.",
  },
  [K.playground_routing_model_label]: {
    key: K.playground_routing_model_label,
    message: "Model used",
    description: "Label for the provider/model that actually served the request.",
  },
  [K.playground_routing_status_label]: {
    key: K.playground_routing_status_label,
    message: "Execution status",
    description: "Label for the execution's terminal status.",
  },
  [K.playground_routing_latency_label]: {
    key: K.playground_routing_latency_label,
    message: "Latency",
    description: "Label for `PublicExecutionSummary.latency_ms`.",
  },
  [K.playground_routing_attempt_count_label]: {
    key: K.playground_routing_attempt_count_label,
    message: "Attempts",
    description: "Label for `PublicExecutionSummary.attempt_count` — how many candidates were tried, including retries and failovers.",
  },
  [K.playground_routing_usage_label]: {
    key: K.playground_routing_usage_label,
    message: "Token usage",
    description: "Label for the usage summary.",
  },
  [K.playground_routing_usage_tokens]: {
    key: K.playground_routing_usage_tokens,
    message: "{input} in / {output} out / {total} total",
    description: "Interpolated token-count line. `input`/`output`/`total` are `messageArgs`.",
  },
  [K.playground_routing_fallback_heading]: {
    key: K.playground_routing_fallback_heading,
    message: "Failover hops",
    description: "Heading for the list of `response.fallback.selected` events observed live during a streamed run.",
  },
  [K.playground_routing_summary_pending]: {
    key: K.playground_routing_summary_pending,
    message: "Loading the routing summary…",
    description: "Shown while the post-run `GET /api/playground/executions/{id}` follow-up is in flight.",
  },
  [K.playground_routing_summary_unavailable]: {
    key: K.playground_routing_summary_unavailable,
    message: "No routing summary yet — send a prompt first.",
    description: "The routing panel's empty state, before any run has completed.",
  },
  [K.playground_routing_summary_public_note]: {
    key: K.playground_routing_summary_public_note,
    message:
      "This is the routing detail available without extra permissions. Per-candidate rank/score and the reason each one was tried are only visible with Detailed diagnostics enabled.",
    description: "Sits under the baseline (non-diagnostic) routing summary to set expectations honestly.",
  },

  [K.playground_diagnostics_heading]: {
    key: K.playground_diagnostics_heading,
    message: "Detailed diagnostics",
    description: "Heading of the diagnostics-mode result panel.",
  },
  [K.playground_diagnostics_candidates_heading]: {
    key: K.playground_diagnostics_candidates_heading,
    message: "Candidate ranking",
    description: "Heading for the `candidate_ranked` event's candidate list.",
  },
  [K.playground_diagnostics_candidate_rank_label]: {
    key: K.playground_diagnostics_candidate_rank_label,
    message: "Rank",
    description: "Label for a candidate's `candidate_rank`.",
  },
  [K.playground_diagnostics_candidate_score_label]: {
    key: K.playground_diagnostics_candidate_score_label,
    message: "Candidate score",
    description: "Label for a candidate's `candidate_score` — usually unset in this MVP-static routing slice.",
  },
  [K.playground_diagnostics_candidate_reason_label]: {
    key: K.playground_diagnostics_candidate_reason_label,
    message: "Selection reason",
    description: "Label for a candidate's `selection_reason` (`AttemptSelectionReason`).",
  },
  [K.playground_diagnostics_attempts_heading]: {
    key: K.playground_diagnostics_attempts_heading,
    message: "Provider attempts",
    description: "Heading for `ExecutionOutcome.attempts`.",
  },
  [K.playground_diagnostics_failure_heading]: {
    key: K.playground_diagnostics_failure_heading,
    message: "Failure",
    description: "Heading shown when `ExecutionOutcome.failure` is present.",
  },

  [K.playground_tools_heading]: {
    key: K.playground_tools_heading,
    message: "Tool calls",
    description: "Heading of the tool-call list.",
  },
  [K.playground_tools_empty]: {
    key: K.playground_tools_empty,
    message: "No tool calls on this run.",
    description: "Shown in diagnostics mode when the run produced no `tool_call_*`/`tool_result` events.",
  },
  [K.playground_tools_unavailable_note]: {
    key: K.playground_tools_unavailable_note,
    message:
      "Tool call activity is not visible on the streaming or non-streaming chat path — Moira's public execution API does not emit it. Enable Detailed diagnostics to see each tool invocation and its result.",
    description:
      "Shown in place of the tool-call list outside diagnostics mode. `tool_call_started`/`tool_call_delta`/`tool_call_completed`/`tool_result` are all dropped from the public SSE stream by `map_runtime_event`.",
  },
  [K.playground_tool_arguments_label]: {
    key: K.playground_tool_arguments_label,
    message: "Arguments",
    description: "Label for a tool call's model-authored arguments (from `tool_call_started`).",
  },
  [K.playground_tool_outcome_label]: {
    key: K.playground_tool_outcome_label,
    message: "Outcome",
    description: "Label for a tool call's outcome (from `tool_result`).",
  },
};

export const CONSOLE_CATALOG_ENTRIES: readonly CatalogEntry[] = Object.values(CONSOLE_CATALOG);
