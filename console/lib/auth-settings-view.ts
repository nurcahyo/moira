// The CLIENT-SAFE view model for `/settings/auth`.
//
// Same split, same reason, as `lib/llm-view.ts` and `lib/keys-view.ts`: the
// orchestration module imports the Moira client and the secret store, both
// server-only, so a `"use client"` organism cannot import it even for a type.
//
// ============================================================================
// WHAT IS DELIBERATELY ABSENT
// ============================================================================
//
// The client secret, in every form — plaintext, sealed, masked, or a length.
// `SealedClientSecret.clientId` and `updatedAt` are here because they are stored
// in the clear precisely so drift can be SHOWN without decrypting anything
// (`lib/console-secrets.ts`), and because "a secret is stored, sealed against
// client id X" is the one sentence that lets an operator tell a working
// deployment from a broken one before they touch anything.
//
// `hasSealedEnvelope` is a boolean and not the envelope: a component needs to
// know whether one exists, never what it contains.

/** The live provider row, narrowed to what the screen renders. */
export interface AuthProviderSummary {
  readonly id: string;
  readonly displayName: string;
  /** `google_oauth` / `generic_oidc` / … . Rendered, never edited here. */
  readonly method: string;
  readonly clientId: string | null;
  readonly discoveryUrl: string | null;
  readonly issuer: string | null;
  readonly authorizationUrl: string | null;
  readonly tokenUrl: string | null;
  readonly allowedEmailDomains: readonly string[];
  readonly enabled: boolean;
  /** Rendered nowhere; carried so a save can send a fresh `If-Match`. */
  readonly version: number;
}

export interface AuthSettingsView {
  /**
   * The provider the CALLER'S OWN SESSION was established through, or `null`
   * when it could not be read.
   *
   * Not "the deployment's provider" and not a row named by a request: the screen
   * edits the authenticator the operator is standing on, which is what makes
   * every save something they can verify by signing in again.
   */
  readonly provider: AuthProviderSummary | null;
  /** The client id this console's stored secret is sealed against. */
  readonly sealedAgainstClientId: string | null;
  /** When that seal was written. */
  readonly sealedAt: string | null;
  readonly hasSealedEnvelope: boolean;
  /**
   * A catalog key describing how Moira's row and this console's seal disagree,
   * or `null` when they agree.
   *
   * From `CONSOLE_SECRET_DRIFT_MESSAGE_KEYS`, which has had no caller on any
   * write path until this screen — the drift states were classifiable and
   * unshown.
   */
  readonly driftKey: string | null;
  /**
   * Whether the signed-in operator is the owner.
   *
   * A pre-check for rendering only. Moira gates the write surface on the same
   * question (`require_primary_actor`), so a `true` here that Moira disagrees
   * with costs a 403, not an unauthorised write.
   */
  readonly isOwner: boolean;
}
