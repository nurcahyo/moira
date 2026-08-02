# Runtime Cache Invalidation

Administrative writes invalidate local runtime configuration caches after committed resource changes. PostgreSQL `NOTIFY` on `moira_runtime_config` broadcasts small invalidation hints containing only resource type and resource ID.

Notifications are not a durable event bus. Runtime caches retain TTLs so a missed notification cannot create permanent staleness. Redis is not required in Phase 3.

Invalidation-producing resources include applications, providers, provider models, provider credentials, trusted JWT issuers, system keys, consumer keys, route definitions, routing policies, agent profiles, provider runtime policies, auth provider settings, the `application_*_policies` family, and the RAG collection and document tables.

The authoritative list is not this paragraph. It is `TRIGGERED_RESOURCE_TYPES` in `src/infra/db.rs`, and that constant is pinned against `pg_trigger` by `tests/runtime_notify_inventory.rs` — in both directions, counted by trigger *function* rather than trigger name. Counting by name is wrong twice over: `auth_provider_settings`'s trigger is called `auth_provider_settings_notify`, and `ALTER TABLE … RENAME` carries a trigger across under its **old** name, which is how three renamed tables sat on this channel unclassified until F52 (`migrations/0023`).

**Every target is scoped, not just the breakers.** One notification is classified into an `InvalidationPlan { caches, circuits }`:

- **Circuit-breaker state.** `providers` and `provider_models` notifications clear the matching breaker entries; the tables in `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` clear none, because their rows cannot change whether a model provider is answering.
- **The three caches.** Cleared together for any configuration change, and not at all for the resource types named in `RUNTIME_DATA_RESOURCE_TYPES` — rows that are per-request data and cannot change anything cached. They share one flag deliberately: the only distinction a payload supports is "this is not configuration at all". Which config table invalidates which cache is a finer question nobody has established, and guessing it is how identity configuration goes stale (CONVENTIONS §7.2).

The two classification lists answer different questions and a table belongs to at most one. `agent_profiles` cannot change provider health, so it is circuit-unaffected — but it is very much configuration and must still clear the caches.

Narrowing is one-way. An unparseable payload, a `resource_id` that is not a UUID, or a resource type the listener does not recognise clears **everything** and resets **every** breaker, exactly as the code behaved before scoping existed. An unknown table is treated as unknown rather than assumed harmless.

**Attaching the NOTIFY trigger to a new table therefore means classifying it in the same change** — `tests/runtime_notify_inventory.rs` fails until the table is added to `TRIGGERED_RESOURCE_TYPES`, and `every_triggered_table_has_a_scope` fails until it is genuinely classified there. Conversely, a table whose rows turn out to be per-request data should **lose its trigger** rather than be classified as data while keeping it: a triggered table that clears no cache announces changes nothing acts on.

Phase 3 invalidates:

- runtime config cache
- provider runtime handle cache
- auth provider settings cache (the enabled auth methods behind `GET /api/v1/admin/setup/auth-methods` and, projected more narrowly, the anonymous `GET /api/v1/admin/setup/sign-in-methods` — one cache serves both, so the two reads cannot drift onto different snapshots of the same rows)
- model candidate calculations
- circuit-breaker configuration/state (scoped, as above)

```mermaid
flowchart TD
    A["Admin write"] --> B["PostgreSQL trigger"]
    B --> C["moira_runtime_config NOTIFY"]
    C --> D["Local listener"]
    D --> D2["Classify: InvalidationPlan"]
    D2 -->|"caches: true"| E["Clear config cache"]
    D2 -->|"caches: true"| F["Clear Rig runtime handles"]
    D2 -->|"caches: true"| G["Clear auth settings cache"]
    D2 --> H["Reset circuit registry (scoped)"]
```
