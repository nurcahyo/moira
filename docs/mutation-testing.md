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

### Measured, so the CI question has an answer

First real run, on the branch that adopted the tool (10 changed files, ~630 added lines under
`src/`):

```
63 mutants tested in 2h: 9 missed, 25 caught, 29 unviable
```

`-j 2` on a laptop. Per mutant: **~50–100 s to build, ~450–880 s to test** — the test run dominates,
because every mutant runs the whole DB-backed workspace suite. The unmutated baseline alone was
227 s build + 340 s test.

So a **scoped** run on a ten-file change is two hours. That is fine for a reviewer to start and come
back to, and it is not a gate a pull request can block on. Wiring it into CI would need the test
command narrowed per changed file, which trades away the DB-backed coverage that is the whole reason
the findings below were findable.

*Reversal condition:* make it a blocking gate only once a scoped run is demonstrably inside the CI
budget — measure it on a real branch, do not assume it. Until then it is a documented local step and
a reviewer's tool, and this file is the documentation.

### What the first run found

All nine survivors were real gaps in code written that same day, and every one of them was in a
test **I believed covered it**:

| Survivor | What it meant |
|---|---|
| `set_primary`'s `is_primary && !current.is_primary` → `\|\|` | A `PATCH {"is_primary": false}` on a grant that never owned anything would demote **the real owner** — a `200 OK` that leaves the deployment ownerless, around the last-primary guard, which only inspects the row being written |
| `already_claimed_on_unique_violation`'s `&&` → `\|\|`, and the same guard in the duplicate-issuer mapper → `true` | Any database failure on those inserts reported as a uniqueness conflict, telling a client to adopt a row that was never created |
| `revoke_identity`'s `if !outcome.replayed` → `if outcome.replayed` | The replay guard had no assertion at all; it was written beside two that did |
| `validate_api_key_prefix`'s `<` → `<=` | The floor was tested at floor−1 and at the default (floor+1), never at the floor itself |
| `is_registered_key_namespace` and `const_str_eq` → `true`, and the loop bound `<` → `==` | A membership test with no negative case answers yes to everything and still satisfies every "is this a member" assertion |

The shape they share: **each test exercised only the side of the boundary the code was written
for.** That is not a mistake mutation testing invents a name for — it is the mistake it makes
visible, and reading the tests would not have shown it.

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
