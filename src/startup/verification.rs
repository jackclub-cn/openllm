use super::*;

/// Tables required before a database can be accepted for restoration.
pub(crate) const REQUIRED_TABLES: &[&str] = &[
    "_sqlx_migrations",
    "api_keys",
    "provider_models",
    "providers",
    "route_targets",
    "routes",
    "webhooks",
];

/// Open a database file read-only and report what is inside it.
///
/// Read-only on purpose: verifying a backup must never modify it.
pub(crate) async fn verify_database_file(
    path: &Path,
) -> anyhow::Result<crate::models::DatabaseFileReport> {
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))?;
    use std::str::FromStr;

    let mut problems = Vec::new();
    if metadata.len() < 100 {
        problems.push("the file is too small to be a SQLite database".to_string());
    } else {
        let mut header = [0u8; 16];
        let mut file = tokio::fs::File::open(path).await?;
        let read = tokio::io::AsyncReadExt::read(&mut file, &mut header).await?;
        if read < header.len() || header != *b"SQLite format 3\0" {
            problems.push("the file does not start with the SQLite header".to_string());
        }
    }

    let url = format!("sqlite://{}", path.to_string_lossy().replace('\\', "/"));
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&url)?
        .read_only(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let mut report = crate::models::DatabaseFileReport {
        file: path.to_string_lossy().replace('\\', "/"),
        ok: false,
        size_bytes: metadata.len(),
        integrity: "not checked".to_string(),
        tables: Vec::new(),
        missing_tables: Vec::new(),
        applied_migrations: 0,
        schema_version: 0,
        providers: 0,
        routes: 0,
        api_keys: 0,
        webhooks: 0,
        provider_pools: 0,
        problems: Vec::new(),
    };

    match sqlx::SqlitePool::connect_with(options).await {
        Ok(pool) => {
            report.integrity = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|error| format!("check failed: {error}"));
            report.tables = sqlx::query_scalar::<_, String>(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                   AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
            report.missing_tables = REQUIRED_TABLES
                .iter()
                .filter(|required| !report.tables.iter().any(|table| table == *required))
                .map(|required| required.to_string())
                .collect();
            let has = |table: &str| report.tables.iter().any(|name| name == table);
            if has("_sqlx_migrations") {
                report.applied_migrations =
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
                        .fetch_one(&pool)
                        .await
                        .unwrap_or(0);
                report.schema_version = sqlx::query_scalar::<_, i64>(
                    "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations",
                )
                .fetch_one(&pool)
                .await
                .unwrap_or(0);
            }
            report.providers = table_count(&pool, "providers").await;
            report.routes = table_count(&pool, "routes").await;
            report.api_keys = table_count(&pool, "api_keys").await;
            report.webhooks = table_count(&pool, "webhooks").await;
            report.provider_pools = table_count(&pool, "provider_pools").await;
            pool.close().await;
        }
        Err(error) => problems.push(format!("could not open the file: {error}")),
    }

    if report.integrity != "ok" {
        problems.push(format!("integrity check reported '{}'", report.integrity));
    }
    if !report.missing_tables.is_empty() {
        problems.push(format!(
            "missing required tables: {}",
            report.missing_tables.join(", ")
        ));
    }
    report.ok = problems.is_empty();
    report.problems = problems;
    Ok(report)
}

/// Row count of `table`, or 0 when the table does not exist in this file.
pub(super) async fn table_count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}

/// Open the database as an operator tool: read-write, no migrations and no
/// instance lease. Used by `--backup`, which must be able to run while the
/// gateway is serving (WAL allows a second connection).
pub(crate) async fn open_operator_pool(path: &Path) -> anyhow::Result<SqlitePool> {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::str::FromStr;

    let url = format!("sqlite://{}", path.to_string_lossy().replace('\\', "/"));
    let options = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(false)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    Ok(pool)
}

/// Open an existing database in strict read-only mode for diagnostics.
///
/// Unlike [`open_operator_pool`], this never changes the journal mode or
/// creates sidecar files, so `--self-check` is safe to run while the gateway is
/// serving and on read-only mounts.
pub(crate) async fn open_operator_read_pool(path: &Path) -> anyhow::Result<SqlitePool> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let url = format!("sqlite://{}", path.to_string_lossy().replace('\\', "/"));
    let options = SqliteConnectOptions::from_str(&url)?
        .read_only(true)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open {} read-only", path.display()))?;
    Ok(pool)
}

/// Resolve an operator-supplied backup name.
///
/// A plain name is looked up in the backups directory. Anything containing a
/// path separator is rejected unless `allow_path` is set, which only the
/// command line does: the HTTP API must never be able to address an arbitrary
/// file on the host.
pub(crate) fn resolve_backup_path(
    database_path: &Path,
    name: &str,
    allow_path: bool,
) -> anyhow::Result<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("a backup name is required");
    }
    let looks_like_a_path =
        trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..");
    if looks_like_a_path && !allow_path {
        anyhow::bail!("the backup name must be a plain file name inside the backups directory");
    }
    if looks_like_a_path {
        return Ok(PathBuf::from(trimmed));
    }
    Ok(backups_directory(database_path).join(trimmed))
}

/// Whether a live instance appears to be serving this database file.
///
/// Used to refuse a restore that would put two writers on one file. A lease
/// whose process cannot be probed counts as alive, and only a lease from *this*
/// host can ever be probed: refusing a restore is recoverable, corrupting a
/// live database is not.
pub(crate) async fn database_is_served(database_path: &Path) -> anyhow::Result<bool> {
    if !database_path.exists() {
        return Ok(false);
    }
    let Ok(pool) = open_operator_pool(database_path).await else {
        return Ok(false);
    };
    let rows = sqlx::query_as::<_, (String, i64, String, i64)>(
        "SELECT id, pid, host, \
           CAST((julianday('now') - julianday(heartbeat_at)) * 86400 AS INTEGER) \
         FROM instance_leases",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    pool.close().await;
    let ttl = lease_ttl_secs();
    let host = host_name();
    for (id, pid, lease_host, heartbeat_age_secs) in rows {
        if heartbeat_age_secs > ttl {
            continue;
        }
        // A lease written on another host (a shared database moved between
        // machines) cannot be probed, so it counts as alive — the same rule as
        // for a local pid that cannot be inspected.
        if lease_host == host && pid_is_running(pid) == Some(false) {
            continue;
        }
        tracing::warn!(
            instance = %id,
            pid,
            host = %lease_host,
            heartbeat_age_secs,
            "a live instance is serving this database"
        );
        return Ok(true);
    }
    Ok(false)
}
