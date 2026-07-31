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

/** Moira notices the console renders through `t()`. */
export const MOIRA_NOTICE_NAMES = ["admin_identity_claimed"] as const;

/** Every Moira key the console mirrors, fully qualified. */
export const MIRRORED_MOIRA_KEYS: readonly string[] = [
  ...MOIRA_SETUP_ERROR_CODES.map(moiraErrorKey),
  ...MOIRA_NOTICE_NAMES.map(moiraNoticeKey),
];
