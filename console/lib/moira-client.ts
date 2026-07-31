// @server-only
//
// Typed client for the Moira admin endpoints the setup wizard needs.
//
// This module carries the bootstrap system key. It must never be imported from a
// client component. Two independent mechanisms enforce that:
//
//   * `import "server-only"` below — a build-time guard. Next.js compiles server
//     code with the `react-server` export condition, which resolves that package
//     to an empty module; a browser bundle resolves the `default` condition,
//     which is a bare `throw`, and `next build` fails.
//   * `tests/unit/architecture/server-only-guards.test.ts` — a static scan that
//     catches what the build guard cannot: an import that is server-legal but
//     architecturally wrong.
//
// DESIGN NOTE — the operation registry is the point.
//
// Every request goes through `MOIRA_OPERATIONS`, a table transcribed from
// `docs/openapi.json`. It records, per operation, which credential the spec
// declares, whether `Idempotency-Key` is declared, and whether `If-Match` is
// required. `request()` reads only that table; no method sets a header directly.
// Consequences that would otherwise be review opinions become mechanical:
//
//   * `Idempotency-Key` is attached only where the spec declares it. Of the ten
//     operations the console binds to, exactly TWO declare it — `POST /setup/claim`
//     and `POST /auth/providers`. `enable` does not, so the wizard's commit step
//     gets its retry safety from `If-Match` plus `enable` being naturally
//     idempotent, not from a key. Passing a key to an operation that does not
//     declare one is a thrown contract error, not a silently ignored header.
//   * Which calls are credential-free is read off the table, never assumed.
//     There are THREE as of plan 09 wave 3, and the number is deliberately not
//     written into any rule here:
//       - `GET  /api/v1/admin/setup/claim-status`     (wave 0)
//       - `POST /api/v1/admin/admin-invites/preview`  (wave 2 — shipped with no
//         `security` block at all, which nothing in this console noticed)
//       - `GET  /api/v1/admin/setup/sign-in-methods`  (finding F15's fix)
//     An earlier version of this note asserted "claim-status is the only
//     credential-free call". It was already false in the tree when it was read,
//     and a note that has to be edited every time Moira adds an anonymous
//     operation is a note that will be wrong again. The registry below is the
//     answer; `tests/contract/openapi-contract.test.ts` re-derives it from the
//     committed spec on every run.
//
// `tests/contract/openapi-contract.test.ts` re-derives the whole table from the
// committed spec on every run.

import "server-only";

import { MoiraRequestError, toMoiraError, toTransportError } from "./errors";
import type {
  AdminIdentityRecord,
  AdminInviteCreateRequest,
  AdminInviteSecretResponse,
  AuthProviderSettingsRecord,
  ConsoleAuthProviderCreateRequest,
  ConsoleClaimAdminIdentityRequest,
  ConsoleTrustedJwtIssuerCreateRequest,
  ListResponse,
  SetupAuthMethodsResponse,
  SetupClaimStatusResponse,
  SetupSignInMethodsResponse,
  TrustedJwtIssuerRecord,
} from "./types";

/* -------------------------------------------------------------------------- */
/* Operation registry                                                         */
/* -------------------------------------------------------------------------- */

export type HttpMethod = "GET" | "POST" | "PATCH" | "DELETE";

/**
 * Which credential the spec's `security` block declares for an operation.
 *
 * `system_key_only` is `[{ systemKeyAuth: [] }]` and nothing else — the claim
 * endpoint. A bearer JWT is refused there even if it verifies.
 * `admin` is the usual `[bearerAuth, systemKeyAuth, consumerKeyAuth]` triple.
 * `none` is an absent `security` block.
 */
export type MoiraCredentialRequirement = "none" | "system_key_only" | "admin";

export interface MoiraOperation {
  /** `operationId` in `docs/openapi.json`. */
  readonly id: string;
  readonly method: HttpMethod;
  /** The spec's path template, `{...}` placeholders intact. */
  readonly path: string;
  readonly credential: MoiraCredentialRequirement;
  /** The spec declares an `Idempotency-Key` header parameter. */
  readonly declaresIdempotencyKey: boolean;
  /** The spec declares `If-Match` as a REQUIRED header parameter. */
  readonly requiresIfMatch: boolean;
}

function op<T extends MoiraOperation>(operation: T): T {
  return operation;
}

export const MOIRA_OPERATIONS = {
  getSetupClaimStatus: op({
    id: "get_setup_claim_status",
    method: "GET",
    path: "/api/v1/admin/setup/claim-status",
    credential: "none",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  getSetupAuthMethods: op({
    id: "get_setup_auth_methods",
    method: "GET",
    path: "/api/v1/admin/setup/auth-methods",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * The ANONYMOUS sign-in projection (finding F15's fix, plan 09 wave 1).
   *
   * `PublicSignInMethod` is deliberately `PublicAuthMethod` MINUS
   * `allowed_email_domains` (that is plan 07 decision D3 — the deny-by-default
   * admin-claim policy, and publishing it anonymously would hand any caller the
   * list of domains that can obtain Moira admin) and minus `jwks_url`.
   *
   * Consequence the console must respect: it is enough to RENDER a sign-in
   * button and NOT enough to RESOLVE the configuration behind one.
   * `resolveAuthConfigs` refuses a row without `allowed_email_domains` or
   * `trusted_jwt_issuer_id`, and neither is in this projection.
   */
  getSetupSignInMethods: op({
    id: "get_setup_sign_in_methods",
    method: "GET",
    path: "/api/v1/admin/setup/sign-in-methods",
    credential: "none",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  claimAdminIdentity: op({
    id: "claim_admin_identity",
    method: "POST",
    path: "/api/v1/admin/setup/claim",
    credential: "system_key_only",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),

  /**
   * `POST /api/v1/admin/admin-invites` — the once-only token mint.
   *
   * `declaresIdempotencyKey: true` and `requiresIfMatch: false` are read OFF THE
   * SPEC, not guessed: the operation declares an optional `Idempotency-Key`
   * header parameter and no `If-Match` at all.
   * `tests/contract/openapi-contract.test.ts:195-206` re-derives both.
   *
   * The idempotent-replay behaviour is what makes the key matter here: a replay
   * returns the SANITIZED record with `secret: null`, not the token again. That
   * is not an error — see `AdminInviteSecretResponse` in `lib/types.ts`.
   */
  createAdminInvite: op({
    id: "create_admin_invite",
    method: "POST",
    path: "/api/v1/admin/admin-invites",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),

  listTrustedJwtIssuers: op({
    id: "list_trusted_jwt_issuers",
    method: "GET",
    path: "/api/v1/admin/jwt-issuers",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createTrustedJwtIssuer: op({
    id: "create_trusted_jwt_issuer",
    method: "POST",
    path: "/api/v1/admin/jwt-issuers",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  enableTrustedJwtIssuer: op({
    id: "enable_trusted_jwt_issuer",
    method: "POST",
    path: "/api/v1/admin/jwt-issuers/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),

  // The auth-provider surface: SEVEN operations, not ten. (Ten is the total
  // including the three setup operations above.)
  listAuthProviders: op({
    id: "list_auth_providers",
    method: "GET",
    path: "/api/v1/admin/auth/providers",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createAuthProvider: op({
    id: "create_auth_provider",
    method: "POST",
    path: "/api/v1/admin/auth/providers",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  getAuthProvider: op({
    id: "get_auth_provider",
    method: "GET",
    path: "/api/v1/admin/auth/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  patchAuthProvider: op({
    id: "patch_auth_provider",
    method: "PATCH",
    path: "/api/v1/admin/auth/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  deleteAuthProvider: op({
    id: "delete_auth_provider",
    method: "DELETE",
    path: "/api/v1/admin/auth/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  enableAuthProvider: op({
    id: "enable_auth_provider",
    method: "POST",
    path: "/api/v1/admin/auth/providers/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableAuthProvider: op({
    id: "disable_auth_provider",
    method: "POST",
    path: "/api/v1/admin/auth/providers/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
} as const;

export type MoiraOperationName = keyof typeof MOIRA_OPERATIONS;

/** The seven operations that make up the auth-provider surface. */
export const AUTH_PROVIDER_OPERATION_NAMES = [
  "listAuthProviders",
  "createAuthProvider",
  "getAuthProvider",
  "patchAuthProvider",
  "deleteAuthProvider",
  "enableAuthProvider",
  "disableAuthProvider",
] as const satisfies readonly MoiraOperationName[];

/* -------------------------------------------------------------------------- */
/* Contract errors — the console built a request it is forbidden to build      */
/* -------------------------------------------------------------------------- */

export class MoiraClientContractError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MoiraClientContractError";
  }
}

/**
 * `POST /api/v1/admin/setup/claim` guard.
 *
 * - `scopes: []` is NOT the same as omitting `scopes`. Omitted yields
 *   `["moira:admin"]`; an explicit empty array creates a grant with zero scopes —
 *   a silent, permanent, un-revocable-by-retry no-op admin. The field must be
 *   absent, not empty.
 * - `setup_token` is rejected with `400 setup_token_not_supported`, not ignored.
 *   The console never sends it.
 * - `email` and `email_verified` are required with no defaults and no
 *   credential-type branch that makes them omittable.
 */
export function assertClaimRequestIsSafe(body: Record<string, unknown>): void {
  if ("scopes" in body) {
    throw new MoiraClientContractError(
      "claim body must omit `scopes` entirely — `scopes: []` creates a grant with zero scopes, " +
        "and a non-empty bad scope is 422 scope_invalid",
    );
  }
  if ("setup_token" in body) {
    throw new MoiraClientContractError(
      "claim body must omit `setup_token` — it is reserved and rejected with 400 setup_token_not_supported",
    );
  }
  if (typeof body["email"] !== "string" || body["email"].length === 0) {
    throw new MoiraClientContractError("claim body requires a non-empty `email`");
  }
  if (typeof body["email_verified"] !== "boolean") {
    throw new MoiraClientContractError("claim body requires a boolean `email_verified`");
  }
}

/**
 * `POST /api/v1/admin/auth/providers` guard.
 *
 * - `enabled` must be ABSENT, not `false`. It is a plain writable boolean in
 *   Moira; "the row is created disabled" is this console's convention, and this
 *   is where the convention is enforced. Only `enableAuthProvider` may enable a row.
 * - `trusted_jwt_issuer_id` must be present and non-empty. Without it,
 *   `admission_policy` matches neither its bound stage (`trusted_jwt_issuer_id
 *   = $2`) nor its unbound one (`issuer = $1`, which is the claim body's issuer —
 *   the console's, not the IdP's), so `policy = None` and every claim is
 *   `403 admin_claim_domain_not_allowed`. From wave 4B it is also what the
 *   console's minted `iss` is read from.
 * - `display_name` is required by the schema; omitting it is a 400.
 */
export function assertProviderCreateIsSafe(body: Record<string, unknown>): void {
  if ("enabled" in body) {
    throw new MoiraClientContractError(
      "provider create body must not contain `enabled` at all — not even `enabled: false`. " +
        "Use enableAuthProvider() as the commit point.",
    );
  }
  const issuerId = body["trusted_jwt_issuer_id"];
  if (typeof issuerId !== "string" || issuerId.length === 0) {
    throw new MoiraClientContractError(
      "provider create body must carry a non-empty `trusted_jwt_issuer_id` — without it the row " +
        "can never govern the console's issuer and every claim is 403 admin_claim_domain_not_allowed",
    );
  }
  if (typeof body["display_name"] !== "string" || body["display_name"].length === 0) {
    throw new MoiraClientContractError(
      "provider create body requires a non-empty `display_name` (schema-required; omitting it is a 400)",
    );
  }
}

/**
 * `POST /api/v1/admin/jwt-issuers` guard.
 *
 * A console-linked issuer must leave `scopes_claim` unset, or a provider row
 * bound to it is refused `400 console_issuer_must_not_assert_scopes` — tokens
 * that self-assert scopes would displace `admin_identities` as the source of
 * human authorization.
 */
export function assertTrustedIssuerCreateIsSafe(body: Record<string, unknown>): void {
  if ("scopes_claim" in body && body["scopes_claim"] != null) {
    throw new MoiraClientContractError(
      "the console's trusted JWT issuer must not declare `scopes_claim` — " +
        "authorization comes from the admin_identities grant, never from a self-asserted claim",
    );
  }
  if ("claim_mapping" in body && body["claim_mapping"] != null) {
    throw new MoiraClientContractError(
      "the console's trusted JWT issuer must not declare `claim_mapping` — it can carry a scopes mapping",
    );
  }
}

/* -------------------------------------------------------------------------- */
/* Client                                                                     */
/* -------------------------------------------------------------------------- */

export interface MoiraClientOptions {
  /** e.g. `https://moira.internal`. Trailing slashes are trimmed. */
  readonly baseUrl: string;
  /** The bootstrap system key. Required for every `system_key_only` operation. */
  readonly systemKey?: string | undefined;
  /** Resolves the console's minted admin JWT. Unused by the wizard. */
  readonly bearerToken?: (() => string | Promise<string>) | undefined;
  /** Injectable for tests. */
  readonly fetch?: typeof fetch | undefined;
  /** Per-request correlation id, sent as `X-Request-Id`. */
  readonly requestId?: (() => string) | undefined;
}

interface RequestOptions {
  readonly pathParams?: Readonly<Record<string, string>>;
  readonly query?: Readonly<Record<string, string | number | undefined>>;
  readonly body?: unknown;
  /**
   * Only permitted on operations whose `declaresIdempotencyKey` is true.
   * Supplying it elsewhere throws — the console does not send headers the spec
   * does not declare, even though they would be ignored at runtime.
   */
  readonly idempotencyKey?: string | undefined;
  /** Required on operations whose `requiresIfMatch` is true. */
  readonly ifMatch?: string | undefined;
}

/** What a request actually sent. Returned alongside results so flows can trace. */
export interface MoiraRequestRecord {
  readonly operation: MoiraOperationName;
  readonly method: HttpMethod;
  readonly url: string;
  readonly headerNames: readonly string[];
}

export class MoiraClient {
  readonly #baseUrl: string;
  readonly #systemKey: string | undefined;
  readonly #bearerToken: (() => string | Promise<string>) | undefined;
  readonly #fetch: typeof fetch;
  readonly #requestId: (() => string) | undefined;

  constructor(options: MoiraClientOptions) {
    this.#baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.#systemKey = options.systemKey;
    this.#bearerToken = options.bearerToken;
    this.#fetch = options.fetch ?? globalThis.fetch;
    this.#requestId = options.requestId;
  }

  /* ---------------------------------------------------------------------- */
  /* Setup surface                                                          */
  /* ---------------------------------------------------------------------- */

  /**
   * `GET /api/v1/admin/setup/claim-status`. THE ONLY anonymous Moira call in
   * this console. One boolean is the whole contract.
   */
  async getSetupClaimStatus(): Promise<SetupClaimStatusResponse> {
    return this.#request<SetupClaimStatusResponse>("getSetupClaimStatus", {});
  }

  /**
   * `GET /api/v1/admin/setup/auth-methods`. Authenticated on purpose — the
   * response is identity configuration. Called server-side only; the raw
   * response never crosses to the browser.
   */
  async getSetupAuthMethods(): Promise<SetupAuthMethodsResponse> {
    return this.#request<SetupAuthMethodsResponse>("getSetupAuthMethods", {});
  }

  /**
   * `GET /api/v1/admin/setup/sign-in-methods`. ANONYMOUS.
   *
   * Enough to render a button, not enough to resolve the configuration behind
   * it — see the registry entry. `/login` uses it for the provider's
   * `display_name` only, and decides whether to render a button at all from
   * `consoleRuntime()`.
   */
  async getSetupSignInMethods(): Promise<SetupSignInMethodsResponse> {
    return this.#request<SetupSignInMethodsResponse>("getSetupSignInMethods", {});
  }

  /**
   * `POST /api/v1/admin/setup/claim`. System key only — a bearer JWT is refused
   * even if it verifies (`401 setup_claim_credential_required`).
   *
   * `idempotencyKey` should be derived deterministically from `(issuer, subject)`
   * so a double-submit replays with 200 rather than conflicting with 409.
   */
  async claimAdminIdentity(
    body: ConsoleClaimAdminIdentityRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminIdentityRecord> {
    assertClaimRequestIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<AdminIdentityRecord>("claimAdminIdentity", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Trusted JWT issuers                                                    */
  /* ---------------------------------------------------------------------- */

  async listTrustedJwtIssuers(
    options: { readonly limit?: number; readonly cursor?: string } = {},
  ): Promise<ListResponse<TrustedJwtIssuerRecord>> {
    return this.#request<ListResponse<TrustedJwtIssuerRecord>>("listTrustedJwtIssuers", {
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /**
   * Exact-match lookup by `issuer`, paging the list.
   *
   * Deliberately not a `?search=` call: `search`'s matching semantics are not
   * part of this console's contract, and an issuer lookup that silently matches
   * a prefix would bind the provider row to the wrong issuer. Exact string
   * comparison here mirrors `resolve_active_issuer`'s own exact match.
   */
  async findTrustedJwtIssuerByIssuer(issuer: string): Promise<TrustedJwtIssuerRecord | null> {
    let cursor: string | undefined;
    // Bounded so a paging bug cannot spin forever during setup.
    for (let page = 0; page < 50; page += 1) {
      const response: ListResponse<TrustedJwtIssuerRecord> = await this.listTrustedJwtIssuers(
        cursor === undefined ? { limit: 100 } : { limit: 100, cursor },
      );
      const match = response.data.find((row) => row.issuer === issuer);
      if (match !== undefined) return match;
      if (!response.pagination.has_more) return null;
      const next = response.pagination.next_cursor;
      if (next === null || next === undefined || next === "") return null;
      cursor = next;
    }
    return null;
  }

  async createTrustedJwtIssuer(
    body: ConsoleTrustedJwtIssuerCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<TrustedJwtIssuerRecord> {
    assertTrustedIssuerCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<TrustedJwtIssuerRecord>("createTrustedJwtIssuer", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /** `POST .../jwt-issuers/{id}/enable`. `If-Match` required; no `Idempotency-Key`. */
  async enableTrustedJwtIssuer(id: string, ifMatch: string): Promise<TrustedJwtIssuerRecord> {
    return this.#request<TrustedJwtIssuerRecord>("enableTrustedJwtIssuer", {
      pathParams: { id },
      ifMatch,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Auth providers                                                         */
  /* ---------------------------------------------------------------------- */

  async listAuthProviders(
    options: { readonly limit?: number; readonly cursor?: string } = {},
  ): Promise<ListResponse<AuthProviderSettingsRecord>> {
    return this.#request<ListResponse<AuthProviderSettingsRecord>>("listAuthProviders", {
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /**
   * `POST /api/v1/admin/auth/providers`.
   *
   * The body type forbids `enabled` and requires `trusted_jwt_issuer_id`; the
   * runtime guard re-checks both for callers that reached here through `any`.
   */
  async createAuthProvider(
    body: ConsoleAuthProviderCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AuthProviderSettingsRecord> {
    assertProviderCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<AuthProviderSettingsRecord>("createAuthProvider", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async getAuthProvider(id: string): Promise<AuthProviderSettingsRecord> {
    return this.#request<AuthProviderSettingsRecord>("getAuthProvider", { pathParams: { id } });
  }

  async patchAuthProvider(
    id: string,
    body: Readonly<Record<string, unknown>>,
    ifMatch: string,
  ): Promise<AuthProviderSettingsRecord> {
    if ("enabled" in body) {
      throw new MoiraClientContractError(
        "use enableAuthProvider()/disableAuthProvider() — `enabled` is not patched directly",
      );
    }
    return this.#request<AuthProviderSettingsRecord>("patchAuthProvider", {
      pathParams: { id },
      body,
      ifMatch,
    });
  }

  async deleteAuthProvider(id: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteAuthProvider", { pathParams: { id }, ifMatch });
  }

  /**
   * `POST .../auth/providers/{id}/enable` — the dual write's commit point.
   *
   * Carries `If-Match` and NO `Idempotency-Key`: the spec does not declare one on
   * this operation. Retry safety is `If-Match` plus `enable` being naturally
   * idempotent, and that is the whole of it.
   */
  async enableAuthProvider(id: string, ifMatch: string): Promise<AuthProviderSettingsRecord> {
    return this.#request<AuthProviderSettingsRecord>("enableAuthProvider", {
      pathParams: { id },
      ifMatch,
    });
  }

  async disableAuthProvider(id: string, ifMatch: string): Promise<AuthProviderSettingsRecord> {
    return this.#request<AuthProviderSettingsRecord>("disableAuthProvider", {
      pathParams: { id },
      ifMatch,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Admin invitations                                                      */
  /* ---------------------------------------------------------------------- */

  /**
   * `POST /api/v1/admin/admin-invites` — mint a once-only invitation token.
   *
   * The response is the ONLY time the raw token exists outside Moira's hash.
   * Note what this method does NOT do: it does not log the response, does not
   * pass it through `lib/errors.ts`, and does not cache it. `#request` returns a
   * 2xx body raw — `toMoiraError` is called only under `if (!response.ok)` — so
   * there is nothing between the JSON parse and whatever the caller does next.
   *
   * `idempotencyKey` should be derived from the invite's own identity
   * `(constraint, value)`. A replay returns `secret: null` with the sanitized
   * record, which is the correct and expected outcome, not a failure.
   */
  async createAdminInvite(
    body: AdminInviteCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminInviteSecretResponse> {
    return this.#request<AdminInviteSecretResponse>("createAdminInvite", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Transport                                                              */
  /* ---------------------------------------------------------------------- */

  async #request<T>(name: MoiraOperationName, options: RequestOptions): Promise<T> {
    const operation: MoiraOperation = MOIRA_OPERATIONS[name];
    const url = this.#buildUrl(operation, options);
    const headers = await this.#buildHeaders(operation, options);

    let response: Response;
    try {
      response = await this.#fetch(url, {
        method: operation.method,
        headers,
        ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
      });
    } catch (cause) {
      throw new MoiraRequestError(toTransportError(cause));
    }

    if (!response.ok) {
      let parsed: unknown;
      try {
        parsed = await response.json();
      } catch {
        parsed = undefined;
      }
      throw new MoiraRequestError(toMoiraError(response.status, parsed));
    }

    if (response.status === 204) return undefined as T;
    return (await response.json()) as T;
  }

  #buildUrl(operation: MoiraOperation, options: RequestOptions): string {
    let path = operation.path;
    const params = options.pathParams ?? {};
    for (const [key, value] of Object.entries(params)) {
      const token = `{${key}}`;
      if (!path.includes(token)) {
        throw new MoiraClientContractError(
          `operation ${operation.id} has no path parameter ${token}`,
        );
      }
      path = path.replace(token, encodeURIComponent(value));
    }
    if (/\{[^}]+\}/.test(path)) {
      throw new MoiraClientContractError(`unsubstituted path parameter in ${path}`);
    }

    const search = new URLSearchParams();
    for (const [key, value] of Object.entries(options.query ?? {})) {
      if (value === undefined) continue;
      search.set(key, String(value));
    }
    const suffix = search.size > 0 ? `?${search.toString()}` : "";
    return `${this.#baseUrl}${path}${suffix}`;
  }

  async #buildHeaders(
    operation: MoiraOperation,
    options: RequestOptions,
  ): Promise<Record<string, string>> {
    const headers: Record<string, string> = {};

    if (options.body !== undefined) headers["Content-Type"] = "application/json";
    headers["Accept"] = "application/json";

    // --- credential, straight from the registry ---------------------------
    switch (operation.credential) {
      case "none":
        // Deliberately nothing. `claim-status` is anonymous by contract.
        break;
      case "system_key_only": {
        if (this.#systemKey === undefined || this.#systemKey === "") {
          throw new MoiraClientContractError(
            `${operation.id} requires the bootstrap system key (X-Moira-System-Key); ` +
              "no bearer token is accepted on this operation",
          );
        }
        headers["X-Moira-System-Key"] = this.#systemKey;
        break;
      }
      case "admin": {
        if (this.#systemKey !== undefined && this.#systemKey !== "") {
          headers["X-Moira-System-Key"] = this.#systemKey;
        } else if (this.#bearerToken !== undefined) {
          headers["Authorization"] = `Bearer ${await this.#bearerToken()}`;
        } else {
          throw new MoiraClientContractError(
            `${operation.id} requires a credential: configure systemKey or bearerToken`,
          );
        }
        break;
      }
    }

    // --- Idempotency-Key: only where the spec declares it -----------------
    if (options.idempotencyKey !== undefined) {
      if (!operation.declaresIdempotencyKey) {
        throw new MoiraClientContractError(
          `${operation.id} does not declare an Idempotency-Key parameter; ` +
            "retry safety there comes from If-Match and natural idempotence",
        );
      }
      headers["Idempotency-Key"] = options.idempotencyKey;
    }

    // --- If-Match: required where the spec says required ------------------
    if (operation.requiresIfMatch) {
      if (options.ifMatch === undefined || options.ifMatch === "") {
        throw new MoiraClientContractError(
          `${operation.id} requires If-Match; read the resource first rather than fabricating a version`,
        );
      }
      headers["If-Match"] = options.ifMatch;
    } else if (options.ifMatch !== undefined) {
      throw new MoiraClientContractError(`${operation.id} does not declare an If-Match parameter`);
    }

    if (this.#requestId !== undefined) headers["X-Request-Id"] = this.#requestId();

    return headers;
  }
}

/** `If-Match` value for a record's current version. */
export function ifMatchFor(record: { readonly version: number }): string {
  return String(record.version);
}
