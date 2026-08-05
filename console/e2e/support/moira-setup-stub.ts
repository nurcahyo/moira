// A stub Moira for the setup-wizard e2e fixture: the eleven operations the
// setup window actually calls, over real TLS, with real state.
//
// ============================================================================
// WHAT THIS IS AND WHAT IT IS NOT
// ============================================================================
//
// It is a SERVER, not a patched `fetch`. `tests/support/moira-stub.ts` is the
// fetch-level stub the unit suites use, and it cannot serve this purpose: the
// fixture console is a separate OS process running the shipped standalone
// build, so the only seam between it and Moira is a socket. Everything the
// console does to reach Moira here — DNS, TLS, the `X-Moira-System-Key` header,
// the `Idempotency-Key`, the `If-Match` precondition — is the shipped path.
//
// It is NOT a model of Moira's semantics. It keeps rows in a `Map`, bumps a
// version on every write, and refuses an unauthenticated admin call. It does
// NOT reproduce Moira's uniqueness indexes, its admission policy, or its
// idempotency store. Those are Moira's own tests' job; what this file exists to
// support is a browser that can reach every step of the wizard so axe can audit
// the DOM each step renders.
//
// The one behaviour it DOES enforce is the credential: every `admin` operation
// requires the bootstrap system key, and `claim-status` requires none. A fixture
// that accepted anonymous admin calls would let a regression that stopped
// attaching the key still pass.

import { createServer, type Server } from "node:https";
import type { IncomingMessage, ServerResponse } from "node:http";

import type {
  AdminIdentityRecord,
  AuthProviderSettingsRecord,
  PublicAuthMethod,
  TrustedJwtIssuerRecord,
} from "../../lib/types";
import {
  SETUP_FIXTURE_CONTROL_PATH,
  type SetupFixtureScenario,
} from "./setup-fixture";

const SYSTEM_KEY_HEADER = "x-moira-system-key";

export interface MoiraSetupStubOptions {
  readonly port: number;
  readonly cert: Buffer;
  readonly key: Buffer;
  /** The one value `X-Moira-System-Key` may carry. */
  readonly systemKey: string;
}

export interface MoiraSetupStub {
  readonly server: Server;
  close(): Promise<void>;
}

/* -------------------------------------------------------------------------- */
/* State                                                                      */
/* -------------------------------------------------------------------------- */

interface StubState {
  claimed: boolean;
  issuers: TrustedJwtIssuerRecord[];
  providers: AuthProviderSettingsRecord[];
  identities: AdminIdentityRecord[];
}

function emptyState(scenario: SetupFixtureScenario): StubState {
  return { claimed: scenario.claimed, issuers: [], providers: [], identities: [] };
}

function nowIso(): string {
  return new Date().toISOString();
}

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : [];
}

/* -------------------------------------------------------------------------- */
/* Records                                                                    */
/* -------------------------------------------------------------------------- */

function newTrustedIssuer(body: Record<string, unknown>): TrustedJwtIssuerRecord {
  const timestamp = nowIso();
  return {
    id: crypto.randomUUID(),
    issuer: stringOrNull(body["issuer"]) ?? "",
    jwks_url: stringOrNull(body["jwks_url"]) ?? "",
    expected_audiences: stringList(body["expected_audiences"]),
    allowed_algorithms: stringList(body["allowed_algorithms"]),
    subject_claim: stringOrNull(body["subject_claim"]) ?? "sub",
    clock_skew_seconds: 60,
    allow_delegation: false,
    status: "active",
    created_at: timestamp,
    updated_at: timestamp,
    version: 1,
  };
}

function newProvider(body: Record<string, unknown>): AuthProviderSettingsRecord {
  const timestamp = nowIso();
  return {
    id: crypto.randomUUID(),
    method: (stringOrNull(body["method"]) ?? "generic_oidc") as AuthProviderSettingsRecord["method"],
    display_name: stringOrNull(body["display_name"]) ?? "",
    // Moira creates a provider DISABLED; the wizard's fourth step is what
    // enables it, and a stub that created it enabled would skip that step.
    enabled: false,
    requested_scopes: stringList(body["requested_scopes"]),
    allowed_email_domains: stringList(body["allowed_email_domains"]),
    allowed_algorithms: stringList(body["allowed_algorithms"]),
    expected_audiences: stringList(body["expected_audiences"]),
    redirect_uris: stringList(body["redirect_uris"]),
    metadata: null,
    status: "active",
    created_at: timestamp,
    updated_at: timestamp,
    version: 1,
    authorization_url: stringOrNull(body["authorization_url"]),
    client_id: stringOrNull(body["client_id"]),
    discovery_url: stringOrNull(body["discovery_url"]),
    issuer: stringOrNull(body["issuer"]),
    jwks_url: stringOrNull(body["jwks_url"]),
    token_url: stringOrNull(body["token_url"]),
    trusted_jwt_issuer_id: stringOrNull(body["trusted_jwt_issuer_id"]),
    userinfo_url: stringOrNull(body["userinfo_url"]),
  };
}

/** The anonymous-safe projection `GET /setup/auth-methods` publishes. */
function publicAuthMethod(provider: AuthProviderSettingsRecord): PublicAuthMethod {
  return {
    id: provider.id,
    method: provider.method,
    display_name: provider.display_name,
    requested_scopes: [...provider.requested_scopes],
    allowed_email_domains: [...provider.allowed_email_domains],
    authorization_url: provider.authorization_url ?? null,
    client_id: provider.client_id ?? null,
    discovery_url: provider.discovery_url ?? null,
    issuer: provider.issuer ?? null,
    jwks_url: provider.jwks_url ?? null,
  };
}

/* -------------------------------------------------------------------------- */
/* Replies                                                                    */
/* -------------------------------------------------------------------------- */

interface Reply {
  readonly status: number;
  readonly body: unknown;
}

function ok(body: unknown): Reply {
  return { status: 200, body };
}

/** Moira's `ErrorResponse` envelope, as `lib/errors.ts` narrows it. */
function moiraError(status: number, code: string): Reply {
  return {
    status,
    body: {
      error: {
        code,
        message_key: `moira.error.${code}`,
        message: `setup fixture stub refused: ${code}`,
        message_args: {},
        request_id: "req_setup_fixture_0001",
      },
    },
  };
}

function list<T>(data: readonly T[]): Reply {
  return ok({ data: [...data], pagination: { has_more: false, next_cursor: null } });
}

/* -------------------------------------------------------------------------- */
/* Routing                                                                    */
/* -------------------------------------------------------------------------- */

const PROVIDER_ID = /^\/api\/v1\/admin\/auth\/providers\/([^/]+)$/;
const PROVIDER_ENABLE = /^\/api\/v1\/admin\/auth\/providers\/([^/]+)\/enable$/;
const ISSUER_ENABLE = /^\/api\/v1\/admin\/jwt-issuers\/([^/]+)\/enable$/;

function handle(
  state: StubState,
  method: string,
  pathname: string,
  body: Record<string, unknown>,
): Reply {
  /* ---- setup surface --------------------------------------------------- */
  if (method === "GET" && pathname === "/api/v1/admin/setup/claim-status") {
    return ok({ claimed: state.claimed });
  }
  if (method === "GET" && pathname === "/api/v1/admin/setup/auth-methods") {
    return ok({ methods: state.providers.map(publicAuthMethod) });
  }
  if (method === "POST" && pathname === "/api/v1/admin/setup/claim") {
    const record: AdminIdentityRecord = {
      id: crypto.randomUUID(),
      issuer: stringOrNull(body["issuer"]) ?? "",
      subject: stringOrNull(body["subject"]) ?? "",
      email: stringOrNull(body["email"]) ?? "",
      email_verified: body["email_verified"] === true,
      granted_scopes: ["moira:admin"],
      status: "active",
      created_at: nowIso(),
      version: 1,
      notice: {
        message_key: "moira.notice.admin_identity_claimed",
        message: "the first administrator has been granted",
      },
      is_primary: true,
    };
    state.identities.push(record);
    state.claimed = true;
    return { status: 201, body: record };
  }

  /* ---- trusted JWT issuers --------------------------------------------- */
  if (method === "GET" && pathname === "/api/v1/admin/jwt-issuers") {
    return list(state.issuers);
  }
  if (method === "POST" && pathname === "/api/v1/admin/jwt-issuers") {
    const created = newTrustedIssuer(body);
    state.issuers.push(created);
    return { status: 201, body: created };
  }
  const issuerEnable = ISSUER_ENABLE.exec(pathname);
  if (method === "POST" && issuerEnable !== null) {
    const row = state.issuers.find((candidate) => candidate.id === issuerEnable[1]);
    if (row === undefined) return moiraError(404, "trusted_jwt_issuer_not_found");
    row.status = "active";
    row.version += 1;
    row.updated_at = nowIso();
    return ok(row);
  }

  /* ---- auth providers --------------------------------------------------- */
  if (method === "GET" && pathname === "/api/v1/admin/auth/providers") {
    return list(state.providers);
  }
  if (method === "POST" && pathname === "/api/v1/admin/auth/providers") {
    const created = newProvider(body);
    state.providers.push(created);
    return { status: 201, body: created };
  }
  const providerEnable = PROVIDER_ENABLE.exec(pathname);
  if (method === "POST" && providerEnable !== null) {
    const row = state.providers.find((candidate) => candidate.id === providerEnable[1]);
    if (row === undefined) return moiraError(404, "auth_provider_not_found");
    row.enabled = true;
    row.version += 1;
    row.updated_at = nowIso();
    return ok(row);
  }
  const providerId = PROVIDER_ID.exec(pathname);
  if (providerId !== null) {
    const row = state.providers.find((candidate) => candidate.id === providerId[1]);
    if (row === undefined) return moiraError(404, "auth_provider_not_found");
    if (method === "GET") return ok(row);
    if (method === "PATCH") {
      Object.assign(row, {
        display_name: stringOrNull(body["display_name"]) ?? row.display_name,
        allowed_email_domains: stringList(body["allowed_email_domains"]),
        requested_scopes: stringList(body["requested_scopes"]),
        redirect_uris: stringList(body["redirect_uris"]),
        ...(body["issuer"] === undefined ? {} : { issuer: stringOrNull(body["issuer"]) }),
        ...(body["discovery_url"] === undefined
          ? {}
          : { discovery_url: stringOrNull(body["discovery_url"]) }),
        ...(body["authorization_url"] === undefined
          ? {}
          : { authorization_url: stringOrNull(body["authorization_url"]) }),
        ...(body["token_url"] === undefined ? {} : { token_url: stringOrNull(body["token_url"]) }),
        ...(body["client_id"] === undefined ? {} : { client_id: stringOrNull(body["client_id"]) }),
        version: row.version + 1,
        updated_at: nowIso(),
      });
      return ok(row);
    }
  }

  return moiraError(404, "not_found");
}

/* -------------------------------------------------------------------------- */
/* The server                                                                 */
/* -------------------------------------------------------------------------- */

/** The one anonymous operation on Moira's admin surface. */
const ANONYMOUS_PATHS = new Set(["/api/v1/admin/setup/claim-status"]);

export function startMoiraSetupStub(options: MoiraSetupStubOptions): Promise<MoiraSetupStub> {
  let state = emptyState({ claimed: false });

  const server = createServer(
    { cert: options.cert, key: options.key },
    (request: IncomingMessage, response: ServerResponse) => {
      void (async () => {
        const url = new URL(request.url ?? "/", "https://localhost");
        const method = (request.method ?? "GET").toUpperCase();
        const raw = await readBody(request);
        let parsed: unknown;
        try {
          parsed = raw === "" ? {} : JSON.parse(raw);
        } catch {
          parsed = {};
        }
        const body = asRecord(parsed);

        /* ---- the spec's control surface, never Moira's ------------------ */
        if (url.pathname === SETUP_FIXTURE_CONTROL_PATH) {
          if (method === "POST") {
            state = emptyState({ claimed: body["claimed"] === true });
            return send(response, ok({ reset: true, claimed: state.claimed }));
          }
          return send(
            response,
            ok({
              claimed: state.claimed,
              issuers: state.issuers.length,
              providers: state.providers.length,
              identities: state.identities.length,
            }),
          );
        }

        /* ---- the credential, enforced ----------------------------------- */
        if (!ANONYMOUS_PATHS.has(url.pathname)) {
          const presented = request.headers[SYSTEM_KEY_HEADER];
          if (presented !== options.systemKey) {
            return send(response, moiraError(401, "setup_credential_required"));
          }
        }

        return send(response, handle(state, method, url.pathname, body));
      })().catch(() => {
        send(response, moiraError(500, "fixture_stub_failure"));
      });
    },
  );

  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(options.port, "127.0.0.1", () => {
      resolve({
        server,
        close: () =>
          new Promise<void>((done) => {
            server.close(() => done());
          }),
      });
    });
  });
}

function readBody(request: IncomingMessage): Promise<string> {
  return new Promise((resolve) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    request.on("error", () => resolve(""));
  });
}

function send(response: ServerResponse, reply: Reply): void {
  const payload = JSON.stringify(reply.body);
  response.writeHead(reply.status, {
    "content-type": "application/json",
    "cache-control": "no-store",
  });
  response.end(payload);
}
