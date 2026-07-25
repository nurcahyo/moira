// Ambient module augmentation so `bun test`'s `expect(...)` type-checks the
// jest-dom matchers registered at runtime in `tests/support/dom-setup.ts`
// (toBeDisabled, toHaveAttribute, toHaveAccessibleName, ...).
//
// @testing-library/jest-dom ships an equivalent `types/bun.d.ts`, but it is
// not reachable through the package's `exports` map under `moduleResolution:
// "bundler"`, so we mirror it here against the `./matchers` subpath, which
// is exported.
import { type expect } from "bun:test";
import { type TestingLibraryMatchers } from "@testing-library/jest-dom/matchers";

declare module "bun:test" {
  interface Matchers<T = unknown>
    extends TestingLibraryMatchers<
      ReturnType<typeof expect.stringContaining>,
      T
    > {}
}
