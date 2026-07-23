# Client Cancellation

For non-streaming requests, the HTTP future owns the execution call. If the client disconnects, normal Axum cancellation drops the request future when the server observes the disconnect.

For streaming requests, a supervisor owns execution after the HTTP handler returns.
Dropping the client stream closes its bounded channel, cancels the execution, waits
for the active attempt to become terminal, persists the response as `cancelled`,
and records `response.stream.cancelled`. Partial output is not appended to
conversation history.

Clients should use `X-Request-Id` for correlation and retry non-streaming creates with the same `Idempotency-Key` when they did not receive a terminal response.
