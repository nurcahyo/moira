# Response Persistence

Phase 4 creates a metadata-only public response record.

Stored:

- response id
- execution id
- request id
- application and external identity bindings
- route, provider, and model references
- status and safe failure fields
- usage summary
- caller metadata after validation
- retention timestamp

Not stored:

- prompt body
- assistant output body
- provider request/response headers
- raw provider response body
- raw authorization headers
- provider credentials or API keys

The policy enum includes `none`, `metadata_only`, `encrypted_content`, and `plain_content`, but the implemented Phase 4 path always returns `output_persisted:false` and does not persist content bodies. Plain content persistence should not be enabled for production.

