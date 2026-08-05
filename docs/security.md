# Security

Production security expectations:

- no plaintext provider secrets in HTTP responses, audit metadata, traces, metrics, or logs
- no raw API keys after create or rotate responses
- no prompt, memory, document, embedding, or retrieved-context content in metrics or logs
- deny-by-default authorization scopes
- HTTPS-only provider/JWKS/image URLs unless explicit local development opt-in is enabled
- outbound URLs supplied by callers or admins pass the shared SSRF guard in
  `src/security/ssrf.rs` before use (see "Outbound URL guard" below)
- non-root container runtime
- read-only root filesystem
- dependency, container, secret, SAST, DAST, and ASVS checks in CI/CD

## Outbound URL guard

One module, `src/security/ssrf.rs`, decides whether Moira may use an outbound URL. It
classifies the address space — loopback, RFC1918, link-local (which covers every cloud
metadata endpoint), CGNAT, unique-local, and the IPv4-in-IPv6 encodings of all of them —
resolves hostnames through the OS resolver under an explicit budget, and requires **every**
resolved address to be permitted, not merely the first.

Two callers share it, and they differ in one way that matters:

| | JWKS URL | Public image URL |
|---|---|---|
| Configured by | an admin | any caller, in a request body |
| Who performs the fetch | Moira | the provider |
| Response controls (content type, byte cap, redirect refusal) | enforced by Moira | **not available** |
| Egress allow-list | not needed | `public_api.image_urls.allowed_hosts` |

Because Moira hands the image URL to the provider rather than fetching it, the guard on
that path is *admission control*, not a fetch policy. The controls that police a response
cannot apply, and the provider resolves DNS again when it connects — so a hostname whose
answer changes between validation and use is not fully closed by validation alone. The
egress allow-list is the control that does not depend on resolution timing, and a
deployment that knows which origins its images come from should set it.

The dev escape hatches (`auth.jwks.allow_insecure_dev_urls`,
`public_api.image_urls.allow_insecure_dev_urls`) are deliberately separate, so loosening
one trust surface for local development does not silently loosen the other. Production
start-up refuses to come up while either is true.

## How long a verification key survives its issuer

A JWKS document is cached for five minutes. When a refresh fails, the last-known-good copy
keeps answering, so a transient outage at the identity provider does not break
authentication — but only up to `auth.jwks.max_stale_seconds` (default 24 hours) measured
from the last *successful* fetch. Past that ceiling the entry is evicted and authentication
against that issuer fails closed.

That bound is what keeps key rotation and key revocation meaningful. Without it, an issuer
that stays unreachable leaves its keys verifying tokens for as long as the process lives, so
retiring a signing key at the provider would have no effect here. Two operational
consequences worth planning for:

- an identity provider that is unreachable for longer than the ceiling will start failing
  authentication, by design — raise `max_stale_seconds` only with that trade in view;
- a successful fetch restarts the clock, so an issuer that merely refreshes slowly never
  approaches the ceiling.

The ceiling is validated at startup, in every environment. It must be greater than zero and
at least the five-minute freshness window: below that it would be a bound the cache never
consults, because a still-fresh entry is served without reference to it, and a setting that
silently means less than it says is worse than no setting. Both cases refuse to start rather
than run on a ceiling that does not hold.

Environment variables remain supported for development. Production should use Vault, AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, Kubernetes Secrets, or an external-secrets operator.
