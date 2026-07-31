// The console's mirror of the Moira i18n keys it renders.
//
// Every key here must exist in `docs/i18n-response-catalog.json`, which
// `tests/unit/lib/moira-keys.test.ts` asserts. A Moira-side rename therefore
// surfaces as a console test failure rather than as an untranslated key in
// production.
//
// This file lists ONLY keys. English copy is never duplicated here — the
// console catalog owns console-originated strings, and for server-originated
// conditions the server's own `message` is the fallback (`t(key, args, message)`).

/** `moira.error.<code>` for a Moira error code. */
export function moiraErrorKey(code: string): string {
  return `moira.error.${code}`;
}

/** `moira.notice.<name>` for a Moira notice. */
export function moiraNoticeKey(name: string): string {
  return `moira.notice.${name}`;
}

/**
 * Error codes on the setup wizard's own request paths.
 *
 * The eight marked NEW are the ones plan 08's body never mapped; several are on
 * the wizard's happy path, which is why an unmapped code would surface to an
 * operator as a bare key or a generic "something went wrong".
 */
export const MOIRA_SETUP_ERROR_CODES = [
  // --- POST /api/v1/admin/setup/claim -------------------------------------
  "unregistered_trusted_issuer", // 400
  "invalid_request", // 400
  "admin_claim_domain_not_allowed", // 403 — actionable setup instruction, not a failure
  "admin_identity_already_claimed", // 409
  "setup_token_not_supported", // 400  NEW
  "setup_claim_credential_required", // 401  NEW — bad system key, NOT a session expiry
  "admin_claim_email_required", // 400  NEW
  "admin_claim_email_not_verified", // 403  NEW
  "scope_invalid", // 422  NEW — 422, not 400

  // --- POST/PATCH /api/v1/admin/auth/providers ----------------------------
  "console_issuer_must_not_assert_scopes", // 400  NEW — reachable only once B1 lands
  "auth_provider_method_config_incomplete", // 400  NEW — also fires on `enable`
  "auth_provider_url_not_allowed", // 400  NEW
  "duplicate_auth_provider", // 409
  "auth_provider_not_found", // 404
  // Finding F23, wave 4A. Both are refusals a setup wizard can hit on its own happy path
  // once a deployment has more than one provider, and both name a remedy the operator can
  // act on — which is the whole reason they are coded rather than mapped constraint
  // violations. `duplicate_enabled_provider_for_issuer` also reaches the CLAIM path, where
  // it means the deployment is already ambiguous.
  "duplicate_enabled_provider_for_issuer", // 409  NEW
  "auth_provider_issuer_shadows_trusted_issuer", // 409  NEW

  // --- POST /api/v1/admin/jwt-issuers -------------------------------------
  "jwks_url_rejected", // 400
  // Retiring an issuer that still authorises live admins. Soft delete and disable alike;
  // the remedy is to revoke the grants first.
  "trusted_issuer_has_active_grants", // 409  NEW

  // --- shared admin-plane codes -------------------------------------------
  "unauthorized", // 401
  "forbidden", // 403
  "not_found", // 404
  "if_match_required", // 400
  "resource_version_conflict", // 409
  "idempotency_conflict", // 409
  "idempotency_in_progress", // 409
  "rate_limited", // 429
  "bad_request", // 400
  "validation_failed", // 422
  "database_unavailable", // 503
  "database_error", // 500 — what an unmapped unique violation looks like
  "internal_error", // 500
] as const;

export type MoiraSetupErrorCode = (typeof MOIRA_SETUP_ERROR_CODES)[number];

/**
 * Error codes on the invitation and ownership paths (plan 09 wave 5).
 *
 * **All eleven shipped in wave 2 and #39. Wave 5 adds none** — plan 09's body
 * lists ten of them as wave-5 deliverables, and every one already has a pinned
 * emitter and a pinned status in Moira. The eleventh, `admin_identity_not_primary`,
 * the plan never names at all, and it is the one the ownership UI must actually
 * render: it is what a non-primary admin gets from `require_primary_actor`, and
 * it is the constructible authorization-denial case (`moira:admins:manage` is not
 * a scope and never was).
 *
 * The status beside each code is Moira's, read from the constructor, not guessed —
 * they decide which remedy applies, and three of them are 403s that a status-only
 * mapping would render as a bare "denied".
 */
export const MOIRA_ADMIN_LIFECYCLE_ERROR_CODES = [
  // --- redeeming an invitation --------------------------------------------
  // 404. The same code for a wrong prefix and a wrong hash — deliberately, so
  // the endpoint is not a guessing oracle.
  "invite_not_found",
  "invite_expired", // 403
  "invite_already_consumed", // 409
  "invite_revoked", // 403
  // 403, and NEVER collapsed into `admin_claim_domain_not_allowed`: the invite's
  // own constraint failed, and the remedy is a new invitation rather than a
  // change to the deployment's allow-list.
  "invite_email_mismatch",
  "invite_domain_mismatch", // 403

  // --- creating an invitation ---------------------------------------------
  // 422. A HARD CAP, refused rather than clamped. The floor
  // (`MIN_INVITE_EXPIRY_SECONDS = 60`) is a plain `invalid_request`, also 422,
  // and is already mirrored above.
  "admin_invite_expiry_too_long",

  // --- managing grants -----------------------------------------------------
  "admin_identity_not_found", // 404
  "admin_identity_already_revoked", // 409
  // 409, decision D-F20. On a deployment with ONE admin this makes
  // `DELETE /admin-identities/{id}` permanently unavailable for that row:
  // `revoke_grant` clears `is_primary` and the last-primary guard refuses that.
  // The UI states it as a rule with a remedy ("transfer ownership first"), never
  // as a failed request.
  "admin_identity_last_primary",
  // 403. THE OWNERSHIP GATE, and it is row state rather than a scope —
  // `is_known_scope("moira:admins:manage")` is asserted FALSE in Moira. The
  // catalogue entry gives the reason in one sentence: "a scope could not express
  // 'not every admin'".
  "admin_identity_not_primary",
] as const;

export type MoiraAdminLifecycleErrorCode = (typeof MOIRA_ADMIN_LIFECYCLE_ERROR_CODES)[number];

/**
 * Every Moira error code this console mirrors.
 *
 * One list, so `tests/unit/lib/errors.test.ts`'s "every mirrored error code has
 * an explicit remedy" covers the invitation family too. Splitting the arrays and
 * leaving that test on the setup half would have been the shape this project
 * keeps rediscovering: a coverage assertion that stops covering the new thing.
 */
export const MOIRA_MIRRORED_ERROR_CODES = [
  ...MOIRA_SETUP_ERROR_CODES,
  ...MOIRA_ADMIN_LIFECYCLE_ERROR_CODES,
] as const;

export type MoiraMirroredErrorCode = (typeof MOIRA_MIRRORED_ERROR_CODES)[number];

/**
 * Moira notices the console renders through `t()`.
 *
 * `admin_identity_claimed` covers the ownership PATCH as well as the claim —
 * `set_primary` returns `record_from_grant_with_notice(grant, claimed_notice())`,
 * verified against the service rather than assumed from the operation's name.
 * There is deliberately **no** `admin_identity_ownership_transferred` notice; the
 * transfer is observable as a metric label, not as a catalogue entry.
 *
 * There is also deliberately no `admin_identity_recovered`: recovery has no
 * backend at all (decision D-W2-1), and `src/i18n/catalog/notices.rs` says so in
 * as many words. Adding the key here would fail `moira-keys.test.ts`, which is
 * the correct outcome.
 */
export const MOIRA_NOTICE_NAMES = [
  "admin_identity_claimed",
  "admin_invite_created",
  "admin_invite_redeemed",
  "admin_identity_revoked",
] as const;

/** Every Moira key the console mirrors, fully qualified. */
export const MIRRORED_MOIRA_KEYS: readonly string[] = [
  ...MOIRA_MIRRORED_ERROR_CODES.map(moiraErrorKey),
  ...MOIRA_NOTICE_NAMES.map(moiraNoticeKey),
];
