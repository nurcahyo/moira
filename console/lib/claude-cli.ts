// @server-only
//
// Mint a Claude subscription token by running the locally installed,
// already-authenticated `claude` CLI on the console host — `claude
// setup-token` — rather than asking an operator to run it themselves and
// paste the result. See `docs/claude-subscription-sidecar.md` and
// `plans/12-feature-expansion-brainstorm.md` §1 for why this exists and why
// it is the ONLY console-driven acquisition mode: there is no third-party
// OAuth client id for Claude, so a browser PKCE flow would have to either
// impersonate Claude Code's own client id (blocked by Anthropic since
// ~Jan 2026, and wrong to build even if it were not) or invent one that does
// not exist. Running the real, already-authenticated CLI sidesteps that
// entirely — the real Claude client performs the authentication, this module
// only asks it for a token.
//
// ============================================================================
// WHAT THIS MODULE OWNS, AND WHAT IT DELIBERATELY DOES NOT
// ============================================================================
//
// This module is ONLY the process-execution boundary: run a fixed command,
// bound its time and output, and classify what came back into one of a small,
// closed set of outcomes. It does not know what a valid subscription token
// looks like (`resolveSubscriptionToken` in `lib/claude-subscription.ts` owns
// that — the caller feeds this module's raw candidate through it, exactly as
// it already does for a pasted token) and it does not talk to Moira.
//
// ============================================================================
// THE THREE HARD RULES, AND HOW EACH IS MET
// ============================================================================
//
//   1. NO USER-CONTROLLED INPUT REACHES THE COMMAND LINE. The argv is the
//      fixed literal `[CLAUDE_CLI_SETUP_TOKEN_ARGS]` below — nothing from a
//      request body, a query string, or any caller-supplied value is ever
//      concatenated into it. `mintClaudeSetupToken` takes no argument that
//      could influence argv; its only parameter is a test seam for the
//      executor itself.
//   2. NO SHELL. The default executor calls `node:child_process.execFile`
//      with `shell` left at its default (`false`): args are passed to the
//      binary as an argv array, never interpolated into a string a shell
//      parses. There is no metacharacter, quoting, or injection surface
//      because there is no shell in the loop at all.
//   3. THE BINARY IS PATH-RESOLVED, NEVER A CALLER-SUPPLIED PATH.
//      `CLAUDE_CLI_BINARY` is the literal `"claude"` — no path separator — so
//      the OS resolves it against `PATH` exactly as typing `claude` at a
//      shell prompt would (execFile with no `shell` option still performs
//      this resolution; it is not a shell feature). Nothing in this module
//      accepts a binary path from configuration, a request, or an
//      environment variable: the console does not get to choose which
//      `claude` runs, the operator's `PATH` does.
//
// ============================================================================
// BOUNDED, ALWAYS
// ============================================================================
//
// `execFile`'s own `timeout` and `maxBuffer` options do the bounding — this
// module does not hand-roll process supervision. A `claude` invocation that
// hangs (for instance, if some future CLI version tries to open a browser for
// an interactive step instead of failing outright when the session is
// stale) is killed at `CLAUDE_CLI_TIMEOUT_MS` rather than left running, and
// output beyond `CLAUDE_CLI_MAX_OUTPUT_BYTES` truncates the process rather
// than growing this process's memory unbounded.

import "server-only";

import { execFile, type ExecFileException } from "node:child_process";

/* -------------------------------------------------------------------------- */
/* The fixed command                                                          */
/* -------------------------------------------------------------------------- */

/** PATH-resolved, never a configurable or caller-supplied path. */
export const CLAUDE_CLI_BINARY = "claude";

/** The one subcommand this module ever runs. A fixed literal, not built. */
export const CLAUDE_CLI_SETUP_TOKEN_ARGS: readonly string[] = ["setup-token"];

/** Killed and reported as a timeout past this many milliseconds. */
export const CLAUDE_CLI_TIMEOUT_MS = 20_000;

/**
 * A `setup-token` value is a few hundred bytes. 64 KiB is generous headroom
 * for a CLI that also prints a status line or two, not an invitation to grow
 * this ceiling if a future version prints more — see `output_too_large`.
 */
export const CLAUDE_CLI_MAX_OUTPUT_BYTES = 64 * 1024;

/* -------------------------------------------------------------------------- */
/* The executor seam                                                          */
/* -------------------------------------------------------------------------- */

/**
 * What running the fixed command produced, ALREADY CLASSIFIED into a closed
 * set of shapes — never a raw Node error. Keeping this the executor's return
 * type (rather than a thrown error `mintClaudeSetupToken` has to interpret)
 * is what makes `mintClaudeSetupToken`'s own logic independent of exactly how
 * Node happens to shape a given failure in a given version: the DEFAULT
 * executor (`nodeProcessExecutor`) does that interpretation once, in one
 * place, and every test below it supplies one of these shapes directly.
 */
export type ExecOutcome =
  | { readonly kind: "success"; readonly stdout: string; readonly stderr: string }
  | { readonly kind: "spawn_error"; readonly code: string | undefined }
  | { readonly kind: "timeout" }
  | { readonly kind: "max_buffer_exceeded" }
  | { readonly kind: "exit_error"; readonly exitCode: number | null; readonly stdout: string; readonly stderr: string };

export interface ProcessExecutorOptions {
  readonly timeoutMs: number;
  readonly maxBufferBytes: number;
}

/** Injectable so a test never spawns a real process. */
export type ProcessExecutor = (
  file: string,
  args: readonly string[],
  options: ProcessExecutorOptions,
) => Promise<ExecOutcome>;

/**
 * The shipped executor. Wraps `child_process.execFile` and classifies
 * whatever Node hands back — this is the ONLY function in this module that
 * touches a real process, and the only one not exercised by mocked unit
 * tests (per the owner-approved testing policy,
 * `plans/12-feature-expansion-brainstorm.md` §1: no test in this repository
 * may require the real `claude` CLI).
 */
export const nodeProcessExecutor: ProcessExecutor = (file, args, options) =>
  new Promise((resolve) => {
    execFile(
      file,
      args as string[],
      { timeout: options.timeoutMs, maxBuffer: options.maxBufferBytes, windowsHide: true },
      (error, stdout, stderr) => {
        if (error === null) {
          resolve({ kind: "success", stdout, stderr });
          return;
        }
        const err = error as ExecFileException;
        if (err.code === "ENOENT") {
          resolve({ kind: "spawn_error", code: err.code });
          return;
        }
        // Node sets `.code` to this string when `maxBuffer` is exceeded (and,
        // on older runtimes that do not set the code, the message still names
        // it) — checked BEFORE `killed`, because exceeding maxBuffer also
        // kills the child, and this branch must win that race.
        if (err.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" || /maxBuffer/i.test(err.message)) {
          resolve({ kind: "max_buffer_exceeded" });
          return;
        }
        if (err.killed === true || err.code === "ETIMEDOUT") {
          resolve({ kind: "timeout" });
          return;
        }
        const exitCode = typeof err.code === "number" ? err.code : null;
        resolve({ kind: "exit_error", exitCode, stdout, stderr });
      },
    );
  });

/* -------------------------------------------------------------------------- */
/* Classifying a failure into an ACTIONABLE, keyed reason                     */
/* -------------------------------------------------------------------------- */

export type ClaudeCliMintReason =
  | "binary_missing"
  | "not_signed_in"
  | "timeout"
  | "output_too_large"
  | "non_zero_exit"
  | "invalid_output";

export type ClaudeCliMintResult =
  | { readonly ok: true; readonly token: string }
  | { readonly ok: false; readonly reason: ClaudeCliMintReason };

/**
 * Best-effort. This console does not control the `claude` CLI's exact
 * wording and has no API contract with it beyond "prints a token on
 * success" — this is a heuristic over the combined stdout/stderr of a
 * NON-ZERO-EXIT run, not a claim that every future CLI version's "you are
 * not signed in" message is covered. A refusal that misses this pattern
 * still surfaces as `non_zero_exit`, which is still a keyed, actionable
 * refusal — never a raw dump of what the CLI printed.
 */
const NOT_SIGNED_IN_PATTERN =
  /not\s+(?:currently\s+)?(?:logged|signed)\s+in|claude\s+login|no\s+active\s+session|please\s+authenticate|authentication\s+required/i;

function classifyExitFailure(stdout: string, stderr: string): ClaudeCliMintReason {
  return NOT_SIGNED_IN_PATTERN.test(stdout) || NOT_SIGNED_IN_PATTERN.test(stderr)
    ? "not_signed_in"
    : "non_zero_exit";
}

/**
 * `claude setup-token`'s stdout, narrowed to the one line that is plausibly
 * the token.
 *
 * The documented contract is "prints a long-lived token"
 * (`docs/claude-subscription-sidecar.md`), which in the simple case is the
 * whole of stdout. Some CLI versions print a status line or two first, so
 * this takes the LAST non-empty trimmed line rather than the whole blob —
 * if there is exactly one line, that is also "the last line", so the common
 * case and the defensive case are the same code path. The result is handed
 * to `resolveSubscriptionToken` by the caller, which is what actually
 * decides whether it is well-formed; this function only picks a candidate.
 */
export function extractSetupTokenCandidate(stdout: string): string {
  const lines = stdout
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  return lines.length > 0 ? lines[lines.length - 1]! : "";
}

/**
 * Process-wide test seam, mirroring `setConsoleApiDependenciesForTests`'s
 * shape: a route-handler test cannot pass an explicit `executor` argument
 * (the route calls `mintClaudeSetupToken()` with none, by design — see the
 * acquire route's own header), so route-level tests substitute the default
 * here instead. `pass:null` restores the real one.
 */
let executorOverride: ProcessExecutor | null = null;

/** Test seam. Never called from shipped code paths. */
export function setClaudeCliExecutorForTests(executor: ProcessExecutor | null): void {
  executorOverride = executor;
}

/**
 * Run `claude setup-token` and classify the result.
 *
 * Takes NO caller-supplied argument that could reach argv — see this
 * module's header, rule 1. `executor` is a test seam only; every shipped
 * call site uses the default, which is `executorOverride` when a test has
 * set one and `nodeProcessExecutor` otherwise.
 */
export async function mintClaudeSetupToken(
  executor: ProcessExecutor = executorOverride ?? nodeProcessExecutor,
): Promise<ClaudeCliMintResult> {
  const outcome = await executor(CLAUDE_CLI_BINARY, CLAUDE_CLI_SETUP_TOKEN_ARGS, {
    timeoutMs: CLAUDE_CLI_TIMEOUT_MS,
    maxBufferBytes: CLAUDE_CLI_MAX_OUTPUT_BYTES,
  });

  switch (outcome.kind) {
    case "spawn_error":
      // ENOENT is the only spawn_error this module's executor emits — see
      // `nodeProcessExecutor` — so this is always "the binary is absent",
      // never a different spawn failure silently relabelled.
      return { ok: false, reason: "binary_missing" };
    case "timeout":
      return { ok: false, reason: "timeout" };
    case "max_buffer_exceeded":
      return { ok: false, reason: "output_too_large" };
    case "exit_error":
      return { ok: false, reason: classifyExitFailure(outcome.stdout, outcome.stderr) };
    case "success": {
      const candidate = extractSetupTokenCandidate(outcome.stdout);
      if (candidate === "") return { ok: false, reason: "invalid_output" };
      return { ok: true, token: candidate };
    }
  }
}
