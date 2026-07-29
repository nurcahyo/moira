# Admin Identity Claiming

Moira grants a *human* admin authority by binding `moira:admin` to a stable `(issuer, subject)` pair from an already-registered trusted JWT issuer. It issues no password, no session cookie and no login page: once a grant exists, that human's existing trusted-JWT bearer token simply carries more authority on the admin plane.

## The setup order is deliberate — configure the domain policy before the first claim

**A correctly configured, freshly deployed Moira refuses its very first claim with `403 admin_claim_domain_not_allowed`.** That is the designed order asserting itself, not a defect. Perform these four steps, in this order:

1. **Bootstrap the system key** — `moira bootstrap-system-key`, unchanged. This is the break-glass root and the only credential the claim endpoint accepts.
2. **Register the trusted JWT issuer** — `POST /api/v1/admin/jwt-issuers`. Moira never accepts a free-text issuer at claim time; an unregistered or inactive issuer is `400 unregistered_trusted_issuer`. An issuer a console links **must** leave `scopes_claim` unset, or its tokens could self-assert authority and `admin_identities` would stop being the sole source of human authorization (`400 console_issuer_must_not_assert_scopes`).
3. **Create and enable the auth provider configuration, including `allowed_email_domains`** — `POST /api/v1/admin/auth/providers`, then `POST /api/v1/admin/auth/providers/{id}/enable`. `enabled` defaults to `false`, so a half-configured method can never be live by accident.
4. **Claim** — `POST /api/v1/admin/setup/claim` with `X-Moira-System-Key`, naming `issuer`, `subject`, `email` and `email_verified`.

Skipping step 3 is what produces the first-claim 403.

## The email-domain policy is deny-by-default, with no exemptions

A claim is refused unless an **enabled** `auth_provider_settings` row governs the target issuer **and** its `allowed_email_domains` explicitly contains the email's domain.

- An **empty** `allowed_email_domains` means **deny all**. There is no "empty means unrestricted" reading.
- **No** configuration governing the issuer denies too — it is a stricter case of "no allowed domains" and shares the same error code, deliberately, because distinguishing them on the wire would tell an unprivileged caller whether a policy exists, and both have the same operator remedy.
- There is **no first-claim exemption and no bootstrap bypass.** Holding the system key authorises you to *submit* a claim; it does not exempt you from *policy*.
- Matching is case-insensitive, on the substring after the last `@`, and **exact**: `example.com` does not admit `sub.example.com`.

A bypass would exist precisely during the setup window, when a deployment is least defended and most attractive — and from the outside it would be indistinguishable from the "first successful admin JWT wins" land-grab this design exists to make structurally impossible. A patch reintroducing one is a security regression to reject in review.

`email` and `email_verified` are required on the claim body. `email_verified` must be `true`. Together they are what make the domain policy enforceable and put a human-identifiable attribute on every grant, so an audit reader can answer "which human holds this?" from the grant row alone.

## Endpoints

| Endpoint | Auth | Notes |
|---|---|---|
| `GET /api/v1/admin/setup/claim-status` | **none** | Returns `{"claimed": bool}` and nothing else. Anonymous because one bit is all it reveals, and a setup wizard needs it before any human holds a credential. |
| `POST /api/v1/admin/setup/claim` | `X-Moira-System-Key` **only** | A bearer JWT is refused even if it verifies (`401 setup_claim_credential_required`). Supports `Idempotency-Key`: a keyed retry replays with `200`, an unkeyed retry conflicts with `409`. |
| `GET /api/v1/admin/setup/auth-methods` | system key or trusted JWT, plus `moira:setup:read` | Authenticated on purpose: the response is identity *configuration*, which is reconnaissance-worthy. A console calls it server-side. |
| `/api/v1/admin/auth/providers…` | `moira:auth-settings:{read,write,delete}` | Seven operations. Every mutation requires `If-Match`. |

`GET /api/v1/admin/setup/status` is a **different, pre-existing** endpoint answering "is the provider/routing configuration structurally complete", and is untouched by this surface.

## After a grant

The granted human's next trusted-JWT request to `/api/v1/admin/*` resolves to an actor carrying the granted scopes. The grant applies on the **admin plane only** — it never widens the public execution API (`/api/v1/responses` and friends), even though both paths verify the same token. Revoking a grant is a direct database operation today (`admin_identities.status`, `revoked_at`); a revoke endpoint is a deferred follow-up.

`setup_state` records whether *any* identity has **ever** been claimed, independently of grant status, so revoking the sole admin cannot silently reopen the unauthenticated claim window. Re-opening it is a deliberate operator action, not an automatic side effect.

## What Moira does not store

Moira holds **no OAuth client secret**, anywhere. `auth_provider_settings` carries non-secret configuration only; no request DTO has a `client_secret` field (sending one is a loud schema error, never a silent drop), no response contains secret material, and there is no `rotate-secret` operation. The secret belongs to the console, in the console's own database, because Better Auth needs the plaintext in process — and making it readable over HTTP would break Moira's invariant that a decrypted secret never crosses a network boundary.

Moira is the source of truth for `client_id` and returns it on every read path, which is what lets a console detect a `client_id` that moved out from under the secret it stored.
