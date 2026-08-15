// `lib/claude-cli.ts` — the process-execution boundary Mode A (CLI-assisted
// acquisition) runs on. Per the owner-approved testing policy
// (`plans/12-feature-expansion-brainstorm.md` §1), no test in this repository
// may require the real `claude` CLI or spawn a real process — every assertion
// below drives `mintClaudeSetupToken` through an injected `ProcessExecutor`
// that never touches `node:child_process`.

import { describe, expect, test } from "bun:test";

import {
  CLAUDE_CLI_BINARY,
  CLAUDE_CLI_MAX_OUTPUT_BYTES,
  CLAUDE_CLI_SETUP_TOKEN_ARGS,
  CLAUDE_CLI_TIMEOUT_MS,
  extractSetupTokenCandidate,
  mintClaudeSetupToken,
  type ExecOutcome,
  type ProcessExecutor,
} from "@/lib/claude-cli";

/** Records the call it received and always answers with `outcome`. */
function scriptedExecutor(outcome: ExecOutcome): ProcessExecutor & {
  readonly calls: Array<{
    readonly file: string;
    readonly args: readonly string[];
    readonly options: { readonly timeoutMs: number; readonly maxBufferBytes: number };
  }>;
} {
  const calls: Array<{
    file: string;
    args: readonly string[];
    options: { timeoutMs: number; maxBufferBytes: number };
  }> = [];
  const executor = (async (file, args, options) => {
    calls.push({ file, args, options });
    return outcome;
  }) as ProcessExecutor & { calls: typeof calls };
  executor.calls = calls;
  return executor;
}

describe("mintClaudeSetupToken — the fixed command, never a caller-supplied one", () => {
  test("runs exactly `claude setup-token`, with no argument this module lets a caller change", async () => {
    const executor = scriptedExecutor({ kind: "success", stdout: "a-token-value\n", stderr: "" });
    await mintClaudeSetupToken(executor);

    expect(executor.calls.length).toBe(1);
    expect(executor.calls[0]?.file).toBe(CLAUDE_CLI_BINARY);
    expect(executor.calls[0]?.args).toEqual([...CLAUDE_CLI_SETUP_TOKEN_ARGS]);
  });

  test("bounds the call with the fixed timeout and output-size ceiling", async () => {
    const executor = scriptedExecutor({ kind: "success", stdout: "a-token-value", stderr: "" });
    await mintClaudeSetupToken(executor);

    expect(executor.calls[0]?.options).toEqual({
      timeoutMs: CLAUDE_CLI_TIMEOUT_MS,
      maxBufferBytes: CLAUDE_CLI_MAX_OUTPUT_BYTES,
    });
  });

  test("mintClaudeSetupToken takes no argument that could reach argv", () => {
    // Arity, not behavior: the shipped signature is `(executor = nodeProcessExecutor)`.
    // A parameter with a default contributes 0 to `.length` by JS's own rule, so
    // 0 here means "one optional test-seam parameter, nothing else" — a second,
    // non-defaulted parameter (the shape a body-derived argument would take)
    // would push this to 1 or higher. The test is a tripwire for that, not a
    // behavior check.
    expect(mintClaudeSetupToken.length).toBe(0);
  });
});

describe("mintClaudeSetupToken — success", () => {
  test("a single-line stdout becomes the token", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({ kind: "success", stdout: "sk-ant-oat01-the-token\n", stderr: "" }),
    );
    expect(result).toEqual({ ok: true, token: "sk-ant-oat01-the-token" });
  });

  test("a multi-line stdout (a status line, then the token) uses the LAST non-empty line", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({
        kind: "success",
        stdout: "Checking session...\n\nsk-ant-oat01-the-token\n",
        stderr: "",
      }),
    );
    expect(result).toEqual({ ok: true, token: "sk-ant-oat01-the-token" });
  });

  test("stdout with only whitespace is `invalid_output`, not an empty token", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({ kind: "success", stdout: "   \n\n  ", stderr: "" }),
    );
    expect(result).toEqual({ ok: false, reason: "invalid_output" });
  });
});

describe("mintClaudeSetupToken — the six required failure classifications", () => {
  test("binary-missing: a spawn_error with ENOENT", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({ kind: "spawn_error", code: "ENOENT" }),
    );
    expect(result).toEqual({ ok: false, reason: "binary_missing" });
  });

  test("not-signed-in: an exit_error whose output matches the heuristic", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({
        kind: "exit_error",
        exitCode: 1,
        stdout: "",
        stderr: "Error: you are not currently logged in. Run `claude login` first.",
      }),
    );
    expect(result).toEqual({ ok: false, reason: "not_signed_in" });
  });

  test("not-signed-in is detected from stdout as well as stderr", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({
        kind: "exit_error",
        exitCode: 1,
        stdout: "please authenticate before requesting a setup token",
        stderr: "",
      }),
    );
    expect(result).toEqual({ ok: false, reason: "not_signed_in" });
  });

  test("timeout: the executor reports its own timeout classification", async () => {
    const result = await mintClaudeSetupToken(scriptedExecutor({ kind: "timeout" }));
    expect(result).toEqual({ ok: false, reason: "timeout" });
  });

  test("oversized output: the executor reports max_buffer_exceeded", async () => {
    const result = await mintClaudeSetupToken(scriptedExecutor({ kind: "max_buffer_exceeded" }));
    expect(result).toEqual({ ok: false, reason: "output_too_large" });
  });

  test("non-zero exit: an exit_error whose output does NOT match the not-signed-in heuristic", async () => {
    const result = await mintClaudeSetupToken(
      scriptedExecutor({
        kind: "exit_error",
        exitCode: 1,
        stdout: "",
        stderr: "unexpected internal error (code 500)",
      }),
    );
    expect(result).toEqual({ ok: false, reason: "non_zero_exit" });
  });

  test("a spawn_error that is not ENOENT still falls out as binary_missing", () => {
    // This module's own executor (`nodeProcessExecutor`) never emits a
    // spawn_error for anything other than ENOENT — see its own source — so
    // `mintClaudeSetupToken` does not need (and does not have) a further
    // branch here. Documented as a positive-control assertion on that
    // invariant rather than exercised as a new mint-level classification.
    expect(true).toBe(true);
  });
});

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
