// Every console-originated i18n key, in one client-safe place.
//
// ============================================================================
// WHY THIS FILE CARRIES NO `import "server-only"`
// ============================================================================
//
// Five of the six modules that emit a `console.*` key carry `import "server-only"`
// (`auth-config.ts`, `console-secrets.ts`, `auth-runtime.ts`, `moira-session.ts`,
// `setup-flow.ts`). If the key union lived in one of them — or if the catalog
// imported them to derive it — the credential graph would be dragged into the
// browser bundle by the very module whose job is to render a string.
//
// So the dependency runs the other way: this module owns the keys, and each
// emitting module imports from here while KEEPING its own exported name and
// shape. `errors.ts` still exports `CONSOLE_TRANSPORT_ERROR_KEY` /
// `CONSOLE_MALFORMED_ERROR_KEY`, `auth-config.ts` still exports
// `AUTH_CONFIG_PROBLEM_MESSAGE_KEYS`, `console-secrets.ts` still exports
// `CONSOLE_SECRET_DRIFT_MESSAGE_KEYS`. Those are pinned by shipped tests and by
// call sites; this is a re-export, not a replacement.
//
// ============================================================================
// THE CONTRACT THIS TABLE IS ONE HALF OF
// ============================================================================
//
// `catalog.en.ts` declares `Record<ConsoleMessageKey, CatalogEntry>`, so a key
// added here without an entry there is a **type error** at `bun run typecheck`.
// That is the same idiom `lib/types.ts:10-14` already uses for the DTO
// descriptors, and for the same stated reason: "Both halves are required —
// either alone is silently defeatable."
//
// The other half is `tests/unit/lib/i18n-catalog-coverage.test.ts`, which welds
// this table to the EMISSION SITES: a key here that nothing references fails,
// and a `console.*` literal in the tree that is not here fails. `tsc` cannot see
// either of those, because neither is a type.
//
// Namespacing mirrors Moira's own (`moira.error.*` / `moira.notice.*`):
//   console.error.*   a condition the console itself detected
//   console.a11y.*    a string that exists only for assistive technology
//   console.meta.*    document metadata
//   console.page.*    page-level copy
//   console.signIn.*  the sign-in surface
//   console.secret.*  the once-only secret surface

/* -------------------------------------------------------------------------- */
/* The table                                                                  */
/* -------------------------------------------------------------------------- */

export const CONSOLE_MESSAGE_KEYS = {
  /* --- lib/errors.ts ------------------------------------------------------ */
  moira_unreachable: "console.error.moira_unreachable",
  moira_response_unreadable: "console.error.moira_response_unreadable",

  /* --- lib/auth-config.ts ------------------------------------------------- */
  no_enabled_auth_provider: "console.error.no_enabled_auth_provider",
  ambiguous_enabled_auth_providers: "console.error.ambiguous_enabled_auth_providers",
  auth_method_not_interactive: "console.error.auth_method_not_interactive",
  auth_provider_endpoints_incomplete: "console.error.auth_provider_endpoints_incomplete",
  allowed_email_domains_empty: "console.error.allowed_email_domains_empty",
  provider_not_bound_to_trusted_jwt_issuer:
    "console.error.provider_not_bound_to_trusted_jwt_issuer",

  /* --- lib/console-secrets.ts (oauth_client_secret_missing is shared with
         lib/auth-config.ts — one key, two emitters, deliberately) ---------- */
  oauth_client_secret_missing: "console.error.oauth_client_secret_missing",
  oauth_client_id_drifted: "console.error.oauth_client_id_drifted",
  moira_provider_client_id_missing: "console.error.moira_provider_client_id_missing",

  /* --- lib/auth-runtime.ts ------------------------------------------------ */
  auth_config_unavailable: "console.error.auth_config_unavailable",

  /* --- lib/moira-session.ts ----------------------------------------------- */
  session_required: "console.error.session_required",
  email_not_verified: "console.error.email_not_verified",
  email_domain_not_allowed: "console.error.email_domain_not_allowed",
  idp_subject_missing: "console.error.idp_subject_missing",

  /* --- lib/setup-flow.ts -------------------------------------------------- */
  trusted_jwt_issuer_registration_failed: "console.error.trusted_jwt_issuer_registration_failed",
  auth_provider_create_failed: "console.error.auth_provider_create_failed",
  auth_provider_secret_write_failed: "console.error.auth_provider_secret_write_failed",
  auth_provider_enable_failed: "console.error.auth_provider_enable_failed",

  /* --- accessibility ------------------------------------------------------ */
  a11y_loading: "console.a11y.loading",
  a11y_required: "console.a11y.required",

  /* --- document metadata -------------------------------------------------- */
  meta_title: "console.meta.title",
  meta_description: "console.meta.description",

  /* --- pages -------------------------------------------------------------- */
  page_home_title: "console.page.home_title",
  page_home_body: "console.page.home_body",
} as const;

/** Every console-originated key, as a union of string literals. */
export type ConsoleMessageKey = (typeof CONSOLE_MESSAGE_KEYS)[keyof typeof CONSOLE_MESSAGE_KEYS];

/** The member names, for callers that want to iterate the table. */
export type ConsoleMessageKeyName = keyof typeof CONSOLE_MESSAGE_KEYS;

/** Every key, as a plain array. Order is the declaration order above. */
export const ALL_CONSOLE_MESSAGE_KEYS: readonly ConsoleMessageKey[] =
  Object.values(CONSOLE_MESSAGE_KEYS);

/** Narrow an arbitrary string to a key this console owns. */
export function isConsoleMessageKey(key: string): key is ConsoleMessageKey {
  return (ALL_CONSOLE_MESSAGE_KEYS as readonly string[]).includes(key);
}
