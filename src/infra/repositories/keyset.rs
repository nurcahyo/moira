//! The one keyset-pagination SQL builder every list query in this crate uses.
//!
//! It started life private to [`super::admin`], where plan 04 introduced it for the nine
//! admin lists. Issue #93 brought the four public lists (`/v1/executions`, `/v1/usage`,
//! `/v1/models`, `/v1/routes`) onto the same mechanism, and a second copy of a predicate
//! whose correctness depends on `<` versus `<=` and on the presence of the `id` tiebreaker
//! is exactly the kind of duplication that drifts. So it lives here, shared, with its unit
//! tests.
//!
//! Nothing in this module ever sees caller input: sort columns are literals chosen by the
//! repository call sites, and cursor *values* are bound as parameters by [`bind_cursor`].

use sqlx::{Postgres, postgres::PgArguments, query::Query};

use crate::domain::ListCursor;

/// How many rows a list query actually asks Postgres for.
///
/// One more than the caller wants. That extra row is the existence proof for `has_more`:
/// the application layer trims it off and reports `has_more = true` if it was there. It is
/// what keeps `has_more` from costing a second `count(*)` over the whole table on every
/// single page.
pub(crate) fn over_fetch_limit(limit: i64) -> i64 {
    limit.saturating_add(1)
}

/// The `order by` / `limit` tail shared by every keyset list query, plus the optional
/// keyset predicate that makes the advertised `cursor` parameter real.
///
/// Three things here are load-bearing:
///
/// * **Strictly less-than.** Every list built with this is ordered descending, so "the page
///   after this cursor" is the rows whose sort key is strictly *below* it. `<=` would
///   re-emit the cursor row itself at the top of every page.
/// * **The `id` tiebreaker.** The comparison is on the row constructor
///   `(sort_column, id)`, not on `sort_column` alone. Without the `id` leg, rows sharing a
///   timestamp come back in an unspecified order, and a page boundary landing inside such
///   a group silently skips or repeats rows — the exact defect P1-4 describes. None of the
///   nine admin lists had this tiebreaker before plan 04, and none of the four public lists
///   had a cursor at all before issue #93.
/// * **The `id` column follows the sort column's qualifier.** `KeysetTail::new("created_at",
///   …)` compares `(created_at, id)`; `KeysetTail::new("r.created_at", …)` compares
///   `(r.created_at, r.id)`. The public execution list joins five tables that each have an
///   `id`, so an unqualified `id` there is not merely untidy — Postgres rejects it as
///   ambiguous. Deriving the qualifier means a call site cannot pair one table's timestamp
///   with another table's id by accident.
///
/// `sort_column` is always a literal chosen by the repository call sites and is never
/// caller input. The cursor's *values* never reach the SQL text at all: they are bound as
/// parameters by [`bind_cursor`].
pub(crate) struct KeysetTail {
    /// The bare keyset condition, or `None` when the caller asked for the first page.
    condition: Option<String>,
    pub(crate) order_and_limit: String,
}

impl KeysetTail {
    /// `first_param` is the next unused `$n` after the query's own fixed parameters, so
    /// the numbering stays correct whether or not a cursor is present.
    pub(crate) fn new(sort_column: &str, cursor: Option<&ListCursor>, first_param: usize) -> Self {
        let id_column = id_column_for(sort_column);
        let (condition, limit_param) = match cursor {
            Some(_) => (
                Some(format!(
                    "({sort_column}, {id_column}) < (${first_param}::timestamptz, ${}::uuid)",
                    first_param + 1
                )),
                first_param + 2,
            ),
            None => (None, first_param),
        };

        Self {
            condition,
            order_and_limit: format!(
                "order by {sort_column} desc, {id_column} desc limit ${limit_param}"
            ),
        }
    }

    /// The condition as an `and …` clause, for a query that already has a `where`.
    pub(crate) fn and_clause(&self) -> String {
        match &self.condition {
            Some(condition) => format!("and {condition}"),
            None => String::new(),
        }
    }

    /// The condition as a `where …` clause, for a query that has none (`audit_logs`, and
    /// the outer query of the public model list).
    pub(crate) fn where_clause(&self) -> String {
        match &self.condition {
            Some(condition) => format!("where {condition}"),
            None => String::new(),
        }
    }
}

/// `"created_at"` → `"id"`; `"r.created_at"` → `"r.id"`.
///
/// The tiebreaker is always the primary key of whatever relation supplies the sort column,
/// so the qualifier is read off the sort column rather than passed separately — there is no
/// way to spell a call site that mixes the two.
fn id_column_for(sort_column: &str) -> String {
    match sort_column.rsplit_once('.') {
        Some((qualifier, _)) => format!("{qualifier}.id"),
        None => "id".to_string(),
    }
}

/// Binds the cursor's two values, in the order [`KeysetTail`] numbered them.
///
/// This is the only place a cursor value meets a query, and it is a bind every time. A
/// forged or malformed cursor has already been rejected by `ListCursor::decode`, and even
/// a valid one is a typed `DateTime<Utc>` / `Uuid` that cannot reach the SQL text.
pub(crate) fn bind_cursor<'q>(
    query: Query<'q, Postgres, PgArguments>,
    cursor: Option<&ListCursor>,
) -> Query<'q, Postgres, PgArguments> {
    match cursor {
        Some(cursor) => query.bind(cursor.ts).bind(cursor.id),
        None => query,
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use uuid::Uuid;

    use super::*;

    fn cursor() -> ListCursor {
        ListCursor::new(
            DateTime::from_timestamp_micros(1_753_401_600_123_456).expect("in-range timestamp"),
            Uuid::parse_str("018f3a7c-1c2d-7e4f-8a9b-0c1d2e3f4a5b").expect("valid uuid"),
        )
    }

    #[test]
    fn keyset_predicate_is_omitted_when_no_cursor_is_supplied() {
        let tail = KeysetTail::new("created_at", None, 1);

        assert_eq!(tail.and_clause(), "");
        assert_eq!(tail.where_clause(), "");
        // With no cursor, the limit takes the first free parameter slot.
        assert_eq!(
            tail.order_and_limit,
            "order by created_at desc, id desc limit $1"
        );
    }

    #[test]
    fn keyset_predicate_uses_strict_less_than_for_descending_lists() {
        let tail = KeysetTail::new("created_at", Some(&cursor()), 1);

        assert_eq!(
            tail.and_clause(),
            "and (created_at, id) < ($1::timestamptz, $2::uuid)"
        );
        // `<=` would re-emit the cursor row at the top of every page.
        assert!(!tail.and_clause().contains("<="));
        assert!(!tail.and_clause().contains('>'));
    }

    #[test]
    fn keyset_predicate_uses_the_occurred_at_column_for_audit_logs() {
        // The one admin list whose sort key is not `created_at`, and the one that has to
        // introduce its own `where`.
        let tail = KeysetTail::new("occurred_at", Some(&cursor()), 1);

        assert_eq!(
            tail.where_clause(),
            "where (occurred_at, id) < ($1::timestamptz, $2::uuid)"
        );
        assert_eq!(
            tail.order_and_limit,
            "order by occurred_at desc, id desc limit $3"
        );
        assert!(!tail.where_clause().contains("created_at"));
    }

    #[test]
    fn every_keyset_ordering_carries_the_id_tiebreaker() {
        // Without `id desc`, rows sharing a timestamp come back in an unspecified order and
        // a page boundary inside such a group silently skips or repeats them. All nine
        // admin lists lacked this before plan 04; all four public lists lacked a cursor
        // entirely before issue #93.
        for (column, id) in [
            ("created_at", "id"),
            ("occurred_at", "id"),
            ("r.created_at", "r.id"),
            ("u.occurred_at", "u.id"),
        ] {
            for cursor in [None, Some(&cursor())] {
                let tail = KeysetTail::new(column, cursor, 1);
                assert!(
                    tail.order_and_limit
                        .starts_with(&format!("order by {column} desc, {id} desc limit $")),
                    "missing id tiebreaker for {column}: {}",
                    tail.order_and_limit
                );
            }
        }
    }

    #[test]
    fn a_qualified_sort_column_qualifies_the_id_tiebreaker_with_the_same_alias() {
        // The public execution list joins `responses`, `execution_attempts`,
        // `route_definitions`, `providers` and `provider_models` — five relations with an
        // `id` each. A bare `id` in the predicate is ambiguous and Postgres refuses the
        // query outright, so this is the difference between a working list and a 500.
        let tail = KeysetTail::new("r.created_at", Some(&cursor()), 5);

        assert_eq!(
            tail.and_clause(),
            "and (r.created_at, r.id) < ($5::timestamptz, $6::uuid)"
        );
        assert_eq!(
            tail.order_and_limit,
            "order by r.created_at desc, r.id desc limit $7"
        );
    }

    #[test]
    fn keyset_parameter_numbering_follows_the_querys_own_fixed_parameters() {
        // `list_provider_models` and `list_user_credentials` each bind one fixed parameter
        // before the cursor, so the cursor starts at `$2` and the limit lands at `$4`.
        let tail = KeysetTail::new("created_at", Some(&cursor()), 2);
        assert_eq!(
            tail.and_clause(),
            "and (created_at, id) < ($2::timestamptz, $3::uuid)"
        );
        assert_eq!(
            tail.order_and_limit,
            "order by created_at desc, id desc limit $4"
        );

        // …and without a cursor the limit moves up to the slot the cursor would have used.
        let first_page = KeysetTail::new("created_at", None, 2);
        assert_eq!(
            first_page.order_and_limit,
            "order by created_at desc, id desc limit $2"
        );
    }

    #[test]
    fn keyset_predicate_binds_parameters_and_never_interpolates_values() {
        // The strongest form of this assertion: two cursors that share no bytes must
        // produce byte-identical SQL. If any cursor-derived value ever reached the query
        // text, these would differ.
        let one = ListCursor::new(
            DateTime::from_timestamp_micros(1).expect("in-range timestamp"),
            Uuid::parse_str("ffffffff-ffff-4fff-bfff-ffffffffffff").expect("valid uuid"),
        );
        let two = cursor();

        for column in ["created_at", "occurred_at", "r.created_at", "m.created_at"] {
            let a = KeysetTail::new(column, Some(&one), 1);
            let b = KeysetTail::new(column, Some(&two), 1);

            assert_eq!(a.and_clause(), b.and_clause());
            assert_eq!(a.where_clause(), b.where_clause());
            assert_eq!(a.order_and_limit, b.order_and_limit);

            // And nothing that looks like a value is present at all — only `$n` holes.
            let fragment = format!("{} {}", a.where_clause(), a.order_and_limit);
            for forbidden in ["ffffffff", "018f3a7c", "1753401600", "'", "1970"] {
                assert!(
                    !fragment.contains(forbidden),
                    "cursor-derived literal {forbidden:?} leaked into SQL: {fragment}"
                );
            }
        }
    }

    #[test]
    fn over_fetch_limit_is_limit_plus_one() {
        assert_eq!(over_fetch_limit(1), 2);
        assert_eq!(over_fetch_limit(50), 51);
        assert_eq!(over_fetch_limit(200), 201);
        // Never panics on a hostile value, and never wraps to a negative limit — Postgres
        // rejects a negative `LIMIT`, so wrapping would turn a bad input into a 500.
        assert_eq!(over_fetch_limit(i64::MAX), i64::MAX);
    }
}
