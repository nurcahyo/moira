# Scaling

Scale Moira API pods horizontally behind a load balancer.

Primary shared bottlenecks:

- PostgreSQL connection pool
- provider concurrency limits
- Redis locks and token buckets
- streaming connection count
- vector search indexes

Use HPA for CPU as a baseline, but production autoscaling should also consider request latency, active streams, queue depth, and provider saturation.

## Rate limiting and concurrency across replicas

A distributed backend for the two controllers that need one already exists and is **off by
default**. `RedisSettings::enabled` defaults to `false` (`src/infra/redis.rs`), and every caller
takes an `Option<RedisClient>`, so the shipped configuration is Postgres plus per-process memory.

Enabling Redis buys exactly two things process memory cannot give:

- a cluster-wide rate-limit window (`RedisClient::check_rate_window`)
- a cluster-wide concurrency counter (`RedisClient::try_acquire_permit` / `release_permit`, with
  a permit TTL so a leaked slot is reclaimed)

The arithmetic is written to match `InMemoryRateLimiter` exactly, so the two backends cannot
disagree about a boundary.

Everything else Moira coordinates is already correct across replicas without Redis: cluster
admission, leader election, idempotency, and runtime-config invalidation. Runtime-config
invalidation in particular is authoritative over Postgres `LISTEN/NOTIFY` fired by a database
trigger (`src/infra/db.rs`); Redis Pub/Sub is an optional second channel, not the source of
truth.

## What stays per-process on purpose

Circuit-breaker state is **not** shared, and that is a decision rather than a gap. A breaker is
opened by a replica observing its own transport failures. Sharing that state would let one
replica's bad network path open the circuit for healthy replicas, converting a local fault into a
cluster-wide outage. Breakers stay per-process even with Redis enabled.
