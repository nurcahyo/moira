# Plan — Moira as an open-source project, Moira Cloud as a service, and where the line falls

**Status:** plan. Nothing here is started. Written 2026-08-17 from a multi-agent review —
researcher, debater, decision-maker, software architect and UI/UX designer — against the tree at
that date. Every code claim is cited; where something does not exist, it says so.

**Companion documents.** `docs/decision-session-identity-and-conversation-scope.md` is the binding
decision this plan implements. `commerce-os/docs/architecture/plan-moira-boundary.md` is the
consumer half; the two are written to be read together and deliberately do not repeat each other.

---

## 1. Three parties, and the code knows about one

The working assumption until now was two: a project, and a consumer. There are three, and they
have materially different postures.

| | Holds data | Obligations | Own terms? |
|---|---|---|---|
| **Moira, the open-source project** | nobody's | **none** — it publishes code | a licence, not a service agreement |
| **Moira Cloud** — hosted, operated by the maintainer | **its tenants'** | **full** | yes, its own |
| A third-party self-hoster | their own | theirs | theirs |

The argument settled in #351 — *a repository cannot be a data processor* — is correct for row 1
and **does not carry to row 2**. Same code, different party, different answer.

**And this is a code problem before it is a contract problem.** Moira today is written on the
assumption that **the operator and the tenant are the same person**: you deploy it, you use it.
Several things that are reasonable under that assumption stop being reasonable when the operator
is a third party holding other people's customers' data. Section 3 enumerates them.

---

## 2. Responsibility matrix

**O** owns/decides · **E** executes under instruction · **—** must *not* take responsibility ·
**?** currently unassigned

| Concern | Moira OSS | Moira Cloud | The caller (e.g. commerce-os) |
|---|---|---|---|
| **Identity mechanism** | **O** — issuer verification, scope model, actor derivation. Ships no identities. **—** must not assume the admin is the tenant | **O** operator identities; **O** a tenant-admin tier *that does not exist today*. **E** verifies claims the tenant's IdP asserts | **O** its own users. **—** must not delegate its user authentication to Moira |
| **Credential custody** | **O** envelope format, AAD binding, custody trait. **—** must not ship a default letting one tenant spend another's credential | **O** platform credentials and their commercial terms. **E** stores tenant credentials sealed. **—** must never let a platform credential silently answer a tenant request | **—** holds no model keys |
| **Conversation content** | **O** the persistence modes and their enforcement. **—** must not default to the most permissive | **O** the default for new tenants; **E** stores per the tenant's policy | **O** system of record |
| **Retention & deletion** | **O** the sweeper, the clocks, erasure propagation | **O** dated commitments per tenant; **O** executing delete-by-tenant. **—** must not promise a window the sweeper cannot enforce | **O** deletion of its own store |
| **PII redaction** | **—** *must refuse this* | **—** *must refuse this* | **O** — it is the only party that knows which field is a phone number |
| **Provider selection** | **O** the routing mechanism; **O** refusing credential shapes whose terms forbid the use | **O** which providers are offered, the sub-processor list, residency. **—** must not route a tenant to a provider absent from that tenant's contract | **O** disclosing Moira Cloud *and* its providers as sub-processors |
| **Audit & evidence** | **O** the append-only schema and **the tenant-scoped read predicate that does not exist** | **O** operator audit; **O** a tenant-inspectable evidence surface. **—** must not offer "trust our word" as evidence | **O** correlating its request ids to Moira's |
| **Incident notification** | **O** the signals and the code-defect disclosure process (`SECURITY.md`) | **?** *no runbook, no notification policy, no RPO/RTO exists* | **O** notifying its own users |
| **Cost & usage** | **O** `usage_records` and per-attempt attribution. **—** must not embed one operator's pricing | **?** *no billing, quota or spend-limit code exists* | **O** allocating cost to its users |

Three cells are the sharpest. **PII redaction is unassigned and both Moira rows must refuse it** —
building a redactor in a router produces a *claim* of privacy rather than privacy, which is the
same reasoning §4.2(1) of the decision document already applies to the RAG exemption. **Incident
notification** and **billing** are unassigned at the Cloud layer and do not exist in any form.

---

## 3. Where the code assumes two parties

Each is verified, and each states what breaks *specifically* in the hosted case.

### 3.1 A system key cannot be scoped to anything

`system_api_keys` has no `application_id` column; the query selects `null::uuid as application_id`
(`src/security/auth.rs:604-612`), and the resulting actor carries `internal_application_id: None`
with `external_tenant_id`/`external_user_id` defaulted. The unbound-caller guard in `public_access`
(`src/application/public.rs:2390-2409`) fires only for `ConsumerKey | TrustedJwt`; a `SystemKey`
reaches the `Ok` arm with all three scope fields `None`.

**Breaks:** there is no key you can hand a Cloud tenant meaning "administer *your* slice". The only
administrative credential Moira has is deployment-wide. A tenant asking for programmatic admin
access can be given nothing, or everything.

### 3.2 The access predicates fold away

Seven sites share the shape `where ($2::boolean or (($3::uuid is null or …) and …))`
(`src/infra/repositories/public.rs:623-627` and siblings). With every parameter `None`, the
predicate degenerates to `true`. **Absent scope means unrestricted, not denied** — it is a filter
wearing an authorization check's clothes.

**Breaks:** cross-tenant disclosure of spend, volume, model mix and activity pattern —
commercially sensitive between paying tenants in a way it is not between an operator and itself.

**And the fix is not one line.** The write path uses `actor.external_user_id.or(actor.subject)`
(`public.rs:1040-1041`) while `public_access` at `:2407` has no fallback, so these tables were
populated with API-key UUIDs and are read with a NULL. Making the predicate strict without a data
fixup means a caller that adopts JWT identity **can no longer read what it wrote**.

### 3.3 Audit has no access predicate, and a document claims otherwise

`list_audit_logs` (`src/infra/repositories/admin.rs:2332-2357`) has no `where` clause but the
keyset cursor. `docs/audit-api.md:10` states *"Consumer principals are constrained to events for
their application."* **There is no such constraint.** That is the class `SECURITY.md` names as
worse than an absent control.

### 3.4 `scope_type = 'global'` is unconditional, and it is the default

`resolve_runtime_credential` (`src/infra/repositories/runtime.rs:1271-1285`) guards the `user`,
`application` and `tenant` arms with `is not null` checks proving the caller presented that
identity. **The `global` arm carries no predicate at all**, and ranks last — so it is a *fallback*,
firing silently exactly when tenant-specific resolution found nothing. And
`default_scope_type()` returns `Global` (`src/domain/admin.rs:139-140`).

**Breaks, three ways:** a tenant with no credential silently spends the platform's; cost
attribution is wrong for the tenant that most needs it right; and it is a **terms breach** —
`docs/claude-subscription-boundary.md` draws a hard line against pointing a subscription at other
people's customers, and a global-scoped subscription credential plus this arm makes that
structural rather than accidental.

### 3.5 Two policy tables disagree about the safe default

`application_execution_policies.persistence_mode` defaults to `metadata_only`
(`migrations/0006:15`). `application_conversation_policies.conversation_content_persistence`
defaults to **`plain_content`** (`migrations/0007:5`), and a *missing* row coalesces to the same
(`src/infra/repositories/conversation.rs:1123-1126`).

**Breaks:** self-hosted, the operator chose the default and owns the consequence. In Cloud, the
*operator* chose it and the *tenant* bears it.

### 3.6 Retention is written, never swept — and production forbids the sweeper

`conversations.retention_expires_at` is computed and stored and read by nothing. The sweeper covers
two tables (`src/infra/workers/retention.rs:97-99`). And `validate_production`
(`src/config/settings.rs:1541-1543`) contains:

```rust
if self.workers.enabled {
    violations.push("workers.enabled must be false until workers are implemented".into());
}
```

**A production deployment refuses to boot with retention on.** The comment is stale — the workers
exist and are tested — but the check is live. **The hardening posture and the retention promise
are in direct contradiction: hardening a deployment is the act that disables its only deletion
path.**

### 3.7 Rate-limit and concurrency keys omit the tenant

`permit:user:{hash}` (`src/orchestration/controls.rs:521-527`) carries no application and no tenant
prefix, unlike `permit:application:{id}` beside it. `external_user_id` is caller-supplied text.

**Breaks:** tenant A's user `"1"` and tenant B's user `"1"` share one limiter — cross-tenant denial
of service by collision, and a timing side channel by rejection rate.

### 3.8 Development defaults fail open, and hardening is opt-in

`deployment.environment` defaults to `Development`, and `validate_production` runs **only** under
`Production`. With `auth.admin.enabled = false` (the shipped default) any **unauthenticated**
request becomes a `DevAdmin` actor holding `moira:admin` (`src/security/auth.rs:452-462`). With
`dev_trust_headers = true` (also default) `x-moira-tenant-id` is read with **no signature and no
verification**.

**Breaks:** for a self-hoster, "insecure until you flip the switch" is a defensible local-dev
trade. For Cloud the entire isolation boundary rests on one environment variable, and the failure
mode is silent full compromise rather than a refusal to start.

### 3.9 Any tenant that can register an issuer can mint itself admin

`ActorType::TrustedJwt` is on `ADMIN_IMPLYING_ACTOR_TYPES` (`src/security/authz.rs:206-219`), and
scope claims are additive rather than granted. **Nothing else on this list matters while this is
open.**

---

## 4. What Moira Cloud needs that self-hosted Moira does not

### The criterion, stated before the verdicts

> A capability belongs in the **open-source** codebase if and only if its absence would make *any*
> deployment serving someone other than its operator unsafe. It is **Cloud-only** if it encodes one
> operator's commercial or infrastructural choices.
>
> **Tie-breaker:** if the Cloud-only implementation would require editing a file under
> `src/security/`, or an access predicate in `src/infra/repositories/`, it belongs upstream instead.

The tie-breaker is the load-bearing half, for two reasons. `SECURITY.md` promises that
cross-tenant findings are in scope for public reporting — unkeepable if the isolation predicates
live in a private fork nobody can read. And a security core that diverges between a public branch
and a private one is a core that gets patched twice and reviewed once.

### Verdicts

| Need | Where | Why |
|---|---|---|
| A real tenant entity | **OSS** | Every multi-user self-hoster has the same problem, and it is a migration plus every access predicate. Tie-breaker triggers. |
| Tenant-scoped authorization — `has_scope(actor, scope, resource)` | **OSS** | It *is* `src/security/authz.rs`. |
| Operator-vs-tenant admin tier | **OSS** | A property of the authorization model, not of a business. |
| Fail-closed default scopes | **OSS** | A default that is wrong for everyone not alone on their deployment. |
| Tenant-scoped audit read | **OSS** | `docs/audit-api.md` already claims it exists. Making a documented control real is not a feature. |
| Retention sweeper covering the conversation domain, and lifting the production ban | **OSS** | Already a tracked defect. A self-hoster under a retention obligation needs it identically. |
| Per-tenant key custody — scope columns on `content_data_keys`, ownership in the AAD | **OSS** schema + trait; **Cloud** the KMS/HSM backend | The schema touches `src/security/data_keys.rs`. The *backend* is infrastructure choice. |
| Tenant-prefixed rate-limit and concurrency keys | **OSS** | One-line class of fix, wrong for everyone. |
| A structural backstop against the dev fail-open | **OSS** | Make the invariant depend on observed state (more than one tenant exists) rather than on a declared environment. |
| Tenant-inspectable evidence surface | **OSS** query + scoping; **Cloud** the portal and any attestation artefact | The predicate is a repository; the presentation is a product. |
| Billing, metering, quota, spend limits | **Cloud** | Pricing is one operator's business model; upstreaming it imposes it on self-hosters. |
| Tenant signup, plan tiers, SLAs, status page | **Cloud** | Commercial surface, touches no security file. |
| Sub-processor list, DPA, residency commitments | **Cloud** (documents); **OSS** the *mechanism* to pin a tenant to an allowed provider set | The list is a fact about one operator's contracts; the enforcement point is routing. |
| Incident runbook, breach notification, RPO/RTO | **Cloud** | Operational commitments of a specific service. `SECURITY.md` covers code-defect disclosure and stays OSS. |
| **PII redaction** | **Neither** | Only the caller knows which field is personal. Building it here produces a claim of privacy rather than privacy. |

**Note what this criterion produces:** almost everything isolation-shaped stays open source. If the
intent is a commercial moat, the criterion needs overriding on business grounds — but overriding it
*on the isolation code specifically* makes `SECURITY.md`'s promise unkeepable, and that cost is
worth naming out loud rather than discovering later.

---

## 5. Architecture — the seams

### 5.1 The persistence redesign this plan assumes

A separate multi-agent review concluded: **delete the derived-content store, quarantine the
deposited-content store, fix the receipt core.** Eleven of nineteen tables dropped, ~14,000 source
lines removed, `conversations` replaced by a caller-minted `conversation_ref uuid` denormalised
onto `responses` and `usage_records`.

That closes §4.2(3) of the decision document — *"the only path by which personal data comes to
rest"* — **by construction**: a uuid has no `title` and no `metadata`. It is recorded in full
separately; this plan depends on it but does not restate it.

### 5.2 The control plane is already a separate artefact

`console/` is a Next.js application distinct from the Rust crate. Research into how permissively
licensed projects sustain a hosted offering found the same answer repeatedly: **nobody monetises a
permissive core with the licence.** ClickHouse gates a cloud-only *architecture*; Supabase gates a
closed control plane; Kong gates Konnect. The gateway is the commodity; the control plane is the
product.

**Consequence for the repo layout:** the recommendation is a **separate private repository** for
any Cloud-only component, not an `ee/` directory. At one-maintainer scale a mixed-licence monorepo
is a per-file licensing risk on every PR, a CI matrix proving the OSS build works without it, and
contributors who may unknowingly patch proprietary files. Cal.com ran the `/ee` pattern and closed
its commercial edition entirely in April 2026 — the directory did not stabilise the boundary, it
marked where the cut would fall.

### 5.3 The three-contract chain

```
end user  →  the caller's tenant  →  the caller  →  Moira Cloud  →  model provider
 data          controller for        processor      sub-processor    sub-sub-processor
 subject       their own users
```

Where the caller and Moira Cloud are **the same legal entity**, the processor→sub-processor
relationship has no arm's-length contract to point at — **and the disclosure obligation to the
tenant is unaffected.** Self-dealing does not exempt the disclosure; it removes the document that
would normally carry it.

---

## 6. UI/UX — four surfaces

Designed against the console's real constraints: every string is a catalog key (JSX text literals
fail a test), the a11y gate asserts the route list bidirectionally, and **a URL may not live in
catalog copy** (`console/lib/llm-view.ts:44-52` — *"a message naming an address has stopped being
copy and become configuration"*), so legal links are view-model data.

### 6.1 Provider data policy, at the point of configuration

Rendered where the decision is made — inside each provider article on `/settings/llm`, above the
controls that make it routable — and again as a comparison table under `/data-handling/providers`.

Three facts as a description list: retention, training-on-input, processing location. Then terms
and DPA links with their provenance date.

**The self-hosted case is a different panel, not the same one with blanks.** Empty cells read as
"we didn't check"; the true statement is *"no third party receives this text."* With one caveat
that must be body copy and never a tooltip: **Moira verifies that the address answers, not who
answers it.**

**Stale links degrade to text.** A link that 404s is worse than plain text, because the operator
concludes the policy moved rather than that the record rotted.

**Nothing on this surface is clickable.** Disclosure with an acknowledgement attached becomes a
speed bump the operator learns to clear without reading.

### 6.2 Terms acceptance — recorded once, versioned

`/data-handling/terms`. The honesty problem is structural: a row reading "Accepted ✓" beside a list
of providers will be read as "we accepted Anthropic's terms", and screenshotted into a compliance
binder as exactly that.

Three devices, in order of load: the page's own lede says whose terms these are; the status column
says **Current / Superseded / Never accepted** rather than the bare word "Accepted"; and a
permanent section states the negative, positioned *between* the table and the form so it is passed
through on the way to the only control on the page.

**Acceptance is recorded by typing the version identifier, not by ticking a box.** A checkbox is
one motion that costs nothing and can be performed without reading. Typing `2026-07-30` requires
having the document open. That friction is spent at the single point in the whole design where a
record of a human decision is manufactured.

**And when the document link is unreachable, the form disables itself:** *"recording an acceptance
of something you cannot read is not a record worth having."*

### 6.3 Per-application data-handling posture

Read-only, no controls — so nobody reading the exceptions is simultaneously being asked to dismiss
them. Four blocks: message bodies, retention window, **"three things this setting does not cover"**,
and provider-side caching.

The heading is a **count**, so its absence is noticeable. The three exceptions —
`title`/`metadata` retained under every value, embeddings stored unsealed, RAG bodies exempt — get
the same visual weight as the policy itself. Burying any of them in a tooltip would be a decision
to hide them.

Retention reads **"90 days recorded, not enforced"** until the sweeper runs. Caching reads *"Off
now. Whether it was ever on is not recorded."*

### 6.4 Operator versus tenant, for Cloud

A **separate route group with its own layout gate**, not a conditional inside the console — the
gate is a property of position, so a route added inside is gated with no edit anywhere.

The tenant sees their own posture, where their text goes, and their own evidence log. They do not
see other tenants, provider base URLs (which name the operator's infrastructure), or any control.

**The shipping state is a refusal.** `external_tenant_id` is a caller-supplied string with no
referential integrity, so a tenant view scoped on it is *"a view any caller can widen by choosing a
different string — not a tenant boundary, a filter that looks like one."* Until a real tenant
identity lands, the page renders: *"This deployment cannot yet tell your data apart from another
tenant's, so this page will not show you anything."* A refusal that names its reason is a
shippable, honest surface. Plausible-looking data scoped on a caller-chosen string is not.

### 6.5 The rule that governs all four

**Never let the interface state something the system cannot back up.** If Moira cannot prove a
tenant never ran with caching enabled, the UI must not imply it can.

---

## 7. Terms — three sets, and where they overlap

| | Moira OSS | Moira Cloud | The caller |
|---|---|---|---|
| Instrument | Apache-2.0 + `TRADEMARK.md` + CLA | service agreement + DPA + sub-processor list | its own terms + DPA with its users |
| Governs | copying, modifying, redistributing | operating the service for tenants | the caller's relationship with its users |
| Names the caller? | **never** — the mechanism is caller-agnostic | **never** — Cloud serves any caller | n/a |

**The overlap is the sub-processor chain**, and it must be stated in both directions: the caller
discloses Moira Cloud *and the model providers beneath it* to its users; Moira Cloud discloses its
providers to the caller. Neither document may assume the other exists.

**A clause that does not work, recorded so it is not re-proposed:** *"we are only a router and are
not responsible for what customers send or how providers process it."* Status follows from what is
actually done with the data, not from what a contract calls it — and the **data subject is not a
party to these terms**, so an agreement between operator and tenant cannot dispose of an end
user's rights.

**The same intent, written as allocation rather than denial, does work:**

> The tenant warrants it has a lawful basis for the personal data it transmits and is responsible
> for the content of its requests. Data transmitted onward is handled under the published policy of
> the provider selected for that request.

---

## 8. Sequencing

**⚠ = one-way door.**

### Phase 0 — legal existence

1. **⚠ A licence.** Nothing else is meaningful; the repository has none. Also one-way in both
   directions: permissive is irrevocable, and every contribution arriving before a CLA lands under
   an implied inbound licence with no recorded terms.
2. **⚠ The CLA decision**, at the same moment and for the same reason.

### Phase 1 — one line, then make the tenant real

3. **Invert `settings.rs:1541-1542`.** One line. Nothing in this plan, or in any competing design,
   functions until it lands.
4. **⚠ A first-class tenant entity.** Retrofitting an owner onto existing rows has no correct
   answer for historical data; `applications.tenant_id NOT NULL` cannot be applied forward. This is
   the mandate's one genuinely perishable permission.
5. Give `authz` a resource dimension; add a tenant-bound actor type.
6. Replace the fold-away predicates with fail-closed ones — **and plan the data fixup**.
7. Add the predicate to `list_audit_logs`, or delete the sentence in `docs/audit-api.md`.

### Phase 2 — make the promises enforceable

8. Sweeper covers the conversation domain and embeddings.
9. **⚠ Default persistence to `metadata_only`** — one-way for any tenant onboarded before it.
10. Narrowest default credential scope; gate the `global` arm behind an explicit opt-in.
11. **Refuse a subscription-backed credential for a tenant-scoped request** — a terms breach, not a
    preference.
12. Tenant-prefixed limiter keys.
13. **⚠ Bind ownership into the AAD** — free at zero sealed rows, a full re-encryption at any other
    number.

### Phase 3 — operator capability

14. Structural backstop for the dev fail-open, keyed on observed state.
15. Erasure by tenant and by conversation, receipted, with a tenant-facing re-count.
16. Incident runbook, breach notification, RPO/RTO.
17. Metering and a spend ceiling — so a runaway tenant is a rejection, not an invoice.

### Phase 4 — contracts, written against what exists by then

18. Moira Cloud service agreement, DPA, sub-processor list, residency statement, PITR window.
19. **⚠ The first external tenant.** Strictly one-way: you cannot un-receive their data, and every
    default in force at that moment becomes a thing you did to someone rather than a thing you
    chose for yourself.

**Minimum viable gate, if only four things ship:** the licence; the one-line worker inversion; a
real tenant entity with fail-closed predicates; and a retention sweeper that actually runs. Without
the fourth, no dated retention clause can honestly be signed.

---

## 9. Decisions only the maintainer can make

1. **Does Moira Cloud store conversation content at all?** A "yes" reverses the persistence
   deletions and makes per-tenant key custody a prerequisite rather than a deferred option.
2. **Managed retrieval: quarantine, delete, or promote to a Cloud product?** The largest fork in
   the review, and the one no reviewer could resolve on evidence.
3. **Is Moira Cloud a separate legal entity from the first caller, or the same one?** Same is
   simpler operationally and creates the self-dealing disclosure gap in §5.3.
4. **Does an open-core split happen at all?** §4's criterion says almost everything security-shaped
   stays OSS.
5. **Do the caller's own tenants become distinct Cloud tenants, or does the caller remain one
   opaque tenant?** Distinct means the caller must propagate tenant identity as a *verified claim*.
   Opaque means its per-tenant isolation guarantee stops at Moira's boundary and its DPA must say
   so.
6. **Will consumer-key-only callers be supported for external Cloud tenants?** A "yes" means
   delete-by-subject is permanently unavailable for them and the DPA must say so.
7. **PITR window** — a number in a contract, not an operational habit.
8. **Per-tenant KEK with an external KMS before the first tenant, or "deletion only, no
   cryptographic erasure" in the DPA?** Choosing the latter is defensible and cheap. Choosing it
   *implicitly*, by shipping and discovering later, is not — the AAD change that makes the former
   possible is free only while zero rows are sealed.
