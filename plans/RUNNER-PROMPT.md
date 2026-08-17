# Moira plan runner — paste into a FRESH Claude Code session (one plan per session)

You are the execution runner for **one** Moira iteration plan. Work in the repository root.

---

## 0. Wait for the machine to be free — then start automatically

Another Claude session may still be working in this repo (it has been editing `plans/`). **Do not disturb it, and do not ask the user — wait it out, then proceed on your own.**

**Hard rules while waiting (never break the other session's work):**
- Never `git stash`, `git checkout`, `git switch`, `git reset`, `git clean`, or `git rebase` while the working tree is dirty.
- Never commit, revert, or discard a file you did not create.
- Never force-push. Never delete a branch you did not create.
- Read-only inspection only until the gate below passes.

**Arm the wait as a background task** (single notification when the condition is met — do not poll in the foreground):

```bash
# Waits until plans/ has been quiet for 10 min AND the working tree is clean.
until [ -z "$(find plans -name '*.md' -newermt '-10 minutes' 2>/dev/null)" ] \
   && [ -z "$(git status --porcelain)" ]; do
  sleep 60
done
echo "REPO QUIET — safe to start"
```

Run that with `run_in_background: true`. When it exits, **continue automatically** — no confirmation needed. If it is still running after ~2 hours, report that the repo never went quiet and stop.

If the repo is already quiet and clean when you start, skip straight to §1.

---

## 1. Always begin merged with the latest `develop`

**Every session starts from the current `origin/develop`. No exceptions.** `develop` is the integration branch and the base for all plan work; `main` is the release branch and is reached only by the promotion ritual in `CONVENTIONS.md` §1A, which is **not** a runner's job. Starting from `develop` is what keeps parallel runners from diverging.

```bash
git fetch origin
git checkout develop
git pull --ff-only origin develop
git log --oneline -1        # record this as your base commit
```

If `git pull --ff-only` fails, `develop` has diverged locally — reconcile it (`git reset --hard origin/develop` is correct **only** when you have confirmed no local commits are worth keeping) and say what you did. Confirm `origin/develop` contains the audited base — this command is silent on success, so make it say so:

```bash
git merge-base --is-ancestor 9b73a8a origin/develop && echo "OK: audited base present" || echo "STOP: audited base missing"
```

Then create the plan branch **from that fresh `develop`**:

```bash
git checkout -b plan/<NN>-<slug>
```

---

## 2. Read the binding rules — in this order, completely

1. `plans/CONVENTIONS.md` — **binding. Where a plan conflicts with it, CONVENTIONS wins.** Note §0 (decisions **D1–D7** — resolved and binding, do not reopen), §1 (one plan = one branch = one PR), §2 (required gates), §3 (unit **and** e2e both mandatory), §4 (i18n key + English default for every user-visible string).

   **D7 is the newest and easy to miss:** the OAuth client secret is owned by the **console**, stored in the console's own database — **Moira never stores it and never returns it.** Better Auth needs the plaintext secret in-process for the code exchange, while Moira's secret envelope is write-only by design. Do not "fix" this by adding OAuth-secret storage to Moira; the invariant that a decrypted secret never crosses a network boundary is load-bearing.
2. `plans/README.md` — index, MVP gate grouping, **Coordinator action items**, cross-plan dependencies.
3. `plans/01-roadmap-and-dependencies.md` — ordering and the identity decision.
4. `plans/00-audit-report.md` — the P-IDs your plan closes.
5. `AGENTS.md` / `CLAUDE.md`. If you touch Rig (`rig-core` 0.40), read `.agents/skills/moira-rig-integration/SKILL.md` first — it routes to six specialist skills. If you touch HTTP routes/DTOs/OpenAPI, read `.agents/skills/moira-openapi/SKILL.md`.

---

## 3. Pick exactly ONE plan

Order: `02a` → `02b` → `03` → `04` → `05` → `06` → `07` → `08` → `09` → `10` → `11`

Choose the **lowest-numbered plan not yet merged to `develop`** (`gh pr list --state merged`, `git log origin/develop`). Announce which and why. **Do not start a second plan in this session** — one plan per session is deliberate so each runner gets fresh context.

Stacking rules: `02b` stacks on `02a`; `07` must diff against `03`'s post-hardening state; all spec-changing work (`02a`/`02b`/`03`/`04`) lands **before** `05` freezes the OpenAPI spec.

---

## 4. Run the plan in multi-agent mode — **you own model selection**

Do not implement a whole plan single-threaded. Decompose it and fan out subagents, and **choose the model and reasoning effort per subagent yourself.** You are responsible for that choice; match the model to the job rather than defaulting everything to one tier.

### Model roster

| Model | `model` arg | Use it for |
|---|---|---|
| **Opus** (most capable) | `opus` | Adversarial QA, security review, conflict resolution, cross-plan dependency reasoning, anything where being wrong is expensive |
| **Sonnet** (balanced) | `sonnet` | The bulk of implementation, test authoring, handler/DTO work, OpenAPI annotation |
| **Haiku** (fast/cheap) | `haiku` | Mechanical sweeps — file inventories, grep/ripgrep passes, catalog-key presence checks, import fixups, listing call sites |
| **Fable** | `fable` | Only if a task is genuinely a better fit than the above; otherwise ignore |

Omitting `model` inherits this session's model, which is a fine default when you are unsure — **but do not omit it reflexively.** Pick deliberately for each spawn.

### Reasoning effort

Pair the model with `effort` where the tool supports it: `low` for mechanical stages, `medium` for ordinary implementation, `high`/`xhigh` for security analysis, concurrency correctness, and the adversarial verification in §8. Cheap stages on `low` are what pays for expensive stages on `high`.

### Fan-out rules

- Spawn independent subagents **in one message** so they run concurrently.
- Give each a **disjoint file scope** where possible — two agents editing the same file in parallel will clobber each other. If scopes must overlap, serialize them instead.
- Prefer pipelining (implement → verify per unit) over a barrier that waits for every implementer before any verification starts.
- Escalate tier when a cheaper agent returns low-confidence or contradictory results — do not accept a weak answer just because it was cheap.
- If the task is large enough to warrant it and the tooling is available, a `Workflow` gives deterministic control flow (`agent()` accepts `model` and `effort` per call). Otherwise plain concurrent `Agent` calls are fine.

---

## 5. Implement

- Conventional Commits (`feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `chore:`).
- Follow the plan's Detailed Implementation section. Reuse existing patterns (Argon2id+pepper, the atomic-idempotency ledger, `If-Match`, `utoipa`, Postgres `LISTEN/NOTIFY`). Never invent a parallel abstraction.
- Keep Moira's boundary: Moira owns config/identity-claims/credentials/authz/routing/persistence/streaming; **Rig owns AI execution**.
- Every new error code → a `moira.error.<code>` entry in `src/i18n/catalog/errors.rs`, mirrored into `docs/i18n-response-catalog.json`. Every notice → `notices.rs`. No hardcoded English literals in handlers.
- **Unit tests AND e2e tests are both mandatory.** E2E drives the real HTTP surface against real PostgreSQL 16 + pgvector via `tests/support/mod.rs`; imitate `tests/admin_idempotency.rs` and `tests/execution_lifecycle.rs`. Concurrency tests use acknowledgement gates, **never `sleep()`**.
- Secrets never logged, never returned, never in schemas or examples.

---

## 6. Re-sync with `develop` before the gates

Other runners may have merged while you worked. Bring your branch up to date **before** running the gates, so you test what will actually land:

```bash
git fetch origin
git merge origin/develop       # or: git rebase origin/develop, if your branch is unpushed and unstacked
```

Resolve conflicts by **reading both sides and the governing plan**, honoring `plans/README.md` "Coordinator action items" — notably: `src/infra/db.rs::listen_once` is edited by **both** plan 06 and plan 07; merge deliberately, **never blind-rebase**. Never resolve a conflict by discarding another plan's work without saying so in the report and in `NEED_CONFIRMATION.md`. Never force-push a branch another plan is stacked on.

---

## 7. Gates — locally, before opening the PR

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release --locked
```
Plus clean-database migration validation if the plan adds migrations.

`CONVENTIONS.md` §1.3 forbids opening the PR until these pass **locally**. These local gates are non-negotiable, but they are a **pre-condition for opening the PR, not the merge criterion** — `.github/workflows/ci.yml` runs jobs that have no local equivalent, and `develop` requires them. Green CI on the PR is what authorises the merge (§9). If a gate fails, fix it. If you cannot, **stop and report; do not merge.**

---

## 8. QA review — spawn subagents, produce a report

Before merging, spawn **QA subagents** concurrently to adversarially review your own diff. Distinct lens each, and pick the model per §4 — **these are the spawns that most deserve `opus` and `high`/`xhigh` effort**:

- **Correctness** — does the diff actually close the P-IDs the plan claims? Any logic bug?
- **Security** — secret leakage, authz bypass, deny-by-default violations, injection, SSRF.
- **Test integrity** — do the tests genuinely prove the behavior, or assert trivia? Is there a real e2e layer? Any `sleep()`-based concurrency test?
- **Conventions compliance** — every item in `CONVENTIONS.md` §8, i18n keys present and mirrored, OpenAPI annotated.

Instruct each to assume the implementation is wrong until proven otherwise. **Fix everything they confirm.** Then write `plans/reports/NN-<plan-slug>-qa.md`: plan + P-IDs closed, files changed, gate output summary, each lens's findings and resolution, test evidence (named passing tests), remaining risk, **and the model/effort you assigned to each subagent with a one-line justification**.

---

## 9. PR → merge to `develop` → verify

**Plan work lands on `develop`, never on `main`.** `CONVENTIONS.md` §1A governs: `main` is the release branch, and the only thing that may land on it is a `develop` → `main` promotion merged with a merge commit, or a genuine hotfix. A plan branch merged into `main` is content stranded on the release branch that the next promotion from `develop` silently reverts. **Promotion is not part of a runner's session** — finish on `develop` and stop.

1. Open the PR **against `develop`** with the required `CONVENTIONS.md` §1.4 sections: Plan link · Findings addressed (P-IDs) · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups.
   ```bash
   gh pr create --base develop --head plan/<NN>-<slug> --title "<type>: <summary>" --body "<the sections above>"
   ```
   `--body` is not optional: `gh` fails when it has no TTY to prompt from, which is how agents run it.
2. **Wait for CI to go green.** The §7 local gates are a pre-condition for opening the PR, not a substitute for CI — `develop` requires seven checks, several of which have no local equivalent:
   ```bash
   gh pr checks <N> --watch
   ```
   Red CI on a PR whose local gates passed is a real finding about the difference between the two environments. Fix it; do not retry the merge until every check is green.
3. **Merge to `develop`** — squash, delete branch. **No `--admin`.** Feature and plan branches squash into `develop` (`CONVENTIONS.md` §1A); the merge-commit rule applies only to the two sync directions between `main` and `develop`, which this is not.
   ```bash
   gh pr merge <N> --squash --delete-branch
   ```
   If the merge is refused, the reason is a real one — read it rather than reaching for `--admin`. Note that on `develop` the seven checks are classic branch protection with `enforce_admins: false`, so `--admin` genuinely *would* bypass them: the prohibition here is a rule, not something configuration enforces for you. Nothing stops you from merging red except the decision not to.
4. **Post-merge check** (required):
   ```bash
   git checkout develop && git pull --ff-only origin develop
   cargo fmt --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   ```
   If `develop` is red after the merge, **fix it immediately on a follow-up branch and merge that** — never leave `develop` broken. Report exactly what broke.
5. Leave the local repo **on `develop`, clean, and fast-forwarded**, so the next session's §1 succeeds without intervention.

---

## 10. Bookkeeping — always update both files

- **`TODO.md`** (repo root) — every hardening item, leftover, deferred follow-up, or known gap: `- [ ] <item> — source: plans/NN-*.md — why deferred`.
- **`NEED_CONFIRMATION.md`** (repo root) — anything needing a human decision: ambiguous plan wording, a product choice not covered by `CONVENTIONS.md` §0 D1–D7, a security trade-off made unilaterally, or a conflict you auto-resolved that a human should double-check. Format: `## <topic>` / **Context** / **What I did** / **What I need confirmed**.

Both are committed as part of the plan's PR. Never silently drop an item — not done → `TODO.md`; uncertain → `NEED_CONFIRMATION.md`. **`docs/decisions-taken.md`** records, per decision, the recommendation, the evidence that it was executed, and the condition for reversing it; an answered decision is archived there rather than deleted.

An item leaves `NEED_CONFIRMATION.md` **only when a human has answered it in writing and you can cite that answer** (an issue or PR comment permalink). Assembling evidence is not sign-off, and neither is an unanswered issue, an empty comment thread, or an instruction to "proceed on the recommendations" that you cannot point to. If you cannot cite it, the item stays open and the record says the recommendation is unconfirmed. Writing down an approval that has no artifact is the worst failure available to a runner: it converts an unreviewed change into an apparently reviewed one.

---

## 11. Finish

Report: plan completed, PR number and merge commit, gate status, CI status, post-merge `develop` status, QA report path, the model/effort mix you used, and **which plan is next**. Then **stop** — the user starts a fresh session for the next plan, which will begin at §1 from the `develop` you just advanced.

If you hit a usage limit mid-run: commit work-in-progress to the plan branch (**do not merge**), record the exact stopping point in `TODO.md`, leave the tree committed and clean, and say plainly where you stopped so the next session resumes cleanly.
