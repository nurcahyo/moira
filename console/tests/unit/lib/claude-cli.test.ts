// `lib/claude-cli.ts` — the process-execution boundary Mode A (CLI-assisted
// acquisition) runs on. Per the owner-approved testing policy
// (`plans/12-feature-expansion-brainstorm.md` §1), no test in this repository
// may require the real `claude` CLI or spawn a real process — every assertion
// below drives the job registry through an injected `ProcessSpawner` (a fake
// child built on plain arrays of listeners) that never touches
// `node:child_process`.
//
// Issue #269 replaced the single bounded `execFile` call PR #263 shipped
// with a job registry: `startClaudeCliJob` spawns and returns a job id
// immediately, and `peekClaudeCliJob`/`takeClaudeCliJobToken` are how a
// caller (the two route handlers) observes and consumes the outcome later.

import { afterEach, describe, expect, test } from "bun:test";

import {
  CLAUDE_CLI_BINARY,
  CLAUDE_CLI_SETUP_TOKEN_ARGS,
  extractAuthorizationUrlCandidate,
  extractSetupTokenCandidate,
  peekClaudeCliJob,
  ptyIsAvailable,
  resetClaudeCliJobRegistryForTests,
  setClaudeCliJobResult,
  setClaudeCliJobStorageFailed,
  setClaudeCliPtyAvailableForTests,
  startClaudeCliJob,
  takeClaudeCliJobToken,
} from "@/lib/claude-cli";
import { fakeClaudeChild as fakeChild } from "../../support/fake-claude-cli";

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

afterEach(() => {
  resetClaudeCliJobRegistryForTests();
  setClaudeCliPtyAvailableForTests(null);
});

/* -------------------------------------------------------------------------- */
/* ptyIsAvailable — an honest capability statement, fixed at `false` today   */
/* (issue #269 follow-up: neither a plain pipe nor `script(1)` gives the CLI */
/* a real terminal from inside a spawned server process — see the module's  */
/* own header for the investigation this is pinned by)                      */
/* -------------------------------------------------------------------------- */

describe("ptyIsAvailable", () => {
  test("is false by default — the real, shipped answer today", () => {
    expect(ptyIsAvailable()).toBe(false);
  });

  test("the test seam can force it true, to exercise the rest of the pipeline as if a real pty existed", () => {
    setClaudeCliPtyAvailableForTests(true);
    expect(ptyIsAvailable()).toBe(true);
  });

  test("the test seam can also force it false explicitly, distinct from merely being unset", () => {
    setClaudeCliPtyAvailableForTests(false);
    expect(ptyIsAvailable()).toBe(false);
  });

  test("`null` restores the real default", () => {
    setClaudeCliPtyAvailableForTests(true);
    setClaudeCliPtyAvailableForTests(null);
    expect(ptyIsAvailable()).toBe(false);
  });
});

/* -------------------------------------------------------------------------- */
/* startClaudeCliJob — the fixed command, never a caller-supplied one         */
/* -------------------------------------------------------------------------- */

describe("startClaudeCliJob — the fixed command, never a caller-supplied one", () => {
  test("spawns exactly `claude setup-token`, with no argument this module lets a caller change", () => {
    const child = fakeChild();
    startClaudeCliJob({ spawner: child.spawner });

    expect(child.calls.length).toBe(1);
    expect(child.calls[0]?.file).toBe(CLAUDE_CLI_BINARY);
    expect(child.calls[0]?.args).toEqual([...CLAUDE_CLI_SETUP_TOKEN_ARGS]);
  });

  test("returns a job id immediately — never awaits the child", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    expect(typeof jobId).toBe("string");
    expect(jobId.length).toBeGreaterThan(0);
    // The child has not exited (no emit* call yet) and the call above already
    // returned — proof this function does not block on the child.
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "running", authorizationUrl: null });
  });

  test("startClaudeCliJob takes no argument that could reach argv", () => {
    // Same tripwire as PR #263's `mintClaudeSetupToken.length` assertion: a
    // single defaulted `options` parameter contributes 0 to `.length`. A
    // second, non-defaulted parameter (the shape a body-derived argument
    // would take) would push this above 0.
    expect(startClaudeCliJob.length).toBe(0);
  });

  test("two jobs get two distinct ids", () => {
    const child = fakeChild();
    const first = startClaudeCliJob({ spawner: child.spawner });
    const second = startClaudeCliJob({ spawner: child.spawner });
    expect(first.jobId).not.toBe(second.jobId);
  });
});

/* -------------------------------------------------------------------------- */
/* Success                                                                    */
/* -------------------------------------------------------------------------- */

describe("a job that exits 0 with a usable candidate", () => {
  test("reports `pending_storage`, and the token is consumed exactly once", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });

    child.emitStdout("sk-ant-oat01-the-token\n");
    child.emitExit(0);

    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "pending_storage" });
    expect(takeClaudeCliJobToken(jobId)).toBe("sk-ant-oat01-the-token");
    expect(takeClaudeCliJobToken(jobId)).toBeNull();
    // Claimed but not yet stored: the same "keep polling" shape as running.
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "running", authorizationUrl: null });
  });

  test("a multi-line stdout (a status line, then the token) uses the LAST non-empty line", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStdout("Checking session...\n\nsk-ant-oat01-the-token\n");
    child.emitExit(0);
    expect(takeClaudeCliJobToken(jobId)).toBe("sk-ant-oat01-the-token");
  });

  test("setClaudeCliJobResult caches the outcome for every later poll", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStdout("sk-ant-oat01-the-token\n");
    child.emitExit(0);
    takeClaudeCliJobToken(jobId);

    setClaudeCliJobResult(jobId, { provider_id: "p1", credential_id: "c1", outcome: "created" });
    const snapshot = peekClaudeCliJob(jobId);
    expect(snapshot).toEqual({
      kind: "succeeded",
      result: { provider_id: "p1", credential_id: "c1", outcome: "created" },
    });
    // Idempotent: asking again does not change anything.
    expect(peekClaudeCliJob(jobId)).toEqual(snapshot);
  });

  test("setClaudeCliJobStorageFailed caches a keyed failure distinct from a process failure", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStdout("sk-ant-oat01-the-token\n");
    child.emitExit(0);
    takeClaudeCliJobToken(jobId);

    setClaudeCliJobStorageFailed(jobId, "console.claudeSubscription.list_truncated");
    expect(peekClaudeCliJob(jobId)).toEqual({
      kind: "storage_failed",
      messageKey: "console.claudeSubscription.list_truncated",
    });
  });

  test("stdout with only whitespace is `invalid_output`, not an empty token", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStdout("   \n\n  ");
    child.emitExit(0);
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "invalid_output" });
    expect(takeClaudeCliJobToken(jobId)).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* The required failure classifications                                       */
/* -------------------------------------------------------------------------- */

describe("process-level failure classification", () => {
  test("binary missing: a spawn error", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitSpawnError("ENOENT");
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "binary_missing" });
  });

  test("not-signed-in: a non-zero exit whose output matches the heuristic", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStderr("Error: you are not currently logged in. Run `claude login` first.");
    child.emitExit(1);
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "not_signed_in" });
  });

  test("not-signed-in is detected from stdout as well as stderr", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStdout("please authenticate before requesting a setup token");
    child.emitExit(1);
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "not_signed_in" });
  });

  test("a generic non-zero exit that does not match the not-signed-in heuristic", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner });
    child.emitStderr("unexpected internal error (code 500)");
    child.emitExit(1);
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "non_zero_exit" });
  });

  test("output_too_large: the child is killed and the job fails once the cap is exceeded", () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner, maxOutputBytes: 5 });
    child.emitStdout("123456");
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "output_too_large" });
    expect(child.killed).toContain("SIGTERM");
    // Further output after the kill must not resurrect the job.
    child.emitStdout("more");
    child.emitExit(0);
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "output_too_large" });
  });

  test("timeout: the budget expires while the child is still running", async () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner, budgetMs: 10, killGraceMs: 10 });
    await sleep(40);
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "process_failed", reason: "timeout" });
    expect(child.killed).toContain("SIGTERM");
  });

  test("timeout: SIGKILL follows if the child ignores SIGTERM past the grace period", async () => {
    const child = fakeChild();
    startClaudeCliJob({ spawner: child.spawner, budgetMs: 10, killGraceMs: 10 });
    await sleep(60);
    expect(child.killed).toEqual(["SIGTERM", "SIGKILL"]);
  });

  test("a job that exits on its own before its budget never times out", async () => {
    const child = fakeChild();
    const { jobId } = startClaudeCliJob({ spawner: child.spawner, budgetMs: 20 });
    child.emitStdout("sk-ant-oat01-the-token\n");
    child.emitExit(0);
    await sleep(40);
    // Still `pending_storage`, not overwritten by a late-firing budget timer.
    expect(peekClaudeCliJob(jobId)).toEqual({ kind: "pending_storage" });
  });
});

/* -------------------------------------------------------------------------- */
/* Unknown jobs                                                               */
/* -------------------------------------------------------------------------- */

describe("an unknown job id", () => {
  test("peekClaudeCliJob reports not_found", () => {
    expect(peekClaudeCliJob("does-not-exist")).toEqual({ kind: "not_found" });
  });

  test("takeClaudeCliJobToken returns null rather than throwing", () => {
    expect(takeClaudeCliJobToken("does-not-exist")).toBeNull();
  });

  test("setClaudeCliJobResult/setClaudeCliJobStorageFailed are no-ops, not throws", () => {
    expect(() => setClaudeCliJobResult("does-not-exist", { a: 1 })).not.toThrow();
    expect(() => setClaudeCliJobStorageFailed("does-not-exist", "some.key")).not.toThrow();
  });
});

/* -------------------------------------------------------------------------- */
/* extractSetupTokenCandidate                                                 */
/* -------------------------------------------------------------------------- */

describe("extractSetupTokenCandidate", () => {
  test("a single line is returned trimmed", () => {
    expect(extractSetupTokenCandidate("  sk-ant-oat01-abc  \n")).toBe("sk-ant-oat01-abc");
  });

  test("the last non-empty line wins when several are printed", () => {
    expect(extractSetupTokenCandidate("line one\n\nline two\nsk-ant-oat01-abc\n")).toBe(
      "sk-ant-oat01-abc",
    );
  });

  test("all-whitespace input yields an empty candidate", () => {
    expect(extractSetupTokenCandidate("\n  \n\t\n")).toBe("");
  });
});

/* -------------------------------------------------------------------------- */
/* extractAuthorizationUrlCandidate                                           */
/* -------------------------------------------------------------------------- */

describe("extractAuthorizationUrlCandidate", () => {
  test("no output yields null", () => {
    expect(extractAuthorizationUrlCandidate("")).toBeNull();
  });

  test("plain text with no URL yields null", () => {
    expect(extractAuthorizationUrlCandidate("Opening browser to sign in...")).toBeNull();
  });

  test("a plain https URL is extracted as-is", () => {
    expect(extractAuthorizationUrlCandidate("Visit https://example.com/authorize?a=1 to continue")).toBe(
      "https://example.com/authorize?a=1",
    );
  });

  test("an OSC 8 hyperlink escape sequence is truncated at the terminator, not swallowed whole", () => {
    // Shape observed from a real `claude setup-token` run under a pty:
    // `\x1b]8;id=…;URL\x1b\\` — the terminator glues directly onto the URL
    // with no separating whitespace.
    const raw = "Use the url below\n\x1b]8;id=abc;https://example.com/authorize?x=1\x1b\\";
    expect(extractAuthorizationUrlCandidate(raw)).toBe("https://example.com/authorize?x=1");
  });
});
