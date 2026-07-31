// The `server-only` shim for a plain `bun` process (NOT `bun test`).
//
// `tests/support/server-only-shim.ts` uses `mock.module`, which only exists
// inside the test runner. The durability probes in `durability-probe.ts` run as
// genuinely separate `bun` processes — that is the whole point of them — so they
// need the same neutralisation through the runtime plugin API instead.
//
// Used as: `bun --preload tests/support/server-only-runtime-shim.ts <script>`.
//
// Same reasoning as the test shim: the guard's teeth are in `next build`, and
// `tests/unit/architecture/server-only-{guards,import}.test.ts` keep watching the
// architectural half. Neutralising the marker in a probe process does not weaken
// either.
Bun.plugin({
  name: "server-only-runtime-shim",
  setup(build) {
    build.module("server-only", () => ({ exports: {}, loader: "object" }));
  },
});
