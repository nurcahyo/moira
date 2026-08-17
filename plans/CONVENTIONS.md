# Cross-Cutting Conventions (binding on every plan)

Authoritative rules that **every** iteration plan (`02a`–`11`) must comply with. Where a plan's own text conflicts with this file, **this file wins** and the plan must be corrected.

All version facts below were verified by web research on **2026-07-25** and must not be changed without re-verification.

---

## 0. Product-owner decisions (RESOLVED — do not reopen)

Recorded **2026-07-25**. These were open questions across the plans; they are now decided and binding. A plan still presenting one of these as "product input required" is out of date and must be corrected.

| # | Decision | Consequence |
|---|----------|-------------|
| **D1** | **P0-2 is fixed by implementing real idempotency replay**, not by removing the `Idempotency-Key` parameter and rejecting with `501`. | The parameter **stays** in the OpenAPI spec on conversation/memory/RAG routes because it is about to become true. |
| **D2** | **The work is split into two branches/PRs**: **02a** (honesty — no migrations, ships fast, closes P0-1/P0-3) and **02b** (replay — closes P0-2, stacked on 02a). | The truthful-API fix is not delayed by the replay implementation and its concurrency tests. |
| **D3** | **Email/domain allow-list is deny-by-default.** An unconfigured list denies every claim. **No first-claim exemption and no bootstrap bypass** — do not add one. | The operator must configure allowed domains before the first admin claim succeeds; this is expected behaviour, not a bug. Error: coded `403 admin_claim_domain_not_allowed`. |
| **D4** | **`GET /api/v1/admin/setup/auth-methods` stays authenticated** (SystemKey \| TrustedJwt + `moira:setup:read`). | The console calls it **server-side** with its system key, never from the browser. Prevents anonymous reconnaissance of the identity configuration. Deliberately contrasts with the anonymous `GET .../setup/claim-status`, which returns only `{"claimed": bool}`. |
| **D5** | **`email` + `email_verified` are required on BOTH claim paths** — system-key and setup-token alike. | `ClaimAdminIdentityRequest.email` is **non-optional** in the DTO and OpenAPI schema (plans 08/09 bind to this). The deny-by-default domain policy is therefore enforceable on every path with no bypass, and every grant carries a human-identifiable audit attribute. |
| **D6** | **Prometheus histograms use the `metrics` facade + `metrics-exporter-prometheus`**, not hand-rolled buckets. | Correct cumulative-bucket semantics, `le="+Inf"` handling, label escaping, and exposition formatting come from the library. Accepted cost: two new dependencies; the hand-rolled `render_prometheus` (`src/infra/metrics.rs:114`) is replaced. The `/metrics` route's `prometheus_enabled` gating and `moira.error.metrics_disabled` contract are preserved unchanged. **`metrics-exporter-prometheus` MUST be declared `default-features = false`** — its default features start an independent HTTP listener that would bypass the `prometheus_enabled` gate. |
| **D7** | **The OAuth client secret is owned by the console, stored in the console's own database — Moira never stores it and never returns it.** | Resolves a real design gap: Better Auth needs the plaintext secret in process to run the code exchange, but Moira's secret envelope is write-only by design. **Moira's load-bearing invariant is preserved: a decrypted secret never crosses a network boundary.** Consequences below. |

### D7 consequences (binding)

**Moira side — the client secret is removed from `auth_provider_settings` entirely.** Delete from plan 07's spec: the encrypted-secret envelope columns (`encrypted_payload`, `encryption_algorithm`, `encryption_version`, `encrypted_data_key`, `nonce`, `secret_fingerprint`, `masked_secret`), the `POST /api/v1/admin/auth/providers/{id}/rotate-secret` endpoint, the `auth_provider_secret_aad` / `AuthProviderSecretAadParts` addition to `src/security/crypto.rs`, and the `auth_provider_secret_rebind_required` (409) error and its i18n key. `auth_provider_settings` keeps **non-secret config only**: issuer, discovery/authorization/token/userinfo/JWKS URLs, client id, requested scopes, `allowed_email_domains`, allowed algorithms, audiences, redirect URIs, `trusted_jwt_issuer_id`, `enabled`, `version`. The frozen contract drops from 11 operations to 10.

**Console side — the console owns the secret.** It is stored in the console's own `console_auth` database (which Better Auth already requires), encrypted at rest, written by the setup wizard, never sent to Moira, never exposed to the browser, never in `NEXT_PUBLIC_*`.

**Drift protection is mandatory.** Two config stores means they can diverge — a `client_id` changed in Moira while the console still holds the old client's secret would fail the code exchange with an opaque provider error. Required mitigations: (1) the wizard writes Moira's provider config and the console's secret **in the same step**, and treats partial success as a failure the operator must resolve; (2) the console stores a **fingerprint of the `client_id`** alongside the secret and compares it against Moira's `client_id` on load, surfacing a specific, actionable keyed error on mismatch rather than letting the OAuth flow fail obscurely; (3) an e2e test asserts the mismatch path produces that actionable error.

---

## 1. Branch & pull-request workflow (one plan = one branch = one PR)

Each iteration plan is executed on its **own branch** and lands via **its own pull request**. No plan may be implemented directly on `main`, and no two plans may share a branch.

| Plan | Branch |
|------|--------|
| 02a | `plan/02a-mvp-boundary-honesty` |
| 02b | `plan/02b-idempotency-replay` (stacked on 02a) |
| 03 | `plan/03-security-hardening` |
| 04 | `plan/04-durability-correctness` |
| 05 | `plan/05-observability-ci-gates` |
| 06 | `plan/06-architecture-test-hygiene` |
| 07 | `plan/07-identity-foundation` |
| 08 | `plan/08-nextjs-console-google-oauth` |
| 09 | `plan/09-generic-oidc-github-invitations` |
| 10 | `plan/10-multi-replica-readiness` |
| 11 | `plan/11-rag-memory-intelligence` |

**Rules**
1. Branch from the **current `develop`** (not from another plan branch) unless the dependency graph in `01-roadmap-and-dependencies.md` requires stacking; if stacked, the PR description must name the base PR and the branch must be rebased once the base merges. `develop` is the integration branch and the base for all plan work (§1A); plan PRs are opened against `develop` and squashed into it. Plans 02a–04 were executed before `develop` existed and their own text still says `main`; that is a record of what happened, not an instruction.
2. **Conventional Commits** (`feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `chore:`) — matching the existing history style (`feat: make admin commands atomic`).
3. The PR **must not** be opened until every gate in §2 passes locally.
4. PR description template (required sections): **Plan link** (`plans/NN-*.md`) · **Findings addressed** (P-IDs from `00-audit-report.md`) · **Migrations included** (filenames, or "none") · **Breaking API/OpenAPI changes** · **Test evidence** (unit + e2e output summary) · **Rollback procedure** · **Deferred follow-ups**.
5. A plan is **not done** when the PR opens — it is done when the PR is merged with all gates green and the plan's Definition of Done objectively verified.
6. Plans that change the OpenAPI surface must land **before** plan 05's OpenAPI-drift gate freezes the spec (see `01` §3 ordering).
7. Never force-push a branch another plan is stacked on.

---

## 1A. Long-lived branches (`main` / `develop`) and merge method

This repository has two long-lived branches. **`develop`** is the default branch and the integration branch; **`main`** is the release branch. (`main` was the default until 2026-08-17 — see "What changing the default branch cost" below, because the switch had a consequence nobody predicted.) Feature and plan branches land on `develop`; `develop` is periodically promoted to `main`. **Every merge into `main` — a promotion or anything else — is followed by a step that puts `main` back inside `develop`**; see "The ritual" below, which is mandatory rather than occasional.

Unlike the rest of this file, this section is **not** scoped to the iteration plans. It binds **every** merge between `main` and `develop`, whoever or whatever performs it, plan-related or not.

> **Note on §1.** §1 above was written before `develop` existed and originally told plan branches to branch from and land on `main`. Its rule 1 has since been reconciled with this section (issue #221) and now says `develop`, which is what current practice already was — of the last twelve merged pull requests, eleven based on `develop` and only the `develop` → `main` promotion based on `main`. Where any remaining plan text still says `main` for a *base branch*, this section governs, and nothing in §1 overrides the merge-method rule below.

**A merge between the two long-lived branches — `develop` into `main`, or `main` into `develop`, in either direction — MUST use a merge commit. Never a squash, never a rebase.** On the command line that is `gh pr merge <N> --merge`; in the GitHub UI it is "Create a merge commit".

**Feature and plan branches merging into `develop` continue to squash.** That rule is unchanged. The prohibition here is deliberately narrow: it applies only to the two sync/promotion directions between `main` and `develop`. Do not generalise it, and do not generalise the squash habit into it.

**Plan work is not an exception.** `plans/RUNNER-PROMPT.md` §9 implements this section for runners: plan PRs are opened against `develop` and merged with `gh pr merge <N> --squash --delete-branch`, with no `--admin`. A plan branch merged into `main` is content stranded on the release branch, and the next promotion from `develop` silently reverts it (issue #221).

### The invariant this section maintains

The goal is **not** that `main` and `develop` are identical. They are identical only in the moment after a promotion in which nothing else landed, and treating that moment as the target is exactly what makes the two branches feel permanently out of sync when nothing is wrong. There is one invariant, and it is directional:

```
main ⊆ develop        everything released is present in the integration branch
```

Mechanically: `git merge-base --is-ancestor origin/main origin/develop` exits `0`.

**The reverse, `develop ⊆ main`, is not an invariant and must not be presented as one.** `develop` being ahead of `main` is the normal, healthy state of an integration branch — it holds everything merged since the last release. Do not "fix" it. Conflating the two directions is what turns an ordinary integration branch into a permanent alarm.

Holding one invariant instead of two splits "out of sync" into three cases, and the right action differs for each:

| Observed | What it means | Action |
|---|---|---|
| `main ⊆ develop` fails, `git diff origin/main origin/develop` is **empty** | Artifact of the promotion merge commit: zero content difference. | Reverse sync by **merge commit** via a PR — ritual step 4. (Direct ref push is refused by `develop`'s PR ruleset.) |
| `main ⊆ develop` fails, the diff is **non-empty** | The branches have diverged in content — either `main` holds something `develop` lacks (a hotfix), or `develop` simply moved on after the promotion, or both. | Reverse sync by **merge commit** via a PR — ritual step 4. Never squash, never rebase. |
| `develop ⊆ main` fails | Normal: `develop` is ahead of the release. | Nothing. |

**Why the asymmetry regenerates on every promotion.** A promotion by merge commit puts one commit on `main` — the merge commit itself — that `develop` does not have, so `main ⊆ develop` fails the instant the promotion lands. Answering that with a second merge commit into `develop` puts a commit on `develop` that `main` does not have, and the next promotion carries it across and produces another merge commit, and so on. In this repository, because `develop` requires pull requests, a direct fast-forward ref push is refused by GitHub ruleset `GH013`. Therefore, **the usual promotion costs two merge commits** — one on `main` for the promotion, and one on `develop` for the reverse sync PR.

"Usual", not "always": the second commit is the price of `develop` having moved on, not of the PR requirement. If `develop` is *strictly behind* `main` when the reverse sync is opened, GitHub fast-forwards the PR and the sync adds no commit at all. PR #199 (`main` → `develop`, merged 2026-08-14) is the recorded instance — `gh api repos/nurcahyo/moira/pulls/199` reports `merge_commit_sha` `91d1319`, which is the PR's own `head.sha`, i.e. `main`'s head, not a newly written commit. Two conditions have to hold together for that: the PR's head is `main` itself rather than a `--no-ff` sync branch, and nothing has landed on `develop` since the promotion. The step-4b recipe below deliberately uses `git merge --no-ff`, which forgoes the fast-forward and always writes the commit; that is the safe default, because the fast-forward case is a coincidence of timing and is not worth racing for.

### Why the reverse sync exists at all — the hotfix case

If `main` only ever receives promotions from `develop`, the only thing `main` can hold that `develop` lacks is the promotion merge commit, and the reverse sync is pure hygiene: it buys a truthful answer to "is everything released also in develop?", and nothing else. That is the empty-diff case, and it is why the case feels skippable.

The moment `main` acquires content of its own, this changes completely. `develop` is now missing real content, and **the next release cut from `develop` will silently revert it.** No test fails, no gate goes red, no conflict is raised — the promotion simply carries a tree that never had the fix, and the bug returns in production. That is the case the ritual protects against, and it is why the reverse sync is mandatory rather than tidy. The empty-diff case is cheap enough that there is no reason to build the habit around anything else.

**How `main` can acquire content, given that it is protected.** A literal `git commit` on `main` followed by `git push` is refused — two `pull_request` rules apply to `main` with no bypass actors (see the configuration snapshot), so nothing reaches `main` except through a pull request. The reachable forms are therefore:

- **a hotfix PR based on `main`** and merged into `main` with `--merge` (the only method the ruleset allows), which is the legitimate emergency path and the one this section exists to make safe;
- **any other PR mistakenly opened against `main`** — including plan work. `plans/RUNNER-PROMPT.md` §9 no longer instructs that (see above), but nothing in configuration prevents a human or an agent from choosing `main` as the base by hand: `conditions.ref_name` cannot tell a promotion from a feature branch.

Both produce the same state and the same silent revert. The protection on `main` prevents a stray local commit; it does **not** prevent this. Steps 3–5 below are what prevent it, and they are keyed on *any* merge into `main` for exactly this reason.

### What changing the default branch cost — 2026-08-17

The default branch moved from `main` to `develop` so that a `Closes #N` in a commit message would actually close its issue: GitHub only auto-closes for commits landing on the **default** branch, so while `main` held that role every fixed issue stayed open until the next promotion, and `plans/NEXT.md` — reconciled from the issue list — inherited the lie.

That part worked. **The switch also silently moved a ruleset**, and this is the part worth remembering:

> A ruleset whose target is `~DEFAULT_BRANCH` follows the default branch. Change the default, and the ruleset changes which branch it protects — without anyone editing the ruleset.

The ruleset named *"main: promotions land as merge commits"* targeted `~DEFAULT_BRANCH` and never named `refs/heads/main`. The instant `develop` became default:

- **`main` lost its merge-method pin entirely.** Squash into `main` became configuration-legal — the exact operation PR #102 performed and that this section exists to prevent. Nothing about the ruleset was touched; it simply pointed elsewhere.
- **`develop` inherited a merge-commit-only rule**, which — intersected with its own ruleset — left squash forbidden on the branch where every feature PR squashes.

Both were repaired the same day by retargeting every ruleset at an explicit `refs/heads/…` and removing `~DEFAULT_BRANCH` from all of them. The configuration is now immune to a future default-branch change.

**The general rule: never target `~DEFAULT_BRANCH` in a ruleset for a repository with two long-lived branches.** It reads as a convenience and behaves as an indirection, and the failure is silent in both directions at once — one branch quietly unprotected, another quietly over-restricted.

Note also what this episode did *not* break: `main` kept its pull-request requirement, its required checks and its force-push refusal throughout, because those came from a different ruleset. The loss was one merge-method pin — which happens to be the only half of §1A that configuration can enforce at all.

### The ritual — a merge into `main` is not finished when the PR merges

**Trigger: steps 3–5 run after *every* merge into `main`, not only after a promotion.** Steps 1–2 describe the promotion because that is the common case, but the invariant is broken by any commit landing on `main`, and a hotfix is the one occasion where skipping the repair costs something. If you merged anything into `main`, you owe steps 3–5 before you walk away.

Follow this in order. The only step requiring judgement is a conflict in 4b, flagged there.

1. **Open the promotion PR** (`develop` → `main`). `--body` is not optional: `gh` fails when it has no TTY to prompt from, which is how agents run it, and §1.4 of this file requires the description sections anyway:

   ```bash
   gh pr create --base main --head develop \
     --title "release: promote develop to main" \
     --body "Promotion of develop to main. See CONVENTIONS.md §1A."
   ```

2. **Merge it with a merge commit.** This is the rule at the top of this section, and on `main` it is config-enforced (see the enforcement table):

   ```bash
   gh pr merge <N> --merge
   ```

   Never `--squash`, never `--rebase`. The merge commit is also the release marker in history.

3. **Re-establish the invariant — check it:**

   ```bash
   git fetch origin
   git merge-base --is-ancestor origin/main origin/develop && echo "ok: main ⊆ develop"
   ```

   If this prints `ok`, you are done. Otherwise continue.

4. **Choose the repair by looking at the diff to check for content divergence:**

   ```bash
   git diff --quiet origin/main origin/develop && echo EMPTY || echo NON-EMPTY
   ```

   This test answers whether the branches carry content differences, not whether a fast-forward push is allowed — **direct push onto `develop` is always refused by repository ruleset enforcement (`GH013`)**, requiring all changes to arrive via pull requests. To answer the *separate* question of whether `main` is holding real content — the hotfix question, the one with production consequences — compare `main` against the merge base rather than against `develop`:

   ```bash
   git diff --quiet "$(git merge-base origin/main origin/develop)" origin/main \
     && echo "main holds no content develop lacks" \
     || git log --oneline origin/develop..origin/main
   ```

   That distinction matters: a non-empty `main`/`develop` diff is the ordinary state a few hours after any promotion and means nothing on its own.

   **Reverse sync by merge commit through a PR:**

   ```bash
   git switch -c sync/main-into-develop origin/develop
   git merge --no-ff origin/main
   git push -u origin sync/main-into-develop
   gh pr create --base develop --head sync/main-into-develop \
     --title "sync: main into develop" \
     --body "Restores main ⊆ develop per CONVENTIONS.md §1A."
   gh pr merge <N> --merge
   ```

   `--merge` is not optional here. `develop` permits all three merge methods because feature PRs squash, so nothing in configuration will stop you from squashing this one — see PR #102 below for what that costs.

   **This is the single authoritative repair path.** Because `develop` requires pull requests, direct `git push origin origin/main:develop` is refused by GitHub with `GH013` (whether or not the diff is empty). A rejection of a direct push is proof of repository rule enforcement, NOT evidence of divergence. Taking this path normally costs two merge commits: one on `main` (promotion) and one on `develop` (reverse sync PR) — with the fast-forward exception noted under "Why the asymmetry regenerates on every promotion" above.

   **Conflicts are the one place this procedure stops being mechanical.** A reverse sync conflicts when `main`'s content touches files `develop` has since rewritten — the realistic hotfix case. Resolve on the sync branch and commit; that is the intended place, and resolving here is exactly what stops the same conflict reappearing at every future promotion. Resolve toward *keeping both* changes: the hotfix's effect must survive, and so must `develop`'s newer work. If you cannot establish that both survived, stop and get the author of the hotfix to confirm — a mis-resolved reverse sync reverts the fix just as silently as skipping the sync entirely, and this section's whole purpose is to prevent that outcome.

5. **Verify, and only then call the merge done:**

   ```bash
   git fetch origin && git merge-base --is-ancestor origin/main origin/develop && echo "ok: main ⊆ develop"
   ```

**Honesty about fast-forward pushes: `develop` rulesets enforce PRs (`GH013`).** These are the observed facts, verified 2026-08-16 (issue #298):

- `develop` carries ruleset `20430947` with a `pull_request` rule, requiring all changes to arrive via pull requests.
- Attempting `git push origin origin/main:develop` is rejected with `GH013: Repository rule violations found for refs/heads/develop. - Changes must be made through a pull request.`.
- Therefore, step 4a (direct fast-forward ref push) is unavailable in this repository. All reverse syncs must use the PR path (step 4b).

**If a direct push is refused, do not force it and do not weaken the branch's protection.** Use the PR reverse-sync path, which requires no special privilege.

One thing the PR sync does not do is smuggle in unverified code: the commit being pushed is `main`'s head, which reached `main` through a PR. Note the seam, though — `main`'s required checks are `develop`'s minus `rotation-gate`, so the *required-check configuration* alone does not guarantee that everything arriving on `develop` this way has passed everything `develop` requires. In practice `.github/workflows/ci.yml` runs on pushes to both branches and `rotation-gate` was green on `2e3937f` — the promotion merge commit PR #297 carried onto `develop` — so the gap is closed by the workflow's triggers rather than by branch protection. That is a weaker guarantee than it looks; if the two check lists are ever allowed to drift further apart, revisit this.

**Worked example — 2026-08-16 (PR #294 / #297).** PR #294 promoted `develop` to `main`. Following step 3, `main ⊆ develop` was checked and step 4's diff was empty. Attempting `git push origin origin/main:develop` was rejected by GitHub ruleset `GH013`. The reverse sync PR #297 was opened (`sync/main-into-develop-294`) and merged into `develop` with `--merge`, restoring `main ⊆ develop` cleanly.

**The two branches never pointed at the same commit during that cycle, and that is the point of the example.** Step 4's *diff* was empty — that is about content, and it is why step 4a was attempted at all — but the tips were never equal, which is a different question and the one this paragraph is about. Work landed on `develop` while the sync PR was open — #297's merge commit `1f2041d` has `3e51671` as its first parent, one commit ahead of the `2f702d3` the PR was opened against — so `git diff origin/main origin/develop` was non-empty the moment the sync merged and has stayed that way since. That is row 3 of the table, the normal state, nothing to do. `main ⊆ develop` held throughout, which is the only thing that was ever being maintained. Equality of the two branch tips is not the goal and is not usually even reachable; treating a non-empty diff as a problem would be reporting one that does not exist.

### Why (the reason is load-bearing — do not delete it and keep the rule)

A squash discards the incoming branch's commits and writes one brand-new commit that has **no parent link to the branch it came from**. The history it appears to carry is not an ancestor of the result. Three consequences follow, and all three have already happened here:

1. **The two branches diverge permanently.** The same change now exists twice, as two commits with different SHAs, and git has no way to know they are the same change.
2. **The ordinary "is this merged?" checks stop telling the truth.** `git branch --merged`, `git merge-base --is-ancestor`, and GitHub's own merged indicators all answer from ancestry. After a squashed sync they report *not merged* for work that is demonstrably present in the tree.
3. **Every subsequent promotion conflicts with the last.** Each promotion re-presents commits the target already contains under a different SHA, so the same conflicts must be resolved again, by hand, every time.

**The instance — PR #102** (`sync/main-into-develop` → `develop`, merged 2026-08-04) squashed an entire `main`-into-`develop` sync into a single commit, `85e9528`, which has exactly one parent. All of the following is verifiable in the repository today:

- The work of PR #57 exists on `main` as commit `b083812` and on `develop` as commit `85e9528`. The two are **patch-identical** — the same 12 files, the same 1096 insertions and 28 deletions — with different SHAs and unrelated parents.
- `git merge-base --is-ancestor origin/main origin/develop` consequently reports that `main` is **not** contained in `develop`. The only substantive commit causing that is `b083812`, whose content `develop` has had since PR #102. *(Status note, 2026-08-14: that particular ancestry failure is gone — the promotion in PR #198 carried `b083812` across and the subsequent fast-forward put it in `develop`'s history, so the check passes today. What the squash cost is not undone: the same change still exists as two commits with unrelated parents, `b083812` and `85e9528`, permanently. The rule below is what stops that being re-created, not something that repaired it.)*
- A later branch audit had to fall back on comparing **PR head SHAs** to establish what had actually shipped, because ancestry no longer answered the question. That fallback is a direct cost of PR #102, not a quirk of the audit.

**The counter-example — PR #129** (`develop` → `main`, "release: promote develop to main", merged 2026-08-05) used a merge commit, `aa269e6`, which has two parents: the previous `main` and the promoted `develop` head. Because of that single choice, `git merge-base --is-ancestor origin/develop origin/main` answers cleanly, and the next promotion starts from a true common ancestor instead of replaying resolved conflicts. This is the shape every sync in both directions must have. *(Read that as ancestry being **answerable at all** — which is what a squash destroys — not as an endorsement of that direction. `develop ⊆ main` is not an invariant and exits non-zero whenever `develop` is ahead, which is normal; see "The invariant this section maintains" above.)*

### Enforcement — what is configuration, and what is discipline

Stated plainly, because a rule that pretends to be enforced is worse than one that admits it is a convention.

A GitHub repository ruleset can restrict merge methods (`pull_request.allowed_merge_methods`), but its `conditions.ref_name` matches only a pull request's **base** branch. **No condition in the ruleset or branch-protection schema inspects a PR's head branch.** Configuration therefore cannot express "no squash when the source is `main`" — only "no squash into this branch, from anywhere". That asymmetry decides what each half of this rule rests on:

| Rule | Pinnable by configuration? | Why |
|---|---|---|
| `develop` → `main` uses a merge commit | **Yes** | `main` receives promotions only. Narrowing `allowed_merge_methods` on `main` to `["merge"]` costs nothing, because no feature branch targets `main`. |
| `main` → `develop` uses a merge commit | **No** | `develop` also receives feature PRs, which must keep squashing. A merge-method restriction on `develop` would hit both kinds of PR, and configuration cannot tell them apart. |
| Every merge into `main` is followed by steps 3–5 (`main ⊆ develop` restored) | **No** | There is no event to attach a rule to. No GitHub setting can require that a merge be followed by a push, or that one branch be fast-forwarded onto another. It is the most forgettable step of the ritual and it is pure discipline — but unlike the two rules above, its *violation* is cheaply detectable after the fact, which the other two are not; see the CI guard below. That is the one place where adding configuration would genuinely change the outcome. |

So: the `develop` → `main` half can and should be pinned in configuration. **The `main` → `develop` half rests entirely on the person or agent performing the merge choosing "Create a merge commit".** No setting will catch that mistake. Only a required status check that compares `head.ref` against `base.ref` could, and no such check exists in this repository.

**Configuration state, verified 2026-08-14 — this is a snapshot, re-check it before relying on it. It supersedes the 2026-08-06 snapshot, which recorded no branch protection and no merge-method restriction; both have changed.**

- **Two** repository rulesets exist, both `enforcement: active`. Both list an **empty `bypass_actors`**, and the API reports **`current_user_can_bypass: "never"`** for the admin account used to verify this — rulesets have no implicit admin override, and `gh pr merge --admin` does not bypass one:
  - id `20430947`, named `develop`, conditions `~DEFAULT_BRANCH` **and** `refs/heads/develop` — a deletion rule plus a pull-request rule with `required_approving_review_count: 0` and `allowed_merge_methods: ["merge", "squash", "rebase"]`.
  - id `20469084`, named `main: promotions land as merge commits`, conditions `~DEFAULT_BRANCH` only — a pull-request rule with `allowed_merge_methods: ["merge"]`.
  - Two pull-request rules therefore apply to `main`, and GitHub enforces the most restrictive combination: the intersection of their allowed merge methods is `["merge"]`. **The `develop` → `main` half of this rule is now pinned in configuration**, which is the narrowing the previous snapshot asked for. On `develop` only the first ruleset applies, so all three methods stay available — as they must, because feature PRs squash.
- Both branches now also carry **classic branch protection** (the previous snapshot recorded 404 for both). `develop` requires 7 checks — `rust`, `secret-scan`, `sast`, `supply-chain`, `container-and-helm`, `console`, `rotation-gate`; `main` requires the same set minus `rotation-gate`. Both have `allow_force_pushes: false` and `enforce_admins: false`, and `required_approving_review_count: 0`.
- Repository-wide, all three merge methods remain enabled.
- A force-push to `main` is refused outright ("Cannot force-push to this branch"). There is **no fast-forward merge method** in GitHub's PR UI or API — the three methods are merge, squash, and rebase — so fast-forward *promotion* is not available here, and obtaining it would mean weakening the release branch's protection. Do not attempt it. The fast-forward in step 4a goes the other way, onto `develop`, and is a direct push rather than a PR.

**What is now pinned: the merge method for `develop` → `main`. What remains convention, enforced by whoever performs the merge: the merge method for `main` → `develop`, and steps 3–5 of the ritual after every merge into `main`.**

### The CI guard for `main ⊆ develop` (implemented — `.github/workflows/branch-invariant.yml`)

The merge-method rule cannot be fully enforced because no condition inspects a PR's head branch. The invariant has no such limitation — it is one command against two refs, needs no pull-request context, and can therefore be checked continuously:

```bash
git fetch origin main develop
if git merge-base --is-ancestor origin/main origin/develop; then
  echo "ok: main ⊆ develop"
  exit 0
fi

# The invariant is broken. Two independent questions follow; do not conflate them.
# (1) Is real content stranded on main?  Compare main against the MERGE BASE.
#     Comparing main against develop answers nothing: that diff is non-empty
#     whenever develop has merely moved on, which is the normal state.
# (2) Which repair is mechanically possible?  Compare the two trees.

if git diff --quiet "$(git merge-base origin/main origin/develop)" origin/main; then
  echo "::warning::main is not an ancestor of develop, but holds no content develop lacks."
  echo "Left-over promotion merge commit. Nothing is at risk. Repair: CONVENTIONS §1A step 4a,"
  echo "  git push origin origin/main:develop   (if refused, use step 4b)"
else
  echo "::error::main holds content develop lacks. A release cut from develop would silently revert it."
  echo "Repair: CONVENTIONS §1A step 4b (merge commit, never squash). Stranded commits:"
  git --no-pager log --oneline origin/develop..origin/main
fi
exit 1
```

**Trigger it on pushes to `main`, plus a schedule as a backstop:**

```yaml
on:
  push:
    branches: [main]
  schedule:
    - cron: "17 6 * * *"
```

`push: [main]` is the load-bearing trigger, because `main` advancing is the *only* event that can break the invariant — a push to `develop` can only extend `develop`, which preserves or restores it, and `allow_force_pushes: false` on both branches rules out the reset case. Triggering on `develop` *instead of* `main` would fire on the one event structurally incapable of breaking what is being checked. (Triggering on `develop` *in addition* is a different proposition, and the shipped workflow does it — see "As implemented" below.) The schedule catches a merge whose workflow run was skipped or cancelled; keep it, but do not rely on it as the primary — a hotfix on `main` and the release that reverts it can be minutes apart, and "daily" is longer than that window.

It needs full history, so `actions/checkout` must use `fetch-depth: 0`.

Note what this guard does and does not do. It reports whether real content is stranded, and separately which repair is possible — it does **not** know a hotfix from any other content on `main`, and it cannot fire *before* the damage, only after `main` moves. It costs seconds and turns the ritual's most forgettable step into something that announces itself at the moment it becomes owed, which is the whole ask: the hotfix case is the only branch of this section that otherwise fails silently in production.

**As implemented.** The sketch above states the intent; the shipped guard is `scripts/branch-invariant.sh`, invoked by `.github/workflows/branch-invariant.yml` on pushes to `main` and `develop`, on the daily schedule, and on `workflow_dispatch`. Where the two differ, **the script is authoritative** — the sketch was written before the states were enumerated properly, and five of its details are deliberately not followed:

- **A warning exits `0`.** The sketch's code block ends in an unconditional `exit 1`, so its "nothing is at risk" branch still goes red. The script exits `0` for every warn state. This is the most important difference: a guard that goes red on the ordinary post-promotion state is a guard that gets muted, and then the one case worth going red for is invisible too.
- **`develop` is also a push trigger.** The sketch is right that a push to `develop` cannot *break* the invariant, but it is the event that *repairs* it (step 4a/4b), so running there is what lets the guard go quiet promptly after a repair instead of staying stale until the next release or the next scheduled run. It costs seconds. The cost is real and is accepted: while content is genuinely stranded on `main`, every merge into `develop` produces a red run on an author who had nothing to do with it. The alternative — the failure staying invisible until the next release — is worse, and the job summary names the repair and the promoter's step rather than implying the author broke something.
- **There is no `pull_request` trigger, and this job must never be added to the required-checks list.** During a promotion the invariant is legitimately false for the minutes between the merge into `main` and the repair. A required check that is red for that window blocks every unrelated PR into `develop` during it. The invariant is a property of two long-lived branches, not of any individual PR.
- **The script separates four broken states, not two.** The sketch's empty/non-empty split is on the merge-base diff, which is the right axis but not sufficient. The script checks byte-identical trees *first* (identical trees prove nothing can be reverted regardless of what the merge-base diff says, and they select repair 4a), then "`main` introduced nothing since the merge base" (warn, 4b), then "`main` introduced something but `develop` already has its effect" (warn, 4b), and only then "content stranded on `main`" (fail, 4b). Note for anyone tempted to simplify it: keying the failure on `git diff origin/main origin/develop` instead of on the merge base makes the guard fail on row 3 of the table above — the repository's ordinary state after every release — and a guard that is red in the normal state is a guard that gets ignored.
- **"Did `main` change anything?" is not the failure test.** A hotfix is routinely cherry-picked onto `develop` while the branches stay unmerged; `main` has then changed content since the merge base and *nothing is stranded*. The script decides by merging `main` into `develop` in memory (`git merge-tree --write-tree`) and asking whether `develop`'s tree would move at all. Known limitation, pinned by a test: if `develop` later rewrites the hotfix's own lines the in-memory merge conflicts, the guard cannot tell whether the fix survived, and it goes red saying so. That is a false alarm about stranded content, in the safe direction.
- **The self-test is a separate job.** Run as a second step of the guard job, a broken self-test would turn the `main ⊆ develop` check red for a reason unrelated to the invariant — the same misattribution the missing `pull_request` trigger exists to avoid.

`scripts/branch-invariant-test.sh` (`make test-branch-invariant`) drives the guard through every state above plus the repaired state and the conflict limitation, using real git objects in throwaway repositories. It was verified by mutation to catch a guard whose failure branch exits `0`, a guard with the ancestry direction reversed, a guard using the naive `main`/`develop` diff, and a guard with the cherry-pick state disabled.

---

## 2. Required gates (every PR, no exceptions)

**Rust (all plans touching `src/`, `migrations/`, `tests/`)**
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release --locked
```
plus clean PostgreSQL migration validation (migrations apply from an empty database).

**Frontend (plans 08, 09)**
```bash
bun install --frozen-lockfile
bun run lint
bun run typecheck
bun test                # unit
bunx playwright test    # e2e
bun run build
```

---

## 3. Testing: unit **and** e2e are mandatory

**Every plan MUST deliver both a unit-test layer and an end-to-end layer.** A plan with only one layer is incomplete and must not be merged. "e2e" means the behavior is exercised through its real external surface, not through an internal function call.

### Rust
- **Unit** — `#[cfg(test)] mod tests` beside the code (pure logic, mappers, validators, hashing, cursor encode/decode, policy decisions). No database required.
- **E2E / integration** — a file under `tests/` driving the real HTTP surface against a **real PostgreSQL 16 + pgvector** (the audit environment), following the existing harness in `tests/support/mod.rs`. Existing exemplars to imitate: `tests/admin_idempotency.rs` (9 tests), `tests/execution_lifecycle.rs` (14 tests), `tests/public_authorization.rs`, `tests/http_error_contract.rs`.
- **Concurrency tests must use acknowledgement gates, not `sleep()`** (see finding P2-12). New sleep-based interleaving is rejected in review.
- DB-dependent tests must fail closed in CI (`panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent) — the existing pattern. **Match on the value, not merely on the variable being present:** use `env::var("CI").is_ok_and(|v| v.eq_ignore_ascii_case("true"))`, never `env::var_os("CI").is_some()`. The latter also fires for `CI=false`, `CI=0`, and `CI=""`, which several shells, IDE terminals, and tool wrappers export — turning an intended local *skip* into a hard panic. GitHub Actions sets `CI=true`, so the value check is strictly correct and strictly safer. All three existing call sites (`tests/support/mod.rs`, `tests/security_foundation.rs`, `tests/admin_idempotency.rs`) use this form; any new DB-dependent test file must copy it verbatim so the harness cannot diverge.

### Frontend (plans 08, 09)
- **Unit** — `bun test` for pure modules (Moira client, JWT/claim helpers, policy guards, atoms/molecules rendering).
- **E2E** — **Playwright** against a running console + a real (test-fixture) Moira instance: setup wizard, sign-in, sign-out, a config round-trip, and an authorization-denial path. OAuth must be driven by a **local mock OIDC provider**, never real Google, in CI.
- **Accessibility** — automated a11y assertions (axe) on every page-level route.
- **Secret-leak test** — assert no Moira system key, admin key, or decrypted credential appears in any client bundle, HTML payload, or browser-visible response.

### Definition of Done addition (all plans)
A finding or `docs/todo.md` item may only be marked complete when a **named, passing test** proves the behavior. "Implemented" is not "done."

---

## 4. i18n: every response carries a message key **and** a default English message

**Requirement: every user-visible response — error *and* success/notice — MUST carry a stable i18n message key plus a default English message.** No handler may return a hardcoded human string that has no catalog entry.

### Existing machinery (reuse it; do not invent a parallel system)
- Catalog entries are `I18nEntry { key, default_message, description }` in **`src/i18n/catalog/errors.rs`** (`moira.error.*`) and **`src/i18n/catalog/notices.rs`** (`moira.notice.*`).
- The wire envelope is `ErrorResponse { error: ErrorDetail { code, message_key, message, message_args, request_id, details } }` (`src/error.rs:52-65`).
- `message_key` is derived as `format!("moira.error.{}", code())` (`src/error.rs:146-148`) — so **every new error `code` requires a matching catalog entry with the same suffix**.
- `docs/i18n-response-catalog.json` is the documentation mirror of the Rust catalog.

### Rules
1. Every new error code added by a plan → a new `moira.error.<code>` entry in `errors.rs` with an English `default_message` and a `description`.
2. Every new success/notice string → a `moira.notice.*` entry in `notices.rs`. Never inline an English literal in a handler response.
3. `message_args` carries interpolation values as structured data — never pre-formatted English prose.
4. Update `docs/i18n-response-catalog.json` in the same PR (it is hand-synced today; plan 06 adds the drift test — until then, sync manually and treat drift as a review failure).
5. **Test requirement:** each plan adds an assertion that its new keys exist in the catalog and that responses carry a non-empty `message_key` + `message`. `tests/http_error_contract.rs` is the exemplar.
6. **Frontend:** the console renders `message_key` through its own i18n layer and falls back to the server-supplied `message`. The console must never hardcode English copy for a server-originated condition.

---

## 5. Frontend toolchain (plans 08, 09) — verified 2026-07-25

| Tool | Pinned choice | Note |
|------|---------------|------|
| **Next.js** | **16.2.11** (latest stable, released 2026-07-21) | App Router. 16.3 is canary/preview only — do not use. |
| **Node.js** | **24.x — Active LTS** (EOL 2028-04-30) | Node 26 is *Current*, not LTS until Oct 2026 — do not use. Node 22 is Maintenance-only. |
| **Bun** | **1.3.14** (released 2026-05-13) | Package manager, script runner, and unit-test runner. |
| **React** | as bundled with Next.js 16.2.11 | Do not pin independently. |

**Rules**
- `bun install --frozen-lockfile` in CI; `bun.lock` is committed.
- Pin exact versions in `package.json` (`"next": "16.2.11"`), and pin Node via `.nvmrc` / `engines` (`"node": ">=24 <25"`).
- Bun is the package manager and test runner; **Playwright** remains the e2e runner (`bunx playwright test`).
- The console lives in its own directory (`console/`) with its own `Dockerfile` and Helm chart additions — it is a **separate deployable** from the Rust service.

---

## 6. Frontend architecture: Atomic Design (mandatory)

The console's UI **must** follow Atomic Design with this exact mapping:

| Layer | Meaning in this project | Location |
|-------|-------------------------|----------|
| **Pages** | Next.js routes/pages (App Router route segments; server components; data fetching, auth guards, redirects) | `console/app/**/page.tsx`, `layout.tsx`, `route.ts` |
| **Organisms** | UI **modules** — composed, feature-aware sections that own a slice of a page (e.g. `SetupWizard`, `ProviderTable`, `CredentialForm`, `AuditLogPanel`) | `console/modules/<feature>/` |
| **Molecules** | Composite UI components built from atoms (e.g. `FormField`, `TableRow`, `ConfirmDialog`, `StatusBadgeGroup`) | `console/components/molecules/` |
| **Atoms** | Primitive UI components (e.g. `Button`, `Input`, `Label`, `Badge`, `Spinner`, `Icon`) | `console/components/atoms/` |

**Rules**
1. **Dependency direction is one-way:** pages → organisms → molecules → atoms. An atom must never import a molecule/organism; a molecule must never import an organism.
2. **Atoms and molecules are presentational and feature-agnostic** — no Moira API calls, no `next/navigation` side effects, no auth logic. They receive data and callbacks via props.
3. **Organisms (modules) own feature logic** — they may call server actions and the Moira client, and compose molecules/atoms.
4. **Pages own routing, auth gating, and server-side data fetching**, then delegate rendering to organisms. Keep page files thin.
5. **Secrets never descend past the page/server boundary** — a system key or decrypted credential must never be passed as a prop into an organism/molecule/atom, since those render client-side.
6. Every atom and molecule ships a **unit test**; every organism is covered by at least a unit test, and by an **e2e** test through the page that hosts it.
7. Shared, non-UI logic lives in `console/lib/` (e.g. `lib/moira-client.ts`, `lib/auth.ts`) — never in `components/`.

---

## 7. Authentication & authorization architecture (binding)

### 7.1 The split (do not blur it)
- **Authentication (who is the human)** happens in the **console BFF**, never in Moira.
- **Authorization (what may this identity do in Moira)** happens in **Moira**, which is the **system of record**: `trusted_jwt_issuers` + `admin_identities` `(issuer, subject)` grants + scopes. Moira never runs an OAuth flow and never stores passwords or sessions.

### 7.2 Auth is configured **in settings at runtime**, not baked into build-time env
Auth provider configuration is **runtime configuration owned by Moira's database** (consistent with how providers, models, routing, and credentials already work — `docs/project-structure.md`: "Runtime provider config belongs in PostgreSQL"). The setup wizard writes it; the console reads it at boot and on invalidation.

Consequences that plans 07/08/09 must honor:
- A migration-backed table stores enabled auth methods and their non-secret config (issuer URL, discovery URL, client id, allowed email domains, allowed algorithms, JWKS URL).
- **Client secrets are owned by the console, not Moira** (decision **D7** above). Moira's `auth_provider_settings` stores **non-secret config only**; the OAuth client secret lives encrypted at rest in the console's own `console_auth` database, written by the setup wizard, never sent to Moira, never returned to the browser. This preserves Moira's invariant that a decrypted secret never crosses a network boundary. *(Provider credentials — the AI-provider API keys — are unaffected and remain encrypted in Moira with `SecretCipher` + AAD as today.)*
- Changing auth settings must invalidate the runtime cache through the existing Postgres `LISTEN/NOTIFY` path (`src/infra/db.rs:43-80`).
- Bootstrap remains the existing out-of-band `bootstrap-system-key` CLI; the first admin is **claimed explicitly** (never "first login wins").

### 7.3 The three supported modes (all three must be reachable from settings)
1. **Google OAuth** — the default first-party option (verified email + hosted-domain policy).
2. **Custom OAuth / generic OIDC** — any provider via OIDC discovery (`discoveryUrl`/`issuer`), so self-hosted and enterprise IdPs work without code changes.
3. **Bring-your-own JWT via JWKS** — the operator registers a `trusted_jwt_issuer` (JWKS URL + allowed algorithms + audience) and Moira accepts that IdP's JWTs directly. **This path needs no console and no OAuth at all**, which is what keeps air-gapped and machine-to-machine deployments working.

### 7.4 Recommended plug-and-play stack: **Better Auth** in the console BFF
**Verified 2026-07-25:** the Auth.js/NextAuth team joined Better Auth in September 2025; Auth.js now receives security patches only, and **Better Auth is the recommended choice for new projects**. Better Auth covers all three modes above natively, which removes the hand-rolled JWT-minting code earlier drafts of plan 08 proposed:

| Requirement | Better Auth mechanism |
|-------------|----------------------|
| Google sign-in | built-in social provider |
| Custom OAuth / generic OIDC | **`genericOAuth` plugin** — OAuth 2.0 + OIDC with `discoveryUrl` auto-discovery and `issuer` validation |
| BFF→Moira short-lived JWT (Mode A) | **`jwt` plugin** — asymmetric signing and a **published JWKS endpoint** (custom path supported, e.g. `/.well-known/jwks.json`) that Moira registers as a `trusted_jwt_issuer` |
| Sessions, CSRF, rate limiting, MFA | built in |

**Why this fits Moira specifically:** the `jwt` plugin's JWKS endpoint is precisely the trust primitive Moira's existing `trusted_jwt_issuers` machinery already consumes — so the console becomes "just another trusted issuer," with **no new trust mechanism invented on the Moira side**.

**Known limitation to record honestly:** Better Auth does not provide enterprise SSO (SAML, or acting as an SP against external enterprise IdPs) out of the box. For SAML-based enterprise SSO, mode 3 (bring-your-own JWT/JWKS, fronted by the customer's own IdP or an SSO gateway) is the supported path. Plans must not claim SAML support.

### 7.5 Non-negotiable security rules
- The BFF-minted JWT **must not carry a `scope` claim** — Moira copies scopes from the JWT verbatim (`actor_from_trusted_claims`), so a self-asserted scope would bypass the `admin_identities` grant. Authorization must come from Moira's grant table alone.
- PKCE, `state`, `nonce`, and an exact redirect-URI allow-list are mandatory on every OAuth flow.
- Verified email required; email/domain allow-list is **deny-by-default**.
- Identity binds to stable **`(issuer, subject)`** — never to email alone.
- System keys, admin keys, and decrypted provider credentials must never reach the browser, `NEXT_PUBLIC_*`, or any client bundle.
- Sessions: httpOnly + Secure + SameSite cookies; logout clears the BFF session; Moira-facing JWTs are short-lived.

---

## 8. Compliance checklist (add to every plan's Definition of Done)

- [ ] Work performed on the plan's own branch; PR opened with the required description sections.
- [ ] All gates in §2 pass (Rust and/or frontend as applicable).
- [ ] **Unit tests** delivered and passing.
- [ ] **E2E tests** delivered and passing (HTTP-level for Rust; Playwright for console).
- [ ] Every new error/notice string has an i18n **key + English default** in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence.
- [ ] (Frontend) Next.js 16.2.11 · Node 24 LTS · Bun 1.3.14 pinned; Atomic Design layering respected with the one-way dependency rule.
- [ ] (Auth-touching) Config is runtime/DB-backed, secrets encrypted, no scope claim in minted JWTs, deny-by-default domain policy.
- [ ] No secret-leak: verified by test.
