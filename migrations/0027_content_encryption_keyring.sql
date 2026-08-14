-- no-transaction
-- Issue #137 — the content-encryption keyring, the exclusivity CHECKs it makes trustworthy,
-- and the audit helper that names an envelope's data key without holding any key.
--
-- Part of the envelope-encryption-at-rest train decided in
-- `docs/decision-encryption-at-rest.md` (§7 for the keyring, §10 for boot validation).
--
-- **Additive only. This migration touches no existing row.** It creates one table, one partial
-- unique index, one function, and five CHECK constraints that every existing row already
-- satisfies structurally. Nothing in `src/` reads or writes a `*_encrypted` column yet.
--
-- ===================================================================================
-- Why this file runs outside sqlx's per-migration transaction
-- ===================================================================================
--
-- `-- no-transaction` on the first line is load-bearing, and it is the first use of it in this
-- tree, so here is the whole argument.
--
-- The five exclusivity CHECKs are added `NOT VALID` and validated in a second statement. That
-- split exists for exactly one reason: `ADD CONSTRAINT ... NOT VALID` takes `ACCESS EXCLUSIVE`
-- but performs **no table scan**, so it holds the lock for a metadata write; `VALIDATE
-- CONSTRAINT` performs the scan but takes only `SHARE UPDATE EXCLUSIVE`, which does not block
-- readers or writers. A bare `ADD CONSTRAINT` does both at once and therefore blocks every
-- writer on the table for the duration of a full scan.
--
-- **That benefit is real only across a transaction boundary.** Locks are held to commit, so
-- `ADD NOT VALID` followed by `VALIDATE` *inside one transaction* holds `ACCESS EXCLUSIVE`
-- across the scan anyway — identical to the bare form it is supposed to avoid, with two
-- statements instead of one. Written that way the split would be decoration, and a later reader
-- would reasonably believe a claim that was not true. So the file commits between the two.
--
-- The cost of leaving the runner's transaction is that a failure mid-file leaves the earlier
-- groups applied and the migration unrecorded, so it is re-run from the top on the next boot.
-- **Every statement below is therefore idempotent** — `if not exists`, `or replace`, and the
-- `drop constraint if exists` / `add constraint` pairing that `0020` established for exactly
-- this reason. Re-running the whole file on a database where it already applied is a no-op.
--
-- ===================================================================================
-- Retiring migration `0021`'s stated assumption, by name, for all five columns
-- ===================================================================================
--
-- `migrations/0021_memory_content_hash_is_content_addressed.sql` says of
-- `memory_records.content_encrypted` that it has "**no writer anywhere in the tree today**
-- (verified by grep: the identifier appears only in this schema, in plan/skill prose, and never
-- in a Rust statement)", and restricts its recompute to rows holding `content_plain` on the
-- strength of it.
--
-- **That sentence is true as of this migration and stops being true when PR 6 of the
-- envelope-encryption train (issue #140) lands the write path.** It is retired here, by name,
-- rather than left to be discovered: this repository has already been bitten once by a comment
-- that quietly stopped being true, and a future reader must not be able to trust this one past
-- its expiry.
--
-- The same assumption is retired for all five sealed columns, not just the one `0021` names —
-- `0021`'s argument applies verbatim to each of them and each acquires a writer in the same PR:
--
--     conversation_messages.content_encrypted
--     conversation_summaries.summary_text_encrypted
--     memory_records.content_encrypted
--     rag_document_versions.content_encrypted
--     rag_chunks.chunk_text_encrypted
--
-- What replaces the assumption is the constraint, not another comment. From this migration
-- onward "which column is non-null" is a **database-enforced** discriminator: a row carries at
-- most one of the pair, forever, whether or not anyone remembers reading this file. `0021`'s
-- recompute stays correct for the rows it already touched, and any future backfill that needs
-- the plaintext of a sealed row must be told the same thing `0021` was: the ciphertext is not
-- the content, the key is not in the database, and no SQL migration can recover it.

-- ---------------------------------------------------------------------------------
-- (a) The keyring.
-- ---------------------------------------------------------------------------------
-- Single digits of rows, ever. One row is one data key: what it is for, what state it is in,
-- who wrapped it, and the sealed bytes.
--
-- The wrapped key lives here and the *master* key never does. `wrapped_key` is opaque
-- ciphertext produced by whichever custody backend `custody_backend` names; a `pg_dump` of this
-- table is worthless without the master key, which is the property the whole design is for.
--
-- `key_check_value` is the first 8 bytes of `HMAC-SHA256(dek, "moira/content-kcv/v1")`. It
-- proves a successful unwrap produced the *right* key rather than merely some key, and it is
-- safe to print: 8 bytes of a truncated HMAC under a 256-bit key identifies the key to an
-- operator comparing two keyrings and tells an attacker nothing they could invert. It is in the
-- database, and not only in memory, so that `keyring status` on one replica can be compared
-- with the stored value rather than with another replica's opinion.
begin;

create table if not exists content_data_keys (
    id                 uuid primary key,
    -- Monotonic, for humans and for ordering. Unique across purposes, not per purpose: an
    -- operator reading a log line should never have to ask "version 3 of which keyring".
    key_version        integer not null unique,
    purpose            varchar(32) not null default 'content'
                         check (purpose in ('content', 'memory_dedupe')),
    -- pending  — minted, not yet writing.
    -- active   — new writes use it. Exactly one per purpose; see the index below.
    -- retiring — not writable, still loaded, still readable, forever.
    -- retired  — not loaded. A row referencing it fails cleanly rather than lazily.
    -- abandoned— the key is permanently lost and that loss has been explicitly acknowledged
    --            (PR 4). Loaded as a *marker* and never unwrapped: attempting the unwrap would
    --            abort boot for precisely the key an operator has already confessed to losing,
    --            which is the deadlock `abandon` exists to break.
    state              varchar(16) not null
                         check (state in ('pending', 'active', 'retiring', 'retired', 'abandoned')),
    -- Informational, never dispatched on: `'environment'` today. Deliberately **not** part of
    -- the wrap AAD — see `crate::security::wrapped_data_key_aad`, which explains why binding it
    -- would force a rewrap of every row the day the same bytes are served from Vault.
    custody_backend    varchar(64) not null,
    master_key_id      varchar(128) not null,
    wrap_algorithm     varchar(64) not null,
    wrap_nonce         bytea not null,
    wrapped_key        bytea not null,
    key_check_value    bytea not null check (octet_length(key_check_value) = 8),
    created_at         timestamptz not null default now(),
    activated_at       timestamptz,
    retired_at         timestamptz,
    abandoned_at       timestamptz,
    abandon_reason     text,
    constraint content_data_keys_wrapped_not_empty check (length(wrapped_key) > 0)
);

-- Exactly one active key PER PURPOSE is a DATABASE invariant, not a convention.
--
-- Two pods racing to rotate produce one winner and one unique violation, which is the correct
-- outcome: the loser learns it lost from the database rather than from a second `active` row
-- nobody notices until two replicas disagree about which key new writes are sealed under.
-- A partial index rather than a constraint because the uniqueness holds only over `active`.
create unique index if not exists content_data_keys_single_active_idx
    on content_data_keys (purpose) where state = 'active';

commit;

-- ---------------------------------------------------------------------------------
-- (b) Naming an envelope's data key from SQL, holding no key at all.
-- ---------------------------------------------------------------------------------
-- This is what makes key retirement auditable. "Is any row still sealed under key X?" has to be
-- answerable before an operator dares move X to `retired`, and it must be answerable by a DBA
-- with a psql prompt and no master key. Without it, the only way to find out is to try to
-- decrypt every row, which needs the key that is being retired.
--
--     select count(*) from conversation_messages
--      where content_envelope_data_key_id(content_encrypted) = '…'::uuid;
--
-- **The offsets and the magic below must agree byte-for-byte with the parser in
-- `src/security/content_envelope.rs`, and the two are changed together or not at all.** The
-- values are, from that module:
--
--     ENVELOPE_MAGIC     = b"MOE1"  = \x4d4f4531   at offset 0, length 4
--     FORMAT_VERSION_V1  = 0x01                    at offset 4
--     data_key_id                                  at offset 8, length 16 (raw UUID bytes)
--     MIN_ENVELOPE_LEN   = 58                      (42-byte header + 16-byte GCM tag)
--
-- A Rust test seals a real envelope, hands the bytes to this function, and asserts it returns
-- the same id — so a drift between the two implementations fails the suite rather than an
-- operator's retirement audit.
--
-- **Three checks, and deliberately not more.** Length, magic and format version are exactly what
-- fixes the *offset* of `data_key_id`; `algorithm_id`, `key_mode` and `reserved` govern how to
-- *open* the blob, which this function never does. Refusing a blob whose key id is perfectly
-- readable because its algorithm byte is one this build cannot decrypt would hide it from the
-- retirement audit, which is the one question this function exists to answer.
--
-- Returns null for anything that is not a v1 Moira envelope, which is itself the audit signal:
-- `where content_encrypted is not null and content_envelope_data_key_id(content_encrypted) is
-- null` lists every stored blob this build cannot even name.
--
-- `immutable` because the same bytes always yield the same id — it reads a fixed offset and
-- consults nothing else — which is what lets it appear in an index predicate later.
-- `parallel safe` because it touches no shared state.
begin;

create or replace function content_envelope_data_key_id(envelope bytea)
returns uuid
language sql
immutable
parallel safe
returns null on null input
as $$
    select case
        when octet_length(envelope) < 58 then null
        when substring(envelope from 1 for 4) <> '\x4d4f4531'::bytea then null
        when get_byte(envelope, 4) <> 1 then null
        else encode(substring(envelope from 9 for 16), 'hex')::uuid
    end
$$;

comment on function content_envelope_data_key_id(bytea) is
    'Reads content_data_keys.id out of a v1 Moira content envelope (MOE1) without decrypting '
    'it. Null when the bytes are not a v1 envelope. Offsets must match '
    'src/security/content_envelope.rs.';

commit;

-- ---------------------------------------------------------------------------------
-- (c) One form or the other, per row, enforced.
-- ---------------------------------------------------------------------------------
-- Each of the five content tables carries a `*_plain` / `*_encrypted` pair. Both being non-null
-- is not a state anything should be able to reach: the two could disagree, and a reader
-- choosing between them would be choosing which of two contents is the truth.
--
-- Validation cannot fail. Every existing row has `*_encrypted is null` structurally, because no
-- code in `src/` binds those identifiers today — see the header, which retires the comment that
-- asserted the same thing without a constraint to back it.
--
-- Note what is **not** required: that one of them be non-null. All five pairs are legitimately
-- both-null — a message whose content the application's persistence policy says not to store at
-- all. `Omitted` is a real state, not a missing value.
--
-- The `drop constraint if exists` / `add constraint` pairing is `0020`'s, and here it is what
-- makes a re-run of this file after a mid-file failure a no-op.
begin;

alter table conversation_messages
    drop constraint if exists conversation_messages_content_single_form;
alter table conversation_messages
    add constraint conversation_messages_content_single_form
    check (content_plain is null or content_encrypted is null) not valid;

alter table conversation_summaries
    drop constraint if exists conversation_summaries_summary_single_form;
alter table conversation_summaries
    add constraint conversation_summaries_summary_single_form
    check (summary_text_plain is null or summary_text_encrypted is null) not valid;

alter table memory_records
    drop constraint if exists memory_records_content_single_form;
alter table memory_records
    add constraint memory_records_content_single_form
    check (content_plain is null or content_encrypted is null) not valid;

alter table rag_document_versions
    drop constraint if exists rag_document_versions_content_single_form;
alter table rag_document_versions
    add constraint rag_document_versions_content_single_form
    check (content_plain is null or content_encrypted is null) not valid;

alter table rag_chunks
    drop constraint if exists rag_chunks_chunk_text_single_form;
alter table rag_chunks
    add constraint rag_chunks_chunk_text_single_form
    check (chunk_text_plain is null or chunk_text_encrypted is null) not valid;

commit;

-- The scans. `SHARE UPDATE EXCLUSIVE` only, after the `ACCESS EXCLUSIVE` taken above has been
-- released by the commit — which is the whole reason this file leaves the runner's transaction.
-- Readers and writers keep working through all five.
--
-- Each is its own transaction so that one long table does not hold a lock on behalf of the
-- other four.
begin;
alter table conversation_messages
    validate constraint conversation_messages_content_single_form;
commit;

begin;
alter table conversation_summaries
    validate constraint conversation_summaries_summary_single_form;
commit;

begin;
alter table memory_records
    validate constraint memory_records_content_single_form;
commit;

begin;
alter table rag_document_versions
    validate constraint rag_document_versions_content_single_form;
commit;

begin;
alter table rag_chunks
    validate constraint rag_chunks_chunk_text_single_form;
commit;
