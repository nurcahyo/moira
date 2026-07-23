# Trusted JWT Issuer Management

Trusted JWT issuers are database-backed and used for administrative bearer token validation.

Endpoints:

- `POST /api/v1/admin/jwt-issuers`
- `GET /api/v1/admin/jwt-issuers`
- `GET/PATCH/DELETE /api/v1/admin/jwt-issuers/{id}`
- `POST /api/v1/admin/jwt-issuers/{id}/enable`
- `POST /api/v1/admin/jwt-issuers/{id}/disable`
- `POST /api/v1/admin/jwt-issuers/{id}/refresh-jwks`

Issuers must use safe algorithms. `none` and symmetric `HS*` algorithms are rejected. JWKS URLs use the same SSRF policy as provider URLs. Refresh failures must not erase a previously valid cache.

Required scopes:

- Read/list/get: `moira:jwt-issuers:read`
- Create/patch/enable/disable/refresh: `moira:jwt-issuers:write`
- Delete: `moira:jwt-issuers:delete`
