# Scaling

Scale Moira API pods horizontally behind a load balancer.

Primary shared bottlenecks:

- PostgreSQL connection pool
- provider concurrency limits
- Redis locks and token buckets
- streaming connection count
- vector search indexes

Use HPA for CPU as a baseline, but production autoscaling should also consider request latency, active streams, queue depth, and provider saturation.

Current in-memory rate and concurrency controllers are still tracked as Phase 6 TODOs for distributed replacement.
