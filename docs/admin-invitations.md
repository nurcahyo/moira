# Admin Invitations

The companion to [docs/admin-identity-claiming.md](admin-identity-claiming.md). That runbook is
about the **first** admin, granted with the bootstrap system key. This one is about **every admin
after them**, granted without it.

That is the whole point: while the only path to a grant is `POST /api/v1/admin/setup/claim`, which
accepts `systemKeyAuth` and nothing else, the bootstrap credential can never be retired. An
invitation is the non-system-key path.

## What an invitation is

A single-use, time-capped token, bound to **one email address** or **one email domain**, hashed at
rest with Argon2id + pepper under the `moira_inv` namespace — the same hasher the API keys use, not
a bare SHA-256.

There is deliberately **no "anyone with the link" invitation**. All three create fields are
required, because an unbound invitation would make a leaked URL equivalent to handing out admin.

## The operator sequence

1. **Create** — `POST /api/v1/admin/admin-invites`
   ```json
   { "constraint": "email", "value": "colleague@example.com", "expires_in_seconds": 86400 }
   ```
   Requires an existing admin's bearer token (or the system key). The response is the **only** time
   the raw token exists outside Moira's hash.

2. **Share the link out of band.** Moira sends no email and neither does the console. The link is
   `<console origin>/invite/<token>`.

3. **The invitee signs in** through whatever provider the deployment has configured, and redeems.
   `POST /api/v1/admin/admin-invites/redeem` declares `bearerAuth` **alone**: no token-asserted
   scope and no bootstrap credential can reach a path that mints a grant.

4. **They are an admin.** The grant is an ordinary `admin_identities` row — same shape, same
   revocation, same audit trail as one created by `claim`.

## The two bounds on `expires_in_seconds`

| | Value | Refusal |
|---|---|---|
| Floor | 60 seconds | `422 invalid_request` |
| Cap | 259 200 seconds (72 hours) | `422 admin_invite_expiry_too_long` |

**The cap is refused, not clamped.** An operator who believes they issued a 30-day invitation and
silently received a 3-day one finds out at the worst possible moment. Two different codes, because
the two remedies are different: "shorten it" and "lengthen it".

## The token is shown exactly once

`AdminInviteSecretResponse.secret` carries the raw token on creation and is **`null` on an
idempotent replay**, where the stored replay body is the sanitized record. That is the normal
outcome of a retried request, not a failure — a client that treats `null` as an error reports a
correct operation as broken, on the one path where people already suspect something went wrong.

Every later read of the invitation returns a shape with no token, no hash and no prefix.

If the link is lost, revoke the invitation and issue a new one. There is no re-display.

## Redemption refusals, and which are the invitee's problem

| Code | Status | What it means | Who fixes it |
|---|---|---|---|
| `invite_not_found` | 404 | No live invitation matches this token. Identical for a wrong prefix and a wrong hash, deliberately — the endpoint is not a guessing oracle. | The inviter issues a new one |
| `invite_expired` | 403 | Past `expires_at`. | The inviter issues a new one |
| `invite_revoked` | 403 | Withdrawn. | The inviter |
| `invite_already_consumed` | 409 | Single-use, already redeemed — possibly by this same person on another device. | Nobody; check the admin list |
| `invite_email_mismatch` | 403 | Signed in as a different address than the invitation names. | Sign in as the invited address, or ask for a new invitation |
| `invite_domain_mismatch` | 403 | Address is at a different domain than the invitation names. | As above |
| `admin_claim_domain_not_allowed` | 403 | The **deployment's** `allowed_email_domains` does not admit this address. | An operator widens the provider's allow-list |
| `admin_claim_email_not_verified` | 403 | The IdP did not verify the address. | The invitee, at their IdP |
| `admin_identity_already_claimed` | 409 | A grant already exists for this `(issuer, subject)`. | Nobody; check the admin list |

The last two rows in the "invite\_\*" block and `admin_claim_domain_not_allowed` are deliberately
**different codes** even though all three are 403s at redemption, because the remedies differ and
one of them is not the invitee's to perform.

`admin_identity_already_claimed` deserves care in any UI that renders it: `admin_identities` is
keyed on `(issuer, subject)` where `issuer` is the console's own, so on a multi-provider deployment
the holder of the colliding grant may be **somebody else**. Do not word it as "you already have
admin".

## An invitation is never a policy exemption

Plan 07 decision **D3** applies unchanged. The deny-by-default `allowed_email_domains` gate is
evaluated on redemption exactly as it is on `claim`, and holding a valid invitation does not waive
it. `email` and `email_verified` are required on redemption for the same reason they are required on
`claim` (**D5**): the grant they create has a non-nullable email, and a grant with no
human-identifiable attribute makes the domain policy unenforceable on that path.

**A policy-denied redemption does not consume the invitation.** The row stays `pending` and the same
link works once the allow-list widens. That ordering is deliberate and load-bearing: validating
inside the transactional envelope would let `insert_grant` pre-empt the invitation's own refusal,
which would tell a stranger holding a leaked token whether an arbitrary identity already holds
admin.

## Revoking an invitation

`POST /api/v1/admin/admin-invites/{id}/revoke` — a POST to a sub-resource. There is no
`DELETE /admin-invites/{id}`.

Revoking a consumed invitation does not revoke the grant it created. Those are separate objects with
separate lifecycles: `DELETE /api/v1/admin/admin-identities/{id}` is what revokes admin authority,
and it is described in [docs/admin-identity-claiming.md](admin-identity-claiming.md#ownership).

## Recovery is not built

There is no `is_recovery` flag, no `replaces_admin_identity_id`, no atomic revoke-and-grant swap and
no `admin_identity_recovered` audit event. Wave 2 of plan 09 omitted them deliberately (**D-W2-1**:
*"a column no code writes is the schema equivalent of a catalog entry with no emitter"*).

What recovery would add over what exists is **atomicity**. "Revoke the locked-out admin's grant, then
invite their replacement" is already two ordinary operations, and both are exposed. Building a panel
that performs them as two independent calls while promising "never a window where both or neither
exist" would be the appearance of a feature rather than the feature.

## Privacy

`AdminInviteRecord.value` is the invited address or domain, and `consumed_subject` is the redeemer's
IdP subject. Both are returned to any holder of `moira:admins:read`. That is the right audience, and
it means the invitation list is a directory of who was invited — worth treating as personal data in
any export, retention policy, or support transcript.
