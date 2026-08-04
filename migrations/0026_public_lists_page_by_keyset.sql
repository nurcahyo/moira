-- Issue #93 — the two public lists that page globally had no index for the order they page in.
--
-- `GET /v1/executions` and `GET /v1/usage` now carry a real keyset cursor: the page after a
-- cursor is `(sort_column, id) < ($ts, $id)` ordered `sort_column desc, id desc`. Two indexes
-- already covered the *scoped* form of exactly that — `responses_owner_cursor_idx` (0006) and
-- `usage_records_public_filters_idx` (0006) both end in `created_at desc, id desc` /
-- `occurred_at desc, id desc` behind their scoping columns.
--
-- Neither helps the unscoped walk. Both list queries begin with
--
--     where ($1::boolean or (($2::uuid is null or application_id = $2) and …))
--
-- and a privileged caller — a system key with `moira:executions:read_all` — passes `$1 = true`,
-- which makes the whole disjunction constant-true and leaves the scoping columns unconstrained.
-- The planner cannot use a leading-column-unbounded index for that, so the query degrades to a
-- sequential scan plus a sort of the entire table, *per page*. Paging a large `responses` or
-- `usage_records` table then costs O(n log n) per request rather than O(limit).
--
-- The two indexes below are the unscoped prefix those queries actually need: the sort key
-- alone, in the exact direction and with the exact tiebreaker the cursor compares on. With them
-- a page is an index range scan starting at the cursor and stopping after `limit + 1` rows,
-- whether or not the caller is scoped.
--
-- # The `id` column is not decoration
--
-- Both orderings end in `id desc` because `created_at` and `occurred_at` are not unique.
-- `usage_records.occurred_at` in particular defaults to `now()`, which is the *transaction*
-- timestamp — every usage row written by one transaction shares it exactly. A cursor keyed on
-- the timestamp alone has no defined position inside such a group, so a page boundary landing
-- there silently skips one row and repeats another. That is the whole reason the tiebreaker
-- exists, and the index has to carry it or the ordering it supports is not the one the query
-- asks for.
--
-- # No `where deleted_at is null`
--
-- The admin cursor indexes are partial because their tables are soft-deleted. Neither of these
-- is: `responses` expires by `expires_at` and `usage_records` is append-only, and neither list
-- query filters on a deletion column. A partial index would simply not match.

create index if not exists responses_cursor_idx
    on responses (created_at desc, id desc);

create index if not exists usage_records_cursor_idx
    on usage_records (occurred_at desc, id desc);
