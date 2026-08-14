use std::sync::Arc;

use crate::{
    app::AppState,
    config::DeploymentEnvironment,
    domain::{
        SetupCheckName, SetupCheckState, SetupChecks, SetupDeploymentEnvironment, SetupStatus,
        SetupStatusResponse,
    },
    error::AppError,
    infra::repositories::{PgSetupRepository, SetupReadinessSnapshot, SetupRepository},
    security::{Actor, ActorType},
};

pub struct SetupService<'a> {
    state: &'a AppState,
    repo: Arc<dyn SetupRepository>,
}

impl<'a> SetupService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        Ok(Self {
            repo: Arc::new(PgSetupRepository::new(state.pool()?.clone())),
            state,
        })
    }

    /// Injects an arbitrary [`SetupRepository`], so the authorization and status-precedence logic
    /// can be unit-tested without a database. `new` remains the only production constructor.
    #[cfg(test)]
    pub(crate) fn with_repository(state: &'a AppState, repo: Arc<dyn SetupRepository>) -> Self {
        Self { state, repo }
    }

    pub async fn status(&self, actor: &Actor) -> Result<SetupStatusResponse, AppError> {
        require_setup_actor(actor)?;
        self.state.authz.require(actor, "moira:setup:read")?;
        let snapshot = self.repo.inspect().await?;
        Ok(build_response(
            self.state.settings.deployment.environment,
            snapshot,
        ))
    }
}

/// `pub(crate)` so `GET /api/v1/admin/setup/auth-methods` (plan 07, module 9) gates on the
/// *same* function as the pre-existing structural endpoint rather than on a second copy of
/// it. Two divergent transcriptions of one authorization rule is how a gate silently
/// weakens on one surface and not the other.
pub(crate) fn require_setup_actor(actor: &Actor) -> Result<(), AppError> {
    match actor.actor_type {
        ActorType::SystemKey | ActorType::TrustedJwt => Ok(()),
        ActorType::ConsumerKey | ActorType::DevAdmin | ActorType::Anonymous => Err(
            AppError::Forbidden("setup status requires a system key or trusted JWT".to_string()),
        ),
    }
}

fn build_response(
    environment: DeploymentEnvironment,
    snapshot: SetupReadinessSnapshot,
) -> SetupStatusResponse {
    let checks = SetupChecks {
        database: SetupCheckState::Ready,
        root_system_key: check(snapshot.root_system_key),
        application: check(snapshot.application),
        route: check(snapshot.route),
        provider: check(snapshot.provider),
        provider_model: check(snapshot.provider_model),
        provider_credential: check(snapshot.provider_credential),
        routing_policy: check(snapshot.routing_policy),
        executable_path: check(snapshot.executable_path),
    };

    let ordered_checks = [
        (SetupCheckName::RootSystemKey, snapshot.root_system_key),
        (SetupCheckName::Application, snapshot.application),
        (SetupCheckName::Route, snapshot.route),
        (SetupCheckName::Provider, snapshot.provider),
        (SetupCheckName::ProviderModel, snapshot.provider_model),
        (
            SetupCheckName::ProviderCredential,
            snapshot.provider_credential,
        ),
        (SetupCheckName::RoutingPolicy, snapshot.routing_policy),
        (SetupCheckName::ExecutablePath, snapshot.executable_path),
    ];
    let missing = ordered_checks
        .into_iter()
        .filter_map(|(name, ready)| (!ready).then_some(name))
        .collect();

    let status = if !snapshot.root_system_key {
        SetupStatus::SetupRequired
    } else if snapshot.executable_path {
        SetupStatus::Ready
    } else {
        SetupStatus::ConfigurationIncomplete
    };

    SetupStatusResponse {
        status,
        deployment_environment: match environment {
            DeploymentEnvironment::Development => SetupDeploymentEnvironment::Development,
            DeploymentEnvironment::Test => SetupDeploymentEnvironment::Test,
            DeploymentEnvironment::Production => SetupDeploymentEnvironment::Production,
        },
        checks,
        missing,
    }
}

fn check(ready: bool) -> SetupCheckState {
    if ready {
        SetupCheckState::Ready
    } else {
        SetupCheckState::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::infra::repositories::InMemorySetupRepository;

    /// Pool-less: `SetupService::with_repository` never calls `state.pool()`, which is exactly
    /// what makes these tests Postgres-free.
    ///
    /// A `LazyLock` no longer fits: `AppState::new` is `async` since it preflights the content
    /// encryption custody, and a `LazyLock` initialiser cannot await. Building one per test is
    /// the cheaper of the two remaining options — the state holds no pool and the preflight is
    /// a single in-process AES wrap/unwrap.
    async fn state() -> AppState {
        AppState::new(crate::config::Settings::default(), None)
            .await
            .unwrap()
    }

    fn actor(actor_type: ActorType, scopes: &[&str]) -> Actor {
        Actor {
            actor_type,
            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
            ..Actor::default()
        }
    }

    #[test]
    fn only_system_keys_and_trusted_jwts_are_setup_actors() {
        assert!(require_setup_actor(&actor(ActorType::SystemKey, &[])).is_ok());
        assert!(require_setup_actor(&actor(ActorType::TrustedJwt, &[])).is_ok());
        assert!(require_setup_actor(&actor(ActorType::ConsumerKey, &[])).is_err());
        assert!(require_setup_actor(&actor(ActorType::DevAdmin, &[])).is_err());
        assert!(require_setup_actor(&actor(ActorType::Anonymous, &[])).is_err());
    }

    #[test]
    fn status_precedence_and_missing_order_are_stable() {
        let empty = build_response(
            DeploymentEnvironment::Development,
            SetupReadinessSnapshot::default(),
        );
        assert_eq!(empty.status, SetupStatus::SetupRequired);
        assert_eq!(
            empty.missing,
            vec![
                SetupCheckName::RootSystemKey,
                SetupCheckName::Application,
                SetupCheckName::Route,
                SetupCheckName::Provider,
                SetupCheckName::ProviderModel,
                SetupCheckName::ProviderCredential,
                SetupCheckName::RoutingPolicy,
                SetupCheckName::ExecutablePath,
            ]
        );

        let incomplete = build_response(
            DeploymentEnvironment::Production,
            SetupReadinessSnapshot {
                root_system_key: true,
                ..SetupReadinessSnapshot::default()
            },
        );
        assert_eq!(incomplete.status, SetupStatus::ConfigurationIncomplete);
        assert_eq!(
            incomplete.deployment_environment,
            SetupDeploymentEnvironment::Production
        );

        let ready = build_response(
            DeploymentEnvironment::Test,
            SetupReadinessSnapshot {
                root_system_key: true,
                application: true,
                route: true,
                provider: true,
                provider_model: true,
                provider_credential: true,
                routing_policy: true,
                executable_path: true,
            },
        );
        assert_eq!(ready.status, SetupStatus::Ready);
        assert!(ready.missing.is_empty());
    }

    /// The payoff of P2-3 (plan 06, Module 8): the *whole* of `SetupService::status` — actor
    /// gating, scope enforcement, the repository read and response assembly — now runs with no
    /// Postgres connection at all. Before `SetupRepository` existed, `SetupService` held a
    /// concrete `PgSetupRepository` and this path could only be reached against a live database,
    /// so it had no unit coverage whatsoever.
    #[tokio::test]
    async fn status_is_setup_required_without_a_root_system_key() {
        let state = state().await;
        let service = SetupService::with_repository(
            &state,
            Arc::new(InMemorySetupRepository::new(
                SetupReadinessSnapshot::default(),
            )),
        );

        let response = service
            .status(&actor(ActorType::SystemKey, &["moira:setup:read"]))
            .await
            .expect("setup status");
        assert_eq!(response.status, SetupStatus::SetupRequired);
        assert_eq!(response.missing[0], SetupCheckName::RootSystemKey);
    }

    #[tokio::test]
    async fn status_is_ready_when_an_executable_path_exists() {
        let state = state().await;
        let service = SetupService::with_repository(
            &state,
            Arc::new(InMemorySetupRepository::new(SetupReadinessSnapshot {
                root_system_key: true,
                application: true,
                route: true,
                provider: true,
                provider_model: true,
                provider_credential: true,
                routing_policy: true,
                executable_path: true,
            })),
        );

        let response = service
            .status(&actor(ActorType::TrustedJwt, &["moira:setup:read"]))
            .await
            .expect("setup status");
        assert_eq!(response.status, SetupStatus::Ready);
        assert!(response.missing.is_empty());
    }

    /// The actor gate runs before the repository is touched, so a rejected caller learns nothing
    /// about deployment readiness.
    #[tokio::test]
    async fn status_rejects_a_consumer_key_before_touching_the_repository() {
        let state = state().await;
        let service = SetupService::with_repository(
            &state,
            Arc::new(InMemorySetupRepository::new(
                SetupReadinessSnapshot::default(),
            )),
        );

        let error = service
            .status(&actor(ActorType::ConsumerKey, &["moira:setup:read"]))
            .await
            .expect_err("consumer keys are not setup actors");
        assert!(matches!(error, AppError::Forbidden(_)));
    }
}
