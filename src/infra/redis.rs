use std::time::Duration;

use redis::AsyncCommands;
use tokio::time::timeout;

use crate::{config::RedisSettings, error::AppError};

#[derive(Debug, Clone)]
pub struct RedisClient {
    client: redis::Client,
    namespace: String,
    connect_timeout: Duration,
    invalidation_channel: String,
}

impl RedisClient {
    pub fn from_settings(settings: &RedisSettings) -> Result<Option<Self>, AppError> {
        if !settings.enabled {
            return Ok(None);
        }
        let url = settings
            .url
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::Config("MOIRA_REDIS__URL is required when Redis is enabled".to_string())
            })?;
        let client = redis::Client::open(url)?;
        Ok(Some(Self {
            client,
            namespace: settings.namespace.clone(),
            connect_timeout: Duration::from_secs(settings.connect_timeout_seconds.max(1)),
            invalidation_channel: settings.invalidation_channel.clone(),
        }))
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn invalidation_channel(&self) -> &str {
        &self.invalidation_channel
    }

    pub async fn ping(&self) -> Result<(), AppError> {
        let mut connection = timeout(
            self.connect_timeout,
            self.client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| AppError::Config("redis connection timed out".to_string()))??;
        let _: String = timeout(
            self.connect_timeout,
            redis::cmd("PING").query_async(&mut connection),
        )
        .await
        .map_err(|_| AppError::Config("redis ping timed out".to_string()))??;
        Ok(())
    }

    pub async fn publish_runtime_invalidation(&self, payload: &str) -> Result<(), AppError> {
        let mut connection = timeout(
            self.connect_timeout,
            self.client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| AppError::Config("redis connection timed out".to_string()))??;
        let _: usize = connection
            .publish(self.invalidation_channel.as_str(), payload)
            .await?;
        Ok(())
    }

    pub fn key(&self, suffix: &str) -> String {
        format!("{}:{suffix}", self.namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_is_optional_by_default() {
        assert!(
            RedisClient::from_settings(&RedisSettings::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn redis_requires_url_when_enabled() {
        let settings = RedisSettings {
            enabled: true,
            ..RedisSettings::default()
        };
        assert!(RedisClient::from_settings(&settings).is_err());
    }
}
