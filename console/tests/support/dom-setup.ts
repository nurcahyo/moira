// Bun test preload, step 2 of 2 (see bunfig.toml `[test].preload`).
//
// Must run strictly after `dom-register.ts` has registered the happy-dom
// globals (bunfig preloads run in array order, each to completion before
// the next starts) — `@testing-library/react`/`dom` capture `document` at
// their own import time. Extends Bun's `expect` with the jest-dom
// accessibility/DOM matchers (toBeDisabled, toHaveAttribute,
// toHaveAccessibleName, ...) and resets the DOM between tests.
import { afterEach, expect } from "bun:test";
import { cleanup } from "@testing-library/react";
import * as matchers from "@testing-library/jest-dom/matchers";

expect.extend(matchers);

afterEach(() => {
  cleanup();
});
