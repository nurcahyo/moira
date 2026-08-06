# Release notes

Operator-facing changes that alter behaviour on upgrade. Newest first.

Only entries that change what a running deployment does belong here. Additions that no existing
request can reach do not — an operator reading this file is looking for what will break, not for a
changelog.

## Unreleased

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

**One knock-on inside Moira.** Memory extraction sends its own schema, so a model that wraps its
JSON in a ```` ```json ```` fence now fails the extraction run instead of being un-fenced by
`parse_candidates`. The run row says `structured_output_invalid` either way, the caller's response
is unaffected as always, and no memory is written in either case — the visible difference is that
a fenced reply used to be accepted.

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
