use std::collections::HashSet;

use crate::error::AppError;

use super::{Actor, ActorType};

pub const ADMIN_SCOPE: &str = "moira:admin";
pub const ADMIN_SCOPES: &[&str] = &[
    ADMIN_SCOPE,
    "moira:setup:read",
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
    // Plan 07 — the runtime auth-provider settings surface. `AuthorizationService::require`
    // rejects an unknown scope with a 500, so these are not decoration: without them the
    // whole `/api/v1/admin/auth/providers…` surface would be unreachable rather than
    // unprotected. Nothing else about `authz.rs` changes for plan 07 — in particular no
    // `ADMIN_IMPLYING_ACTOR_TYPES` entry is added, because decision D1 defers the
    // `ActorType::SetupToken` variant that would have raised the question.
    "moira:auth-settings:read",
    "moira:auth-settings:write",
    "moira:auth-settings:delete",
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

    /// Actor types for which holding [`ADMIN_SCOPE`] implies every other scope.
    ///
    /// **An allow-list, deliberately, because the previous formulation failed open.** This was
    /// `actor.actor_type != ActorType::ConsumerKey`, which grants admin implication to every actor
    /// type *except* one — so a new [`ActorType`] variant inherited full admin authority silently,
    /// with no match to make non-exhaustive and therefore no compiler warning.
    ///
    /// That was not exploitable with the variants that existed, but it is a fail-open default in the
    /// authorization core. Plan 07 proposes adding `ActorType::SetupToken` intending it to be denied
    /// implication; under the old form it would have received full admin authority instead.
    ///
    /// Adding a variant here must be a deliberate decision. A variant absent from this list is
    /// denied implication, which is the safe direction to be wrong in.
    const ADMIN_IMPLYING_ACTOR_TYPES: &'static [ActorType] = &[
        ActorType::DevAdmin,
        ActorType::SystemKey,
        ActorType::TrustedJwt,
    ];

    fn admin_scope_implies_all(actor: &Actor) -> bool {
        Self::ADMIN_IMPLYING_ACTOR_TYPES.contains(&actor.actor_type)
    }

    pub fn has_scope(&self, actor: &Actor, required_scope: &str) -> bool {
        let scopes: HashSet<_> = actor.scopes.iter().map(String::as_str).collect();
        scopes.contains(required_scope)
            || (Self::admin_scope_implies_all(actor) && scopes.contains(ADMIN_SCOPE))
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

    /// Whether `actor` may mint a key carrying `requested`.
    ///
    /// Uses the same allow-list as [`Self::has_scope`] for the same reason: the previous
    /// `!= ConsumerKey` form handed grant authority to any future [`ActorType`] by default.
    pub fn can_grant(&self, actor: &Actor, requested: &[String]) -> bool {
        Self::admin_scope_implies_all(actor)
            && requested.iter().all(|scope| self.has_scope(actor, scope))
    }

    pub fn is_known_scope(scope: &str) -> bool {
        ADMIN_SCOPES.contains(&scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Admin implication is an allow-list, so an actor type nobody has thought about is denied.
    ///
    /// The regression this guards is subtle: the previous form was
    /// `actor.actor_type != ActorType::ConsumerKey`, which grants implication to everything except
    /// one variant. A new variant would silently inherit full admin authority, and because there is
    /// no `match`, the compiler could not warn. Plan 07 intends to add `ActorType::SetupToken`
    /// *denied* implication — under the old form it would have been granted it instead.
    ///
    /// `Anonymous` stands in here for "a variant not on the list". It is a real tightening: an
    /// anonymous actor carrying `moira:admin` previously received implication.
    #[test]
    fn admin_implication_is_denied_to_actor_types_not_on_the_allow_list() {
        let authz = AuthorizationService::new();
        let admin_scoped = |actor_type: ActorType| Actor {
            actor_type,
            scopes: vec![ADMIN_SCOPE.to_string()],
            ..Default::default()
        };

        for actor_type in [
            ActorType::DevAdmin,
            ActorType::SystemKey,
            ActorType::TrustedJwt,
        ] {
            assert!(
                authz.has_scope(&admin_scoped(actor_type), "moira:applications:write"),
                "{actor_type:?} is on the allow-list and must keep admin implication"
            );
        }

        for actor_type in [ActorType::Anonymous, ActorType::ConsumerKey] {
            let actor = admin_scoped(actor_type);
            assert!(
                !authz.has_scope(&actor, "moira:applications:write"),
                "{actor_type:?} is not on the allow-list, so holding moira:admin must NOT imply \
                 every other scope — the old `!= ConsumerKey` form failed open here"
            );
            assert!(
                !authz.can_grant(&actor, &["moira:applications:write".to_string()]),
                "{actor_type:?} must not be able to mint scopes it only holds by implication"
            );
        }
    }

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
        assert!(authz.require(&actor, "moira:setup:read").is_ok());
        assert!(authz.require(&consumer, "moira:setup:read").is_err());

        let setup_reader = Actor {
            actor_type: ActorType::TrustedJwt,
            scopes: vec!["moira:setup:read".to_string()],
            ..Actor::default()
        };
        assert!(authz.require(&setup_reader, "moira:setup:read").is_ok());
    }
}
