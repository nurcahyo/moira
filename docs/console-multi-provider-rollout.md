# Console Multi-Provider Rollout (Stage 4A)

How to get the console from *one* sign-in provider to *N*, in the order the
change is safe in, and how to tell at each step whether it worked.

The console today refuses to resolve sign-in when more than one auth provider is
enabled. That refusal — `ambiguous_enabled_providers` — is deliberate and is
still standing. It may only be removed after Moira's own server-side refusal is
**running**, not merely merged. This document is the procedure for getting there.

**Nothing here is automatable.** Step 1 is a deployment; a CI loop must not
perform it, and the removal in step 3 must not be scheduled ahead of the
verification in step 2.

## Stage 4A, concretely

"Stage 4A" is plan 09 wave 4, stage A — PR #41, merge `c98aeb7`. It is entirely
server-side: **Moira**, not the console. It shipped the invariant that makes the
console's refusal redundant, and deliberately did not remove that refusal.

What it consists of in this tree:

| artefact | what it does |
| --- | --- |
| `migrations/0020_github_oauth_and_one_enabled_provider_per_issuer.sql` | (a) `auth_provider_settings_method_check` admits `github_oauth`; (b) `auth_provider_settings_method_shape` gains a GitHub arm (`client_id`, `authorization_url`, `token_url` present, `issuer` and `discovery_url` **null**); (c) the partial unique index `auth_provider_settings_one_enabled_per_trusted_issuer`. |
| `src/infra/repositories/auth_settings.rs` | `admission_policy`'s deterministic two-stage lookup — no `ORDER BY`, no `LIMIT` — plus `single_policy`, which refuses a duplicate set instead of taking its first row, and `duplicate_enabled_provider_for_issuer()`. |
| `src/application/auth_settings.rs` | `reject_duplicate_enabled_provider`, the pre-envelope check on create/patch/enable, and `auth_provider_issuer_shadows_trusted_issuer`, which refuses a row carrying an issuer string belonging to a trusted issuer it is not bound to. |
| `src/i18n/catalog/errors.rs` | `moira.error.duplicate_enabled_provider_for_issuer`, `moira.error.auth_provider_issuer_shadows_trusted_issuer`, `moira.error.trusted_issuer_has_active_grants`. |

The invariant, in one line: **at most one enabled provider may be bound to any
one trusted JWT issuer.** Before it, `governing_policy` broke a tie on
`created_at asc`, so the *oldest* row bound to a trusted issuer supplied
`allowed_email_domains` for every admin claim and every invite redemption —
whichever provider actually authenticated the human. That is finding F23.

Stage **4B** (PR #42, `da384c8`) is the console half: per-provider minted `iss`,
N `genericOAuth` entries, N sign-in buttons. It is already merged, and it is
already exercised — see [local verification](#local-verification-before-you-deploy-anything).
It is dormant behind the guard, not unbuilt.

## Before you deploy: the replica-count question

The gating issue for this work (#78) names a "non-negotiable prerequisite":
`charts/moira-console/values.yaml:81` is still `replicaCount: 1` and "must be
raised at deploy time". **Do not raise it.** That reads plan 09's wave-1 scope
line — where "non-negotiable" attaches to *secret durability and a stable JWKS*,
not to the replica count — as an instruction, and it does not survive contact
with the chart.

Read `charts/moira-console/values.yaml` lines 29–81. Wave 1 made the console's
OAuth client secrets, its Better Auth ES256 key pair, its sessions and its rate
limits all durable and shared through `CONSOLE_DATABASE_URL` — four of the five
reasons the count was pinned. The fifth is still there:

> The auth-config **snapshot** in `console/lib/auth-runtime.ts` is per process.
> Two pods can hold different provider configurations — including different
> OAuth client secrets after a rotation — for an unbounded time.

The symptom is an `invalid_client` from the IdP on some sign-ins and not others,
depending on which pod the load balancer picked. `autoscaling.enabled` and
`podDisruptionBudget.enabled` are both `false` against that same constraint, and
`autoscaling.minReplicas: 2` is documented as the value to restore *alongside*
the replica count, never on its own.

Two facts settle it:

- **Stage 4A is a Moira change.** The console replica count has no bearing on
  whether migration `0020` has run or whether the deployed Moira binary refuses
  the ambiguous state. Raising it neither helps nor is required.
- **The chart's own reversal condition is different.** `replicaCount` goes to 2
  "when the snapshot in `auth-runtime.ts` is gone or is backed by shared
  storage". That work is not done. Raising the count today trades four
  demonstrated fixes for one undemonstrated failure mode.

So: **leave `replicaCount: 1`.** If you disagree and raise it anyway, raise
`autoscaling.minReplicas` and `podDisruptionBudget.enabled` with it, and expect
intermittent, load-balancer-dependent sign-in failures after the next client
secret rotation.

## The sequence

Deploy → verify → remove the guard → verify again. Do not compress it.

The order is not stylistic. Until Moira's refusal is running in production, the
console guard is the only thing in front of F23. Land both in one release and any
rollout that puts the console ahead of Moira opens exactly the window Stage 4A
exists to close. Stage 4A goes in release N; the guard removal goes in release
N+1.

### Step 1 — deploy Stage 4A

An ordinary Moira release containing `c98aeb7` or later. Nothing special is
required of it beyond what [deployment.md](deployment.md) already says:

- `charts/moira/templates/migration-job.yaml` is a blocking
  `pre-install,pre-upgrade` Helm hook and runs `{{ .Values.image.repository }}:{{ .Values.image.tag }}`
  — the **same image** as the Deployment, which is what lets the schema evidence
  in step 2 stand in for evidence about the running binary.
- `MOIRA_DATABASE__MIGRATE_ON_STARTUP=false`, as production already requires.

**If the migration Job fails on the index, your deployment is already
ambiguous.** That is the designed outcome, not a broken migration:

```
ERROR: could not create unique index "auth_provider_settings_one_enabled_per_trusted_issuer"
DETAIL: Key (trusted_jwt_issuer_id)=(…) is duplicated.
```

The hook fails, the upgrade aborts, and the old pods keep serving on the old
schema. `0020` deliberately contains no repairing `UPDATE` — which provider
governs admission is an operator decision. Find the duplicates:

```sql
select trusted_jwt_issuer_id, array_agg(id order by created_at) as provider_ids
  from auth_provider_settings
 where enabled and status = 'active' and deleted_at is null
   and trusted_jwt_issuer_id is not null
 group by trusted_jwt_issuer_id having count(*) > 1;
```

Then disable all but the one that should govern, through
`POST /api/v1/admin/auth/providers/{id}/disable` (so the change is audited and
the runtime cache is invalidated — not a direct `UPDATE`), and re-run the
upgrade.

### Step 2 — verify Stage 4A is live

Three cheap schema checks, then one optional probe that proves the binary. Run
them against the deployment's own Moira database.

```bash
DSN='postgres://…'   # the deployment's Moira database

# 1. The migration is recorded, and recorded as successful.
psql "$DSN" -tAc \
  "select version, description, success from _sqlx_migrations where version = 20;"

# 2. The invariant exists, with the right predicate.
psql "$DSN" -tAc \
  "select indexdef from pg_indexes
    where indexname = 'auth_provider_settings_one_enabled_per_trusted_issuer';"

# 3. The method vocabulary widened.
psql "$DSN" -tAc \
  "select pg_get_constraintdef(oid) from pg_constraint
    where conname = 'auth_provider_settings_method_check';"
```

| check | it worked | it did not |
| --- | --- | --- |
| 1 | `20\|github oauth and one enabled provider per issuer\|t` | **No row** — `0020` never ran here. The Helm hook did not run, or it ran against a different database. Stop. **`success = f`** — a partial apply. Stop; do not repair by re-running the binary. |
| 2 | `CREATE UNIQUE INDEX auth_provider_settings_one_enabled_per_trusted_issuer ON public.auth_provider_settings USING btree (trusted_jwt_issuer_id) WHERE (enabled AND ((status)::text = 'active'::text) AND (deleted_at IS NULL))` | **Empty output.** This is the one that matters: a migration row with no index means somebody hand-repaired the ledger. The invariant is not enforced. Stop. |
| 3 | the `ANY (ARRAY[…])` list contains `'github_oauth'` | The list has three methods, not four — you are looking at a database still on `0013`'s constraint. Stop. |

Check 2 is not redundant with check 1. `_sqlx_migrations` records intent; the
index is the enforcement. They can disagree, and only one of them refuses a
write.

**The optional probe — the only check that exercises the deployed binary.** It
mutates, so run it on staging first, and clean up after. With a bootstrap system
key, pick the trusted issuer your incumbent provider is already bound to and
create a second, **disabled**, row against it:

```bash
curl -s -X POST "$MOIRA/api/v1/admin/auth/providers" \
  -H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" -H 'Content-Type: application/json' \
  -d '{"method":"generic_oidc","display_name":"4A probe — delete me",
       "enabled":false,
       "issuer":"https://stage-4a-probe.invalid",
       "discovery_url":"https://stage-4a-probe.invalid/.well-known/openid-configuration",
       "client_id":"probe","allowed_email_domains":["example.test"],
       "trusted_jwt_issuer_id":"<the incumbent'"'"'s trusted_jwt_issuer_id>"}'
```

`enabled` defaults to `false` and disabled rows do not compete, so this create is
expected to succeed with `201`. The `issuer` is not decoration: without it the
row keys to `('generic_oidc','')` under
`auth_provider_settings_method_issuer_active_unique`, and on a deployment whose
incumbent is a discovery-only OIDC row the create fails
`409 duplicate_auth_provider` before it ever reaches the invariant you are
probing. Any `https` string nothing else uses will do; it must not equal the
`issuer` of a trusted issuer this row is not bound to, or the create is refused
`auth_provider_issuer_shadows_trusted_issuer`.

Then try to enable it. `If-Match` carries the row's current `version` and is
**required** — take it from the create response's `ETag`; a fresh row is `1`:

```bash
curl -s -w '\n%{http_code}\n' \
  -X POST "$MOIRA/api/v1/admin/auth/providers/$PROBE_ID/enable" \
  -H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" -H "If-Match: 1"
```

| you get | what it means |
| --- | --- |
| **`409` with `error.code = duplicate_enabled_provider_for_issuer`** | **Stage 4A is live in the running binary.** This is the pass. |
| `5XX`, or a 409 with no recognisable code | The *schema* is refusing (`0020`'s index fired) and the binary does not map the violation to a code. The database is ahead of the image: the deployed Moira predates 4A. Do not proceed. |
| `409 duplicate_auth_provider` or `409 idempotency_conflict` | You probed the wrong thing — the row collided on `(method, issuer)` or on a replay key, not on the invariant. Re-create the probe with a different `issuer` and no `Idempotency-Key`. |
| **`200`** | Nothing refused it. Your deployment is now ambiguous: **disable and delete the probe row immediately.** This contradicts check 2, so go back to it — you are almost certainly looking at a different database than the one the API is using. |

Delete the probe row (`DELETE /api/v1/admin/auth/providers/{id}`) before moving
on. A leftover disabled row is harmless to Moira and confusing to the next
person.

### Step 3 — remove the guard

**Only after step 2 passed.** This is a code change, reviewed and merged
normally, in a release *after* the one verified above.

Delete, by path:

| path | what to delete |
| --- | --- |
| `console/lib/auth-config.ts` | the exported function `ambiguityGuard`, its doc comment, and its single call site — the `return ambiguityGuard(enabled.length, resolution);` at the end of `loadAuthConfigs` in the same file, which becomes `return resolution;`. |
| `console/lib/auth-config.ts` | the `ambiguous_enabled_providers` member of the `AuthConfigProblem` union and its entry in `AUTH_CONFIG_PROBLEM_MESSAGE_KEYS`. |
| `console/tests/unit/lib/auth-config.test.ts` | the whole `describe("ambiguityGuard", …)` block (three tests), the `ambiguityGuard` import, and the `loadAuthConfigs` case asserting `result.problem === "ambiguous_enabled_providers"`. |
| `console/lib/i18n/keys.ts`, `console/lib/i18n/catalog.en.ts` | the `ambiguous_enabled_auth_providers` key and its catalog entry. `console/tests/unit/lib/i18n-catalog-coverage.test.ts` ("no catalog key is unreachable") fails on a catalog key nothing emits, so these go in the same change, not later. |

Locate them without trusting line numbers, which drift:

```bash
grep -rn 'ambiguityGuard\|ambiguous_enabled_providers\|ambiguous_enabled_auth_providers' \
  console/ --exclude-dir=node_modules
```

**Do not "fix" the guard to count per trusted issuer instead of globally.**
`migrations/0020` makes two enabled rows on one trusted issuer unrepresentable,
so a per-trusted-issuer version could never fire — and a guard that cannot fire
is finding F25 with extra steps.

**There is a second refusal, and the code comment for the guard does not mention
it.** `provisioningAdmissionFor` in `console/app/api/setup/route.ts` returns
`409 setup_single_enabled_provider_only` for any provisioning run that would take
the deployment-wide enabled count above one. Its stated justification is
`ambiguityGuard` — "this console supports exactly ONE enabled auth provider …
that is not a policy choice made here, it is what `lib/auth-config.ts` does"
(that comment's paths are console-relative — it means `console/lib/auth-config.ts`). So
deleting `ambiguityGuard` alone leaves the setup wizard still refusing to create
a second provider, with a message that now cites a function that no longer
exists. Decide explicitly whether that refusal goes too; its tests are in
`console/tests/unit/api/setup-route.test.ts` and it is referenced from
`console/tests/unit/modules/setup/AuthSettingsStep.test.tsx`. It is a separate
judgement from the guard, and it is not covered by the issue's "remove the guard
+ its test".

Comments in `console/modules/signIn/SignInPanel.tsx`,
`console/modules/setup/AuthSettingsStep.tsx`, `console/lib/setup-flow.ts` and
`docs/console-architecture.md` describe the guard as standing. Update them in the
same change. No behaviour depends on them; a stale comment about a removed safety
gate is how the next person re-derives the wrong conclusion.

### Step 4 — verify multi-provider actually works

Multi-provider behaviour **cannot be verified in step 2.** With the guard in
place, two enabled providers produce no sign-in at all, so there is nothing to
observe. Step 2 verifies Stage 4A; this step verifies the capability.

After the guard removal is deployed, configure a second provider bound to its
**own** trusted JWT issuer (a second issuer row, not the incumbent's — one
enabled provider per trusted issuer, still), and give it a client secret through
the console's setup route.

That second trusted issuer's `issuer` string must be `<bffIssuerUrl>/idp/<slug>`,
where `bffIssuerUrl` is `MOIRA_BFF_ISSUER_URL` and falls back to
`CONSOLE_PUBLIC_ORIGIN` when unset (`console/lib/env.ts`), and `slug` matches
`/^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/`. Anything else derives no `providerId`
and gets no button — `consoleProviderIdFor` returns `null` rather than inventing
one, because `account.providerId` cannot be migrated after the first sign-in.
The incumbent keeps the frozen id `moira-console-idp` and the bare `bffIssuerUrl`
as its issuer; do not give it a slug.

| what to look at | it worked | it is broken |
| --- | --- | --- |
| `/login` | **two** sign-in buttons, one per provider | **Zero buttons** and a keyed message. `no_enabled_auth_provider` means neither row is enabled and `active`. `console_secret_unavailable` means the console has no `console_provider_secret` row for that provider, or the stored `client_id` has drifted from Moira's. `trusted_jwt_issuer_not_resolvable` means the trusted issuer's `issuer` string is outside `<bffIssuerUrl>/idp/*`, so no stable `providerId` can be derived from it. **`ambiguous_enabled_auth_providers` means the guard is still deployed** — you are running the old console image. |
| **one** button when you expected two | the second row resolved as a *problem*, not a failure. This is correct behaviour: a drifted second provider must not take the first one's sign-in down. Read `consoleRuntime()`'s `problems` list for which row and why. |
| sign in through each in turn | each completes and lands in the console | an `error=` query parameter on a redirect hop. `invalid_client` is the client secret; `user_info_is_missing` is a `github_oauth` row with no `userinfo_url`. |
| the minted token for each session | the two `iss` values **differ**, and each equals its own trusted issuer's registered string | **both tokens carry the same `iss`** — that is finding F24 live. `admin_identities` is keyed `(issuer, subject)`, so two IdPs returning the same `sub` collapse to **one** admin grant. Roll back the console immediately. |
| `admin_identities` after two humans, one per provider | two grants, distinct `(issuer, subject)` | one grant, or a grant whose `issuer` is the incumbent's for both. Same failure as above. |

The last two rows are the ones with teeth. "Both buttons work" is true under the
defect as well — there is one ES256 key pair and one `kid`, and `iss` is not part
of the signature, so both tokens verify against the JWKS either way. Verifying
the signature proves nothing here. **Compare the two `iss` values.**

## If verification fails

**The guard stays until step 2 and step 4 both pass.** There is no partial
rollout of this change and no "remove it now, verify next week".

| failure | what to do |
| --- | --- |
| Step 2, any check | The guard is untouched — nothing to undo in the console. Roll the Moira release back with `helm rollback`. `0020` is forward-only and is **not** reversed by a binary rollback: the index and the widened CHECK stay. That is safe — both only refuse states the old code also could not serve correctly — so leave them, and re-deploy once you know why the migration did not land. |
| Step 2, the enable probe returned `200` | Disable and delete the probe row **first** — the deployment is ambiguous while it exists. Then roll back. |
| Step 4, zero or one button | Not a rollback. This is configuration: work the message keys in the table above. The console is serving; the second provider is not resolvable yet. |
| Step 4, the two `iss` values match | Roll the console back to the image that still carries the guard, immediately. The guard is what prevents the ambiguous state from being served, and F24 is an authorization defect, not a cosmetic one. Then disable the second provider through Moira's admin API so the restored guard resolves again. |
| Step 4, sign-in broke for the **incumbent** | Check that `CONSOLE_OAUTH_PROVIDER_ID` (`moira-console-idp`) and the incumbent's issuer string are unchanged. Both are frozen: `account.providerId` holds that literal for every admin who has ever signed in, and it is the last path segment of the redirect URI registered at the IdP. Changing either is a total sign-in outage no console-side code can repair. |

A rollback of the console is always available and always safe, because the guard
is a refusal: the worst it does is decline to serve sign-in, and it declines
loudly with a keyed message rather than guessing.

## Local verification before you deploy anything

You do not have to take Stage 4B's multi-provider machinery on trust. It runs on
a laptop today, against real PostgreSQL, a real TLS OIDC provider and a
GitHub-shaped mock, with the guard still in place.

See **[local-testing.md § Two sign-in providers, locally](local-testing.md#two-sign-in-providers-locally)**
for the runnable steps and for what the guard looks like when you meet it in a
browser.

## Related

- [deployment.md](deployment.md) — the release mechanics this builds on.
- [kubernetes.md](kubernetes.md) — the chart layout and the migration Job.
- [production-checklist.md](production-checklist.md) — the pre-rollout list.
- [console-architecture.md](console-architecture.md) — how `/login` resolves.
- [admin-identity-claiming.md](admin-identity-claiming.md) — `(issuer, subject)`
  and the `allowed_email_domains` policy this invariant protects.
