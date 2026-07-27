# Kubernetes

Kubernetes assets live in `deploy/kubernetes` and `charts/moira`. Helm is the
recommended production path because its migration Job is a blocking
`pre-install,pre-upgrade` hook.

The manifests include:

- Deployment
- Service
- Ingress
- ConfigMap
- Secret placeholder
- PDB
- ServiceMonitor
- NetworkPolicy
- PriorityClass
- RBAC

Security defaults:

- non-root UID `65532`, the sole non-root user in `gcr.io/distroless/cc-debian12:nonroot`.
  The deployment and the migration job both pin it; the image has no shell or package manager
  with which to create another, so all three must agree.
- read-only root filesystem
- dropped Linux capabilities
- liveness and readiness probes
- no plaintext production secrets committed

The raw manifests are validation and customization references, not an
upgrade-safe release workflow. A raw-manifest rollout must be orchestrated by a
release system that recreates `migration-job.yaml`, waits for completion, and
only then applies or restarts the API Deployment.

Before production, replace the placeholder image and secret values, narrow
NetworkPolicy egress to approved database and provider ranges, and validate
rendered manifests with `helm lint`, `helm template`, and `kubeconform`.
