# Provider Management

Providers store non-secret runtime configuration. Secret provider headers, organization keys, and project keys belong in provider credentials.

Endpoints:

- `POST /api/v1/admin/providers`
- `GET /api/v1/admin/providers`
- `GET /api/v1/admin/providers/{id}`
- `PATCH /api/v1/admin/providers/{id}`
- `DELETE /api/v1/admin/providers/{id}`
- `POST /api/v1/admin/providers/{id}/enable`
- `POST /api/v1/admin/providers/{id}/disable`
- `POST /api/v1/admin/providers/{provider_id}/models`
- `GET /api/v1/admin/providers/{provider_id}/models`
- `GET/PATCH/DELETE /api/v1/admin/provider-models/{id}`
- `POST /api/v1/admin/provider-models/{id}/enable`
- `POST /api/v1/admin/provider-models/{id}/disable`

Provider base URLs pass the outbound URL policy: HTTPS by default, no embedded credentials, no loopback/private/link-local/multicast/cloud metadata targets, and DNS resolution checks. HTTP/private URLs require explicit local development opt-ins.

Required scopes:

- Providers read/write/delete: `moira:providers:*`
- Models read/write/delete: `moira:models:*`
