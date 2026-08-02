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

## What `GET /api/v1/responses/{response_id}` returns instead of the output

A **completed** response read back later never carries an empty `output` array. An empty array
there would be indistinguishable from a model that returned nothing, which is a legitimate and
very different result — an empty completion is served as `output_text` with `text: ""`. Instead
the read returns one `output_unavailable` part whose `reason` names the persistence mode
recorded on the response row at the moment it completed, not the application's policy today:

| recorded `persistence_mode` | `reason` |
|---|---|
| `metadata_only` | `metadata_only_persistence` |
| `none` | `persistence_disabled` |
| `plain_content`, `encrypted_content` | `content_persistence_not_implemented` |

`reason` is an unconstrained string in the OpenAPI schema and these values may grow. Do not
branch on it for control flow; it exists to explain a missing body to a human.

Responses that are queued, in progress, failed or cancelled return `output: []`, because they
genuinely produced no output and `status` already says which.

