# Provider Management

Providers store non-secret runtime configuration. Secret provider headers, organization keys, and project keys belong in provider credentials.

Endpoints:

- `POST /api/v1/admin/providers`
- `GET /api/v1/admin/providers`
- `GET /api/v1/admin/providers/{id}`
- `PATCH /api/v1/admin/providers/{id}`
- `DELETE /api/v1/admin/providers/{id}`
- `POST /api/v1/admin/providers/{id}/enable`
- `POST /api/v1/admin/providers/{id}/disable`
- `POST /api/v1/admin/providers/{provider_id}/models`
- `GET /api/v1/admin/providers/{provider_id}/models`
- `GET/PATCH/DELETE /api/v1/admin/provider-models/{id}`
- `POST /api/v1/admin/provider-models/{id}/enable`
- `POST /api/v1/admin/provider-models/{id}/disable`

Provider base URLs pass the outbound URL policy: HTTPS by default, no embedded credentials, no loopback/private/link-local/multicast/cloud metadata targets, and DNS resolution checks. HTTP/private URLs require explicit local development opt-ins.

## Where a provider credential is allowed to be sent

Every address Moira sends a decrypted provider credential to is governed by the two
`provider_security` flags, at **both** write time and use time. Read this section before
changing either flag or before debugging a provider that stopped executing after an upgrade.

| Setting | Waives | Rejected in production by `Settings::validate_production`? |
| --- | --- | --- |
| `provider_security.allow_http_provider_urls` | the `https` requirement | yes |
| `provider_security.allow_private_provider_urls` | loopback / RFC1918 / link-local / other non-routable addresses | no |

The two are independent. Granting only `allow_private_provider_urls` — the normal shape for an
in-cluster provider reachable at `https://10.x.y.z` — keeps the HTTPS requirement in force, and
that combination works everywhere: the admin write accepts it and so does every execution.
Nothing waives the cloud instance-metadata addresses (`169.254.169.254`,
`metadata.google.internal`, and the rest); those are refused with every flag granted.

Three inputs feed that address, and all three are now checked:

1. **`providers.base_url`** — checked on `POST`/`PATCH` (including a DNS resolution), and
   re-checked, without DNS, every time a model is built. The re-check is the backstop for a
   row that reached the table without an admin request — a migration, a restore, a direct
   `psql` edit.
2. **An `azure_openai` credential's `secret.endpoint`** — the Azure arms of both the completion
   and the embedding factory read this **in preference to** `providers.base_url`. Before this
   change nothing validated it at all, which made `moira:credentials:write` on its own enough
   to point a credential-bearing request at an internal address that the providers surface
   would have refused. It is now held to exactly the rules above, on create and on rotate, and
   re-checked at every model build.
3. **`providers.metadata["oauth_token_endpoint"]`** — validated immediately before every
   refresh by the `oauth-token-refresh` worker, never at write time. See below.

### `oauth-token-refresh` is stricter than the rest of the provider surface, on purpose

The token endpoint carries a **decrypted refresh token**, the longest-lived secret Moira
stores, so the refresh worker requires **both** flags before it will use an address the
address-space rules would otherwise refuse — not either one. `Settings::validate_production`
rejects `allow_http_provider_urls`, so that conjunction is unsatisfiable in production by
construction.

**The operational consequence, stated plainly:** a production deployment whose identity
provider is only reachable on a private in-cluster address **cannot use OAuth token refresh**.
`allow_private_provider_urls` alone buys nothing here, and there is no third flag. The
supported remedy is to give the IdP a publicly-resolvable `https` name (it does not have to be
publicly *reachable* — a split-horizon or internal-CA name that resolves to a routable address
is fine); the unsupported one is running the deployment as non-production. If your IdP cannot
have such a name, `oauth-token-refresh` is not usable for it and credentials on that provider
must be rotated by another means.

The refresh worker additionally uses a dedicated HTTP client with `redirect::Policy::none()`,
so a validated endpoint that answers `307`/`308` never causes the refresh token to be re-sent
to the `Location` target — validating a URL says something about the request Moira issues, not
about wherever the response points next.

Creating a `chatgpt_oauth` provider (issue #216) additionally requires `provider_security.allow_chatgpt_subscription = true`. It is off by default in every environment, including production: ChatGPT/Codex subscriptions are personal, single-user under OpenAI's terms, and wiring one into a multi-tenant gateway is a deployment operator's own explicit ToS risk acceptance, never a silent default. Without the opt-in, `POST /api/v1/admin/providers` refuses the request with `403 chatgpt_subscription_opt_in_required`; the same gate is re-checked at execution time as defense in depth. See `docs/chatgpt-subscription-spike.md` and `docs/rig-integration.md`.

Required scopes:

- Providers read/write/delete: `moira:providers:*`
- Models read/write/delete: `moira:models:*`
