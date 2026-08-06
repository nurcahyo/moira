# Release notes

Operator-facing changes that alter behaviour on upgrade. Newest first.

Only entries that change what a running deployment does belong here. Additions that no existing
request can reach do not — an operator reading this file is looking for what will break, not for a
changelog.

## Unreleased

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
