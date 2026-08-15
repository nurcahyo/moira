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
//   console.llm.*     the /settings/llm screen — providers, models, routing
//   console.graph.*   the /graph screen — the derived relationship graph (plan 12 §4)

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
  auth_config_stale: "console.error.auth_config_stale",

  /* --- app/api/auth/[...all]/route.ts -------------------------------------- */
  auth_provider_unreachable: "console.error.auth_provider_unreachable",

  /* --- lib/moira-session.ts ----------------------------------------------- */
  session_required: "console.error.session_required",
  email_not_verified: "console.error.email_not_verified",
  email_domain_not_allowed: "console.error.email_domain_not_allowed",
  idp_subject_missing: "console.error.idp_subject_missing",
  session_provider_unknown: "console.error.session_provider_unknown",

  /* --- lib/setup-flow.ts -------------------------------------------------- */
  trusted_jwt_issuer_registration_failed: "console.error.trusted_jwt_issuer_registration_failed",
  auth_provider_create_failed: "console.error.auth_provider_create_failed",
  auth_provider_update_failed: "console.error.auth_provider_update_failed",
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
  setup_resume_state_conflict: "console.error.setup_resume_state_conflict",
  setup_ordering_violated: "console.error.setup_ordering_violated",
  setup_claim_step_unreachable: "console.error.setup_claim_step_unreachable",
  setup_email_not_verified: "console.error.setup_email_not_verified",
  setup_claim_domain_not_allowed: "console.error.setup_claim_domain_not_allowed",
  setup_claim_issuer_mismatch: "console.error.setup_claim_issuer_mismatch",
  setup_enabled_provider_requires_session: "console.error.setup_enabled_provider_requires_session",
  setup_enabled_provider_session_mismatch: "console.error.setup_enabled_provider_session_mismatch",
  setup_single_enabled_provider_only: "console.error.setup_single_enabled_provider_only",
  setup_provider_enabled_mid_save: "console.error.setup_provider_enabled_mid_save",

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
  sign_in_go_to_setup: "console.signIn.go_to_setup",
  sign_in_request_failed: "console.signIn.request_failed",
  sign_in_rate_limited: "console.signIn.rate_limited",
  sign_in_no_redirect_url: "console.signIn.no_redirect_url",

  page_admins_title: "console.page.admins_title",
  page_invite_title: "console.page.invite_title",
  page_graph_title: "console.page.graph_title",

  /* --- generic actions ---------------------------------------------------- */
  action_copy: "console.action.copy",
  action_copied: "console.action.copied",
  action_copy_failed: "console.action.copy_failed",
  action_cancel: "console.action.cancel",

  /* --- the authenticated chrome (plan 09 wave 5) -------------------------- */
  chrome_nav_label: "console.chrome.nav_label",
  chrome_nav_home: "console.chrome.nav_home",
  chrome_nav_admins: "console.chrome.nav_admins",
  chrome_nav_llm_settings: "console.chrome.nav_llm_settings",
  chrome_nav_graph: "console.chrome.nav_graph",
  chrome_nav_keys: "console.chrome.nav_keys",
  chrome_nav_auth_settings: "console.chrome.nav_auth_settings",
  chrome_nav_skills: "console.chrome.nav_skills",
  chrome_nav_provider_health: "console.chrome.nav_provider_health",
  chrome_nav_evals: "console.chrome.nav_evals",
  chrome_nav_flows: "console.chrome.nav_flows",
  chrome_sign_out: "console.chrome.sign_out",
  chrome_sign_out_pending: "console.chrome.sign_out_pending",
  chrome_sign_out_failed: "console.chrome.sign_out_failed",

  /* --- the /settings/keys screen (issue #180) ----------------------------- */
  keys_page_title: "console.keys.page_title",
  keys_page_intro: "console.keys.page_intro",
  keys_applications_heading: "console.keys.applications_heading",
  keys_applications_empty: "console.keys.applications_empty",
  keys_add_application_heading: "console.keys.add_application_heading",
  keys_application_name_label: "console.keys.application_name_label",
  keys_application_name_hint: "console.keys.application_name_hint",
  keys_application_slug_label: "console.keys.application_slug_label",
  keys_application_slug_hint: "console.keys.application_slug_hint",
  keys_create_application_button: "console.keys.create_application_button",
  keys_issued_heading: "console.keys.issued_heading",
  keys_issued_empty: "console.keys.issued_empty",
  keys_prefix_label: "console.keys.prefix_label",
  keys_scopes_label: "console.keys.scopes_label",
  keys_last_used_label: "console.keys.last_used_label",
  keys_never_used: "console.keys.never_used",
  keys_expires_label: "console.keys.expires_label",
  keys_expires_never: "console.keys.expires_never",
  keys_status_active: "console.keys.status_active",
  keys_status_revoked: "console.keys.status_revoked",
  keys_status_expired: "console.keys.status_expired",
  keys_status_deleted: "console.keys.status_deleted",
  keys_mint_heading: "console.keys.mint_heading",
  keys_key_name_label: "console.keys.key_name_label",
  keys_key_name_hint: "console.keys.key_name_hint",
  keys_scopes_hint: "console.keys.scopes_hint",
  keys_mint_button: "console.keys.mint_button",
  keys_revoke_button: "console.keys.revoke_button",
  keys_revoke_pending: "console.keys.revoke_pending",
  keys_revoke_confirm_body: "console.keys.revoke_confirm_body",
  keys_unattached_heading: "console.keys.unattached_heading",
  keys_unattached_intro: "console.keys.unattached_intro",
  keys_truncated_notice: "console.keys.truncated_notice",
  keys_request_body_invalid: "console.keys.request_body_invalid",
  keys_application_required: "console.keys.application_required",
  keys_display_name_required: "console.keys.display_name_required",
  keys_request_failed: "console.keys.request_failed",
  keys_load_failed: "console.keys.load_failed",
  keys_secret_heading: "console.keys.secret_heading",
  keys_secret_notice: "console.keys.secret_notice",
  keys_scope_responses_create: "console.keys.scope_responses_create",
  keys_scope_responses_stream: "console.keys.scope_responses_stream",
  keys_scope_responses_read: "console.keys.scope_responses_read",
  keys_scope_conversations_create: "console.keys.scope_conversations_create",
  keys_scope_conversations_read: "console.keys.scope_conversations_read",
  keys_scope_conversations_write: "console.keys.scope_conversations_write",
  keys_scope_memories_create: "console.keys.scope_memories_create",
  keys_scope_memories_read: "console.keys.scope_memories_read",
  keys_scope_rag_collections_read: "console.keys.scope_rag_collections_read",
  keys_scope_rag_documents_read: "console.keys.scope_rag_documents_read",
  keys_scope_usage_read: "console.keys.scope_usage_read",

  /* --- the /settings/auth screen (issue #185) ------------------------------ */
  authsettings_page_title: "console.authSettings.page_title",
  authsettings_page_intro: "console.authSettings.page_intro",
  authsettings_not_owner: "console.authSettings.not_owner",
  authsettings_owner_grant_absent: "console.authSettings.owner_grant_absent",
  authsettings_owner_lookup_truncated: "console.authSettings.owner_lookup_truncated",
  authsettings_current_heading: "console.authSettings.current_heading",
  authsettings_secret_hint: "console.authSettings.secret_hint",
  authsettings_secret_required_for_new_client_id:
    "console.authSettings.secret_required_for_new_client_id",
  authsettings_sealed_against: "console.authSettings.sealed_against",
  authsettings_sealed_absent: "console.authSettings.sealed_absent",
  authsettings_update_heading: "console.authSettings.update_heading",
  authsettings_update_button: "console.authSettings.update_button",
  authsettings_rotate_heading: "console.authSettings.rotate_heading",
  authsettings_rotate_intro: "console.authSettings.rotate_intro",
  authsettings_rotate_button: "console.authSettings.rotate_button",
  authsettings_saved: "console.authSettings.saved",
  authsettings_drift_after_write: "console.authSettings.drift_after_write",
  authsettings_request_body_invalid: "console.authSettings.request_body_invalid",
  authsettings_client_id_required: "console.authSettings.client_id_required",
  authsettings_domains_required: "console.authSettings.domains_required",
  authsettings_secret_required: "console.authSettings.secret_required",
  authsettings_no_provider: "console.authSettings.no_provider",
  authsettings_load_failed: "console.authSettings.load_failed",
  authsettings_request_failed: "console.authSettings.request_failed",

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

  /* --- the /setup wizard (modules/setup/** + app/setup/**) -----------------
         The ONE item that owns the `console.setup.*` namespace. The BFF setup
         door owns only `console.error.setup_*` above — wizard UI copy is here. */
  setup_page_title: "console.setup.page_title",
  setup_unavailable_heading: "console.setup.unavailable_heading",
  setup_steps_label: "console.setup.steps_label",
  setup_step_welcome: "console.setup.step_welcome",
  setup_step_auth_settings: "console.setup.step_auth_settings",
  setup_step_sign_in: "console.setup.step_sign_in",
  setup_step_claim: "console.setup.step_claim",
  setup_step_done: "console.setup.step_done",
  setup_welcome_heading: "console.setup.welcome_heading",
  setup_welcome_claim_once: "console.setup.welcome_claim_once",
  setup_welcome_provider_first: "console.setup.welcome_provider_first",
  setup_welcome_continue: "console.setup.welcome_continue",
  setup_auth_heading: "console.setup.auth_heading",
  setup_auth_existing_heading: "console.setup.auth_existing_heading",
  setup_auth_existing_configured: "console.setup.auth_existing_configured",
  setup_auth_method_label: "console.setup.auth_method_label",
  setup_auth_method_google: "console.setup.auth_method_google",
  setup_auth_method_generic: "console.setup.auth_method_generic",
  setup_auth_slug_label: "console.setup.auth_slug_label",
  setup_auth_slug_hint: "console.setup.auth_slug_hint",
  setup_auth_display_name_label: "console.setup.auth_display_name_label",
  setup_auth_client_id_label: "console.setup.auth_client_id_label",
  setup_auth_client_secret_label: "console.setup.auth_client_secret_label",
  setup_auth_client_secret_hint: "console.setup.auth_client_secret_hint",
  setup_auth_discovery_url_label: "console.setup.auth_discovery_url_label",
  setup_auth_issuer_label: "console.setup.auth_issuer_label",
  setup_auth_authorization_url_label: "console.setup.auth_authorization_url_label",
  setup_auth_token_url_label: "console.setup.auth_token_url_label",
  setup_auth_allowed_domains_label: "console.setup.auth_allowed_domains_label",
  setup_auth_allowed_domains_hint: "console.setup.auth_allowed_domains_hint",
  setup_auth_form_incomplete: "console.setup.auth_form_incomplete",
  setup_auth_submit: "console.setup.auth_submit",
  setup_auth_pending: "console.setup.auth_pending",
  setup_auth_retry: "console.setup.auth_retry",
  setup_auth_discard: "console.setup.auth_discard",
  setup_auth_failure_region: "console.setup.auth_failure_region",
  setup_auth_not_complete: "console.setup.auth_not_complete",
  setup_request_unreachable: "console.setup.request_unreachable",
  setup_sign_in_heading: "console.setup.sign_in_heading",
  setup_sign_in_intro: "console.setup.sign_in_intro",
  setup_sign_in_edit_settings: "console.setup.sign_in_edit_settings",
  setup_claim_heading: "console.setup.claim_heading",
  setup_claim_button: "console.setup.claim_button",
  setup_claim_pending: "console.setup.claim_pending",
  setup_claim_signed_in_as: "console.setup.claim_signed_in_as",
  setup_domain_not_allowed_title: "console.setup.domain_not_allowed.title",
  setup_domain_not_allowed_body: "console.setup.domain_not_allowed.body",
  setup_domain_not_allowed_action: "console.setup.domain_not_allowed.action",
  setup_done_heading: "console.setup.done_heading",
  setup_done_admin_email: "console.setup.done_admin_email",
  setup_done_open_console: "console.setup.done_open_console",

  /* --- the once-only secret surface --------------------------------------- */
  secret_modal_heading: "console.secret.modal_heading",
  secret_shown_once: "console.secret.shown_once",
  secret_key_label: "console.secret.key_label",
  secret_no_expiry: "console.secret.no_expiry",
  secret_token_label: "console.secret.token_label",
  secret_link_label: "console.secret.link_label",
  secret_dismiss: "console.secret.dismiss",
  secret_already_shown: "console.secret.already_shown",
  secret_expires_at: "console.secret.expires_at",

  /* --- the /settings/llm screen (issue #74) ------------------------------- */
  //
  // A NAMESPACE OF ITS OWN. `console.setup.*` belongs to the first-run wizard and
  // is being rewritten on another branch; LLM configuration is ordinary
  // administration that happens long after setup, so it takes `console.llm.*`
  // and the two catalogs never touch the same lines.

  /* --- the page itself --------------------------------------------------- */
  llm_page_title: "console.llm.page_title",
  llm_page_intro: "console.llm.page_intro",
  llm_load_failed: "console.llm.load_failed",
  llm_request_failed: "console.llm.request_failed",
  llm_request_body_invalid: "console.llm.request_body_invalid",
  llm_action_unknown: "console.llm.action_unknown",
  llm_general_route_missing: "console.llm.general_route_missing",
  llm_list_truncated: "console.llm.list_truncated",

  /* --- the provider list ------------------------------------------------- */
  llm_providers_heading: "console.llm.providers_heading",
  llm_providers_empty: "console.llm.providers_empty",
  llm_status_active: "console.llm.status_active",
  llm_status_disabled: "console.llm.status_disabled",
  llm_models_heading: "console.llm.models_heading",
  llm_models_empty: "console.llm.models_empty",
  llm_key_rows_heading: "console.llm.key_rows_heading",
  llm_key_row_present: "console.llm.key_row_present",
  llm_key_row_missing: "console.llm.key_row_missing",
  llm_routing_heading: "console.llm.routing_heading",
  llm_policy_present: "console.llm.policy_present",
  llm_policy_missing: "console.llm.policy_missing",
  llm_disable_provider: "console.llm.disable_provider",
  llm_disable_model: "console.llm.disable_model",
  llm_disable_key_row: "console.llm.disable_key_row",
  llm_disable_policy: "console.llm.disable_policy",
  llm_enable_model: "console.llm.enable_model",
  llm_enable_key_row: "console.llm.enable_key_row",
  llm_enable_policy: "console.llm.enable_policy",

  /* --- adding a provider by hand ----------------------------------------- */
  llm_add_provider_heading: "console.llm.add_provider_heading",
  llm_provider_name_label: "console.llm.provider_name_label",
  llm_provider_name_hint: "console.llm.provider_name_hint",
  llm_provider_base_url_label: "console.llm.provider_base_url_label",
  llm_provider_base_url_hint: "console.llm.provider_base_url_hint",
  llm_add_provider_submit: "console.llm.add_provider_submit",
  llm_provider_created: "console.llm.provider_created",
  llm_display_name_required: "console.llm.display_name_required",
  llm_base_url_required: "console.llm.base_url_required",
  llm_base_url_invalid: "console.llm.base_url_invalid",
  llm_base_url_scheme_unsupported: "console.llm.base_url_scheme_unsupported",
  llm_base_url_userinfo_rejected: "console.llm.base_url_userinfo_rejected",

  /* --- finishing the chain: model, credential row, routing --------------- */
  llm_chain_heading: "console.llm.chain_heading",
  llm_chain_complete: "console.llm.chain_complete",
  llm_chain_incomplete: "console.llm.chain_incomplete",
  llm_step_model_missing: "console.llm.step_model_missing",
  llm_step_enable_missing: "console.llm.step_enable_missing",
  llm_add_model_label: "console.llm.add_model_label",
  llm_add_model_hint: "console.llm.add_model_hint",
  llm_add_model_submit: "console.llm.add_model_submit",
  llm_model_key_required: "console.llm.model_key_required",
  llm_model_required: "console.llm.model_required",
  llm_model_not_found: "console.llm.model_not_found",
  llm_model_not_selectable: "console.llm.model_not_selectable",
  llm_key_label: "console.llm.key_label",
  llm_add_key_row_hint: "console.llm.add_key_row_hint",
  llm_add_key_row_submit: "console.llm.add_key_row_submit",
  llm_key_row_not_found: "console.llm.key_row_not_found",
  llm_bind_routing_model_label: "console.llm.bind_routing_model_label",
  llm_bind_routing_no_model: "console.llm.bind_routing_no_model",
  llm_bind_routing_submit: "console.llm.bind_routing_submit",
  llm_policy_not_found: "console.llm.policy_not_found",

  /* --- the Connect-local-endpoint shortcut ------------------------------- */
  llm_connect_heading: "console.llm.connect_heading",
  llm_connect_intro: "console.llm.connect_intro",
  llm_connect_endpoint_label: "console.llm.connect_endpoint_label",
  llm_connect_discover_submit: "console.llm.connect_discover_submit",
  llm_connect_discovered_heading: "console.llm.connect_discovered_heading",
  llm_connect_submit: "console.llm.connect_submit",
  llm_connect_pending: "console.llm.connect_pending",
  llm_connect_done: "console.llm.connect_done",
  llm_connect_step_failed: "console.llm.connect_step_failed",
  llm_discovery_unreachable: "console.llm.discovery_unreachable",
  llm_discovery_refused: "console.llm.discovery_refused",
  llm_discovery_response_too_large: "console.llm.discovery_response_too_large",
  llm_discovery_invalid_response: "console.llm.discovery_invalid_response",

  /* --- the chain trace --------------------------------------------------- */
  llm_trace_heading: "console.llm.trace_heading",
  llm_step_provider: "console.llm.step_provider",
  llm_step_provider_model: "console.llm.step_provider_model",
  llm_step_provider_credential: "console.llm.step_provider_credential",
  llm_step_provider_enable: "console.llm.step_provider_enable",
  llm_step_routing_policy: "console.llm.step_routing_policy",
  llm_step_unknown: "console.llm.step_unknown",
  llm_outcome_created: "console.llm.outcome_created",
  llm_outcome_reused: "console.llm.outcome_reused",
  llm_outcome_enabled: "console.llm.outcome_enabled",
  llm_outcome_skipped: "console.llm.outcome_skipped",

  /* --- the "Connect Claude subscription" panel (issue #211) --------------- */
  claude_subscription_heading: "console.claudeSubscription.heading",
  claude_subscription_intro: "console.claudeSubscription.intro",
  claude_subscription_token_label: "console.claudeSubscription.token_label",
  claude_subscription_token_hint: "console.claudeSubscription.token_hint",
  claude_subscription_submit: "console.claudeSubscription.submit",
  claude_subscription_pending: "console.claudeSubscription.pending",
  claude_subscription_created: "console.claudeSubscription.created",
  claude_subscription_rotated: "console.claudeSubscription.rotated",
  claude_subscription_token_required: "console.claudeSubscription.token_required",
  claude_subscription_token_too_long: "console.claudeSubscription.token_too_long",
  claude_subscription_token_invalid: "console.claudeSubscription.token_invalid",
  claude_subscription_request_body_invalid: "console.claudeSubscription.request_body_invalid",
  claude_subscription_list_truncated: "console.claudeSubscription.list_truncated",

  /* --- the /graph screen (plan 12 §4, issue #234) -------------------------- */
  graph_intro: "console.graph.intro",
  graph_request_failed: "console.graph.request_failed",
  graph_empty: "console.graph.empty",
  graph_legend_label: "console.graph.legend_label",
  graph_node_type_agent: "console.graph.node_type_agent",
  graph_node_type_skill: "console.graph.node_type_skill",
  graph_node_type_eval_suite: "console.graph.node_type_eval_suite",
  graph_node_type_flow: "console.graph.node_type_flow",
  graph_node_type_provider: "console.graph.node_type_provider",
  graph_node_type_model: "console.graph.node_type_model",
  graph_node_type_memory_scope: "console.graph.node_type_memory_scope",
  graph_canvas_label: "console.graph.canvas_label",

  /* --- the /skills screen (plan 12 §5, issue #237) ------------------------ */
  page_skills_title: "console.page.skills_title",
  skills_page_intro: "console.skills.page_intro",
  skills_load_failed: "console.skills.load_failed",
  skills_request_failed: "console.skills.request_failed",
  skills_request_body_invalid: "console.skills.request_body_invalid",
  skills_skill_key_required: "console.skills.skill_key_required",
  skills_display_name_required: "console.skills.display_name_required",
  skills_kind_required: "console.skills.kind_required",
  skills_bulk_enable_empty: "console.skills.bulk_enable_empty",
  skills_import_document_required: "console.skills.import_document_required",
  skills_executor_method_invalid: "console.skills.executor_method_invalid",
  skills_executor_url_required: "console.skills.executor_url_required",
  skills_executor_timeout_invalid: "console.skills.executor_timeout_invalid",

  skills_list_heading: "console.skills.list_heading",
  skills_list_empty: "console.skills.list_empty",
  skills_kind_tool: "console.skills.kind_tool",
  skills_kind_guard: "console.skills.kind_guard",
  skills_status_draft: "console.skills.status_draft",
  skills_status_enabled: "console.skills.status_enabled",
  skills_status_disabled: "console.skills.status_disabled",
  skills_no_description: "console.skills.no_description",
  skills_tags_none: "console.skills.tags_none",
  skills_select_for_bulk_enable: "console.skills.select_for_bulk_enable",
  skills_enable: "console.skills.enable",
  skills_disable: "console.skills.disable",
  skills_edit: "console.skills.edit",
  skills_edit_cancel: "console.skills.edit_cancel",
  skills_edit_save: "console.skills.edit_save",
  skills_edit_saved: "console.skills.edit_saved",
  skills_delete: "console.skills.delete",
  skills_delete_confirm_title: "console.skills.delete_confirm_title",
  skills_delete_confirm_body: "console.skills.delete_confirm_body",
  skills_delete_confirm_action: "console.skills.delete_confirm_action",
  skills_bulk_enable_button: "console.skills.bulk_enable_button",
  skills_bulk_enable_none_selected: "console.skills.bulk_enable_none_selected",
  skills_bulk_enable_done: "console.skills.bulk_enable_done",

  skills_field_skill_key_label: "console.skills.field_skill_key_label",
  skills_field_skill_key_hint: "console.skills.field_skill_key_hint",
  skills_field_display_name_label: "console.skills.field_display_name_label",
  skills_field_kind_label: "console.skills.field_kind_label",
  skills_field_description_label: "console.skills.field_description_label",
  skills_field_tags_label: "console.skills.field_tags_label",
  skills_field_tags_hint: "console.skills.field_tags_hint",

  skills_create_heading: "console.skills.create_heading",
  skills_create_submit: "console.skills.create_submit",
  skills_create_success: "console.skills.create_success",

  skills_import_heading: "console.skills.import_heading",
  skills_import_intro: "console.skills.import_intro",
  skills_import_field_label: "console.skills.import_field_label",
  skills_import_field_hint: "console.skills.import_field_hint",
  skills_import_submit: "console.skills.import_submit",
  skills_import_invalid_json: "console.skills.import_invalid_json",
  skills_import_success: "console.skills.import_success",
  skills_import_results_heading: "console.skills.import_results_heading",

  skills_executor_heading: "console.skills.executor_heading",
  skills_executor_show: "console.skills.executor_show",
  skills_executor_hide: "console.skills.executor_hide",
  skills_executor_loading: "console.skills.executor_loading",
  skills_executor_none: "console.skills.executor_none",
  skills_executor_load_failed: "console.skills.executor_load_failed",
  skills_executor_allowed_host_label: "console.skills.executor_allowed_host_label",
  skills_executor_method_label: "console.skills.executor_method_label",
  skills_executor_url_label: "console.skills.executor_url_label",
  skills_executor_timeout_label: "console.skills.executor_timeout_label",
  skills_executor_key_row_label: "console.skills.executor_key_row_label",
  skills_executor_key_row_hint: "console.skills.executor_key_row_hint",
  skills_executor_save: "console.skills.executor_save",
  skills_executor_saved: "console.skills.executor_saved",
  skills_executor_delete: "console.skills.executor_delete",
  skills_executor_delete_confirm_title: "console.skills.executor_delete_confirm_title",
  skills_executor_delete_confirm_body: "console.skills.executor_delete_confirm_body",
  skills_executor_delete_confirm_action: "console.skills.executor_delete_confirm_action",
  skills_executor_deleted: "console.skills.executor_deleted",

  /* --- the /providers/health screen (issue #83) --------------------------- */
  page_provider_health_title: "console.page.provider_health_title",
  providerhealth_page_intro: "console.providerHealth.page_intro",
  providerhealth_load_failed: "console.providerHealth.load_failed",
  providerhealth_table_label: "console.providerHealth.table_label",
  providerhealth_column_provider: "console.providerHealth.column_provider",
  providerhealth_column_type: "console.providerHealth.column_type",
  providerhealth_column_status: "console.providerHealth.column_status",
  providerhealth_column_probes: "console.providerHealth.column_probes",
  providerhealth_column_latency: "console.providerHealth.column_latency",
  providerhealth_column_last_probe: "console.providerHealth.column_last_probe",
  providerhealth_column_last_success: "console.providerHealth.column_last_success",
  providerhealth_column_last_failure: "console.providerHealth.column_last_failure",
  providerhealth_status_healthy: "console.providerHealth.status_healthy",
  providerhealth_status_degraded: "console.providerHealth.status_degraded",
  providerhealth_status_unhealthy: "console.providerHealth.status_unhealthy",
  providerhealth_status_unknown: "console.providerHealth.status_unknown",
  providerhealth_empty: "console.providerHealth.empty",
  providerhealth_never: "console.providerHealth.never",
  providerhealth_latency_unknown: "console.providerHealth.latency_unknown",

  /* --- the /evals screen (plan 12 §3) -------------------------------------- */
  page_evals_title: "console.page.evals_title",
  evals_page_intro: "console.evals.page_intro",
  evals_load_failed: "console.evals.load_failed",
  evals_request_failed: "console.evals.request_failed",
  evals_request_body_invalid: "console.evals.request_body_invalid",
  evals_suite_key_required: "console.evals.suite_key_required",
  evals_display_name_required: "console.evals.display_name_required",
  evals_grading_kind_required: "console.evals.grading_kind_required",
  evals_case_input_required: "console.evals.case_input_required",
  evals_case_expected_required: "console.evals.case_expected_required",
  evals_run_not_available: "console.evals.run_not_available",

  evals_suites_heading: "console.evals.suites_heading",
  evals_suites_empty: "console.evals.suites_empty",
  evals_status_active: "console.evals.status_active",
  evals_status_inactive: "console.evals.status_inactive",
  evals_expand: "console.evals.expand",
  evals_collapse: "console.evals.collapse",
  evals_edit: "console.evals.edit",
  evals_edit_cancel: "console.evals.edit_cancel",
  evals_edit_save: "console.evals.edit_save",
  evals_edit_saved: "console.evals.edit_saved",
  evals_delete: "console.evals.delete",
  evals_delete_confirm_title: "console.evals.delete_confirm_title",
  evals_delete_confirm_body: "console.evals.delete_confirm_body",
  evals_delete_confirm_action: "console.evals.delete_confirm_action",

  evals_field_suite_key_label: "console.evals.field_suite_key_label",
  evals_field_suite_key_hint: "console.evals.field_suite_key_hint",
  evals_field_display_name_label: "console.evals.field_display_name_label",
  evals_field_description_label: "console.evals.field_description_label",
  evals_create_heading: "console.evals.create_heading",
  evals_create_submit: "console.evals.create_submit",
  evals_create_success: "console.evals.create_success",

  evals_cases_heading: "console.evals.cases_heading",
  evals_cases_empty: "console.evals.cases_empty",
  evals_case_grading_label: "console.evals.case_grading_label",
  evals_case_input_label: "console.evals.case_input_label",
  evals_case_input_hint: "console.evals.case_input_hint",
  evals_case_expected_label: "console.evals.case_expected_label",
  evals_case_expected_hint: "console.evals.case_expected_hint",
  evals_case_add_submit: "console.evals.case_add_submit",
  evals_case_added: "console.evals.case_added",
  evals_case_invalid_json: "console.evals.case_invalid_json",
  evals_case_delete: "console.evals.case_delete",
  evals_grading_exact_match: "console.evals.grading_exact_match",
  evals_grading_contains: "console.evals.grading_contains",
  evals_grading_schema_valid: "console.evals.grading_schema_valid",

  evals_runs_heading: "console.evals.runs_heading",
  evals_runs_empty: "console.evals.runs_empty",
  evals_run_trigger: "console.evals.run_trigger",
  evals_run_status_pending: "console.evals.run_status_pending",
  evals_run_status_running: "console.evals.run_status_running",
  evals_run_status_completed: "console.evals.run_status_completed",
  evals_run_status_failed: "console.evals.run_status_failed",
  evals_run_score_label: "console.evals.run_score_label",
  evals_run_score_none: "console.evals.run_score_none",
  evals_run_created_label: "console.evals.run_created_label",

  /* --- the /flows screen (plan 12 §6) -------------------------------------- */
  page_flows_title: "console.page.flows_title",
  flows_page_intro: "console.flows.page_intro",
  flows_load_failed: "console.flows.load_failed",
  flows_request_failed: "console.flows.request_failed",
  flows_request_body_invalid: "console.flows.request_body_invalid",
  flows_flow_key_required: "console.flows.flow_key_required",
  flows_display_name_required: "console.flows.display_name_required",
  flows_steps_invalid: "console.flows.steps_invalid",
  flows_run_not_available: "console.flows.run_not_available",

  flows_list_heading: "console.flows.list_heading",
  flows_list_empty: "console.flows.list_empty",
  flows_status_active: "console.flows.status_active",
  flows_status_inactive: "console.flows.status_inactive",
  flows_expand: "console.flows.expand",
  flows_collapse: "console.flows.collapse",
  flows_edit: "console.flows.edit",
  flows_edit_cancel: "console.flows.edit_cancel",
  flows_edit_save: "console.flows.edit_save",
  flows_edit_saved: "console.flows.edit_saved",
  flows_delete: "console.flows.delete",
  flows_delete_confirm_title: "console.flows.delete_confirm_title",
  flows_delete_confirm_body: "console.flows.delete_confirm_body",
  flows_delete_confirm_action: "console.flows.delete_confirm_action",

  flows_field_flow_key_label: "console.flows.field_flow_key_label",
  flows_field_flow_key_hint: "console.flows.field_flow_key_hint",
  flows_field_display_name_label: "console.flows.field_display_name_label",
  flows_field_description_label: "console.flows.field_description_label",
  flows_create_heading: "console.flows.create_heading",
  flows_create_submit: "console.flows.create_submit",
  flows_create_success: "console.flows.create_success",

  flows_steps_heading: "console.flows.steps_heading",
  flows_steps_empty: "console.flows.steps_empty",
  flows_step_key_label: "console.flows.step_key_label",
  flows_step_order_label: "console.flows.step_order_label",
  flows_step_agent_profile_label: "console.flows.step_agent_profile_label",
  flows_step_agent_profile_none: "console.flows.step_agent_profile_none",
  flows_step_on_failure_label: "console.flows.step_on_failure_label",
  flows_on_failure_abort: "console.flows.on_failure_abort",
  flows_on_failure_continue: "console.flows.on_failure_continue",
  flows_step_add: "console.flows.step_add",
  flows_step_remove: "console.flows.step_remove",
  flows_step_agent_profiles_load_failed: "console.flows.step_agent_profiles_load_failed",

  flows_runs_heading: "console.flows.runs_heading",
  flows_runs_empty: "console.flows.runs_empty",
  flows_run_trigger: "console.flows.run_trigger",
  flows_run_status_running: "console.flows.run_status_running",
  flows_run_status_completed: "console.flows.run_status_completed",
  flows_run_status_failed: "console.flows.run_status_failed",
  flows_run_status_cancelled: "console.flows.run_status_cancelled",
  flows_run_created_label: "console.flows.run_created_label",
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
