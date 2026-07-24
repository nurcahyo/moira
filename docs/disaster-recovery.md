# Disaster Recovery

Durable state lives in PostgreSQL/pgvector. Redis stores only ephemeral coordination state.

Back up:

- PostgreSQL base data
- WAL archives
- pgvector tables and indexes
- secret-manager versions and rotation metadata
- deployment configuration and Helm values

Restore drill:

1. Restore PostgreSQL to a staging namespace.
2. Recreate required secrets from the secret manager.
3. Start Moira with Redis empty.
4. Run `/health/ready`.
5. Verify admin auth, public response creation, credential resolution, conversation access, and RAG metadata queries.

Document target RPO/RTO per environment. Redis loss should reduce coordination quality temporarily, not lose durable user data.
