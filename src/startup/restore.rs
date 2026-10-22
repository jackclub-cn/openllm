use super::*;

/// Suffix of the file the previous database is moved to during a restore.
pub(crate) const PRE_RESTORE_SUFFIX: &str = "db.pre-restore";

/// Result of a restore, reported to the operator.
pub(crate) struct RestoreOutcome {
    pub restored_from: PathBuf,
    pub restored_to: PathBuf,
    /// Where the previous database was moved, if there was one.
    pub previous: Option<PathBuf>,
    /// Instance leases dropped from the restored file.
    pub cleared_leases: u64,
    pub report: crate::models::DatabaseFileReport,
}

/// Drop the instance leases recorded in a restored database.
///
/// A lease is a claim held by a *process*; a file that was just restored has no
/// process holding it. The recorded pid may be dead, may now belong to an
/// unrelated process that reused the number, or may name another host (a backup
/// restored after a machine loss). Any of those would make the next start wait
/// out the lease TTL — or refuse — which is exactly the disaster-recovery path
/// that has to work. Clearing the table makes the restored file startable.
pub(super) async fn clear_instance_leases(database_path: &Path) -> anyhow::Result<u64> {
    let pool = open_operator_pool(database_path).await?;
    let result: anyhow::Result<u64> = async {
        let table = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'instance_leases'",
        )
        .fetch_one(&pool)
        .await?;
        if table == 0 {
            return Ok(0);
        }
        let removed = sqlx::query("DELETE FROM instance_leases")
            .execute(&pool)
            .await?
            .rows_affected();
        // Fold the WAL back into the file so the next process opens a clean
        // database rather than replaying our delete.
        sqlx::raw_sql("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await?;
        Ok(removed)
    }
    .await;
    pool.close().await;
    let removed = result.context("failed to clear the restored instance lease")?;
    if removed > 0 {
        tracing::info!(
            leases = removed,
            "cleared the instance lease recorded in the restored database"
        );
    }
    Ok(removed)
}

/// Replace the database file with a verified backup.
///
/// Intentionally only touches the file when the gateway is not running: the
/// caller is the `--restore-backup` command, which returns before any pool or
/// listener exists. The previous database is renamed rather than deleted so a
/// restore that turns out to be wrong is reversible.
pub(crate) async fn restore_backup(
    database_path: &Path,
    backup: &Path,
) -> anyhow::Result<RestoreOutcome> {
    let report = verify_database_file(backup).await?;
    if !report.ok {
        anyhow::bail!(
            "refusing to restore {}: {}",
            backup.display(),
            report.problems.join("; ")
        );
    }
    if let Some(parent) = database_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Stage next to the target so the final step is an atomic rename on the
    // same filesystem.
    let staging = sidecar(database_path, ".restoring");
    tokio::fs::copy(backup, &staging).await.with_context(|| {
        format!(
            "failed to copy {} to {}",
            backup.display(),
            staging.display()
        )
    })?;
    crate::secure_file::set_private_permissions(&staging)
        .with_context(|| format!("failed to secure {}", staging.display()))?;

    // Set the current database aside *with* its WAL sidecars before the restore
    // lands. Deleting the sidecars first would lose committed-but-not-yet-
    // checkpointed data as soon as a later step failed, and leaving a stale
    // `-wal` next to the target would make SQLite replay the old log onto the
    // restored file. Moving them to names derived from `.pre-restore` keeps the
    // set-aside database openable.
    let previous = if database_path.exists() {
        let previous = database_path.with_extension(PRE_RESTORE_SUFFIX);
        if previous.exists() {
            tokio::fs::remove_file(&previous).await.ok();
        }
        tokio::fs::rename(database_path, &previous)
            .await
            .with_context(|| {
                format!(
                    "failed to set the current database aside at {}",
                    previous.display()
                )
            })?;
        move_sidecars(database_path, &previous).await?;
        Some(previous)
    } else {
        None
    };
    // A sidecar still named after the target now belongs to a database that is
    // no longer there.
    for suffix in ["-wal", "-shm"] {
        let stale = sidecar(database_path, suffix);
        if stale.exists() {
            tokio::fs::remove_file(&stale).await.with_context(|| {
                format!("failed to remove the stale sidecar {}", stale.display())
            })?;
        }
    }
    if let Err(error) = tokio::fs::rename(&staging, database_path).await {
        // Put the previous database back, so a failed restore is a no-op rather
        // than a database missing from its path.
        if let Some(previous) = &previous
            && let Err(restore_error) = restore_previous(previous, database_path).await
        {
            tracing::error!(
                error = %restore_error,
                "failed to put the previous database back after a failed restore"
            );
        }
        return Err(error)
            .with_context(|| format!("failed to move {} into place", staging.display()));
    }
    tracing::info!(
        restored_from = %backup.display(),
        restored_to = %database_path.display(),
        previous = %previous.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "none".to_string()),
        "restored a database from a backup"
    );
    let cleared_leases = clear_instance_leases(database_path).await?;
    Ok(RestoreOutcome {
        restored_from: backup.to_path_buf(),
        restored_to: database_path.to_path_buf(),
        previous,
        cleared_leases,
        report,
    })
}

/// Compare the migrations this binary ships with the ones the database has.
///
/// Connects without migrating and never writes: this is the pre-flight check a
/// deploy runs before stopping the running gateway.
pub(crate) async fn migration_report(
    config: &crate::Config,
) -> anyhow::Result<crate::models::MigrationReport> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let path = database_path(config);
    let migrator = sqlx::migrate!("./migrations");
    let mut report = crate::models::MigrationReport {
        database: path.to_string_lossy().replace('\\', "/"),
        up_to_date: false,
        applied: Vec::new(),
        pending: migrator
            .iter()
            .map(|migration| crate::models::MigrationEntry {
                version: migration.version,
                description: migration.description.to_string(),
            })
            .collect(),
        unknown_applied: Vec::new(),
        data_present: false,
        backup_would_be_written: false,
        problems: Vec::new(),
    };

    if !path.exists() {
        // Do not create the file: opening a missing SQLite path creates an
        // empty database, and a pre-flight check must not have side effects.
        report.up_to_date = report.pending.is_empty();
        return Ok(report);
    }

    let url = format!("sqlite://{}", path.to_string_lossy().replace('\\', "/"));
    let options = SqliteConnectOptions::from_str(&url)?
        .read_only(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open {} read-only", path.display()))?;

    let applied = sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    report.applied = migrator
        .iter()
        .filter(|migration| applied.contains(&migration.version))
        .map(|migration| crate::models::MigrationEntry {
            version: migration.version,
            description: migration.description.to_string(),
        })
        .collect();
    report
        .pending
        .retain(|migration| !applied.contains(&migration.version));
    report.unknown_applied = applied
        .iter()
        .filter(|version| {
            !migrator
                .iter()
                .any(|migration| migration.version == **version)
        })
        .copied()
        .collect();
    report.unknown_applied.sort_unstable();
    report.data_present = holds_user_data(&pool).await.unwrap_or(false);
    report.backup_would_be_written = report.data_present && !report.pending.is_empty();
    report.up_to_date = report.pending.is_empty();
    if !report.unknown_applied.is_empty() {
        let versions = report
            .unknown_applied
            .iter()
            .map(|version| version.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        report.problems.push(format!(
            "the database was migrated by a newer build (unknown versions: {versions}); running this build would be a downgrade"
        ));
    }
    pool.close().await;
    Ok(report)
}
