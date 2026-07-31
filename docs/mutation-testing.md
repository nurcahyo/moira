# Mutation testing

A passing test proves the test ran. It does not prove the test would have failed had the code
been wrong. Mutation testing measures that difference directly: change the code in a way that
should break something, and see whether anything turns red.

This project adopted it because hand-written mutations kept finding real gaps —
**six of six** cases where a test passed against deliberately broken code, including one that
*nothing* in the suite caught. That last one is how finding F19 (invite redemption doubling as
an identity-enumeration oracle) surfaced: the mutation was invisible to the whole suite, which
is what prompted asking what the guard was actually for. Reading a test does not tell you
whether it works.

## Scope: the code a change touches, not the tree

`cargo mutants` with no filter walks every function under `src/`. Each mutant it cannot
immediately rule out costs a workspace build plus a full test run, and this workspace is 400+
crates. A whole-tree run is measured in hours and is **not** a gate a pull request can wait on.

So the adopted scope is the diff against the merge base — the code a reviewer is being asked to
trust. That is what `scripts/mutants.sh` runs.

*Reversal condition:* wire it into CI as a blocking gate only once a scoped run is
demonstrably inside the CI budget — measure it, do not assume it. Until then it is a
documented local step and a reviewer's tool, and this file is the documentation.

## Running it

```bash
cargo install cargo-mutants --locked      # once
scripts/mutants.sh                        # mutants in this branch's src/ diff vs origin/main
scripts/mutants.sh --list                 # what would be tested, without testing it
scripts/mutants.sh --base <ref>           # diff against something other than origin/main
scripts/mutants.sh -- --file 'src/security/*.rs'   # extra args go to cargo mutants
```

`MOIRA_TEST_DATABASE_URL` must point at a reachable database and the script refuses to start
otherwise. This is not politeness: with the DB suites skipping, every survivor would be
reported as "no test catches this" when the truth is "this code was never exercised" — the
same false-green `scripts/gates.sh` asserts against.

Configuration lives in `.cargo/mutants.toml`. It says how a mutant is judged (which test
command, which calls to skip, which files are data rather than behaviour); it deliberately says
nothing about *what* to mutate, because that is the per-run `--in-diff` decision above.

## Reading the result

Non-zero exit means at least one mutant survived, timed out, or could not be classified.
`mutants.out/outcomes.json` has the detail; `mutants.out/` is gitignored.

A **surviving mutant** is a claim, not a verdict: it says "this line can be changed and the
suite stays green." Three things it can mean, in the order worth checking:

1. **A missing assertion.** The common case, and the one worth fixing. The test exercises the
   path but asserts on something the mutation does not move — the trap plan 11 wave 1 fell
   into, where a status assertion passed because the code under test also wrote the value being
   asserted.
2. **Equivalent code.** The mutation genuinely changes nothing observable — a redundant guard, a
   defensive branch a database constraint already makes unreachable. Worth a comment saying so,
   because the next reader will wonder too.
3. **Behaviour nothing should depend on.** Log text, a `Debug` impl, a metrics label whose only
   contract is that it comes from a closed set already asserted elsewhere. Candidates for
   `skip_calls` or `exclude_globs` in `.cargo/mutants.toml` — with the reason written down.

What a survivor never means is "add an assertion that pins this exact line." A test written to
kill a mutant, rather than to state a property, is a test that will be deleted the next time the
line is refactored, and it will be right to delete it.
