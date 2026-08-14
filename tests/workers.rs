//! Workers, retention and cluster admission — five suites, one test binary.
//!
//! # Why these five, and why sharing a process is safe for them
//!
//! Every suite here takes its databases from [`support::TestDatabase`], and **every advisory
//! lock any of them takes is taken inside its own private clone** — `pg_advisory_xact_lock`
//! in `cluster_admission`, `pg_try_advisory_lock` under the `b"moiralrt"` key in
//! `worker_leader_election`. PostgreSQL advisory locks are database-scoped, so none of them
//! can contend with a neighbour's, and that is the property that makes co-residency safe
//! rather than merely convenient. It is also the property to re-check before adding anything
//! here: a lock taken on the *maintenance* database is cluster-wide and does not qualify.
//!
//! None of the five takes [`support::TEMPLATE_LOCK_KEY`] other than through
//! `TestDatabase::create`, which takes it **shared**. That is load-bearing. A suite holding it
//! *exclusively* would block every fixture creation in this binary, and each blocked fixture
//! is already holding a `FIXTURE_BUDGET` permit while it waits — four of them behind one such
//! test starves the whole process. `tests/test_database_sweep.rs` is exactly that suite and is
//! deliberately left as its own target.
//!
//! # What must not be added here
//!
//! - **Anything that takes `TEMPLATE_LOCK_KEY` exclusively, or calls `sweep_leaked_databases`.**
//!   See above; `tests/test_database_sweep.rs` stays alone for this reason.
//! - **Anything calling `support::install_log_capture`.** It installs a process-global,
//!   unfiltered `TRACE` subscriber behind a `Once` and appends to a cumulative buffer shared by
//!   every test in the binary. Adding it here would subject all 45 of these tests' logs to the
//!   leak suites' needle scans and buffer their `TRACE` output unbounded.
//! - **Anything that skips.** `support::announce_skip` writes straight to fd 2 to bypass
//!   libtest's capture, and `scripts/test-log-lib.sh` reds the gate on *any* skip line. One
//!   unset variable in a co-resident suite would red all five of these. That is the specific
//!   reason `tests/cluster_coordination.rs` — the only Redis-driving suite — is **not** here
//!   despite being this group's obvious sixth member: a missing `MOIRA_TEST_REDIS_URL` would
//!   stop being one small binary's problem and become this one's.
//! - **A second `TestDatabase::create_with_max_connections` suite.** These five all use the
//!   default 8-connection pool, so the worst case is `CONCURRENT_FIXTURES` × 8 = 32 of
//!   PostgreSQL's 100. A 16-connection suite would take that to 64.
//! - **Any fixture in this file.** This root owns nothing but module declarations — no
//!   helpers, no statics, no `Case` type. A shared fixture that a test held while building its
//!   own would be two `FIXTURE_BUDGET` permits in one test, which is the documented four-way
//!   deadlock. Keeping the root empty makes that unreachable by construction rather than by
//!   review.
//!
//! # Why the members need `#[path]`
//!
//! `tests/workers.rs` is itself a crate root, so its module directory is `tests/` — a plain
//! `mod worker_queue;` here resolves to `tests/worker_queue.rs` and fails with E0583, never to
//! `tests/workers/worker_queue.rs`. Measured, not assumed. The shape that avoids `#[path]` — a
//! root at `tests/workers/main.rs` — was rejected because cargo would then name the target
//! from the directory, `ls tests/*.rs` would stop listing it, and that listing is the
//! independent source `tl_expected_units` builds the whole completeness gate on
//! (`scripts/test-log-lib.sh`). One attribute per member is the cheaper price.
//!
//! **Each `#[path]`/`mod` pair is load-bearing and its loss is silent.** Delete one and the
//! member file stays in git, cargo never compiles it, and the target still runs — so the
//! completeness gate diffs clean and up to ten tests leave the build green.
//! `every_group_member_is_declared_by_its_root` in `tests/test_database_isolation.rs` names the
//! file, and `tl_assert_test_count` catches the count. Neither existed before this group did.

mod support;

#[path = "workers/cluster_admission.rs"]
mod cluster_admission;
#[path = "workers/coordination_default_path.rs"]
mod coordination_default_path;
#[path = "workers/job_dispatch.rs"]
mod job_dispatch;
#[path = "workers/retention_worker.rs"]
mod retention_worker;
#[path = "workers/worker_leader_election.rs"]
mod worker_leader_election;
#[path = "workers/worker_queue.rs"]
mod worker_queue;
