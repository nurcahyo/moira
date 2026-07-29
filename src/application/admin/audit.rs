//! Read-only access to the admin audit log.
//!
//! Both methods are reads; nothing here mutates, so this service never touches the command
//! envelope.
//!
//! Constructed and owned by [`crate::application::AdminService`], which re-exposes every
//! method below under its original name and signature.

use uuid::Uuid;

use crate::{
    app::AppState,
    application::admin::shared::{AUDIT_LOGS_CURSOR, PageRequest, paginate},
    domain::{AuditLogRecord, ListCursor, ListResponse},
    error::AppError,
    infra::repositories::{AdminRepository, PgAdminRepository},
    security::Actor,
};

pub(crate) struct AuditAdminService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
}

impl<'a> AuditAdminService<'a> {
    pub(crate) fn new(state: &'a AppState, repo: PgAdminRepository) -> Self {
        Self { state, repo }
    }

    pub(crate) async fn list_audit_logs(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<AuditLogRecord>, AppError> {
        self.state.authz.require(actor, "moira:audit:read")?;
        let page = page.into();
        let rows = self
            .repo
            .list_audit_logs(page.decode(AUDIT_LOGS_CURSOR)?, page.limit())
            .await?;
        // The one admin list keyed on `occurred_at` rather than `created_at`.
        Ok(paginate(rows, &page, AUDIT_LOGS_CURSOR, |row| {
            ListCursor::new(row.occurred_at, row.id)
        }))
    }

    pub(crate) async fn get_audit_log(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<AuditLogRecord, AppError> {
        self.state.authz.require(actor, "moira:audit:read")?;
        self.repo.get_audit_log(id).await
    }
}
