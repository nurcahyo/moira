use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::{config::TelemetrySettings, error::AppError};

pub fn init(settings: &TelemetrySettings) -> Result<(), AppError> {
    let filter = EnvFilter::try_new(&settings.env_filter)
        .or_else(|_| EnvFilter::try_from_default_env())
        .map_err(|err| AppError::Config(format!("invalid telemetry filter: {err}")))?;

    if settings.json {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .try_init()
            .map_err(|err| AppError::Config(format!("initialize tracing: {err}")))?;
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .try_init()
            .map_err(|err| AppError::Config(format!("initialize tracing: {err}")))?;
    }

    Ok(())
}
