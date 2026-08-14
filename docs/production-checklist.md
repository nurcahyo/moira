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

Admin console:

- Console chart left at `replicaCount: 1`, with `autoscaling` and
  `podDisruptionBudget` disabled, until the `auth-runtime.ts` snapshot is shared.
- Exactly one enabled `auth_provider_settings` row, and it is bound to a trusted
  JWT issuer with a non-empty `allowed_email_domains`.
- After any Stage 4A rollout: migration `0020` recorded successful, the partial
  unique index `auth_provider_settings_one_enabled_per_trusted_issuer` present,
  and the `ambiguous_enabled_providers` guard still in place until both are
  verified — [console-multi-provider-rollout.md](console-multi-provider-rollout.md).
