# Security

Production security expectations:

- no plaintext provider secrets in HTTP responses, audit metadata, traces, metrics, or logs
- no raw API keys after create or rotate responses
- no prompt, memory, document, embedding, or retrieved-context content in metrics or logs
- deny-by-default authorization scopes
- HTTPS-only provider/JWKS URLs unless explicit local development opt-in is enabled
- non-root container runtime
- read-only root filesystem
- dependency, container, secret, SAST, DAST, and ASVS checks in CI/CD

Environment variables remain supported for development. Production should use Vault, AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, Kubernetes Secrets, or an external-secrets operator.
