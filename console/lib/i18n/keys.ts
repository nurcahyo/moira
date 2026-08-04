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
//   console.action.*  a generic control's label, reused across surfaces
//   console.secret.*  the once-only secret surface
//   console.chrome.*  the authenticated shell: navigation and sign-out
//   console.expiry.*  invitation lifetimes (the ExpiryPicker molecule)
//   console.admins.*  the /admins screen — grants, invitations, ownership
//   console.invite.*  the public /invite/[token] redemption page

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
  trusted_jwt_issuer_not_resolvable: "console.error.trusted_jwt_issuer_not_resolvable",

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
  session_provider_unknown: "console.error.session_provider_unknown",

  /* --- lib/setup-flow.ts -------------------------------------------------- */
  trusted_jwt_issuer_registration_failed: "console.error.trusted_jwt_issuer_registration_failed",
  auth_provider_create_failed: "console.error.auth_provider_create_failed",
  auth_provider_secret_write_failed: "console.error.auth_provider_secret_write_failed",
  auth_provider_enable_failed: "console.error.auth_provider_enable_failed",

  /* --- lib/setup-window.ts + app/api/setup/route.ts (the BFF setup door) ---
         Refusals the CONSOLE decided, before or instead of a Moira request.
         Wizard UI copy is NOT here — it belongs to the setup-wizard-ui item. */
  setup_system_key_absent: "console.error.setup_system_key_absent",
  setup_already_claimed: "console.error.setup_already_claimed",
  setup_request_body_invalid: "console.error.setup_request_body_invalid",
  setup_action_unknown: "console.error.setup_action_unknown",
  setup_method_unsupported: "console.error.setup_method_unsupported",
  setup_display_name_required: "console.error.setup_display_name_required",
  setup_client_id_required: "console.error.setup_client_id_required",
  setup_client_secret_required: "console.error.setup_client_secret_required",
  setup_issuer_or_discovery_required: "console.error.setup_issuer_or_discovery_required",
  setup_allowed_email_domains_required: "console.error.setup_allowed_email_domains_required",
  setup_provider_slug_invalid: "console.error.setup_provider_slug_invalid",
  setup_resume_state_invalid: "console.error.setup_resume_state_invalid",
  setup_ordering_violated: "console.error.setup_ordering_violated",
  setup_claim_step_unreachable: "console.error.setup_claim_step_unreachable",
  setup_email_not_verified: "console.error.setup_email_not_verified",
  setup_claim_domain_not_allowed: "console.error.setup_claim_domain_not_allowed",

  /* --- accessibility ------------------------------------------------------ */
  a11y_loading: "console.a11y.loading",
  a11y_required: "console.a11y.required",

  /* --- document metadata -------------------------------------------------- */
  meta_title: "console.meta.title",
  meta_description: "console.meta.description",

  /* --- pages -------------------------------------------------------------- */
  page_home_title: "console.page.home_title",
  page_home_body: "console.page.home_body",
  page_login_title: "console.page.login_title",

  /* --- sign-in ------------------------------------------------------------ */
  sign_in_heading: "console.signIn.heading",
  sign_in_button: "console.signIn.button",
  sign_in_button_generic: "console.signIn.button_generic",
  sign_in_pending: "console.signIn.pending",
  sign_in_unavailable_heading: "console.signIn.unavailable_heading",
  sign_in_request_failed: "console.signIn.request_failed",
  sign_in_rate_limited: "console.signIn.rate_limited",
  sign_in_no_redirect_url: "console.signIn.no_redirect_url",

  page_admins_title: "console.page.admins_title",
  page_invite_title: "console.page.invite_title",

  /* --- generic actions ---------------------------------------------------- */
  action_copy: "console.action.copy",
  action_copied: "console.action.copied",
  action_copy_failed: "console.action.copy_failed",
  action_cancel: "console.action.cancel",

  /* --- the authenticated chrome (plan 09 wave 5) -------------------------- */
  chrome_nav_label: "console.chrome.nav_label",
  chrome_nav_home: "console.chrome.nav_home",
  chrome_nav_admins: "console.chrome.nav_admins",
  chrome_sign_out: "console.chrome.sign_out",
  chrome_sign_out_pending: "console.chrome.sign_out_pending",
  chrome_sign_out_failed: "console.chrome.sign_out_failed",

  /* --- invitation lifetimes (the ExpiryPicker molecule) ------------------- */
  expiry_label: "console.expiry.label",
  expiry_hint: "console.expiry.hint",
  expiry_option_one_hour: "console.expiry.option_one_hour",
  expiry_option_hours: "console.expiry.option_hours",

  /* --- the /admins screen ------------------------------------------------- */
  admins_heading: "console.admins.heading",
  admins_intro: "console.admins.intro",
  admins_per_grant_note: "console.admins.per_grant_note",
  admins_table_label: "console.admins.table_label",
  admins_column_email: "console.admins.column_email",
  admins_column_status: "console.admins.column_status",
  admins_column_created: "console.admins.column_created",
  admins_column_actions: "console.admins.column_actions",
  admins_owner_badge: "console.admins.owner_badge",
  admins_status_active: "console.admins.status_active",
  admins_status_revoked: "console.admins.status_revoked",
  admins_empty: "console.admins.empty",
  admins_activity_label: "console.admins.activity_label",
  admins_working: "console.admins.working",
  admins_request_failed: "console.admins.request_failed",
  admins_transfer: "console.admins.transfer",
  admins_transfer_confirm_title: "console.admins.transfer_confirm_title",
  admins_transfer_confirm_body: "console.admins.transfer_confirm_body",
  admins_transfer_confirm_action: "console.admins.transfer_confirm_action",
  admins_revoke: "console.admins.revoke",
  admins_revoke_confirm_title: "console.admins.revoke_confirm_title",
  admins_revoke_confirm_body: "console.admins.revoke_confirm_body",
  admins_revoke_confirm_action: "console.admins.revoke_confirm_action",
  admins_owner_not_revocable: "console.admins.owner_not_revocable",

  /* --- the invite form ---------------------------------------------------- */
  admins_invite_heading: "console.admins.invite_heading",
  admins_invite_constraint_label: "console.admins.invite_constraint_label",
  admins_invite_constraint_email: "console.admins.invite_constraint_email",
  admins_invite_constraint_domain: "console.admins.invite_constraint_domain",
  admins_invite_value_label_email: "console.admins.invite_value_label_email",
  admins_invite_value_label_domain: "console.admins.invite_value_label_domain",
  admins_invite_value_hint_email: "console.admins.invite_value_hint_email",
  admins_invite_value_hint_domain: "console.admins.invite_value_hint_domain",
  admins_invite_value_required: "console.admins.invite_value_required",
  admins_invite_submit: "console.admins.invite_submit",
  admins_invite_pending: "console.admins.invite_pending",
  admins_invite_domain_not_in_allow_list: "console.admins.invite_domain_not_in_allow_list",
  admins_invite_no_enabled_provider: "console.admins.invite_no_enabled_provider",
  admins_invite_multi_provider_warning: "console.admins.invite_multi_provider_warning",

  /* --- the invitation list ------------------------------------------------ */
  admins_invites_heading: "console.admins.invites_heading",
  admins_invites_table_label: "console.admins.invites_table_label",
  admins_invites_empty: "console.admins.invites_empty",
  admins_invites_privacy_note: "console.admins.invites_privacy_note",
  admins_invite_column_value: "console.admins.invite_column_value",
  admins_invite_column_status: "console.admins.invite_column_status",
  admins_invite_column_expires: "console.admins.invite_column_expires",
  admins_invite_status_pending: "console.admins.invite_status_pending",
  admins_invite_status_consumed: "console.admins.invite_status_consumed",
  admins_invite_status_revoked: "console.admins.invite_status_revoked",
  admins_invite_status_expired: "console.admins.invite_status_expired",
  admins_invite_revoke: "console.admins.invite_revoke",
  admins_invite_revoke_confirm_title: "console.admins.invite_revoke_confirm_title",
  admins_invite_revoke_confirm_body: "console.admins.invite_revoke_confirm_body",
  admins_invite_revoke_confirm_action: "console.admins.invite_revoke_confirm_action",

  /* --- the public /invite/[token] page ------------------------------------ */
  invite_panel_label: "console.invite.panel_label",
  invite_heading_email: "console.invite.heading_email",
  invite_heading_domain: "console.invite.heading_domain",
  invite_expires_at: "console.invite.expires_at",
  invite_sign_in_first: "console.invite.sign_in_first",
  invite_accept: "console.invite.accept",
  invite_accept_pending: "console.invite.accept_pending",
  invite_accepted: "console.invite.accepted",
  invite_request_failed: "console.invite.request_failed",
  invite_unusable_heading: "console.invite.unusable_heading",
  invite_domain_not_allowed: "console.invite.domain_not_allowed",
  invite_already_claimed: "console.invite.already_claimed",

  /* --- the once-only secret surface --------------------------------------- */
  secret_modal_heading: "console.secret.modal_heading",
  secret_shown_once: "console.secret.shown_once",
  secret_token_label: "console.secret.token_label",
  secret_link_label: "console.secret.link_label",
  secret_dismiss: "console.secret.dismiss",
  secret_already_shown: "console.secret.already_shown",
  secret_expires_at: "console.secret.expires_at",
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
