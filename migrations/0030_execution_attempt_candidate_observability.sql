-- Issue #213 — Workstream D: context router, MVP-static slice (plans/12-feature-expansion-
-- brainstorm.md §2, "Observability of routing decisions").
--
-- Per-attempt record of which candidate served and why, joined to the existing
-- `execution_attempts` row rather than a new table, since one attempt already = one candidate
-- today (`docs/retry-and-fallback.md`: "Each upstream attempt is persisted separately").
--
-- `candidate_rank` is the candidate's 0-based position in the ordered list `select_candidates`
-- returned for this execution (index 0 = the first candidate the fallback loop tries).
-- `candidate_score` is nullable and, in this MVP-static slice, always NULL — no scoring function
-- exists yet (Later phase, `routing_policies.scoring_enabled`, migration 0029). `selection_reason`
-- records why this particular candidate was tried: 'priority' (first candidate, no caller hint),
-- 'explicit_hint' (the caller's `provider_hint`/`model_hint` selected it), 'fallback_after_failure'
-- (reached only because an earlier-ranked candidate failed), or 'scored' (reserved for the Later
-- scoring phase — never emitted by this slice).
alter table execution_attempts
    add column if not exists candidate_rank integer,
    add column if not exists candidate_score double precision,
    add column if not exists selection_reason varchar(64);

alter table execution_attempts
    drop constraint if exists execution_attempts_selection_reason_valid;
alter table execution_attempts
    add constraint execution_attempts_selection_reason_valid check (
        selection_reason is null
        or selection_reason in ('priority', 'explicit_hint', 'scored', 'fallback_after_failure')
    );
