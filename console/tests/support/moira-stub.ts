// A recording stub for Moira's HTTP surface.
//
// Deliberately a `fetch` replacement rather than a hand-rolled fake client: the
// tests that matter are about what actually goes on the wire — the order of the
// requests, which headers are attached, and what is in each body. A fake client
// would let all three drift.
//
// HANDLERS ARE KEYED ON `"<METHOD> <path>"`, QUERY EXCLUDED, ON PURPOSE.
// Moira's twelve admin list endpoints accept all 26 `PageQuery` fields and
// silently ignore the ones they do not implement — `provider_id` on
// `GET /api/v1/admin/provider-credentials` is one of them (`src/domain/admin.rs`
// documents the behaviour; the SQL in `src/infra/repositories/admin.rs` has no
// filter clause). A stub that honoured the query would be MORE capable than the
// server and would green-light a caller that trusts the filter; that is exactly
// how the credential-reuse bug in `lib/claude-subscription.ts` reached main.
// A fixture that wants to prove a filter works must model the filtering itself,
// and the raw query is available to every handler on `RecordedRequest.url`.

export interface RecordedRequest {
  readonly method: string;
  readonly url: string;
  readonly path: string;
  readonly headers: Readonly<Record<string, string>>;
  readonly body: unknown;
  /** `"<METHOD> <path>"`, the key the tests assert order on. */
  readonly route: string;
}

export interface StubResponse {
  readonly status: number;
  readonly body?: unknown;
}

export type StubHandler = (request: RecordedRequest) => StubResponse;

export interface MoiraStub {
  readonly fetch: typeof fetch;
  /** Every request, in order. */
  readonly requests: RecordedRequest[];
  /** `"<METHOD> <path>"` for each request, in order. */
  routes(): string[];
  bodyOf(route: string): unknown;
  requestsFor(route: string): RecordedRequest[];
}

const BASE_URL = "https://moira.test";

export function createMoiraStub(handlers: Record<string, StubHandler>): MoiraStub {
  const requests: RecordedRequest[] = [];

  const stubFetch = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input);
    const method = (init?.method ?? "GET").toUpperCase();
    const path = new URL(url).pathname;
    const headers: Record<string, string> = {};
    for (const [key, value] of Object.entries((init?.headers ?? {}) as Record<string, string>)) {
      headers[key] = value;
    }
    const rawBody = init?.body;
    const body = typeof rawBody === "string" ? JSON.parse(rawBody) : undefined;
    const route = `${method} ${path}`;
    const recorded: RecordedRequest = { method, url, path, headers, body, route };
    requests.push(recorded);

    const handler = handlers[route];
    if (handler === undefined) {
      throw new Error(`moira-stub: no handler registered for "${route}"`);
    }
    const response = handler(recorded);
    return new Response(response.body === undefined ? null : JSON.stringify(response.body), {
      status: response.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;

  return {
    fetch: stubFetch,
    requests,
    routes: () => requests.map((request) => request.route),
    bodyOf: (route) => requests.find((request) => request.route === route)?.body,
    requestsFor: (route) => requests.filter((request) => request.route === route),
  };
}

export const MOIRA_STUB_BASE_URL = BASE_URL;

/** A Moira `ErrorResponse` envelope, including the fields that must not cross. */
export function errorEnvelope(
  code: string,
  options: {
    readonly message?: string;
    readonly messageArgs?: unknown;
    readonly requestId?: string;
    readonly details?: unknown;
  } = {},
) {
  return {
    error: {
      code,
      message_key: `moira.error.${code}`,
      message: options.message ?? `stub message for ${code}`,
      message_args: options.messageArgs ?? {},
      request_id: options.requestId ?? "req_stub_0001",
      details: options.details ?? { internal: "must not cross the boundary" },
    },
  };
}
