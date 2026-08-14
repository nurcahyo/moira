// Binding a port when the port number has to be known BEFORE the bind.
//
// ============================================================================
// READ `bindConsoleServer` FIRST
// ============================================================================
//
// Almost nothing needs this. A fixture that only has to be reachable binds
// `port: 0`, lets the OS choose, and reads `server.port` back — `mock-github.ts`,
// `mock-idp.ts` and `console-server.ts` all do exactly that, and none of them can
// collide with anything.
//
// The residue this file exists for is the case where the number is an INPUT: a
// server that must advertise an origin it has not bound yet. `startMockIdp`'s
// `publicOrigin` is the one instance — the issuer is signed into the ID token,
// so the port has to be chosen, written into the certificate and the discovery
// document, and only then bound.
//
// There is no way to make that atomic, so it is made RETRYABLE instead. Issue
// #195 is what the un-retried version costs: something took the port between the
// probe and the bind, and the test died with `Failed to start server. Is port
// 34899 in use?`.
//
// HOW STRONG IS THE RETRY? Unknown, honestly. What took the port in #195 was
// never identified — it was not a second test file, because `bun test` runs
// files sequentially unless `--concurrent`/`--parallel` is passed and neither is
// passed in this repo. If the competitor is the kernel reusing a just-released
// port as an ephemeral source port for the harness's own loopback `fetch`es,
// then retrying against the same allocator is a mitigation of unmeasured
// strength rather than a bound. It is used here only because this one call site
// genuinely cannot bind first and ask later; everything else in the suite binds
// `port: 0` and is immune by construction. Prefer that shape. If this loop ever
// starts exhausting its attempts, the answer is not a bigger ATTEMPTS — it is to
// find a way for the IdP to learn its port after binding.

import { NativeResponse } from "./native-globals";

/** How many ports to burn before giving up. */
const ATTEMPTS = 12;

/**
 * A free port, released before it is returned.
 *
 * Inherently advisory — anything may take the port between the release and the
 * caller's bind, which is precisely why the only exported entry point below
 * wraps this in a retry rather than exposing it.
 */
function probeFreePort(): number {
  // `NativeResponse`, not the global `Response`, for the same reason
  // `console-server.ts` uses it: a reply handed back to `Bun.serve` is rejected
  // by constructor identity if it is happy-dom's. This handler is never invoked
  // — the socket is closed before the port is returned — but a probe that would
  // throw if it ever did answer is a trap for whoever reuses this next.
  const probe = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new NativeResponse("") });
  const port: number | undefined = probe.port;
  probe.stop(true);
  if (port === undefined) throw new Error("Bun.serve did not report a port");
  return port;
}

/**
 * Bun's shape for a port that is already taken.
 *
 * Bun 1.3.14 raises `Failed to start server. Is port 34899 in use?` with
 * `code: "EADDRINUSE"`. Both are checked: the code is the contract, the message
 * is the fallback for a Bun release that stops setting it, and a retry loop that
 * silently stopped recognising the error would restore the flake this module
 * exists to remove.
 */
function isAddressInUse(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const code = (error as { code?: unknown }).code;
  if (code === "EADDRINUSE") return true;
  const message = (error as { message?: unknown }).message;
  return typeof message === "string" && /is port \d+ in use/i.test(message);
}

/**
 * Run `start` against a free port, retrying on `EADDRINUSE` with a fresh one.
 *
 * `start` must bind the port it is given, and must not leave anything behind
 * when it throws — it is called again with a different number.
 */
export async function withFreePort<T>(start: (port: number) => Promise<T>): Promise<T> {
  let last: unknown;
  for (let attempt = 0; attempt < ATTEMPTS; attempt += 1) {
    const port = probeFreePort();
    try {
      return await start(port);
    } catch (error) {
      if (!isAddressInUse(error)) throw error;
      last = error;
    }
  }
  throw new Error(
    `could not bind a free port in ${ATTEMPTS} attempts — every candidate was taken between ` +
      `the probe and the bind. Something on this machine is claiming ports faster than the ` +
      `loop can use them. Last error: ${String(last)}`,
  );
}
