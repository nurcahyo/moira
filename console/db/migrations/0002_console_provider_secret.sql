-- The console's own OAuth client-secret store (D7).
--
-- HAND-WRITTEN, unlike 0001. Better Auth knows nothing about this table, so it
-- never appears in `getMigrations()` and the drift gate in
-- `tests/integration/console-db-migrations.test.ts` does not cover it. Its own
-- coverage is `tests/integration/console-secret-store-postgres.test.ts`.
--
-- WHAT IS AND IS NOT A SECRET HERE
--
-- `ciphertext` is AES-256-GCM output; `iv` is the nonce; the GCM tag is the last
-- 16 bytes of `ciphertext`. None of it is usable without the content-encryption
-- key, which lives in CONSOLE_SECRET_ENCRYPTION_KEY and is NEVER written to this
-- database. A dump of this table is not a disclosure of any client secret; a
-- dump of this table PLUS the environment is.
--
-- `client_id` is stored in the clear on purpose. It is not a secret, and having
-- it readable is what lets `classifySecretDrift` compare against Moira's row
-- without decrypting anything. It is ALSO bound into the AEAD as additional
-- authenticated data together with `provider_id`, so editing either column here
-- makes the open fail rather than silently decrypting the wrong provider's
-- secret.
--
-- One row per Moira `auth_provider_settings` row: `provider_id` is that row's
-- id, and it is the primary key because a provider has exactly one current
-- client secret. Rotation REPLACES the row (`on conflict do update`); there is
-- no history table, deliberately — a superseded client secret is a credential
-- with no remaining purpose and every retained copy is another place it leaks
-- from.
create table console_provider_secret (
  provider_id       text        not null primary key,
  envelope_version  integer     not null,
  iv                text        not null,
  ciphertext        text        not null,
  client_id         text        not null,
  updated_at        timestamptz not null,

  -- Cheap structural checks. An empty ciphertext or a missing client_id would
  -- otherwise surface as an opaque "could not be opened" on the sign-in path,
  -- which is the one place with no operator watching.
  constraint console_provider_secret_iv_present         check (length(iv) > 0),
  constraint console_provider_secret_ciphertext_present check (length(ciphertext) > 0),
  constraint console_provider_secret_client_id_present  check (length(client_id) > 0)
);

-- `newestUpdatedAt()` runs on every auth-config resolution, because it feeds the
-- cache key that decides whether the Better Auth instance is rebuilt. It is a
-- `max()` over a table with one row per provider, so this index is close to
-- decorative today — it is here so that it does not become the thing someone
-- has to notice later.
create index console_provider_secret_updated_at_idx
  on console_provider_secret (updated_at desc);
