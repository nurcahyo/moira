# Client Cancellation

For non-streaming requests, the HTTP future owns the execution call. If the client disconnects, normal Axum cancellation drops the request future when the server observes the disconnect.

For streaming requests, the SSE stream owns execution dispatch. Dropping the stream cancels any still-pending future at the transport boundary. Because Phase 4 uses the Phase 3 event collector, disconnect auditing is best-effort and may not record a distinct `response.stream.disconnected` event in every race.

Clients should use `X-Request-Id` for correlation and retry non-streaming creates with the same `Idempotency-Key` when they did not receive a terminal response.

