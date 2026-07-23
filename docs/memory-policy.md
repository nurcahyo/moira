# Memory Policy

Admin endpoints:

- `GET /api/v1/admin/applications/{application_id}/memory-policy`
- `PUT /api/v1/admin/applications/{application_id}/memory-policy`

Defaults are conservative:

- memory disabled
- consent mode `explicit_only`
- automatic extraction disabled
- automatic retrieval disabled
- manual memory disabled
- normal sensitivity only

Public memory operations fail closed unless policy enables them.

