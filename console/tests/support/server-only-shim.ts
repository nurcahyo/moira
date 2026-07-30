// Bun test preload, step 0 of 3 (see bunfig.toml `[test].preload`).
//
// ============================================================================
// WHY THIS FILE EXISTS
// ============================================================================
//
// `server-only` is a marker package with exactly two files and an `exports` map
// that selects between them by export *condition*:
//
//     "exports": { ".": { "react-server": "./empty.js", "default": "./index.js" } }
//
// `index.js` is a bare `throw new Error(...)` at module scope. That throw IS the
// guard: a bundler compiling for the browser resolves the `default` condition,
// evaluates the module, and the build fails. Next.js compiles server code with
// the `react-server` condition, resolves `empty.js`, and nothing happens.
//
// `bun test` resolves the `default` condition, so importing any module that
// carries `import "server-only"` would throw at import time and take the whole
// suite with it. That is precisely why Wave 1 could not land the real import and
// shipped only a static architecture test.
//
// This preload registers `server-only` as an empty module for the duration of
// the test process — the same thing the `react-server` condition does in a real
// Next.js server build, expressed in the one mechanism Bun gives us. It is NOT a
// weakening of the guard:
//
//   * The guard's teeth are in `next build`, which is a CI step (`bun run
//     build`). If a `"use client"` module ever imports a credential-carrying
//     module, the *build* fails, not the test run.
//   * `tests/unit/architecture/server-only-guards.test.ts` remains, and catches a
//     different class of mistake — an import that is server-legal but
//     architecturally wrong (a credential module reachable from `components/**`),
//     plus the credential-shaped-literal and error-boundary rules. Neither the
//     build guard nor this shim can see those.
//   * `tests/unit/architecture/server-only-import.test.ts` asserts every module
//     in the server-only set actually carries the import, so the shim cannot
//     silently make the import optional.
//
// REVERSAL CONDITION: if Bun ever supports per-run export conditions for the
// test runner (a `conditions` key in `bunfig.toml` `[test]`, or an inherited
// `--conditions` flag), delete this file and set `conditions = ["react-server"]`
// instead. That resolves `empty.js` through the package's own exports map rather
// than substituting a module for it, which is strictly more faithful.
import { mock } from "bun:test";

mock.module("server-only", () => ({}));
