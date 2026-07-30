# Console storage: the `console_auth` database

Operator runbook for the console's own PostgreSQL database — what is in it, what
happens when it is absent, and how to rotate the two keys it depends on.

This describes what is **implemented**, unlike most of
`docs/console-architecture.md`. Every claim below has a test named beside it.

## What is in the database

| Table | Owner | Why it must be durable |
|---|---|---|
| `user`, `session`, `account`, `verification` | Better Auth core | A session started on one process must be valid on the next. `account.accountId` is the IdP subject that Moira's `admin_identities` grant is keyed on. |
| `jwks` | Better Auth `jwt` plugin | The ES256 key pair the console signs Moira-bound tokens with. Moira **caches** the JWKS document; a new key pair means every freshly minted token fails verification against the cached copy, from a console that is up and serving sign-in normally. |
| `rateLimit` | Better Auth | Better Auth's default storage is per-process memory with `enabled: isProduction`. Per-process counters make the effective sign-in limit scale with replica count. |
| `console_provider_secret` | the console (D7) | The OAuth client secret, sealed AES-256-GCM with its AAD bound to `(providerId, clientId)`. Moira never stores it and has no endpoint that would return it. |
| `console_auth_migrations` | `console/db/migrate.ts` | The migration ledger: one row per applied file with its SHA-256. |

**It is not Moira's database.** They may share a *server*; they may not share a
*database*. Moira's schema is owned by the repository-root `migrations/` and
applied by Moira's binary; the console's is owned by `console/db/migrations/`
and applied by `console/db/migrate.ts`. Two independent ledgers in one
`search_path` is the failure the separation exists to make unreachable.

## Configuration

    CONSOLE_DATABASE_URL=postgres://console:...@host:5432/console_auth

Required under `NODE_ENV=production` — `console/lib/env.ts` refuses to boot
without it and names the variable. Outside production it is optional, and its
absence selects the ephemeral path (Better Auth's `memoryAdapter` plus
`InMemoryConsoleSecretStore`), which is what keeps `bun test` and `next dev`
runnable without a database. `consoleStorageMode()` reports which is in use.

The chart injects it from the Secret named by `secret.name`; `_helpers.tpl`
fails the render if it appears in the ConfigMap instead.

## Migrations

    bun run db:migrate            # apply pending files
    bun run db:check              # report pending files, exit 1, change nothing
    bun run db:generate           # emit the DDL better-auth wants and the DB lacks
    bun run db:generate -- --fresh  # ... the whole schema, via a temporary database

`db/migrate.ts` takes a `pg_advisory_lock`, applies each file in its own
transaction together with its ledger row, and refuses to run if a file that was
already applied has since been edited. Migrations are **append-only**: a
better-auth upgrade that changes the schema produces the next numbered file, not
a rewrite of an earlier one.

`0001_better_auth_core.sql` is generated, not hand-written. It comes from
better-auth's own compiler (`better-auth/db/migration` →
`getMigrations().compileMigrations()`) at the pinned version.

### Why not `@better-auth/cli`

Plan 09 §0.2 D2 specifies "Better Auth CLI migrations". That is not available:
npm's newest `@better-auth/cli` is **1.4.21** (`release-1.4` 1.4.22, `beta`
1.5.0-beta.13) while `console/package.json` pins **better-auth 1.6.25**, and the
CLI carries its own `better-auth: 1.4.21` dependency. It would generate the
schema of a library two minor versions behind the one the console runs — exactly
the drift a generated schema exists to eliminate. `db/generate.ts` calls the same
entry point the CLI's `generate` command calls, at the correct version.

`tests/integration/console-db-migrations.test.ts` closes the loop: after every
committed migration is applied, better-auth must report nothing left to create or
add, and the options in `db/schema.ts` must describe the same tables as the
instance `lib/auth.ts` builds.

## The two keys, and what happens when each is absent or rotated

Neither key is stored in the database. A dump of `console_auth` is not a
disclosure of any client secret; a dump plus the environment is.

### `CONSOLE_SECRET_ENCRYPTION_KEY` — 32 raw bytes, base64

Seals `console_provider_secret.ciphertext`.

* **Absent** — the console does not boot; `readConsoleEnv` names the variable.
  There is no degraded mode.
* **Wrong or rotated** — every stored row fails to open. `reveal()` throws
  `SecretEnvelopeError`, deliberately without saying whether the cause was the
  key, the ciphertext or the client id, since that distinction is an oracle.

**To rotate:** set the new key, restart, and re-enter each provider's client
secret through the console (a `put` reseals the row under the new key). There is
deliberately no re-encryption path and no key list — supporting one means keeping
the old key available to the process, which is most of what rotating it was for.
With one provider this is one form.

### `BETTER_AUTH_SECRET` — 32+ characters

Signs session cookies **and** encrypts the ES256 private key in the `jwks` table
(better-auth 1.6.25 `plugins/jwt/utils.ts`; encryption is on unless
`jwks.disablePrivateKeyEncryption` is set, and the console does not set it).

* **Absent** — the console does not boot.
* **Rotated against a durable database** — this is the dangerous one, and it
  fails in the worst available shape:
  * `getJwks` serves the plaintext `publicKey` column, so **the JWKS document is
    unchanged** and Moira's cached copy stays valid;
  * `signJWT` must decrypt the private half and raises `Failed to decrypt private
    key`;
  * it does **not** regenerate — a key that cannot be decrypted is not a missing
    key, and `signJWT` only mints a new pair when there is none or it has
    expired.

  So the console keeps publishing a JWKS it can no longer sign for, sign-in still
  works, and every minted-token call into Moira fails with nothing pointing at
  the cause. Verified in
  `tests/integration/console-jwks-stability.test.ts` ("BETTER_AUTH_SECRET is a
  SECOND key the durable pair depends on").

**To rotate:** delete the `jwks` rows in the same operation.

```sql
delete from "jwks";
```

Better Auth mints a fresh pair on the next JWKS read. Moira will keep rejecting
tokens until it re-fetches the document, so schedule it like a key rotation, not
like a config change. Sessions are invalidated either way.

## Replica count

`charts/moira-console/values.yaml` stays at `replicaCount: 1`. Secrets, the JWKS
key pair, sessions and rate limits are now shared through this database — but the
auth-config **snapshot** in `console/lib/auth-runtime.ts` is still per process,
because reading Moira's provider configuration needs a credential (finding F15).
Two pods can therefore hold different provider configurations, including
different client secrets after a rotation, and the symptom is an `invalid_client`
on some sign-ins and not others. The values file carries the full reasoning and
the reversal condition.
