use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::domain::{ProviderConfig, PublicAuthMethod};

#[derive(Debug, Clone)]
pub struct RuntimeConfigCache {
    ttl: Duration,
    providers: Arc<RwLock<HashMap<Uuid, CacheEntry<ProviderConfig>>>>,
}

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    value: T,
    expires_at: Instant,
}

impl RuntimeConfigCache {
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_seconds),
            providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get_provider(&self, id: Uuid) -> Option<ProviderConfig> {
        let providers = self.providers.read().await;
        providers
            .get(&id)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.value.clone())
    }

    pub async fn put_provider(&self, provider: ProviderConfig) {
        let mut providers = self.providers.write().await;
        providers.insert(
            provider.id,
            CacheEntry {
                value: provider,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    pub async fn invalidate_all(&self) {
        self.providers.write().await.clear();
    }
}

/// The enabled auth methods a deployment offers, cached in process (plan 07, module 13a).
///
/// CONVENTIONS §7.2 requires that changing auth settings invalidate the runtime cache
/// through the existing Postgres `LISTEN/NOTIFY` path. That requirement needs a cache to
/// invalidate; this is it.
///
/// # Why it holds one whole list rather than a keyed map
///
/// [`RuntimeConfigCache`] keys providers by id because callers ask for one provider at a
/// time. The only reader here — `GET /api/v1/admin/setup/auth-methods`, called by plan 08's
/// console at boot — always wants *every* enabled method, and `auth_provider_settings` is a
/// handful of rows by construction (three methods exist). A map keyed by nothing is one
/// slot, so it is spelled as one slot.
///
/// # It caches the public projection, not the record
///
/// Storing [`PublicAuthMethod`] rather than the full row means the narrow projection
/// happens once, at the repository, and nothing downstream can accidentally widen the
/// response by reaching for a field the cache happened to retain.
///
/// The TTL is a backstop, not the invalidation mechanism: `listen_once` clears this cache
/// on every `moira_runtime_config` notification, which is what makes a write on one
/// instance visible on all of them. The TTL only bounds staleness if a notification is
/// lost — e.g. across a listener reconnect.
#[derive(Debug, Clone)]
pub struct AuthProviderSettingsCache {
    ttl: Duration,
    methods: Arc<RwLock<Option<CacheEntry<Vec<PublicAuthMethod>>>>>,
}

impl AuthProviderSettingsCache {
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_seconds),
            methods: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn enabled_methods(&self) -> Option<Vec<PublicAuthMethod>> {
        let methods = self.methods.read().await;
        methods
            .as_ref()
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.value.clone())
    }

    pub async fn put_enabled_methods(&self, methods: Vec<PublicAuthMethod>) {
        *self.methods.write().await = Some(CacheEntry {
            value: methods,
            expires_at: Instant::now() + self.ttl,
        });
    }

    pub async fn invalidate_all(&self) {
        *self.methods.write().await = None;
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::domain::AuthMethod;

    fn method(client_id: &str) -> PublicAuthMethod {
        PublicAuthMethod {
            id: Uuid::nil(),
            method: AuthMethod::GoogleOauth,
            display_name: "Google".to_string(),
            issuer: Some("https://accounts.google.com".to_string()),
            discovery_url: None,
            authorization_url: None,
            jwks_url: None,
            client_id: Some(client_id.to_string()),
            requested_scopes: vec!["openid".to_string()],
            allowed_email_domains: vec!["example.com".to_string()],
        }
    }

    #[tokio::test]
    async fn the_auth_settings_cache_serves_then_forgets_on_invalidation() {
        let cache = AuthProviderSettingsCache::new(300);
        assert!(cache.enabled_methods().await.is_none());

        cache.put_enabled_methods(vec![method("cid-1")]).await;
        let cached = cache.enabled_methods().await.expect("a cached read");
        assert_eq!(cached[0].client_id.as_deref(), Some("cid-1"));

        cache.invalidate_all().await;
        assert!(
            cache.enabled_methods().await.is_none(),
            "an invalidated cache must miss, so the next read rebuilds from the database"
        );
    }

    /// A zero TTL expires immediately, which is the property the listener relies on being
    /// *additional* to explicit invalidation rather than a substitute for it.
    #[tokio::test]
    async fn an_expired_entry_is_not_served() {
        let cache = AuthProviderSettingsCache::new(0);
        cache.put_enabled_methods(vec![method("cid-2")]).await;
        assert!(cache.enabled_methods().await.is_none());
    }
}
