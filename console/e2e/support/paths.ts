import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Absolute path to the `console/` workspace root.
 *
 * Resolved from this module's own location (`console/e2e/support/paths.ts`)
 * rather than from `process.cwd()`, so the specs behave identically whether
 * `playwright test` is invoked from `console/`, from the repository root with
 * `--config console/playwright.config.ts`, or from CI.
 *
 * The upward search is a belt-and-braces fallback for any runner that does not
 * expose `import.meta.url`.
 */
function resolveConsoleRoot(): string {
  try {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const candidate = path.resolve(here, "..", "..");
    if (fs.existsSync(path.join(candidate, "playwright.config.ts"))) {
      return candidate;
    }
  } catch {
    // fall through to the upward search
  }

  let dir = process.cwd();
  for (let i = 0; i < 6; i += 1) {
    if (fs.existsSync(path.join(dir, "playwright.config.ts"))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return process.cwd();
}

export const CONSOLE_ROOT = resolveConsoleRoot();

/** `console/app` — the App Router source of truth for page-level routes. */
export const APP_DIR = path.join(CONSOLE_ROOT, "app");

/** `console/.next` — the build output shipped by the Dockerfile. */
export const NEXT_BUILD_DIR = path.join(CONSOLE_ROOT, ".next");
