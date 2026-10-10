use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::Config;

/// Default SQLite connection-pool size.
pub(crate) const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
/// Upper bound for `OPENLLM_DB_MAX_CONNECTIONS` so a typo cannot spawn an
/// unbounded number of connections.
const MAX_DB_MAX_CONNECTIONS: u64 = 256;
/// Default `busy_timeout` for a contended SQLite write.
pub(crate) const DEFAULT_DB_BUSY_TIMEOUT_SECS: u64 = 10;
/// Default wait for a pooled connection before a request fails.
pub(crate) const DEFAULT_DB_ACQUIRE_TIMEOUT_SECS: u64 = 15;

/// Parses `OPENLLM_DB_MAX_CONNECTIONS`, clamped to `1..=256`.
pub(crate) fn parse_db_max_connections(value: Option<&str>) -> u32 {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(1, MAX_DB_MAX_CONNECTIONS) as u32)
        .unwrap_or(DEFAULT_DB_MAX_CONNECTIONS)
}

/// Parses a positive SQLite timeout in seconds, keeping `default` otherwise.
pub(crate) fn parse_db_timeout_secs(value: Option<&str>, default: u64) -> u64 {
    match value.map(str::trim) {
        None | Some("") => default,
        Some(raw) => raw.parse::<u64>().ok().filter(|secs| *secs > 0).unwrap_or(default),
    }
}

/// Configured SQLite connection-pool size.
pub(crate) fn db_max_connections() -> u32 {
    parse_db_max_connections(std::env::var("OPENLLM_DB_MAX_CONNECTIONS").ok().as_deref())
}

/// Configured `busy_timeout` for contended SQLite writes.
pub(crate) fn db_busy_timeout_secs() -> u64 {
    parse_db_timeout_secs(
        std::env::var("OPENLLM_DB_BUSY_TIMEOUT_SECS").ok().as_deref(),
        DEFAULT_DB_BUSY_TIMEOUT_SECS,
    )
}

/// Configured wait for a pooled connection before a request fails.
pub(crate) fn db_acquire_timeout_secs() -> u64 {
    parse_db_timeout_secs(
        std::env::var("OPENLLM_DB_ACQUIRE_TIMEOUT_SECS").ok().as_deref(),
        DEFAULT_DB_ACQUIRE_TIMEOUT_SECS,
    )
}

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let database_url = match &config.database_url {
            Some(url) => url.clone(),
            None => {
                tokio::fs::create_dir_all(&config.data_dir)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to create data directory {}",
                            config.data_dir.display()
                        )
                    })?;
                let path = config.data_dir.join("openllm.db");
                sqlite_url(&path)
            }
        };

        let options = SqliteConnectOptions::from_str(&database_url)
            .with_context(|| format!("invalid SQLite URL: {database_url}"))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .pragma("cache_size", "-16384")
            .pragma("temp_store", "MEMORY")
            .busy_timeout(Duration::from_secs(db_busy_timeout_secs()));

        let pool = SqlitePoolOptions::new()
            .max_connections(db_max_connections())
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(db_acquire_timeout_secs()))
            .connect_with(options)
            .await
            .context("failed to connect to SQLite")?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to run database migrations")?;

        Ok(Self { pool })
    }
}

fn sqlite_url(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    format!("sqlite://{normalized}")
}
