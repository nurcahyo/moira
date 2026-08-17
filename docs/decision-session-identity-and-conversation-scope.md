# Decision — end-user identity, conversation scope, and what Moira stores of a conversation

**Status:** decided by the maintainer, 2026-08-17. Binding. Not yet implemented — the work is
listed in §9 and issued as separate tickets.

**Decided by:** the repository maintainer, in response to a written architecture review covering
session state, context assembly, and prompt caching. This document is the record of that decision.
It is not a proposal and it is not awaiting sign-off.

**Scope:** how an end-user identity reaches Moira, how the caller attaches a conversation, what
Moira persists of that conversation, and how a caller's distinct surfaces are separated into
independent applications.

**commerce-os is the worked example, not the design constraint.** This document names it throughout
because it is the first caller, but every mechanism specified here — a JWKS-published `ES256`
assertion carrying `aud = moira:<surface>`, a pairwise pseudonymous subject, one application per
surface — assumes nothing about who the caller is. Any consumer that can publish a JWKS satisfies
it. Read the commerce-os references as an instance, and do not bend a second integration to match
its incidental choices.

> This notice sits here, above §1, on purpose. An earlier revision placed it inside §2, where a
> reader arriving at §4 or §5 through a cross-reference — which is how a contract drafter arrives —
> never saw it.

**Supersedes nothing.** Retires one standing assumption: that Moira would eventually become the
system of record for conversation content. It will not. See §4.

---

## 1. Why this file

`plans/CONVENTIONS.md` §0 records conventions and `docs/decisions-taken.md` records decisions
plan runners took unilaterally and which are *awaiting* human confirmation. This is neither: it is
a maintainer decision, taken deliberately, that constrains several future tickets and one legal
posture. It follows `docs/decision-encryption-at-rest.md` in getting its own file for that reason.

Three of the four decisions below were taken in a stated order — identity first, then the
conversation attachment, then application separation — because each later one depends on the
earlier one being settled. The order is preserved here.

---

## 2. Decision 1 — end-user identity arrives as a short-lived asymmetric assertion

**Decided:** end-user identity arrives as an assertion signed by a trusted issuer that publishes a
JWKS. **Moira does not accept HS256 and holds no issuer secret** — that is a property owed to every
issuer, not a term negotiated with one. commerce-os, the first such issuer, has committed to
publishing one.

The reasoning is a security boundary, not a preference: a verifier that also holds the signing
secret can mint the tokens it verifies. That collapses the distinction between "Moira checked this
claim" and "Moira could have written this claim", and it is the distinction the whole per-user
isolation story rests on.

### Shape

| Property | Value | Note |
|---|---|---|
| Algorithm | `ES256` | Supported today — see §8 |
| Audience | `moira:<surface>` | See the correction in §2.1 |
| Lifetime | ~5 minutes | See the skew note in §2.2 |
| Key discovery | `/.well-known/jwks.json` | `trusted_jwt_issuers.jwks_url` |
| Rotation | `kid` + overlapping validity | Unverified, see §7 |
| Subject | pairwise pseudonym | `hash(tenant_id, user_id, salt)`, salt per application |
| Claims | `iss`, `sub`, `aud`, `exp`, `iat` | No tenant slug. No email. No phone number. |
| JOSE header | `alg`, `kid` | **`kid` is a header, not a claim** — see below |

Two corrections to this table, both found in review, and both of the kind that would have been
caught at first integration rather than in production — but this table is the artifact two
repositories agree on before either writes code, so it has to be right on paper:

- **`iss` is structurally required and was missing.** `src/security/auth.rs:705-706` refuses a token
  with no `iss` *before any key is fetched*, and `load_issuer` selects the `trusted_jwt_issuers` row
  `where issuer = $1` — the issuer lookup is keyed on it. It is bound again at `auth.rs:1334` through
  `set_issuer`. A minter built to the original table would have 401'd on every request.
- **`kid` is a JOSE header, not a claim.** It is read from `header.kid` (`auth.rs:700-703`) and is
  likewise required. Listing it among the claims invites a minter to put it in the payload, where
  Moira will not look for it.

This is a **separate assertion minted for Moira**, not the caller's own session token. It is minted
per request path, for Moira, and is useless anywhere else — so the HS256 refusal above constrains
what Moira accepts, not what the caller uses internally. Concretely: commerce-os keeps HS256 for its
own sessions, and nothing about its existing session handling changes.

The subject is a **pairwise pseudonym**: Moira gets stable per-user isolation and per-user quota
without ever learning who the person is. Moira cannot reverse it, and cannot correlate the same
person across two surfaces, because the salt differs per application.

### 2.1 Correction adopted: the surface goes in `aud`, not in a bespoke claim

The original specification carried a "surface label" claim. **A JWT claim Moira does not know is
silently ignored** — there is no `deny_unknown_fields` equivalent for claims — so such a label
would be decorative: it would not stop an assertion minted for the seller chat being replayed at
the platform assistant.

`expected_audiences` is enforced per issuer and has a test proving a wrong audience is rejected.
Encoding the surface as `aud = moira:seller-chat` therefore makes surface binding a **verified**
property rather than a convention. Adopted.

### 2.2 Consequence: set `clock_skew_seconds` explicitly — and know what it does not do

`trusted_jwt_issuers.clock_skew_seconds` defaults to `60`. Set it on registration rather than
inheriting the default.

**The advice stands; the reason first given for it did not, and the corrected reason matters more.**
An earlier revision said the leeway applies to `exp` and `iat` alike, giving a ~7-minute window.
That is wrong in both halves:

- **`iat` is never validated.** `jsonwebtoken` 9.3.1's `ClaimsForValidation` deserialises `exp`,
  `nbf`, `sub`, `iss` and `aud` — there is no `iat` field at all, so the claim is not read and
  cannot be checked.
- **`nbf` is not validated either.** `validate_nbf` defaults to `false`, and
  `trusted_jwt_validation` (`auth.rs:1332-1359`) never enables it.

So the leeway extends only the **trailing** edge, and the real acceptance window is lifetime plus
skew — about **6 minutes**, not 7. That error was in the safe direction.

The unsafe half is what it implies about the leading edge: **a token whose `iat` is set in the
future still authenticates.** Nothing bounds how early an assertion may claim to have been issued.
If the leading edge ever needs to matter — a replay window, a mint-ahead attack — it must be
enforced with `nbf` plus `validate_nbf`, or checked outside the library. Do not assume `iat` is
doing it.

### 2.3 Consequence: per-user quota is per-surface, not per-person

One application per surface, each with its own salt, means one human holds as many unrelated
pseudonyms as there are surfaces. That is the unlinkability the pairwise scheme is chosen for, and
it is also the reason a single person can draw a separate per-user quota on each surface. This is
accepted as a deliberate trade, recorded here so that a future "the limiter is broken" report can be
answered without re-deriving it.

---

## 3. Decision 2 — the caller attaches a conversation; the caller stays the system of record

**Decided:** the caller attaches a real `conversation` object and remains the system of record for
the transcript. The identifier Moira receives is an **opaque UUID**, stable across turns, carrying
no PII and no tenant slug — the standard rule for an identifier handed across a trust boundary to a
third-party processor.

For the first integration that means commerce-os attaches the object and keeps the transcript in
`chat_messages`, in the tenant schema.

### 3.1 The contract must not be widened

`ResponseConversationInput` is exactly `{ id, create, title, metadata }` and carries
`#[serde(deny_unknown_fields)]`. The conversation object must **never** become an entry point for
a `tools` array, a system turn, or a model override. The current contract is correct; a future
change that adds a field to it is a change to this decision and requires the same signature.

The `deny_unknown_fields` attribute enforces this at the wire today. Do not remove it.

---

## 4. Decision 3 — `metadata_only`: Moira stores no conversation content

**Decided:** `conversation_content_persistence = metadata_only` for every application provisioned
under this integration, and — per #336 — as the shipped default for all newly provisioned
applications.

**Moira is not a processor for the *storage* of conversation content. It remains a processor for
its transmission and use.**

### 4.0 The narrower claim, and why the wider one was withdrawn

An earlier revision of this document said flatly that *"Moira does not become a processor of
conversation content under UU PDP"*. **That conclusion does not follow from its own premise, and it
is withdrawn.** The decision itself — `metadata_only` — is unchanged; only the legal characterisation
of what that decision achieves is corrected.

UU PDP No. 27/2022 Art. 16(1) enumerates processing as *pemerolehan dan pengumpulan*, *pengolahan
dan penganalisisan*, *penyimpanan*, and onward. **Storage is one activity among several, not the
definition.** And §4.1 states the premise that defeats the wider claim: the caller sends the full
conversation history on every turn — which the first integration does, so this is not a
hypothetical. Moira receives it, budgets and assembles it through
`src/application/context_planner.rs`, and transmits it to a provider. Declining to persist removes
one activity; it does not exit processor status.

The generic form is the stronger one: Moira transmits whatever *any* caller sends it, so the
argument holds for every integration rather than for one.

**Why this correction is not cosmetic.** §4 exists precisely to settle the processor question, so
it is the sentence most likely to be lifted verbatim into a tenant agreement. A contract drafted on
the wider claim would omit the processor obligations that do apply — documented processing
instructions, Art. 39 security duties, breach notification, sub-processor consent for the LLM
providers Moira routes to — on the belief that none were owed. **The obligations in §4.3 are
therefore a floor, not the whole set**; what else is owed is a question for counsel, raised as a
separate issue rather than answered here.

### 4.1 Why this costs nothing — for a caller that replays its own history

Because the caller is the system of record, it sends the full history each turn, and Moira was
therefore never going to use server-side history replay for this integration. What `metadata_only`
gives up — history replay, summarisation, cross-conversation memory extraction — is precisely the
set of features that decision 2 already declined.

**State the condition, because it is what makes the trade free.** `metadata_only` costs nothing
**for a caller that retains its own transcript and replays it every turn**. A caller that expects
Moira to hold the history loses exactly those three features and must select a different
persistence value. An earlier revision presented this caller-conditional trade as an unconditional
one, which is false for any integration built the other way round.

What it keeps is the part that matters:

**Prompt caching is a wire concern, not a storage concern.** `cache_control` breakpoints are placed
on the outbound provider request. A caller that sends full append-only history each turn can be
given a pinned prefix breakpoint and a rolling tail breakpoint without Moira persisting a single
byte. The measured ~87% saving on a long conversation survives `metadata_only` intact.

Also retained: per-conversation usage attribution, per-conversation rate limiting, cache shard
affinity, and a stable correlation id across turns.

### 4.2 Three exemptions that must not be misread as content storage — but must be stated

1. **RAG is deliberately exempt.** `none` and `metadata_only` are explicitly **not** honoured for
   `rag_document_versions.content_plain` and `rag_chunks.chunk_text_plain`. The reason is recorded
   on `ContentWrite::under_policy_for_rag`: honouring them there would produce a *claim* of privacy
   rather than privacy. A RAG-enabled deployment therefore does store tenant document plaintext.
   That is tenant-uploaded catalog material, not conversation content — but the distinction is a
   contractual one and must appear in the tenant agreement, not be inferred.

2. **Embeddings are unencrypted under every value of the policy.** `memory_embeddings` and
   `rag_chunk_embeddings` are computed from plaintext and stored unsealed regardless of the
   persistence setting. Erasure must therefore propagate to them explicitly; a retention clock over
   the content tables alone does not discharge a deletion obligation.

3. **`title` and `metadata` are not governed by the persistence policy.** They are the caller's own
   JSON and are retained under every value, including `metadata_only`. **They are therefore the only
   path by which personal data comes to *rest* in Moira** — nothing in Moira currently prevents it,
   and the caller-side rule in decision 2 is not hygiene but the sole control. §9 carries a ticket to
   make it machine-enforced rather than relying on caller discipline.

   **Read that as "at rest", not "at all".** An earlier revision called these the only path by which
   personal data could reach Moira *at all*, which contradicted §4.1 two paragraphs above it: the
   full conversation history arrives in the request body on every turn. It is processed and
   transmitted, and simply not stored. Distinguishing *reaches* from *rests* is the whole of the
   correction in §4.0, and it is why these two fields matter disproportionately — they are what
   survives the request.

### 4.3 Contractual obligations this creates — a floor, not the whole set

These follow from what Moira *holds*: whatever it holds, it must be able to surrender and destroy.
The tenant contract must carry:

- **dated retention** — not "we delete periodically";
- **delete by conversation id**;
- **delete by tenant**, propagating to embeddings per §4.2(2).

`conversations.retention_expires_at` has been written and ignored since migration 0007, and the
retention sweeper currently covers two tables. Shipping these obligations without shipping the
sweeper would be a contract Moira cannot honour.

**What this list does not cover** is the obligations that follow from processing Moira performs
*without* storing — see §4.0. Documented processing instructions, Art. 39 security duties, breach
notification, and sub-processor consent for the LLM providers Moira routes to are all plausibly
owed and are **not** discharged by the three bullets above. That is a question for counsel rather
than for this document, and it is raised as its own issue in §9.

---

## 5. Decision 4 — one application per surface

**Decided:** surfaces with materially different risk profiles get separate applications — separate
consumer keys, policy rows and retention clocks. Zero Moira change required.

A cross-tenant surface, a single-tenant surface, and a near-zero-risk surface each carry different
exposure, and **one leaked consumer key must not bring down the others**. The pairwise salt differs
per application, so the separation is an isolation boundary rather than a labelling convention.

For the first integration this yields three applications — a platform assistant that touches
cross-tenant data, a single-tenant seller chat, and a near-zero-risk content service. **The count is
a property of the caller's surface inventory, not of Moira.**

**Deferred, not rejected:** consolidating onto one application with a `policy_key` discriminator.
That would require relaxing `application_conversation_policies`'s primary key from `application_id`
to `(application_id, policy_key)` and populating the already-present, currently-unreferenced
`conversations.conversation_policy_id` foreign key. The named trigger for revisiting it is **budget
pooling across surfaces**. Until then, one application per surface is both cheaper and safer.

---

## 5A. No tenant is under a zero-data-retention or data-residency commitment

Answered by the maintainer, 2026-08-17, and recorded here so the next caching decision does not
have to ask again.

**No current or prospective tenant carries a ZDR or data-residency obligation.** That removes one
class of blocker from prompt caching — a provider-side cache is a retention event (5 minutes to an
hour on Anthropic, 30 minutes on OpenAI, rented by the hour on Gemini), and under a ZDR contract
that alone would forbid it regardless of cost.

**This is not permission to enable caching.** The remaining gates are unchanged and are all
measurement, not contract: a pricing table with dated and tiered rates, the
`cache_creation_input_tokens` mapping, and the requirement that the cache decision be a function of
the *resolved credential* rather than the application.

**This answer expires the moment a tenant signs such a commitment.** If that happens, caching needs
a per-credential control and a recorded effective cache mode on `execution_attempts` *before* the
tenant is onboarded, not after — because the audit question is "prove this tenant never ran with
caching on", and that cannot be answered retroactively from data nobody wrote down.

---

## 6. What this decision does *not* settle

- Whether Moira ever enables prompt caching. That is gated on a pricing table and on measured hit
  rate; see §9. Nothing here authorises turning it on — §5A removes a contractual blocker, not the
  measurement ones.
- The consolidation in §5, pending the named trigger.
- Any surface beyond those enumerated above, including ones a caller has scoped but not yet built.

---

## 7. Explicitly unverified

**JWKS cache and refresh behaviour during overlapping key rotation.** The `kid` mechanism exists
and key selection works, but the cache TTL was not read. If Moira caches a JWKS longer than the
overlap window, the first rotation fails closed for the length of the difference. This must be
measured before the first rotation, not during it. §9 carries the ticket.

---

## 8. Verification record

Each claim below was read in the tree at the commit this document was written against, rather than
assumed. Where the original specification and the code disagreed, the code won and §2.1 records it.

| Claim | Evidence |
|---|---|
| `ES256` is accepted by the issuer algorithm allowlist | `src/application/admin/shared.rs:768` — also `ES384`, `EdDSA`, `PS*` |
| EC keys verify via JWKS | `src/security/auth.rs:717`, `:978` — `DecodingKey::from_jwk` |
| `aud` is enforced, not merely stored | `tests/identity_claim.rs:474-509` — wrong audience is rejected |
| `clock_skew_seconds` defaults to 60 | `src/domain/admin.rs:753` |
| The subject claim is configurable per issuer | `trusted_jwt_issuers.subject_claim`, default `sub` |
| The conversation contract is closed | `src/domain/conversation.rs:613-622` — `deny_unknown_fields` |
| `metadata_only` stores no body in either column | `src/domain/conversation.rs:175`, `:202` — `ContentWrite::Omitted` |
| It governs memory bodies too | `src/domain/conversation.rs:88-94` (issue #140) |
| Memory extraction returns early with nothing to extract | `src/application/conversation.rs:1277` |
| RAG is deliberately exempt | `src/domain/conversation.rs:102-107` (issue #141) |
| `title`/`metadata` are retained under every value | `src/domain/conversation.rs:62-63` |

---

## 9. Work this decision authorises

Each becomes its own ticket. None is started by this document.

1. Register commerce-os as a trusted JWT issuer for its three surfaces — `ES256`, JWKS URL,
   `aud = moira:<surface>` per application, explicit `clock_skew_seconds`. The §2 Shape table is
   Moira's general trusted-issuer contract, not a commerce-os-specific one; any issuer registered
   later must satisfy the same table.
2. Measure JWKS cache/refresh behaviour under overlapping rotation, and document the minimum
   overlap window. Blocks the first rotation. (§7)
3. Enforce the conversation-id contract in Moira rather than trusting the caller: validate that
   `conversation.id` is an opaque UUID, and refuse a non-null `title` for applications that declare
   themselves PII-free. (§4.2(3))
4. Erasure propagation — delete by conversation id and delete by tenant, reaching
   `memory_embeddings` and `rag_chunk_embeddings`. (§4.2(2), §4.3)
5. Make the retention sweeper honour `conversations.retention_expires_at`. (§4.3)
6. Pricing table with **effective-from dates** — a prerequisite for any caching decision, and dated
   because provider list prices are now time-bounded in practice.
7. Map `cache_creation_input_tokens` through to usage records. Without the write count, a 0.1x read
   and a 1.25x write are indistinguishable in the usage record, and the caching question stays
   unanswerable.
8. **Establish which processor obligations Moira owes for the content it transmits but never
   stores** (§4.0, §4.3). This is a legal question, not an engineering one — it needs counsel, and
   it must be settled before a tenant agreement is drafted, because the wider claim this document
   used to make is exactly the sentence such an agreement would have been drafted on.

---

## 10. Reversal conditions

- **Decision 1** reverses if commerce-os cannot publish a JWKS. The fallback is not HS256; it is
  staying application-scoped and accepting that per-user isolation does not exist.
- **Decision 3** reverses to `encrypted_content` only if a requirement appears that genuinely needs
  server-side history replay or cross-conversation memory. Note that this reversal is **not
  retroactive in either direction**: switching does not encrypt or decrypt existing rows. It also
  re-opens the UU PDP processor question that §4 exists to close.
- **Decision 4** reverses to one application plus `policy_key` when budget pooling across surfaces
  is required, and not before.
