# Production Checklist

- Database migrations validated against clean pgvector PostgreSQL.
- Root system key created through the bootstrap CLI.
- Master encryption key and API-key pepper loaded from a production secret source.
- Admin docs exposure intentionally configured.
- Redis readiness enabled and monitored when distributed coordination is enabled.
- `/health/live`, `/health/ready`, and `/metrics` scraped by the platform.
- Kubernetes probes, HPA, PDB, NetworkPolicy, and resource requests set.
- Provider credentials rotated and plaintext responses verified absent.
- Audit export path tested.
- Backup and restore drill completed.
- Load and chaos tests executed against a staging environment.
- Security scans and dependency audits are clean or explicitly accepted.
