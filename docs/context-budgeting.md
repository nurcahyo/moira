# Context Budgeting

Public requests are bounded before dispatch.

- Request size is capped by `public_api.maximum_request_bytes`.
- Message count is capped by `public_api.maximum_messages`.
- Content item count is capped by application policy `maximum_input_items`.
- Text part size is capped by `public_api.maximum_text_part_bytes`.
- Image count is capped by `public_api.maximum_image_count`.
- Output tokens are capped by application policy `maximum_output_tokens`.
- Timeout overrides require policy allowance and `moira:execution:override-timeout`.

The current budgeter validates explicit request sizes and configured limits. Provider-specific tokenizer estimation remains in the runtime/model layer.

Phase 5 adds a context-planning boundary for conversation history, summaries, memory, and RAG. The current implementation records the deterministic ordering and schema; tokenizer-aware memory/RAG budget enforcement remains pending.
