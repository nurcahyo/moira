use std::collections::HashSet;

use crate::error::AppError;

use super::{Actor, ActorType};

pub const ADMIN_SCOPE: &str = "moira:admin";
pub const ADMIN_SCOPES: &[&str] = &[
    ADMIN_SCOPE,
    "moira:applications:read",
    "moira:applications:write",
    "moira:applications:delete",
    "moira:providers:read",
    "moira:providers:write",
    "moira:providers:delete",
    "moira:models:read",
    "moira:models:write",
    "moira:models:delete",
    "moira:credentials:read",
    "moira:credentials:write",
    "moira:credentials:rotate",
    "moira:credentials:disable",
    "moira:credentials:delete",
    "moira:jwt-issuers:read",
    "moira:jwt-issuers:write",
    "moira:jwt-issuers:delete",
    "moira:system-keys:read",
    "moira:system-keys:write",
    "moira:system-keys:rotate",
    "moira:system-keys:revoke",
    "moira:consumer-keys:read",
    "moira:consumer-keys:write",
    "moira:consumer-keys:rotate",
    "moira:consumer-keys:revoke",
    "moira:audit:read",
    "moira:identity:delegate",
    "moira:routes:read",
    "moira:routes:write",
    "moira:routes:delete",
    "moira:routing-policies:read",
    "moira:routing-policies:write",
    "moira:routing-policies:delete",
    "moira:agent-profiles:read",
    "moira:agent-profiles:write",
    "moira:agent-profiles:delete",
    "moira:runtime-policies:read",
    "moira:runtime-policies:write",
    "moira:runtime:diagnose",
    "moira:responses:create",
    "moira:responses:stream",
    "moira:responses:read",
    "moira:executions:read",
    "moira:usage:read",
    "moira:capabilities:read",
    "moira:execution-policies:read",
    "moira:execution-policies:write",
    "moira:execution:override-route",
    "moira:execution:override-model",
    "moira:execution:override-provider",
    "moira:execution:override-credential",
    "moira:execution:override-timeout",
    "moira:execution:use-tools",
    "moira:conversations:create",
    "moira:conversations:read",
    "moira:conversations:write",
    "moira:conversations:delete",
    "moira:conversations:export",
    "moira:memories:create",
    "moira:memories:read",
    "moira:memories:write",
    "moira:memories:delete",
    "moira:rag-collections:read",
    "moira:rag-collections:write",
    "moira:rag-collections:delete",
    "moira:rag-documents:read",
    "moira:rag-documents:write",
    "moira:rag-documents:delete",
    "moira:rag-documents:ingest",
    "moira:conversation-policies:read",
    "moira:conversation-policies:write",
    "moira:memory-policies:read",
    "moira:memory-policies:write",
    "moira:retrieval-policies:read",
    "moira:retrieval-policies:write",
    "moira:embedding-policies:read",
    "moira:embedding-policies:write",
    "moira:memory:extract",
    "moira:memory:diagnose",
    "moira:retrieval:diagnose",
];

#[derive(Debug, Clone, Default)]
pub struct AuthorizationService;

impl AuthorizationService {
    pub fn new() -> Self {
        Self
    }

    pub fn require(&self, actor: &Actor, required_scope: &str) -> Result<(), AppError> {
        if !Self::is_known_scope(required_scope) {
            return Err(AppError::Internal(format!(
                "unknown required scope {required_scope}"
            )));
        }
        if self.has_scope(actor, required_scope) {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "missing required scope {required_scope}"
            )))
        }
    }

    pub fn has_scope(&self, actor: &Actor, required_scope: &str) -> bool {
        let scopes: HashSet<_> = actor.scopes.iter().map(String::as_str).collect();
        scopes.contains(required_scope)
            || (actor.actor_type != ActorType::ConsumerKey && scopes.contains(ADMIN_SCOPE))
    }

    pub fn normalize_scopes(scopes: &[String]) -> Result<Vec<String>, AppError> {
        let mut normalized = Vec::new();
        for scope in scopes {
            let scope = scope.trim();
            if scope.is_empty() {
                return Err(AppError::unprocessable(
                    "scope_invalid",
                    "api key scopes must not contain empty values",
                ));
            }
            if !Self::is_known_scope(scope) {
                return Err(AppError::unprocessable(
                    "scope_invalid",
                    format!("unknown scope {scope}"),
                ));
            }
            normalized.push(scope.to_string());
        }
        normalized.sort();
        normalized.dedup();
        Ok(normalized)
    }

    pub fn can_grant(&self, actor: &Actor, requested: &[String]) -> bool {
        actor.actor_type != ActorType::ConsumerKey
            && requested.iter().all(|scope| self.has_scope(actor, scope))
    }

    pub fn is_known_scope(scope: &str) -> bool {
        ADMIN_SCOPES.contains(&scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_by_default_and_admin_implies_admin_scopes() {
        let authz = AuthorizationService::new();
        let actor = Actor {
            actor_type: ActorType::SystemKey,
            scopes: vec!["moira:admin".to_string()],
            ..Actor::default()
        };
        assert!(authz.require(&actor, "moira:providers:write").is_ok());

        let consumer = Actor {
            actor_type: ActorType::ConsumerKey,
            scopes: vec!["moira:admin".to_string()],
            ..Actor::default()
        };
        assert!(authz.require(&consumer, "moira:providers:write").is_err());

        let empty = Actor::default();
        assert!(authz.require(&empty, "moira:providers:read").is_err());

        assert!(authz.require(&actor, "moira:responses:create").is_ok());
        assert!(authz.require(&consumer, "moira:responses:create").is_err());
    }
}
