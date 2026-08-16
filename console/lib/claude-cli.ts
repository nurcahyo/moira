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
// ISSUE #269 — WHY THIS MODULE IS A JOB REGISTRY, NOT A SINGLE BOUNDED CALL
// ============================================================================
//
// `claude setup-token` is INTERACTIVE: it opens a browser, starts a loopback
// listener, and waits for a human to finish an OAuth login — a process that
// takes minutes, not seconds. The version of this module PR #263 shipped ran
// it through `execFile` with a 20-SECOND timeout, on the theory that a
// mis-set timeout was the only bug. It was not: a request-scoped exec is the
// wrong SHAPE for a flow that needs human-speed interaction, at any timeout
// value. This version spawns the CLI DETACHED from any one HTTP request, in
// an in-process job registry keyed by job id, so the "start" request can
// return immediately and a separate "status" request can poll until the
// human finishes — see `app/api/settings/llm/claude-subscription/acquire/
// start/route.ts` and `.../status/route.ts`.
//
// ============================================================================
// WHAT THE INVESTIGATION FOUND, AND WHY IT CHANGES WHAT "WORKS" MEANS HERE
// ============================================================================
//
// Issue #269 also asked to evaluate a cheaper fix first: detect an ALREADY
// authenticated local CLI session and reuse its credential, avoiding the
// browser round trip entirely. `claude --help` documents `claude auth
// status`, which reports login state (email, org, subscription plan) — but
// never a token. There is no documented, non-interactive flag on `claude
// setup-token` itself (its own `--help` lists only `-h`), and no other
// documented command prints a reusable long-lived token. The only
// undocumented path — reading the CLI's own local credential store directly
// — is exactly the "reverse-engineered" shape the issue says to avoid. So
// there is no cheaper alternative to fall back on; the two-phase job is the
// only way forward found.
//
// Separately — and this is NOT something issue #269 anticipated — running
// `claude setup-token` with its stdout/stderr as plain pipes (exactly what
// `execFile`/`spawn` give you with no further work) produces ZERO bytes of
// output, for as long as you wait. Verified locally: piping stdin from
// /dev/null and capturing stdout+stderr for a full 10 seconds with no
// timeout at all yields 0 bytes on both streams. The CLI's interactive UI
// (built on Ink) requires a real TTY to render at all; it is not merely slow
// over a plain pipe, it is silent.
//
// TWO fixes for that were investigated, and BOTH were ruled out — this is
// worth spelling out because the first attempt looked promising before it
// was actually measured in the shape that matters.
//
//   1. The native `node-pty` package (allocate a real pty per spawn). Ruled
//      out on its own terms without needing to measure anything: it is a
//      native addon with no pure-JS equivalent, shipped into a distroless,
//      Trivy-CRITICAL/HIGH-gated production image (`console/Dockerfile`) for
//      a mode the console's own docs already scope as "local/dev-oriented,
//      not a fit for a console host shared across operators". That is an
//      architecture decision, not a bug fix, and this module does not make
//      it unilaterally — see this PR's description for the full reasoning.
//
//   2. Wrapping the spawn in the SYSTEM `script(1)` instead — zero new
//      dependencies, already on every macOS and Linux dev machine. This
//      looked like it worked: `script -q <file> claude setup-token` run
//      interactively from a real terminal DOES render the CLI's spinner and
//      authorization URL. But that test was run FROM a terminal — exactly
//      the condition a Next.js server process does not have. Measured from a
//      shape that actually matches how this module spawns a child (stdin NOT
//      a tty, as a server process's own stdin never is):
//
//          $ /usr/bin/script -q /dev/null claude setup-token
//          script: tcgetattr/ioctl: Operation not supported on socket
//
//      BSD `script` (macOS, and the BSDs) requires a controlling terminal
//      already attached to ITS OWN stdin — it does not allocate one from
//      nothing, it borrows the one it inherits. A server process has none,
//      so `script` fails before `claude` ever starts, on the exact platform
//      this is most likely to be run on locally. (util-linux `script -qec`
//      on Linux allocates its own pty independently and likely WOULD work —
//      but shipping a feature that silently only works on one platform and
//      silently fails on another is worse than shipping neither; a console
//      feature's behavior should not depend on which OS the operator's
//      laptop happens to run.)
//
// Consequence: `nodeProcessSpawner` is a plain, unwrapped pipe — the same
// shape PR #263 shipped, because no dependency-free way to give it a pty was
// found. `ptyIsAvailable()` says so honestly: it returns `false`
// unconditionally today, not a per-host capability probe, because there is
// currently no host shape (with or without `script`, with or without a real
// terminal) this module can turn into a working pty from inside a spawned
// server child. `.../acquire/start` calls it and refuses IMMEDIATELY —
// before ever creating a job — with a keyed message naming the
// `claude setup-token` + paste fallback. That is deliberately NOT "start a
// job, let it run the full multi-minute budget, then report failure": once
// success is known to be impossible, making the operator wait five minutes
// to be told so would be a slower, still-opaque version of the exact bug
// #269 exists to fix, one layer up. `setClaudeCliPtyAvailableForTests` lets
// tests override this to exercise the REST of the pipeline (the job
// registry, both routes, the panel's polling UI) as if a real pty existed —
// that machinery is unchanged, tested, and ready for the day `ptyIsAvailable`
// has something real to report; only today's answer is fixed at "no".
// `extractAuthorizationUrlCandidate` still scans captured output for a URL
// on every call for the same reason — inert today, no caller needs to change
// the day it stops being inert.
//
// What improves regardless of the pty question at all: `.../acquire/start`
// answers in milliseconds — either a job id, or (today, always) an honest
// refusal — instead of blocking for up to 20 seconds and then 409-ing with a
// message that named nothing actionable. The job registry itself still
// bounds a hypothetical run by wall-clock minutes rather than a 20-second
// `execFile` timeout, still bounds output, still kills and reaps a child
// rather than leaving one running past its budget, and still turns every
// failure into a keyed, ACTIONABLE message — never a bare 409. See
// `CONSOLE_MESSAGE_KEYS.claude_subscription_cli_pty_unavailable` and
// `claude_subscription_cli_timeout` in `catalog.en.ts`.
//
// ============================================================================
// WHAT THIS MODULE OWNS, AND WHAT IT DELIBERATELY DOES NOT
// ============================================================================
//
// This module is ONLY the process-execution boundary: spawn a fixed command,
// bound its time and output, and expose a small, closed set of job states. It
// does not know what a valid subscription token looks like
// (`resolveSubscriptionToken` in `lib/claude-subscription.ts` owns that) and
// it does not talk to Moira — the status route handler owns calling
// `connectClaudeSubscription` once a job's raw candidate is available, using
// ITS OWN freshly-authenticated `MoiraClient` (the one `withConsoleSession`
// hands it for that request), never a client captured at job-start time that
// could have gone stale across the minutes a login can take.
//
// ============================================================================
// THE THREE HARD RULES, AND HOW EACH IS MET (unchanged from PR #263)
// ============================================================================
//
//   1. NO USER-CONTROLLED INPUT REACHES THE COMMAND LINE. The argv is the
//      fixed literal `[CLAUDE_CLI_SETUP_TOKEN_ARGS]` below — nothing from a
//      request body, a query string, or any caller-supplied value is ever
//      concatenated into it. `startClaudeCliJob` takes no argument that could
//      influence argv; its only parameters are test seams and numeric bounds.
//   2. NO SHELL. The default spawner calls `node:child_process.spawn` with
//      `shell` left at its default (`false`): args are passed to the binary
//      as an argv array, never interpolated into a string a shell parses.
//   3. THE BINARY IS PATH-RESOLVED, NEVER A CALLER-SUPPLIED PATH.
//      `CLAUDE_CLI_BINARY` is the literal `"claude"` — no path separator —
//      so the OS resolves it against `PATH` exactly as typing `claude` at a
//      shell prompt would. Nothing in this module accepts a binary path from
//      configuration, a request, or an environment variable.

import "server-only";

import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";

/* -------------------------------------------------------------------------- */
/* The fixed command                                                          */
/* -------------------------------------------------------------------------- */

/** PATH-resolved, never a configurable or caller-supplied path. */
export const CLAUDE_CLI_BINARY = "claude";

/** The one subcommand this module ever runs. A fixed literal, not built. */
export const CLAUDE_CLI_SETUP_TOKEN_ARGS: readonly string[] = ["setup-token"];

/**
 * A human needs to open a browser, sign in, and consent — issue #269 asks
 * for "wall-clock minutes", plural, not a re-tuned seconds value. Killed and
 * reported as `timeout` past this many milliseconds of the job NOT having
 * exited on its own.
 */
export const CLAUDE_CLI_JOB_BUDGET_MS = 5 * 60_000;

/** SIGTERM first; SIGKILL after this much longer if the child is still alive. */
export const CLAUDE_CLI_JOB_KILL_GRACE_MS = 5_000;

/**
 * Combined stdout+stderr ceiling per job. A `setup-token` value is a few
 * hundred bytes; 64 KiB is generous headroom for a CLI that also prints a
 * spinner/status text, not an invitation to grow this ceiling if a future
 * version prints more — see `output_too_large`.
 */
export const CLAUDE_CLI_JOB_MAX_OUTPUT_BYTES = 64 * 1024;

/**
 * How long a job's record survives in the in-process registry after it
 * reaches a terminal state, before being swept. Long enough that a slow
 * client still gets the final poll's answer; short enough that an abandoned
 * job (closed tab, crashed browser) does not hold its buffers forever.
 */
export const CLAUDE_CLI_JOB_RETENTION_MS = 10 * 60_000;

/* -------------------------------------------------------------------------- */
/* The spawner seam                                                           */
/* -------------------------------------------------------------------------- */

/**
 * A running (or just-exited) child, narrowed to what this module needs.
 * Injectable so a test never spawns a real process — see
 * `setClaudeCliSpawnerForTests`.
 */
export interface SpawnedProcessHandle {
  onStdout(listener: (chunk: Buffer) => void): void;
  onStderr(listener: (chunk: Buffer) => void): void;
  /** Fires once, on normal exit. Never fires after `onSpawnError` has. */
  onExit(listener: (code: number | null, signal: NodeJS.Signals | null) => void): void;
  /** Fires for a launch failure (e.g. ENOENT) instead of `onExit`. */
  onSpawnError(listener: (code: string | undefined) => void): void;
  kill(signal: NodeJS.Signals): void;
}

export type ProcessSpawner = (file: string, args: readonly string[]) => SpawnedProcessHandle;

/**
 * The shipped spawner. Plain pipes — see this module's header for the full
 * investigation (plain pipe, then `script(1)`, both ruled out) into why that
 * means `claude setup-token`'s interactive UI renders nothing today, and why
 * that is a documented limitation rather than a bug this module can fix on
 * its own.
 */
export const nodeProcessSpawner: ProcessSpawner = (file, args) => {
  const child: ChildProcess = spawn(file, args as string[], {
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  return {
    onStdout: (listener) => {
      child.stdout?.on("data", listener);
    },
    onStderr: (listener) => {
      child.stderr?.on("data", listener);
    },
    onExit: (listener) => {
      child.on("exit", listener);
    },
    onSpawnError: (listener) => {
      child.on("error", (error) => listener((error as NodeJS.ErrnoException).code));
    },
    kill: (signal) => {
      child.kill(signal);
    },
  };
};

/** Process-wide test seam. `null` restores the real spawner. */
let spawnerOverride: ProcessSpawner | null = null;

/** Test seam. Never called from shipped code paths. */
export function setClaudeCliSpawnerForTests(spawner: ProcessSpawner | null): void {
  spawnerOverride = spawner;
}

/* -------------------------------------------------------------------------- */
/* Whether this host can run the interactive step at all                      */
/* -------------------------------------------------------------------------- */

/**
 * `null` (the default) means "no test override — use the real, fixed
 * answer". Distinguished from `false` so a test can also explicitly force
 * the unavailable path without relying on the production default.
 */
let ptyAvailableOverride: boolean | null = null;

/** Test seam. Never called from shipped code paths. */
export function setClaudeCliPtyAvailableForTests(value: boolean | null): void {
  ptyAvailableOverride = value;
}

/**
 * Whether this host can give `claude setup-token` a real terminal to render
 * its interactive UI into. An honest capability statement, not a per-host
 * probe: see this module's header for the investigation this answer is
 * pinned by (plain pipe: silent; `script(1)`: requires a controlling
 * terminal this server process does not have, on the platform this is most
 * likely to run on). There is currently no host shape this module can turn
 * into a working answer other than `false` — that is fixed today, not
 * computed, and stays fixed until a real pty-capable executor exists.
 *
 * `.../acquire/start` calls this BEFORE ever creating a job and refuses
 * immediately when it is `false`, rather than starting a job that can only
 * ever expire — see `CLAUDE_CLI_JOB_BUDGET_MS`'s own callers.
 *
 * `setClaudeCliPtyAvailableForTests` lets a test force `true` to exercise
 * the REST of this module (the job registry) and both routes as if a real
 * pty-capable spawner existed, without needing one — that machinery is
 * unchanged by this answer being fixed at `false` today.
 */
export function ptyIsAvailable(): boolean {
  return ptyAvailableOverride ?? false;
}

/* -------------------------------------------------------------------------- */
/* Classifying what the process produced                                      */
/* -------------------------------------------------------------------------- */

export type ClaudeCliJobFailureReason =
  | "binary_missing"
  | "not_signed_in"
  | "timeout"
  | "output_too_large"
  | "non_zero_exit"
  | "invalid_output";

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

function classifyExitFailure(stdout: string, stderr: string): ClaudeCliJobFailureReason {
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
 * case and the defensive case are the same code path.
 */
export function extractSetupTokenCandidate(stdout: string): string {
  const lines = stdout
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  return lines.length > 0 ? lines[lines.length - 1]! : "";
}

/**
 * Best-effort authorization-URL scan over captured output. Inert today (the
 * shipped spawner captures nothing to scan — see this module's header and
 * `ptyIsAvailable`), kept so a future pty-capable executor needs no change
 * here or in either route handler to start actually surfacing a URL.
 *
 * Stops at whitespace, an ESC byte, a BEL byte, or `]` — a raw OSC 8
 * hyperlink escape (`\x1b]8;id=…;URL\x1b\\`, or BEL-terminated as
 * `\x1b]8;id=…;URL\x07`) glues its terminator directly onto the URL with no
 * separating whitespace, and none of ESC/BEL/`]` is a legal unencoded URL
 * character, so all three are safe boundaries.
 */
export function extractAuthorizationUrlCandidate(combinedOutput: string): string | null {
  const match = /https?:\/\/[^\s\x1b\x07\]]+/.exec(combinedOutput);
  return match === null ? null : match[0];
}

/* -------------------------------------------------------------------------- */
/* The job registry                                                           */
/* -------------------------------------------------------------------------- */

type JobState =
  | { readonly kind: "running" }
  | { readonly kind: "process_failed"; readonly reason: ClaudeCliJobFailureReason }
  /** Exited 0 with a usable candidate. `token` is null once a status poll has claimed it. */
  | { readonly kind: "exited_ok"; token: string | null }
  | { readonly kind: "storage_failed"; readonly messageKey: string }
  | { readonly kind: "stored"; readonly result: Readonly<Record<string, unknown>> };

interface JobRecord {
  readonly id: string;
  readonly handle: SpawnedProcessHandle;
  stdout: string;
  stderr: string;
  outputBytes: number;
  state: JobState;
  budgetTimer: ReturnType<typeof setTimeout> | null;
  killTimer: ReturnType<typeof setTimeout> | null;
  evictionTimer: ReturnType<typeof setTimeout> | null;
}

const registry = new Map<string, JobRecord>();

export type ClaudeCliJobSnapshot =
  | { readonly kind: "not_found" }
  | { readonly kind: "running"; readonly authorizationUrl: string | null }
  | { readonly kind: "process_failed"; readonly reason: ClaudeCliJobFailureReason }
  | { readonly kind: "storage_failed"; readonly messageKey: string }
  | { readonly kind: "succeeded"; readonly result: Readonly<Record<string, unknown>> }
  /** Exited 0; a status poll must call `takeClaudeCliJobToken` and store it. */
  | { readonly kind: "pending_storage" };

export interface StartClaudeCliJobOptions {
  readonly spawner?: ProcessSpawner;
  readonly budgetMs?: number;
  readonly maxOutputBytes?: number;
  readonly killGraceMs?: number;
}

/**
 * Spawn `claude setup-token`, register it, and return its job id IMMEDIATELY
 * — this function never awaits the child. Takes no argument that could reach
 * argv (see this module's header, rule 1); every parameter is a bound or a
 * test seam.
 */
export function startClaudeCliJob(options: StartClaudeCliJobOptions = {}): { readonly jobId: string } {
  const spawner = options.spawner ?? spawnerOverride ?? nodeProcessSpawner;
  const budgetMs = options.budgetMs ?? CLAUDE_CLI_JOB_BUDGET_MS;
  const maxOutputBytes = options.maxOutputBytes ?? CLAUDE_CLI_JOB_MAX_OUTPUT_BYTES;
  const killGraceMs = options.killGraceMs ?? CLAUDE_CLI_JOB_KILL_GRACE_MS;

  ensureShutdownHandlersRegistered();

  const jobId = randomUUID();
  const handle = spawner(CLAUDE_CLI_BINARY, CLAUDE_CLI_SETUP_TOKEN_ARGS);
  const record: JobRecord = {
    id: jobId,
    handle,
    stdout: "",
    stderr: "",
    outputBytes: 0,
    state: { kind: "running" },
    budgetTimer: null,
    killTimer: null,
    evictionTimer: null,
  };
  registry.set(jobId, record);

  function clearBudget(): void {
    if (record.budgetTimer !== null) {
      clearTimeout(record.budgetTimer);
      record.budgetTimer = null;
    }
  }

  function killChild(): void {
    try {
      handle.kill("SIGTERM");
    } catch {
      // Already gone.
    }
    record.killTimer = setTimeout(() => {
      try {
        handle.kill("SIGKILL");
      } catch {
        // Already gone.
      }
    }, killGraceMs);
  }

  function scheduleEviction(): void {
    record.evictionTimer = setTimeout(() => {
      registry.delete(jobId);
    }, CLAUDE_CLI_JOB_RETENTION_MS);
  }

  function finishProcessFailed(reason: ClaudeCliJobFailureReason): void {
    if (record.state.kind !== "running") return; // Already terminal.
    clearBudget();
    record.state = { kind: "process_failed", reason };
    scheduleEviction();
  }

  function appendOutput(target: "stdout" | "stderr", chunk: Buffer): void {
    if (record.state.kind !== "running") return;
    // Any chunk that would PUSH the total past the cap fails the job
    // immediately, exactly like `execFile`'s own `maxBuffer` — this must not
    // silently truncate a chunk instead: slicing a `Buffer` at an arbitrary
    // byte offset can split a multi-byte UTF-8 character, and a single
    // oversized write must not be treated as "fits, barely" just because
    // nothing had been captured yet.
    if (record.outputBytes + chunk.byteLength > maxOutputBytes) {
      killChild();
      finishProcessFailed("output_too_large");
      return;
    }
    record[target] += chunk.toString("utf8");
    record.outputBytes += chunk.byteLength;
  }

  handle.onStdout((chunk) => appendOutput("stdout", chunk));
  handle.onStderr((chunk) => appendOutput("stderr", chunk));

  handle.onSpawnError(() => {
    // This module's shipped spawner never emits a spawn error for anything
    // other than ENOENT — see `nodeProcessSpawner` — so every spawn error is
    // reported the same way.
    finishProcessFailed("binary_missing");
  });

  handle.onExit((code) => {
    // The process is confirmed dead either way — no SIGKILL fallback needed.
    if (record.killTimer !== null) {
      clearTimeout(record.killTimer);
      record.killTimer = null;
    }
    if (record.state.kind !== "running") return; // Already handled (kill raced the natural exit).
    clearBudget();
    if (code === 0) {
      const candidate = extractSetupTokenCandidate(record.stdout);
      record.state =
        candidate === "" ? { kind: "process_failed", reason: "invalid_output" } : { kind: "exited_ok", token: candidate };
    } else {
      record.state = { kind: "process_failed", reason: classifyExitFailure(record.stdout, record.stderr) };
    }
    scheduleEviction();
  });

  record.budgetTimer = setTimeout(() => {
    if (record.state.kind !== "running") return;
    killChild();
    finishProcessFailed("timeout");
  }, budgetMs);

  return { jobId };
}

/** Read-only. Never exposes a raw token — see `takeClaudeCliJobToken`. */
export function peekClaudeCliJob(jobId: string): ClaudeCliJobSnapshot {
  const record = registry.get(jobId);
  if (record === undefined) return { kind: "not_found" };

  switch (record.state.kind) {
    case "running":
      return {
        kind: "running",
        authorizationUrl: extractAuthorizationUrlCandidate(record.stdout + record.stderr),
      };
    case "process_failed":
      return { kind: "process_failed", reason: record.state.reason };
    case "storage_failed":
      return { kind: "storage_failed", messageKey: record.state.messageKey };
    case "stored":
      return { kind: "succeeded", result: record.state.result };
    case "exited_ok":
      // `token === null` means a concurrent status poll already claimed it
      // and is (or was) mid-store; report the same shape as "still working"
      // rather than a fourth client-visible state for what is, from the
      // browser's point of view, the same "keep polling" instruction.
      return record.state.token === null ? { kind: "running", authorizationUrl: null } : { kind: "pending_storage" };
  }
}

/**
 * Consume the raw candidate exactly once. Returns `null` if the job is not
 * in `pending_storage` (including: already claimed by a concurrent poll).
 * Server-side only, and never logged — the caller (`.../acquire/status`)
 * feeds this straight into `resolveSubscriptionToken`.
 */
export function takeClaudeCliJobToken(jobId: string): string | null {
  const record = registry.get(jobId);
  if (record === undefined || record.state.kind !== "exited_ok" || record.state.token === null) return null;
  const token = record.state.token;
  record.state = { kind: "exited_ok", token: null };
  return token;
}

/** Cache the storage outcome so a repeated poll returns the same answer without re-storing. */
export function setClaudeCliJobResult(jobId: string, result: Readonly<Record<string, unknown>>): void {
  const record = registry.get(jobId);
  if (record === undefined) return;
  record.state = { kind: "stored", result };
  record.evictionTimer = setTimeout(() => registry.delete(jobId), CLAUDE_CLI_JOB_RETENTION_MS);
}

/** Cache a storage failure. `messageKey` travels verbatim from `ClaudeSubscriptionError`/`resolveSubscriptionToken`. */
export function setClaudeCliJobStorageFailed(jobId: string, messageKey: string): void {
  const record = registry.get(jobId);
  if (record === undefined) return;
  record.state = { kind: "storage_failed", messageKey };
  record.evictionTimer = setTimeout(() => registry.delete(jobId), CLAUDE_CLI_JOB_RETENTION_MS);
}

/** Test seam: drop every tracked job and cancel its timers, without touching the spawner override. */
export function resetClaudeCliJobRegistryForTests(): void {
  for (const record of registry.values()) {
    if (record.budgetTimer !== null) clearTimeout(record.budgetTimer);
    if (record.killTimer !== null) clearTimeout(record.killTimer);
    if (record.evictionTimer !== null) clearTimeout(record.evictionTimer);
  }
  registry.clear();
}

/* -------------------------------------------------------------------------- */
/* Orphan cleanup on console shutdown                                         */
/* -------------------------------------------------------------------------- */

/**
 * Nothing else in this codebase registers a `SIGTERM`/`SIGINT` listener
 * (checked before writing this). Node only skips its own default
 * immediate-exit behavior on these signals once ANY listener is attached, so
 * this handler calls `process.exit()` itself at the end — otherwise adding
 * it would make the console outlive a `SIGTERM` it used to shut down on
 * immediately, past whatever grace period an orchestrator (Kubernetes) gives
 * it before a hard `SIGKILL`. If a future shutdown orchestrator is added
 * elsewhere in this app, fold this into it rather than layering a second
 * competing `process.exit()` call.
 */
let shutdownHandlersRegistered = false;

function ensureShutdownHandlersRegistered(): void {
  if (shutdownHandlersRegistered) return;
  shutdownHandlersRegistered = true;

  const handleShutdown = (signal: NodeJS.Signals): void => {
    for (const record of registry.values()) {
      if (record.state.kind === "running") {
        try {
          record.handle.kill("SIGKILL");
        } catch {
          // Already gone.
        }
      }
    }
    process.exit(signal === "SIGINT" ? 130 : 143);
  };

  process.once("SIGTERM", () => handleShutdown("SIGTERM"));
  process.once("SIGINT", () => handleShutdown("SIGINT"));
}
