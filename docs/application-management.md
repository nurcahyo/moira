# Application Management

Applications are managed with internal UUIDv7 path IDs. External application IDs and slugs are resource fields and filters.

Endpoints:

- `POST /api/v1/admin/applications`
- `GET /api/v1/admin/applications`
- `GET /api/v1/admin/applications/{id}`
- `PATCH /api/v1/admin/applications/{id}`
- `DELETE /api/v1/admin/applications/{id}`
- `POST /api/v1/admin/applications/{id}/enable`
- `POST /api/v1/admin/applications/{id}/disable`

Create requires at least one of `external_application_id` or `application_slug`. Patch updates identifiers, display name, and metadata only. Enable and disable are explicit action endpoints.

Required scopes:

- Read/list/get: `moira:applications:read`
- Create/patch/enable/disable: `moira:applications:write`
- Delete: `moira:applications:delete`

Mutable responses include `version`; clients must send `If-Match: "<version>"` for patch/delete/action requests.
