use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::Config;

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
            .busy_timeout(Duration::from_secs(10));

        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(15))
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
