# Load Testing

Load tests should cover both streaming and non-streaming public response flows.

Scenarios:

- 1k concurrent users
- 5k concurrent users
- 10k concurrent users
- 50k concurrent users

Measure:

- CPU
- memory
- HTTP latency
- TTFT
- tokens/sec
- database utilization
- Redis latency
- network throughput
- provider errors and fallback rate

Do not use production secrets or real customer prompts in test fixtures.
