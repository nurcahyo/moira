// Bun test preload: the per-test DEADLINE, and nothing else.
//
// Bun's default deadline is 5s, and several React suites spend most of a test
// inside `userEvent.type` — one awaited macrotask per keystroke, through
// happy-dom, through React's controlled-input round trip. On an unloaded machine
// those finish in well under a second; on a machine also running a Rust build,
// or on slower CI hardware, they cross 5s and the suite reds with `timed out`
// rather than with a failed expectation. `SetupWizard.test.tsx` reports 25 pass
// at a longer deadline and up to 11 `timed out` failures at 5s, on the same
// commit, with no assertion changed.
//
// This raises the DEADLINE and nothing else: no assertion is relaxed, no test is
// skipped, and a test that genuinely hangs still fails — 20s just fails it for
// the right reason.
//
// WHY IT LIVES HERE AND NOT IN `bunfig.toml`. It was written as
// `[test] timeout = 20000` first. Bun 1.3.14 parses that file (its `preload`
// array is what loads this module) but silently ignores an unknown `[test]` key,
// so the deadline stayed at 5s and the setting read as coverage it did not
// provide. `jest.setTimeout` is the mechanism Bun actually honours from a
// preload, and it is verifiable: a test that sleeps 6s passes with this loaded
// and fails `timed out after 5000ms` without it. If a future Bun gains a real
// bunfig key, move it back and delete this file — but check the 6s probe first.
import { jest } from "bun:test";

jest.setTimeout(20_000);
