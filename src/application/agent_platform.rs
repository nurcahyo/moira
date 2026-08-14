//! Agent-platform admin service (issue #214, plan 12 §3/§5): skills CRUD.
//!
//! Sub-plan 1 of 3, SCHEMA + CRUD only — no execution engine. This mirrors
//! [`crate::application::RuntimeAdminService`]'s shape (scope check, idempotency replay/record,
//! audit written inside the write's own transaction, keyset pagination, `If-Match`), because a
//! skill registry is the same kind of admin resource as an agent profile. It deliberately does
//! **not** invalidate any runtime cache: nothing in `ProviderRuntimeCache` or the runtime-config
//! cache keys on a skill yet (the tool loop is deferred to #84), so a skill write is not runtime
//! configuration — see the migration header for why these tables carry no NOTIFY trigger.
//!
//! Evals and flows CRUD are a documented follow-up; their tables and domain types already exist.

use chrono::{Duration, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{RequestContext, admin::actor_fingerprint},
    domain::{
        AuditLogInsert, AuditResult, CursorScope, IdempotencyRecord, ListCursor, ListResponse,
        Pagination, SkillBulkEnableRequest, SkillBulkEnableResponse, SkillCreateRequest,
        SkillPatchRequest, SkillRecord,
    },
    error::AppError,
    infra::repositories::{AdminRepository, PgAdminRepository, PgAgentPlatformRepository},
    security::Actor,
};

/// Keyset cursor scope for `GET /api/v1/admin/skills`, minted and validated only here so a
/// cursor for this list can never be replayed against another list.
const SKILLS_SCOPE: CursorScope = CursorScope::new("admin.skills");

/// Largest skill-id batch a single bulk-enable accepts. A guard against an unbounded array, in
/// the spirit of the 300-operation import cap (§5 decision 23).
const MAX_BULK_ENABLE: usize = 500;

pub struct AgentPlatformService<'a> {
    state: &'a AppState,
    repo: PgAgentPlatformRepository,
    admin_repo: PgAdminRepository,
}

impl<'a> AgentPlatformService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            repo: PgAgentPlatformRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool),
        })
    }

    pub async fn create_skill(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SkillCreateRequest,
    ) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "skill.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_key("skill_key", &request.skill_key)?;
        validate_display_name(&request.display_name)?;
        validate_json_object("params_schema", &request.params_schema)?;
        validate_tags(&request.tags)?;
        validate_metadata(&request.metadata)?;
        let id = Uuid::now_v7();
        let record = self
            .repo
            .create_skill(
                id,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "skill.create",
                    "skill",
                    Some(id.to_string()),
                    json!({ "skill_key": &request.skill_key, "kind": &request.kind }),
                ),
            )
            .await?;
        self.record_idempotency(ctx, actor, "skill.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_skills(
        &self,
        actor: &Actor,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<SkillRecord>, AppError> {
        self.state.authz.require(actor, "moira:skills:read")?;
        let cursor = ListCursor::decode_optional(cursor, SKILLS_SCOPE)?;
        let rows = self.repo.list_skills(cursor, limit).await?;
        Ok(paginate(rows, limit, SKILLS_SCOPE))
    }

    pub async fn get_skill(&self, actor: &Actor, id: Uuid) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:read")?;
        self.repo.get_skill(id).await
    }

    pub async fn patch_skill(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: SkillPatchRequest,
    ) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if let Some(display_name) = &request.display_name {
            validate_display_name(display_name)?;
        }
        if let Some(params_schema) = &request.params_schema {
            validate_json_object("params_schema", params_schema)?;
        }
        if let Some(tags) = &request.tags {
            validate_tags(tags)?;
        }
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        self.repo
            .patch_skill(
                id,
                expected_version,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "skill.update",
                    "skill",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn delete_skill(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:skills:delete")?;
        self.repo
            .soft_delete_skill(
                id,
                expected_version,
                self.audit(
                    actor,
                    ctx,
                    "skill.delete",
                    "skill",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn set_skill_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        self.repo
            .set_skill_status(
                id,
                expected_version,
                if enabled { "enabled" } else { "disabled" },
                self.audit(
                    actor,
                    ctx,
                    if enabled {
                        "skill.enable"
                    } else {
                        "skill.disable"
                    },
                    "skill",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn bulk_enable_skills(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SkillBulkEnableRequest,
    ) -> Result<SkillBulkEnableResponse, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if request.skill_ids.is_empty() {
            return Err(AppError::BadRequest(
                "skill_ids must contain at least one id".to_string(),
            ));
        }
        if request.skill_ids.len() > MAX_BULK_ENABLE {
            return Err(AppError::BadRequest(format!(
                "skill_ids must contain at most {MAX_BULK_ENABLE} ids"
            )));
        }
        let data = self
            .repo
            .enable_skills_bulk(
                &request.skill_ids,
                self.audit(
                    actor,
                    ctx,
                    "skill.bulk_enable",
                    "skill",
                    None,
                    json!({ "requested": request.skill_ids.len() }),
                ),
            )
            .await?;
        Ok(SkillBulkEnableResponse { data })
    }

    /// Builds this service's audit row; the repository writes it inside the write's own
    /// transaction, exactly as `RuntimeAdminService::runtime_audit` does.
    fn audit(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        action: &str,
        resource_type: &str,
        resource_id: Option<String>,
        metadata: Value,
    ) -> AuditLogInsert {
        AuditLogInsert {
            request_id: Some(ctx.request_id.clone()),
            actor_type: Some(format!("{:?}", actor.actor_type)),
            actor_subject: actor.subject.clone(),
            delegated_subject: actor.delegated_subject.clone(),
            external_user_id: actor.external_user_id.clone(),
            external_tenant_id: actor.external_tenant_id.clone(),
            application_id: actor.internal_application_id,
            resource_type: resource_type.to_string(),
            resource_id,
            action: action.to_string(),
            result: AuditResult::Success,
            source_ip: ctx.source_ip,
            user_agent: ctx.user_agent.clone(),
            metadata,
        }
    }

    /// Current-formula idempotency replay. Unlike `RuntimeAdminService`, these are brand-new
    /// tables with no rows written under any legacy fingerprint/key-hash spelling, so the
    /// historical sweep is unnecessary here.
    async fn idempotency_replay<Req, Resp>(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        operation: &str,
        request: &Req,
    ) -> Result<Option<Resp>, AppError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let Some(key) = &ctx.idempotency_key else {
            return Ok(None);
        };
        let hasher = &self.state.idempotency_hasher;
        let request_bytes = serde_json::to_vec(request)
            .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))?;
        let key_hash = hasher.hash(key.as_bytes());
        let fingerprint = actor_fingerprint(hasher, actor);
        let Some(record) = self
            .admin_repo
            .get_idempotency_record(&key_hash, &fingerprint, operation)
            .await?
        else {
            return Ok(None);
        };
        if !hasher.verify(&request_bytes, &record.request_hash) {
            return Err(AppError::conflict(
                "idempotency_conflict",
                "same Idempotency-Key was used with a different request",
            ));
        }
        let Some(response_body) = record.response_body else {
            return Ok(None);
        };
        serde_json::from_value(response_body)
            .map(Some)
            .map_err(|err| AppError::Internal(format!("decode idempotent response: {err}")))
    }

    async fn record_idempotency<Req, Resp>(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        operation: &str,
        request: &Req,
        response: &Resp,
    ) -> Result<(), AppError>
    where
        Req: Serialize,
        Resp: Serialize,
    {
        let Some(key) = &ctx.idempotency_key else {
            return Ok(());
        };
        let response_body = serde_json::to_value(response).ok();
        let resource_id = response_body
            .as_ref()
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let hasher = &self.state.idempotency_hasher;
        let request_bytes = serde_json::to_vec(request)
            .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))?;
        let record = IdempotencyRecord {
            id: Uuid::now_v7(),
            idempotency_key_hash: hasher.hash(key.as_bytes()),
            actor_fingerprint: actor_fingerprint(hasher, actor),
            operation: operation.to_string(),
            request_hash: hasher.hash(&request_bytes),
            response_status: Some(200),
            response_body,
            resource_id,
            expires_at: Utc::now() + Duration::hours(24),
        };
        self.admin_repo.put_idempotency_record(&record).await?;
        Ok(())
    }
}

/// Trims a `limit + 1`-row over-fetch to `limit`, computes `has_more`, and encodes
/// `next_cursor` from the last returned row. Mirrors `runtime_admin::paginate_by_created_at`.
fn paginate(
    mut rows: Vec<SkillRecord>,
    limit: i64,
    scope: CursorScope,
) -> ListResponse<SkillRecord> {
    let has_more = (rows.len() as i64) > limit;
    if has_more {
        rows.truncate(limit.max(0) as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|record| ListCursor::new(record.created_at, record.id).encode(scope))
    } else {
        None
    };
    ListResponse {
        data: rows,
        pagination: Pagination {
            next_cursor,
            has_more,
        },
    }
}

fn validate_key(label: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.len() > 128 {
        return Err(AppError::BadRequest(format!(
            "{label} must be 1-128 characters"
        )));
    }
    let first = value.chars().next().unwrap();
    let last = value.chars().next_back().unwrap();
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return Err(AppError::BadRequest(format!(
            "{label} must start and end with an alphanumeric character"
        )));
    }
    if value
        .chars()
        .any(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-'))
    {
        return Err(AppError::BadRequest(format!(
            "{label} may contain only lowercase ASCII letters, digits, hyphen, or underscore"
        )));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.len() > 200 {
        return Err(AppError::BadRequest(
            "display_name must be 1-200 characters".to_string(),
        ));
    }
    Ok(())
}

/// A skill's `params_schema` is a JSON Schema for its tool arguments; it must be a JSON object
/// (or the empty default), never a scalar or array.
fn validate_json_object(label: &str, value: &Value) -> Result<(), AppError> {
    if value.is_null() || value.is_object() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "{label} must be a JSON object"
        )))
    }
}

fn validate_tags(tags: &[String]) -> Result<(), AppError> {
    if tags.len() > 64 {
        return Err(AppError::BadRequest(
            "tags must contain at most 64 entries".to_string(),
        ));
    }
    if tags
        .iter()
        .any(|tag| tag.trim().is_empty() || tag.len() > 64)
    {
        return Err(AppError::BadRequest(
            "each tag must be 1-64 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_metadata(value: &Value) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| AppError::BadRequest(format!("metadata is invalid JSON: {err}")))?;
    if bytes.len() > 16 * 1024 {
        return Err(AppError::BadRequest(
            "metadata must be at most 16KiB".to_string(),
        ));
    }
    Ok(())
}
