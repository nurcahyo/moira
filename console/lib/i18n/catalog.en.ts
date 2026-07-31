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
      "`resolveAuthConfig` found no active, enabled `auth_provider_settings` row. This is the " +
      "normal first-run state, not a failure — it is the setup wizard's whole reason to exist.",
  },
  [K.ambiguous_enabled_auth_providers]: {
    key: K.ambiguous_enabled_auth_providers,
    message:
      "More than one sign-in provider is enabled. The console will not guess which one governs — " +
      "disable all but one in Moira.",
    description:
      "`resolveAuthConfig` found `enabled.length > 1`. Moira permits several enabled rows and " +
      "picks one by a documented ordering at claim time; the console refuses to, because a " +
      "sign-in button that silently uses whichever row sorted first is worse than an error.",
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
      "The single sign-in button, when the provider's display name is known. `{provider}` is the " +
      "`display_name` from the anonymous `GET /api/v1/admin/setup/sign-in-methods` projection. " +
      "There is AT MOST ONE button by construction: `resolveAuthConfig` returns " +
      "`ambiguous_enabled_providers` when more than one provider is enabled, so a picker is wrong " +
      "in this wave rather than merely unbuilt.",
  },
  [K.sign_in_button_generic]: {
    key: K.sign_in_button_generic,
    message: "Continue with your identity provider",
    description:
      "The sign-in button when the anonymous sign-in-methods call yielded no display name for the " +
      "resolved provider — Moira unreachable, or the row absent from the projection. The " +
      "configuration is already resolved at this point, so the button still works.",
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
};

/** Every entry, as a plain array. */
export const CONSOLE_CATALOG_ENTRIES: readonly CatalogEntry[] = Object.values(CONSOLE_CATALOG);
