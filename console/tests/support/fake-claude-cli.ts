// A fake `claude` child process for `lib/claude-cli.ts`'s job registry —
// driven by the test rather than the OS. Shared by `claude-cli.test.ts` and
// the two `.../acquire/{start,status}` route test files so all three agree
// on what "the CLI ran and did X" looks like at the `ProcessSpawner` seam.
// Per the owner-approved testing policy (`plans/12-feature-expansion-
// brainstorm.md` §1), no test in this repository may spawn a real process.

import type { ProcessSpawner, SpawnedProcessHandle } from "@/lib/claude-cli";

export interface FakeClaudeChild {
  readonly spawner: ProcessSpawner;
  readonly calls: Array<{ readonly file: string; readonly args: readonly string[] }>;
  readonly killed: NodeJS.Signals[];
  emitStdout(text: string): void;
  emitStderr(text: string): void;
  emitExit(code: number | null, signal?: NodeJS.Signals | null): void;
  emitSpawnError(code: string | undefined): void;
}

export function fakeClaudeChild(): FakeClaudeChild {
  const calls: Array<{ file: string; args: readonly string[] }> = [];
  const killed: NodeJS.Signals[] = [];
  const stdoutListeners: Array<(chunk: Buffer) => void> = [];
  const stderrListeners: Array<(chunk: Buffer) => void> = [];
  const exitListeners: Array<(code: number | null, signal: NodeJS.Signals | null) => void> = [];
  const errorListeners: Array<(code: string | undefined) => void> = [];

  const handle: SpawnedProcessHandle = {
    onStdout: (listener) => stdoutListeners.push(listener),
    onStderr: (listener) => stderrListeners.push(listener),
    onExit: (listener) => exitListeners.push(listener),
    onSpawnError: (listener) => errorListeners.push(listener),
    kill: (signal) => killed.push(signal),
  };

  const spawner: ProcessSpawner = (file, args) => {
    calls.push({ file, args });
    return handle;
  };

  return {
    spawner,
    calls,
    killed,
    emitStdout: (text) => stdoutListeners.forEach((l) => l(Buffer.from(text, "utf8"))),
    emitStderr: (text) => stderrListeners.forEach((l) => l(Buffer.from(text, "utf8"))),
    emitExit: (code, signal = null) => exitListeners.forEach((l) => l(code, signal)),
    emitSpawnError: (code) => errorListeners.forEach((l) => l(code)),
  };
}
