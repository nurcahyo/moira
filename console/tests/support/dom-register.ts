// Bun test preload, step 1 of 2 (see bunfig.toml `[test].preload`).
//
// Registers a DOM (happy-dom) as globals so React Testing Library can
// render components under `bun test`. This file must import nothing
// besides the registrator itself: `@testing-library/react` and
// `@testing-library/dom` read `typeof document` at their own module-load
// time, and ES module imports are hoisted and evaluated before any
// top-level statement in the importing file runs — so if this file also
// statically imported testing-library here, `GlobalRegistrator.register()`
// below would run too late, after those modules already decided
// `document` was undefined. `dom-setup.ts` (preloaded second) does the
// testing-library wiring, once this file has fully finished.
import { GlobalRegistrator } from "@happy-dom/global-registrator";

GlobalRegistrator.register();
