# Context Budgeting

Public requests are bounded before dispatch.

- Request size is capped by `public_api.maximum_request_bytes`.
- Message count is capped by `public_api.maximum_messages`.
- Content item count is capped by application policy `maximum_input_items`.
- Text part size is capped by `public_api.maximum_text_part_bytes`.
- Image count is capped by `public_api.maximum_image_count`.
- Output tokens are capped by application policy `maximum_output_tokens`.
- Timeout overrides require policy allowance and `moira:execution:override-timeout`.

## Context assembly budget (plan 11 Sub-Phase D)

`application_conversation_policies.maximum_history_tokens` bounds **everything the planner
assembles**, including the caller's own turn.

- The caller's turn is required and is never truncated.
- Optional sections are dropped in `ContextPlanner::optional_drop_order()`. `recent_messages` is
  thinned oldest-first rather than dropped wholesale — the turn immediately before the current
  one is almost always the most useful context there is.
- If the caller's turn alone exceeds the budget, the response is `422
  moira.error.context_length_exceeded`, never a silent truncation. The envelope's `details`
  carries `reason`, `required_tokens` and `maximum_history_tokens` as **numbers**, so an operator
  can diagnose without diagnostic-scope access and without prose carrying structured data.

## The token estimate is an approximation, and says so

Moira has no tokenizer and `rig-core` 0.40 exposes none. `budget_tokens` takes the **maximum** of
the whitespace word count and `chars / 4`.

The maximum, not either alone, because the failure directions are asymmetric: under-counting lets
Moira assemble a context the provider then rejects, while over-counting only wastes headroom. The
pre-existing `estimate_tokens` (`split_whitespace().count()`) counts a 4000-character Japanese
message as one token, which is exactly the dangerous direction.
`the_token_estimate_errs_toward_over_counting` pins it.

The `chars / 4` ratio is the widely-used English rule of thumb and is documented as an
approximation everywhere it appears — it is not claimed to be a measurement.
