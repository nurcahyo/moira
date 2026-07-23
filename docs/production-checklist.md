# Production Checklist

- Database migrations validated against clean pgvector PostgreSQL.
- `MOIRA_DEPLOYMENT__ENVIRONMENT=production` validation passes before rollout.
- API startup has `database.migrate_on_startup=false`; migrations run through
  `moira migrate`.
- Exactly one API replica is configured for the MVP.
- Root system key created through the bootstrap CLI.
- Master encryption key and API-key pepper loaded from a production secret source.
- Admin docs exposure intentionally configured.
- Redis readiness enabled and monitored when distributed coordination is enabled.
- `/health/live`, `/health/ready`, and `/metrics` scraped by the platform.
- Kubernetes probes, PDB, NetworkPolicy, and resource requests set; HPA remains
  disabled while process-local controls require one replica.
- Provider credentials rotated and plaintext responses verified absent.
- Audit export path tested.
- Backup and restore drill completed.
- Load and chaos tests executed against a staging environment.
- Security scans and dependency audits are clean or explicitly accepted.
