pub mod coordination;
pub mod db;
pub mod metrics;
pub mod pg_rows;
pub mod redis;
pub mod repositories;
// Issue #275 (workstream R2 of #272). The `moira-runner` control-plane client — an adapter to an
// external HTTP service, sibling to `db`/`redis`, and the whole of Moira's half of the trust
// chain that keeps Docker access out of this process.
pub mod runner_control;
pub mod workers;
