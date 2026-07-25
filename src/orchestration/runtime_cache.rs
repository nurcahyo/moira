use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::domain::ProviderConfig;

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
