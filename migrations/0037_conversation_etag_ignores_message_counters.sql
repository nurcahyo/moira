-- A persisted turn must not advance the conversation's ETag.
--
-- `conversations_bump_version` (0007_conversations_memory_rag.sql:509-511) was created with no
-- `WHEN` clause, so every update to the row bumped `version` and `updated_at`. `add_message`
-- (`src/infra/repositories/conversation.rs:1239-1249`) ends with
--
--     update conversations set message_count = message_count + 1, last_message_at = now()
--
-- and one turn is two messages, so an ordinary turn advanced the ETag twice.
--
-- WHY THAT IS A DEFECT AND NOT MERELY CHURN. A caller holding an `If-Match` on a conversation
-- got a 412 mid-conversation through no fault of its own: the precondition it captured was
-- invalidated by traffic rather than by a competing writer, which is the inverse of what an
-- ETag is for. `message_count` and `last_message_at` are counters. They are not part of the
-- resource's optimistic-concurrency identity, and nothing an ETag protects is decided by them.
--
-- THE CLAUSE IS EXCLUSION-BASED, DELIBERATELY. The obvious spelling lists the identity columns
-- and fires when one of them changes. That spelling is wrong in the dangerous direction: a
-- column added later is silently *outside* the ETag until somebody remembers this file, and the
-- failure is invisible — an edit that does not bump the version looks exactly like no edit. By
-- subtracting the four columns that are not identity, anything added later is inside it by
-- default. The same reasoning as `base_validation` in `src/security/auth.rs`: a guarantee that
-- depends on being restated is not a guarantee.
--
-- `version` and `updated_at` are subtracted because they are what the trigger itself writes;
-- comparing them here would be comparing the statement's input against nothing meaningful.
--
-- MEASURED, not assumed, against pgvector/pgvector:pg16 on this schema's shape:
--   update … set message_count = message_count + 1, last_message_at = now()  -> version unchanged
--   update … set title = 'b'                                                 -> version bumped
--   update … set title = 'c', message_count = message_count + 1              -> version bumped
-- The third case is the one worth stating: a statement that touches identity *and* counters
-- still bumps, because the identity really did change.
--
-- A second effect, which is a consequence rather than the goal. `updated_at` is carried by two
-- indexes on this table (`conversations_owner_keyset_idx` and `conversations_updated_cursor_idx`,
-- both in 0010_list_cursor_indexes.sql), while `message_count` and `last_message_at` are carried
-- by none. Before this change the counter update was necessarily non-HOT because the trigger
-- moved an indexed column; now it touches no indexed column and becomes HOT-eligible.
--
-- Scope: `conversations` only. The other `*_bump_version` triggers from 0004 stay as they are —
-- they sit on configuration tables whose every column is part of the resource, and giving them
-- a clause they do not need would add a maintenance obligation for no property gained.

drop trigger if exists conversations_bump_version on conversations;

create trigger conversations_bump_version
before update on conversations
for each row
when (
    to_jsonb(old) - 'message_count' - 'last_message_at' - 'version' - 'updated_at'
    is distinct from
    to_jsonb(new) - 'message_count' - 'last_message_at' - 'version' - 'updated_at'
)
execute function moira_bump_resource_version();
