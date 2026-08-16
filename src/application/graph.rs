//! Read-only relationship-graph admin service (plan 12 §4, issue #234).
//!
//! `GraphService::get_graph` is the only entry point: check the `moira:graph:read` scope, read
//! every registry's rows through `PgGraphRepository` (read-only, owns none of the tables it
//! reads), and hand them to `domain::assemble_graph` — a pure function, unit-tested on its own
//! with hand-built rows so the assembly logic needs no database to verify. No audit row, no
//! idempotency envelope: a `GET` that changes nothing gets neither, the same posture every
//! other admin list/get handler already takes.

use chrono::Utc;

use crate::{
    app::AppState, domain::GraphResponse, error::AppError, infra::repositories::PgGraphRepository,
    security::Actor,
};

pub struct GraphService<'a> {
    state: &'a AppState,
    repo: PgGraphRepository,
}

impl<'a> GraphService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            repo: PgGraphRepository::new(pool),
        })
    }

    pub async fn get_graph(&self, actor: &Actor) -> Result<GraphResponse, AppError> {
        self.state.authz.require(actor, "moira:graph:read")?;
        let data = self.repo.fetch_graph_data().await?;
        Ok(crate::domain::assemble_graph(data, Utc::now()))
    }
}
