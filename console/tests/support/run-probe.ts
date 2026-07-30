// Spawn `durability-probe.ts` as a real, separate process and read its answer.
//
// The separation is the evidence. Everything the second run can see is what
// PostgreSQL kept, because there is no other channel between the two: no shared
// module registry, no shared heap, no shared `Map`.
import { join } from "node:path";

const CONSOLE_ROOT = join(import.meta.dir, "..", "..");
const PROBE = join(CONSOLE_ROOT, "tests", "support", "durability-probe.ts");
const SHIM = join(CONSOLE_ROOT, "tests", "support", "server-only-runtime-shim.ts");

export interface ProbeResult {
  readonly ok: boolean;
  readonly [key: string]: unknown;
}

/**
 * Run one probe command.
 *
 * `--preload` installs the `server-only` neutraliser; without it every module
 * the probe imports throws at import time.
 *
 * The environment is passed through so the probe inherits PATH and the Bun
 * install, but nothing about the store is inherited: every value the probe uses
 * arrives on `argv`.
 */
export async function runProbe(args: readonly string[]): Promise<ProbeResult> {
  const proc = Bun.spawn(["bun", "--preload", SHIM, PROBE, ...args], {
    cwd: CONSOLE_ROOT,
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);

  const line = stdout.trim().split("\n").at(-1) ?? "";
  let parsed: ProbeResult;
  try {
    parsed = JSON.parse(line) as ProbeResult;
  } catch {
    throw new Error(
      `probe \`${args.join(" ")}\` produced no JSON (exit ${exitCode}).\n` +
        `stdout: ${stdout}\nstderr: ${stderr}`,
    );
  }
  if (!parsed.ok) {
    throw new Error(
      `probe \`${args.join(" ")}\` failed (exit ${exitCode}): ${String(parsed["error"])}\n${stderr}`,
    );
  }
  return parsed;
}
