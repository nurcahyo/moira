//! The auto-provisioning read shared by every `get_or_create_*_policy` — finding F47.
//!
//! # What this replaces, and why it was not merely slow
//!
//! Four of the five family members were spelled
//!
//! ```sql
//! insert into <table> (application_id) values ($1)
//! on conflict (application_id) do update set application_id = excluded.application_id
//! returning *
//! ```
//!
//! `do update set <pk> = excluded.<pk>` is the usual trick for making `returning` fire on the
//! conflict path. It is also a real `UPDATE` of the row, and on these five tables an `UPDATE`
//! is not a private matter. Measured against the live schema, one such *read* does all of
//! this:
//!
//! 1. writes a new heap tuple and a WAL record — `xmin` and `ctid` both advance, leaving a
//!    dead tuple behind, concentrated on **one row per application**;
//! 2. fires `<table>_bump_version`, so the row's `version` — the value served as the `ETag`
//!    on `GET …/policy` and required back on `If-Match` for the `PUT` — **increments**, and
//!    `updated_at` moves to `now()`;
//! 3. fires `<table>_runtime_config_notify`, so `pg_notify('moira_runtime_config', …)` is
//!    emitted and every replica's listener runs `apply_invalidation`, which calls
//!    `cache.invalidate_all()`, `runtime_handles.invalidate_all()` and
//!    `auth_settings.invalidate_all()` **unconditionally**, regardless of the notification's
//!    circuit scope;
//! 4. takes a row-level lock for the duration, serialising concurrent callers that touch the
//!    same application's policy.
//!
//! A conversation-linked turn reads the conversation policy twice and the memory policy once,
//! so the default configuration — where all three reads end in an early return — bumped two
//! ETags and wiped every replica's runtime-config and provider-handle cache **three times per
//! turn**.
//!
//! # Why `on conflict do nothing` alone is not the fix
//!
//! `do nothing` performs no `UPDATE`, so it fires no trigger and takes no row lock — but it
//! also returns **no row** on the conflict path (`INSERT 0 0`), which is precisely why the
//! `do update` trick existed. The row therefore has to be fetched separately, and that
//! reintroduces the race the row lock used to make impossible: a concurrent inserter can
//! commit between this transaction's insert and its select.
//!
//! # How the race is closed
//!
//! Each statement here runs on its own connection at the pool's default isolation
//! (`READ COMMITTED`), so every statement takes a **fresh snapshot**. That is what makes the
//! loop below terminate rather than spin:
//!
//! * **Steady state** — the row exists: the first `select` returns it. One statement, no
//!   write, no lock, no notification. This is the overwhelmingly common case and it is now
//!   the cheapest of the three.
//! * **First touch** — no row: the `select` misses, the `insert` inserts and `returning`
//!   yields the new row. Exactly one heap write, exactly one `INSERT` notification, which is
//!   correct — a policy really was created.
//! * **Concurrent first touch** — two callers race: both `select`s miss; both `insert`s
//!   attempt the same key. Postgres makes the loser wait on the winner's speculative
//!   insertion, and once the winner commits the loser's `do nothing` skips the row and
//!   returns nothing. The loser's next `select` runs under a new snapshot and **sees the
//!   committed row**. Verified against Postgres directly, not reasoned about: the losing
//!   session returns `INSERT 0 0` and the following `select` returns the row.
//!
//! So the loop needs at most `select → insert → select` and cannot return `None` for a row
//! that exists. It is bounded anyway: the only way to consume an iteration without
//! terminating is for the row to be **deleted** between the insert and the select, which
//! happens when the owning `applications` row is removed (`on delete cascade`). Retrying is
//! right for a delete that has since been rolled back and pointless for one that committed —
//! in which case the next `insert` fails its foreign key and surfaces that, which is the
//! honest answer. [`ATTEMPTS`] exists so a pathological interleaving ends in a loud error
//! rather than an unbounded loop.
//!
//! # Why the bare read-then-insert it also replaces was not safe either
//!
//! `get_or_create_application_execution_policy` was already `select`-then-`insert`, so it
//! never had the write amplification above — and it had **no `on conflict` clause at all**,
//! so it carried the correctness bug the row lock was hiding on the other four. Two
//! concurrent first requests for a new application made one of them fail with
//! `duplicate key value violates unique constraint "application_execution_policies_pkey"`,
//! reproduced directly against Postgres. It is on the hot path of every `POST /v1/responses`.
//! Routing it through here fixes that rather than trading it.

use sqlx::{PgPool, postgres::PgRow};
use uuid::Uuid;

use crate::error::AppError;

/// Bound on `select → insert` rounds. Two suffice for every interleaving that does not
/// involve the row being deleted underneath us; the third exists so the failure is a coded
/// error instead of a spin.
const ATTEMPTS: usize = 3;

/// Reads an application's policy row, creating the default one if it does not exist yet,
/// **without writing on the path where it already does**.
///
/// `table` and `projection` are interpolated into the SQL text. Both are `&'static str`, and
/// every caller passes a literal written in this crate — no caller-supplied value can reach
/// either, and none may ever be allowed to. `application_id` is bound, never interpolated.
pub(super) async fn get_or_create_policy_row(
    pool: &PgPool,
    table: &'static str,
    projection: &'static str,
    application_id: Uuid,
) -> Result<PgRow, AppError> {
    let select_sql = format!("select {projection} from {table} where application_id = $1");
    let insert_sql = format!(
        "insert into {table} (application_id) values ($1) \
         on conflict (application_id) do nothing returning {projection}"
    );

    for _ in 0..ATTEMPTS {
        if let Some(row) = sqlx::query(&select_sql)
            .bind(application_id)
            .fetch_optional(pool)
            .await?
        {
            return Ok(row);
        }
        if let Some(row) = sqlx::query(&insert_sql)
            .bind(application_id)
            .fetch_optional(pool)
            .await?
        {
            return Ok(row);
        }
    }

    Err(AppError::Internal(format!(
        "{table} policy row for this application could not be read or created after \
         {ATTEMPTS} attempts"
    )))
}
