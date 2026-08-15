// `POST /api/playground/stream` — the playground's streaming-first execution
// path, `text/event-stream` end to end.
//
// ============================================================================
// WHY THIS IS A SEPARATE HANDLER FROM `.../run/route.ts`
// ============================================================================
//
// `MoiraClient#request<T>` — the transport every other BFF route in this
// console uses — always does `await response.json()`. Reusing it here would
// read the whole SSE body into memory before returning, which defeats
// streaming entirely (the browser would see nothing until the execution
// finished, indistinguishable from the non-streaming path except slower).
// `MoiraClient#streamResponse` is the one operation that returns the raw
// upstream `Response` instead — see its doc comment in `lib/moira-client.ts`.
//
// ============================================================================
// WHAT THIS HANDLER DOES AND DOES NOT DO WITH THE STREAM'S BYTES
// ============================================================================
//
// It does not parse a single frame. `upstream.body` (a live
// `ReadableStream<Uint8Array>`) is handed straight to `Response` unmodified —
// re-encoding it server-side would add a full buffering hop for no benefit,
// since the SSE frames Moira emits are already exactly what the browser's own
// parser (`lib/sse.ts`, hand-rolled because `EventSource` cannot `POST`)
// expects. The one exception is a NON-OK upstream response: that is read once,
// synchronously, and re-shaped into this console's own keyed error envelope —
// see `withConsoleSession`'s catch, which handles a transport-level failure
// (Moira unreachable), and the `!upstream.ok` branch below, which handles a
// reached-but-refused one (4xx/5xx with a JSON body).
//
// ============================================================================
// CANCELLATION
// ============================================================================
//
// `request.signal` — the incoming request's own `AbortSignal` — is forwarded
// into `streamResponse`'s outbound fetch. When the browser's stop button
// aborts its `fetch` to this route (`modules/playground/PlaygroundScreen.tsx`),
// Next.js aborts `request.signal`, and because the SAME signal object is
// passed through to the outbound call, the fetch to Moira aborts too —
// `docs/streaming-api.md`: "Dropping the client connection cancels the
// supervised execution."

import { badRequest, moiraErrorBody, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { toMoiraError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { buildPublicResponseRequest, readPlaygroundRunBody } from "@/lib/playground-request";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.playground_request_body_invalid);

    const input = readPlaygroundRunBody(body);
    if (input === null) return badRequest(CONSOLE_MESSAGE_KEYS.playground_prompt_required);

    const upstream = await client.streamResponse(buildPublicResponseRequest(input), {
      signal: request.signal,
    });

    if (!upstream.ok) {
      let parsed: unknown;
      try {
        parsed = await upstream.json();
      } catch {
        parsed = undefined;
      }
      const moiraError = toMoiraError(upstream.status, parsed);
      return Response.json(moiraErrorBody(moiraError), { status: upstream.status, headers: NO_STORE });
    }

    return new Response(upstream.body, {
      status: 200,
      headers: {
        "content-type": "text/event-stream",
        ...NO_STORE,
        // Disables response buffering on the handful of reverse proxies that
        // still default to it for anything that is not explicitly opted out —
        // nginx's own docs name this exact header for this exact purpose.
        "x-accel-buffering": "no",
      },
    });
  });
}
