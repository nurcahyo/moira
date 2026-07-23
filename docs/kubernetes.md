# Kubernetes

Kubernetes assets live in `deploy/kubernetes/moira.yaml` and `charts/moira`.

The manifests include:

- Deployment
- Service
- Ingress
- ConfigMap
- Secret placeholder
- HPA
- PDB
- ServiceMonitor
- NetworkPolicy
- PriorityClass
- RBAC

Security defaults:

- non-root UID `10001`
- read-only root filesystem
- dropped Linux capabilities
- liveness and readiness probes
- no plaintext production secrets committed

Before production, replace the placeholder image and secret values, narrow NetworkPolicy egress to approved providers and secret stores, and validate rendered manifests with `helm lint`, `helm template`, and `kubeconform`.
