# Release notes

Operator-facing changes that alter behaviour on upgrade. Newest first.

Only entries that change what a running deployment does belong here. Additions that no existing
request can reach do not — an operator reading this file is looking for what will break, not for a
changelog.

## Unreleased

### Breaking: `encrypted_content` now actually encrypts, and rolling back past this release hides those rows

Closes [#139](https://github.com/nurcahyo/moira/issues/139), the release-train step that turns the
key custody, envelope format and keyring of the four preceding entries into behaviour. Decided in
[`decision-encryption-at-rest.md`](decision-encryption-at-rest.md) §8, §13 and §14.

**Run this before upgrading.** It names every application this release changes behaviour for:

```sql
select application_id from application_conversation_policies
 where conversation_content_persistence = 'encrypted_content';
```

Those applications are, **today, silently receiving plaintext** — that is finding F32, and the
value was accepted before the 422 refusal existed. On upgrade day they begin receiving ciphertext,
with no further action and no setting to delay it. If that query returns rows, read the rollback
paragraph below before you deploy.

**Switching an application to `encrypted_content` does not encrypt its existing history.** This is
the single biggest thing to misread here. The policy governs subsequent writes only. Messages and
summaries already stored under `plain_content` stay in `content_plain`, stay readable, and stay
plaintext. Removing them is retention's job, not this policy's. The same holds in reverse:
switching *away* from `encrypted_content` does not decrypt anything already sealed.

**Rolling the binary back past this release is a data-visibility rollback.** An older build selects
`content_plain`, sees `NULL`, and renders the content as *absent* rather than erroring —
silently, for every row written under `encrypted_content` while this build was live. Message reads
return `null` bodies, summarization refuses with `no_persisted_content`, memory extraction stops,
and cross-turn history is planned as if those turns carried no text. Nothing logs an error,
because to an older build the row simply has no body. There is no flag to soften this; that is the
accepted cost of the no-feature-flag decision, and [#125](https://github.com/nurcahyo/moira/issues/125)
— a pre-existing switch found never to have been wired to the code that read it — is why the
decision was taken that way. The data is not lost; a forward roll reads it again.

**What changed, concretely.**

| | before | after |
|---|---|---|
| `PUT` conversation policy with `encrypted_content` | `422 conversation_content_persistence_unsupported`, always | accepted, unless this deployment has no usable content keyring |
| message body under `encrypted_content` | not stored at all | sealed into `conversation_messages.content_encrypted` |
| summary body under `encrypted_content` | not stored at all | sealed into `conversation_summaries.summary_text_encrypted` |
| reading either back | `null` | the original string, opened transparently |
| `content_size_bytes`, `token_count`, the 262,144-byte cap | plaintext | **unchanged — still plaintext** |

**The 422 narrowed rather than disappearing.** `conversation_content_persistence_unsupported` now
fires only when encryption is configured but unusable at write time. Removing it outright would
have left no write-time refusal for a key-custody failure, which is a real and permanent
condition.

**Refusal, never fallback.** A write under `encrypted_content` with no usable active content key
returns `503 content_key_unavailable` and stores **nothing** — it does not fall back to plaintext.
Four new error codes accompany it, all catalogued: `content_key_unavailable` (503),
`content_key_abandoned` (500), `content_envelope_unsupported` (500) and `content_decryption_failed`
(500). The last two are split deliberately: a framing failure is decided before any key is touched
and names its discriminant **in the log**, so "you are running a build that predates this format"
is distinguishable from "your key is wrong"; an AEAD failure gets one opaque code and one opaque
log line, because saying more is an oracle. No response body carries a key id, a reason or a
fragment of the row.

**Capacity.** Ciphertext is incompressible, so a sealed row loses TOAST compression and gains a
42-byte envelope header plus a 16-byte tag. Budget for growth on `conversation_messages` and
`conversation_summaries` proportional to how much of your traffic runs under `encrypted_content`.

**Rotation is unaffected by any of this.** New writes pick up the active data key at each
replica's next keyring refresh; rows written under an earlier key stay under it and stay readable
forever. See `moira keyring status`.

### Breaking: production requires `MOIRA_CONTENT_ENCRYPTION__KEYS`, and will not start without it

Closes [#135](https://github.com/nurcahyo/moira/issues/135), the first step of the
envelope-encryption-at-rest train decided in
[`decision-encryption-at-rest.md`](decision-encryption-at-rest.md).

**Every production deployment fails to boot after upgrading until this variable is set.** The
refusal happens in `AppState::new`, before the listener binds, so the pod never becomes Ready —
it does not start degraded and it does not fail later on a request.

**Set this before upgrading:**

```
MOIRA_CONTENT_ENCRYPTION__CUSTODY=environment
MOIRA_CONTENT_ENCRYPTION__KEYS=content-v1:$(openssl rand -base64 32)
MOIRA_CONTENT_ENCRYPTION__ACTIVE_KEY_ID=content-v1
MOIRA_CONTENT_ENCRYPTION__ALLOW_INSECURE_DEV_KEY=false
```

On the Helm chart the key bytes belong in the operator-supplied `moira-secrets` Secret, exactly
like the three existing 32-byte secrets; `CUSTODY`, `ACTIVE_KEY_ID` and `ALLOW_INSECURE_DEV_KEY`
are already in the chart's ConfigMap. `KEYS` is a list — `"<id>:<base64>,<id>:<base64>"` — and
`ACTIVE_KEY_ID` must name one of its ids.

**Why it is required unconditionally**, even on a deployment where no application stores
encrypted content. The persistence policy is flippable at runtime through the admin API without
a restart, so requiring the key only when some policy row selects it would make a boot invariant
depend on mutable database state: a replica that booted yesterday would fail today because
someone changed a policy on another replica. That was considered and rejected.

**What this release does *not* do.** Nothing reads or writes an encrypted column yet. No stored
data changes, no request behaviour changes, no endpoint changes. This ships the configuration
surface, the pluggable key-custody seam and the boot validation **one full release ahead** of any
behaviour change, which is the entire mitigation for the break above — the variable can be in
place before the release that uses it.

**No fallback, deliberately.** Leaving `KEYS` unset does *not* fall back to
`MOIRA_SECRETS__MASTER_KEY_BASE64`. Provider credentials and user content have different
retention and different blast radius, and a configuration that looks set while being inert is the
class of failure this refusal exists to prevent.

**Development is unaffected.** `content_encryption.allow_insecure_dev_key` defaults to `true`
outside production and substitutes a built-in sentinel, reported in the startup WARN as
`insecure_content_encryption_key`. Pasting that sentinel into `KEYS` is refused in *every*
environment. `scripts/dev-env.sh` generates real random material and carries it across
`make env-force`, so an existing local database keeps working.

**Rotating it later** means adding the new key to the list, leaving the old one there, and then
moving `ACTIVE_KEY_ID`; nothing is re-encrypted and previously wrapped keys keep opening. See the
rotation section of [`local-testing.md`](local-testing.md).

### Breaking: a structured-output reply that is not JSON is now a 422 instead of a null value

Closes [#80](https://github.com/nurcahyo/moira/issues/80) (finding F29's deferred fail-hard flip).
**This takes effect immediately on upgrade. There is no setting that restores the previous
behaviour**, and that is a deliberate decision, not an omission.

**Before.** A request carrying `response_format: json_schema` whose provider answered with
something that is not JSON returned **`200 completed`** with the model's prose in the output text
and **nothing at all to say the schema had not been honoured**. `PublicResponse` carries no
structured-output field, so the only signal a caller could have acted on was the parse they were
about to attempt themselves. Inside Moira the same reply produced `structured_output: null` on a
`succeeded` execution — byte-for-byte what a model that legitimately answered "nothing" produces —
so neither surface could tell *"the provider did not comply"* from *"the answer was empty"*.

**After.** The execution fails with `structured_output_invalid`:

| Endpoint | What the caller receives |
| --- | --- |
| `POST /api/v1/responses` | `422 Unprocessable Entity`, `"code": "structured_output_invalid"` |
| `POST /v1/responses` (compat) | the same `422` |
| `POST /api/v1/responses/stream` | transport stays `200`; the same code arrives as the terminal `response.failed` event, after the deltas the caller has already received |

A `200` can now only carry an answer. On the runtime diagnostic surface — the one place an
`ExecutionOutcome` is serialised verbatim — `structured_output: null` on a `succeeded` execution
now means the model sent the JSON literal `null`, never that something went wrong.

**Where the boundary is.** Moira **parses JSON** here; it does not validate against the schema
(enforcement is the provider's, under the `strict: true` every schema-carrying request is sent
with). So:

- `null`, `{}` and `[]` all parse. An empty *answer* is still a `200`.
- A reply that is valid JSON but violates the caller's schema still succeeds, exactly as before.
- Only bytes that are not a JSON document at all — prose, an empty reply, JSON wrapped in a
  ```` ```json ```` fence, or JSON with commentary around it — fail.

The error message is a fixed string. It never contains the provider's reply, and the prose is not
returned alongside the error.

**Who is affected.** Only callers sending `response_format: json_schema` (or `text.format` of type
`json_schema` on the compat endpoint) to a model that does not reliably honour it. The exposure is
concentrated on `openai_compatible` and `local` provider types: they receive the schema on the
wire, but whether a self-hosted backend honours it is a property of that backend, which Moira
cannot check at admission time. Providers that cannot receive a schema at all are already excluded
from structured requests at routing (ledger F39), so this does not turn those into failures — it
turns *non-compliance* into a failure.

**Before upgrading**, find the models that can be routed a schema by a backend Moira cannot verify.
Nothing on the `responses` row records the requested response format, so this is a question about
configuration rather than about past traffic:

```sql
select p.provider_type, p.display_name, m.model_key
from provider_models m
join providers p on p.id = m.provider_id
where m.deleted_at is null
  and p.deleted_at is null
  and m.status = 'active'
  and coalesce((m.capabilities ->> 'structured_output')::boolean, false)
  and p.provider_type in ('openai_compatible', 'local');
```

Each row is a backend that will be sent a schema and is trusted, unverifiably, to honour it — the
population this change is about. An empty result means your structured traffic goes to hosted
providers that constrain generation to the schema in their own API, or to `deepseek`, which is
already excluded from structured requests at routing (ledger F39); in either case this release is
very unlikely to change anything for you. It is not a guarantee: any provider can return something
Moira cannot parse, and that has always been the request that gets an answer nobody can use.

**Retries and fallback are unchanged, and deliberately do not apply.** `structured_output_invalid`
is in none of `is_retryable`, `is_fallback_eligible` or `is_circuit_failure`: a zero-temperature
resample is the same reply, another provider is a different answerer rather than a fix, and a
caller must never be able to open a circuit breaker that refuses traffic for every other tenant on
that provider. The failure is terminal on the first non-conforming reply, and exactly one provider
call is made.

**One knock-on inside Moira, and it is a regression: fenced extraction replies stop working.**
Memory extraction sends its own schema, so it is subject to this change like any other caller.
A model that wraps its JSON in a ```` ```json ```` fence **used to produce memories** — the
execution succeeded with `structured_output: null`, extraction fell back to the raw text, and
`parse_candidates` stripped the fence before deserialising the envelope. As of this release the
execution fails first, the reply is discarded with the rest of the failed outcome, and the run
ends as `failed` / `structured_output_invalid` with **no memories written**. Fencing is what a
provider that ignores `output_schema` commonly does — and whether a backend ignores it is exactly
what Moira cannot check for the `openai_compatible` and `local` types.

**The exposure here is wider than the query above**, which lists models *claiming* the
`structured_output` capability. Extraction sets no `required_capabilities`, so it can be routed to
any active model on the route it uses — including one that never claimed to honour a schema.
Treat every extraction-enabled application as affected until its run rows say otherwise.

There is no caller-visible symptom to alert you: extraction is fail-open by design, so the
response is unaffected, no error is returned, and this release note is the notice. Check for it
directly — the run rows carry the answer:

```sql
select date_trunc('hour', started_at) as hour, status, failure_class, count(*)
from memory_extraction_runs
where started_at > now() - interval '24 hours'
group by 1, 2, 3
order by 1 desc;
```

A run population that turns from `completed` to `failed` / `structured_output_invalid` at the
upgrade is this condition and not a provider outage. The fence tolerance in `parse_candidates` is
left in the code but is no longer reachable from an execution, and the reversal condition is
recorded on `structured_output_from_text` in `src/application/execution.rs`: if a deployment
measures fenced replies from schema-receiving backends, the tolerance moves to that one site
rather than being duplicated.

**Provider tokens spent on a refused reply are still metered.** The provider answered and will
invoice for the answer, so the refused attempt records the real counts on `execution_attempts` and
writes its `usage_records` row exactly as the same request did while it still succeeded. Totals
per request are therefore unchanged by this release, and a caller cannot obtain unmetered provider
work by pointing a schema at a backend that does not honour it.

What *is* new is that `usage_records` can now contain a row for an attempt whose status is
`failed` — until this release every row belonged to a successful one. The row carries
`metadata.attempt_outcome = "failed"` and `metadata.failure_class`, so a job that must distinguish
them can, without joining back to `execution_attempts`. `GET /api/v1/usage` does not expose
`metadata` and its output is unaffected. See `docs/execution-attempts-and-usage.md`.

**OpenAPI.** Unchanged: no new error code, no new enum value, no new operation — 152 operations
across 100 paths and 183 schemas, identical to the previous release. `structured_output_invalid`
was already declared and already mapped to `422`; what changed is which conditions raise it.

### Breaking: a route whose agent profile is missing or disabled now fails the request

Closes [#79](https://github.com/nurcahyo/moira/issues/79) (finding F50). **This takes effect
immediately on upgrade. There is no setting that restores the previous behaviour**, and that is a
deliberate decision, not an omission — see `docs/agent-profile-resolution.md`.

**Before.** If a route definition named an agent profile that had been disabled or soft-deleted,
the reference was left dangling — neither operation clears `route_definitions.agent_profile_id` —
and every execution on that route silently ran *without* the profile's `preamble`, `temperature`
and `max_tokens`, reporting `succeeded`. Since the previous release the condition was at least
announced (a `warn!`, an `agent_profile_unavailable` runtime event and an `agent_profile.unavailable`
audit row), but the request was still served.

**After.** The request is refused before any provider is contacted:

| Condition | Error code | `POST /api/v1/responses` |
| --- | --- | --- |
| the profile is disabled | `agent_profile_disabled` | `409 Conflict` |
| the profile was deleted, or no row has that id | `agent_profile_not_found` | `404 Not Found` |

On `POST /api/v1/responses/stream` the same codes arrive as the terminal `response.failed` event;
the transport status stays `200`, as it does for every failure raised after the response head.

**Who is affected.** Only deployments where a live route names an agent profile that is disabled or
deleted. A route with no agent profile at all — the configuration every default install ships with —
is unchanged and still executes.

**Before upgrading**, find the routes this applies to:

```sql
select r.route_key, r.agent_profile_id, p.profile_key, p.status, p.deleted_at
from route_definitions r
join agent_profiles p on p.id = r.agent_profile_id
where r.deleted_at is null
  and (p.deleted_at is not null or p.status <> 'active');
```

Each row is a route that will start refusing traffic. Re-enable the profile, or repoint the route.
An empty result means this release changes nothing for you.

**What did not change.** The observability shipped with F50 is unchanged and still fires, now
alongside the refusal; its payloads gained a `reason` field (`"disabled"` or `"missing"`) matching
the caller's error code. The `agent_profile_unavailable` runtime event remains off the public SSE
contract.

**OpenAPI.** `ExecutionFailureClass` gains two enum values. No operation was added, removed or
changed: 152 operations across 100 paths, unchanged.

### Breaking: `conversation_content_persistence` now takes effect, and `encrypted_content` is refused

Finding **F32** ([#57](https://github.com/nurcahyo/moira/issues/57)). This entry is written late: the
change landed earlier in this same unreleased window and was documented in
`docs/conversation-persistence.md` and the OpenAPI schema, but never announced here. It is announced
now because the envelope-encryption release train ([#86](https://github.com/nurcahyo/moira/issues/86))
builds directly on it, and an operator should meet this break on its own terms rather than buried
inside an encryption release.

**Before.** `conversation_content_persistence` was a setting that read back correctly and changed
nothing. `add_message` bound the caller's text into `conversation_messages.content_plain`
unconditionally, and the write path never consulted the policy anywhere. An operator who selected
`none` or `metadata_only` — deliberately, to keep message bodies out of the database — got full
plaintext stored anyway, with two source comments asserting the policy was honoured. The setting was
a data-protection control that protected nothing.

**After.** The policy is enforced at two write points, both of which persist a body derived from
caller content:

| Value | Message body | Length-revealing metadata |
| --- | --- | --- |
| `plain_content` (default) | stored | stored |
| `metadata_only` | **not stored** | stored |
| `none` | **not stored** | **not stored** — `content_size_bytes` is `0`, `token_count` is null |
| `encrypted_content` | **not stored** | stored |

Enforcement lives in `add_message` rather than at its three application-layer callers, deliberately:
it is the only path into `conversation_messages`, so a fourth writer inherits the policy instead of
having to remember it. The second point is the summarization write — a summary derived from bodies
the policy excludes is withheld rather than stored.

**`encrypted_content` is refused on write.** `PUT /api/v1/admin/applications/{application_id}/conversation-policy`
now returns **422 `conversation_content_persistence_unsupported`** for that value. No cipher is
wired to any `*_encrypted` column today, so accepting it would promise encryption Moira does not
perform. Rows that already hold the value keep parsing and keep failing closed — they store no
plaintext, which is the half of the promise that can be kept — but the value can no longer be set.

**This refusal is temporary in its current form.** PR 5 of the encryption train narrows it rather
than removing it: once a cipher exists, `encrypted_content` becomes settable and the 422 fires only
when encryption is configured but unusable at write time. See
`docs/decision-encryption-at-rest.md` §"`conversation_content_persistence_unsupported` (422) —
narrows, does not disappear".

**Who is affected.**

- Any deployment or IaC that writes `encrypted_content` on this endpoint. It previously succeeded
  and now fails with a 422 — this is the break most likely to surface at upgrade, because it fires
  from configuration management rather than from traffic.
- Any application already set to `none` or `metadata_only` and silently relying on the plaintext
  being readable back through the conversation API. Those bodies stop being written.

**Before upgrading**, find the applications whose stored policy is about to start mattering:

```sql
select application_id, conversation_content_persistence
from application_conversation_policies
where conversation_content_persistence is not null
  and conversation_content_persistence <> 'plain_content';
```

An empty result means this release changes nothing for you. It is not retroactive: plaintext already
stored under `plain_content` stays stored and stays readable. Removing it is a retention concern,
not a persistence-policy one.

**What this does not govern.** `memory_records.content_plain` and `rag_document_versions.content_plain`
are written under their own policies; this value is scoped to conversation messages and the summaries
derived from them. `content_hash` is retained under every value — it is an HMAC under a
deployment-held pepper, not a content address — as is caller-supplied `metadata`, which is the
caller's own JSON rather than something derived from the message body.
