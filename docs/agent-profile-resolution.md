# Agent profile resolution — fail-closed

A route definition may name an agent profile (`route_definitions.agent_profile_id`). The profile
supplies the execution's `preamble`, `temperature` and `max_tokens`.

**If the route names a profile the runtime cannot use, Moira refuses the request.** It does not
serve it without the profile. This is the decision taken on
[issue #79](https://github.com/nurcahyo/moira/issues/79) on 2026-08-06, closing finding F50.

## Why fail-closed

A preamble is where guardrails live. Serving a request whose route declares a preamble, without
that preamble, is an unguarded model answering production traffic under a configuration that says
otherwise — and until F50's observability shipped it did so reporting `succeeded`, so nothing
downstream could tell the difference. The fail-open alternative keeps such a deployment serving,
but what it keeps serving is the thing the operator's configuration says must not happen.

There is **no setting** that restores the old behaviour, and that is deliberate. A flag here would
be a second code path plus a chance to be silently inert — on the day this was decided, a
pre-existing switch in this repository (`accept_legacy_hashes`) was found never to have been wired
to the code that reads it.

## The two answers, and why they are different

Resolution reads the `agent_profiles` row **without** filtering on `status` or `deleted_at`
(`RuntimeRepository::find_agent_profile_reference`), then classifies it
(`domain::AgentProfileResolution`):

| Condition | Failure class | HTTP | Remedy |
| --- | --- | --- | --- |
| `status = 'active'`, not deleted | — | — | the profile is used |
| `status = 'disabled'` | `agent_profile_disabled` | `409 Conflict` | re-enable the profile, or point the route at an active one |
| no row, or `deleted_at` set / `status = 'deleted'` | `agent_profile_not_found` | `404 Not Found` | create a profile and repoint the route |

The distinction is the point. Re-enabling is a one-field admin write; a soft-deleted profile cannot
be re-enabled at all, because every admin write filters `deleted_at is null` and
`GET /api/v1/admin/agent-profiles/{id}` already answers `404` for that id. A single code would make
the receiver go and look, and a single status would make Moira contradict its own admin plane.

`404` puts `agent_profile_not_found` in the family it belongs to: `route_not_found`,
`model_not_found` and `credential_not_found` are the same shape — a reference on the resolution
chain that does not resolve — and none of those is named by the caller either. `409` for the
disabled case is the state conflict it is: the resource exists, it is addressable, and its current
state forbids the request. It is not `503`, which would promise that waiting helps, and not `502`,
which would blame a provider that was never contacted.

## What the caller is told

The error `message` names the route, the profile id, and the profile key when a row still exists —
enough to correct the deployment without access to the server's logs. The profile's `preamble` is
never included: it is the one field on an agent profile that can carry sensitive prompt content.

On `POST /api/v1/responses/stream` the refusal arrives as the terminal `response.failed` event
carrying the same code, because the response head is written before the execution starts.

## What the operator is told

Unchanged from F50, and it still fires on the refusal path:

* a `warn!` naming the execution, the request, the route and the profile;
* an `agent_profile_unavailable` runtime event, returned verbatim by
  `POST /api/v1/admin/runtime/diagnose` and deliberately **not** mapped onto the public SSE stream;
* an `agent_profile.unavailable` audit row against the execution id.

All three carry `reason` (`"disabled"` or `"missing"`), which is the same distinction the caller's
error code makes, so the two sides of an incident describe one fact.

## What did not change

A route with **no** agent profile (`agent_profile_id` is `NULL`) is the normal case. It executes,
and it announces nothing. Only a route that names a profile and cannot use it is refused.
