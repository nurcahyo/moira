# Provider Credential Management

Provider credentials store encrypted secret material and never return plaintext.

Endpoints:

- `POST /api/v1/admin/provider-credentials`
- `GET /api/v1/admin/provider-credentials`
- `GET/PATCH/DELETE /api/v1/admin/provider-credentials/{id}`
- `POST /api/v1/admin/provider-credentials/{id}/rotate`
- `POST /api/v1/admin/provider-credentials/{id}/enable`
- `POST /api/v1/admin/provider-credentials/{id}/disable`
- `PUT /api/v1/admin/users/{external_user_id}/provider-credentials/{provider_id}`
- `GET /api/v1/admin/users/{external_user_id}/provider-credentials`
- `DELETE /api/v1/admin/users/{external_user_id}/provider-credentials/{id}`

Create and rotate use a tagged `scope` object and a credential-type-specific `secret` payload. Responses include `masked_secret` and `secret_fingerprint`, never `secret`.

Custom headers reject dangerous outbound names including `Host`, `Authorization`, `Cookie`, `Set-Cookie`, `Forwarded`, and `X-Forwarded-*`.

Required scopes:

- Read: `moira:credentials:read`
- Create/patch: `moira:credentials:write`
- Rotate: `moira:credentials:rotate`
- Enable/disable: `moira:credentials:disable`
- Delete: `moira:credentials:delete`

Consumer principals may manage only credentials bound to their own application.
