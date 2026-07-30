// The Kubernetes probe target.
//
// ============================================================================
// WHY THIS IS A LIVENESS ANSWER AND NOT A DEPENDENCY REPORT
// ============================================================================
//
// This route answers one question: is this process able to serve HTTP? It
// deliberately does NOT check Moira, the IdP, or the console's own database.
//
// A liveness probe that fails on a downstream outage restarts healthy pods,
// which turns a Moira blip into a console outage and removes the one thing that
// could have shown an operator what was wrong. `charts/moira-console` points
// both `livenessProbe` and `readinessProbe` here, and the chart's comment names
// the condition under which they should be split — when a readiness signal that
// DOES depend on Moira becomes worth having, it belongs at a different path, not
// here.
//
// There is no Docker `HEALTHCHECK`. Moira dropped its own for the same reason
// (Kubernetes probes own liveness under an orchestrator), and the console's
// runtime image is distroless — no shell, so a `CMD`-form healthcheck could not
// run in it regardless.

/** Reads no request state, but must never be prerendered into a static 200. */
export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export function GET(): Response {
  return Response.json(
    { status: "ok" },
    {
      status: 200,
      // A cached probe response is a probe that stops probing.
      headers: { "cache-control": "no-store" },
    },
  );
}
