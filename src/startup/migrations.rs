use super::*;
use sha2::{Digest, Sha384};

/// Prefix of a pre-migration backup file, used for pruning.
pub(crate) const MIGRATION_BACKUP_PREFIX: &str = "openllm-pre-migration-";
/// How many pre-migration backups to keep.
pub(crate) const MAX_MIGRATION_BACKUPS: usize = 5;

/// A pending migration, as far as the backup logic needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingMigration {
    pub(crate) version: i64,
    pub(crate) description: String,
}

/// Migrations the binary knows about that the database has not applied yet.
pub(crate) async fn pending_migrations(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<Vec<PendingMigration>> {
    let applied = applied_versions(pool).await?;
    Ok(migrator
        .iter()
        .filter(|migration| !applied.contains(&migration.version))
        .map(|migration| PendingMigration {
            version: migration.version,
            description: migration.description.to_string(),
        })
        .collect())
}

pub(super) async fn applied_versions(pool: &SqlitePool) -> anyhow::Result<Vec<i64>> {
    let table_exists: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table_exists.is_none() {
        return Ok(Vec::new());
    }
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
            .fetch_all(pool)
            .await?,
    )
}

/// Normalizes migration checksums written by builds that checked out SQL files
/// with Windows line endings.
///
/// Older Windows builds embedded CRLF migration bytes while Linux and macOS
/// builds embedded LF. SQLx validates the exact bytes before applying a new
/// migration, so a database could otherwise refuse a valid cross-platform
/// upgrade. Only checksums matching the same SQL with either line ending are
/// rewritten; a genuinely edited migration still fails validation.
pub(crate) async fn normalize_legacy_migration_checksums(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<usize> {
    let table_exists: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table_exists.is_none() {
        return Ok(0);
    }

    let rows = sqlx::query_as::<_, (i64, Vec<u8>)>(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = 1",
    )
    .fetch_all(pool)
    .await?;
    let applied = rows
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    let mut transaction = pool.begin().await?;
    let mut normalized = 0;

    for migration in migrator.iter() {
        let Some(stored) = applied.get(&migration.version) else {
            continue;
        };
        let lf_sql = migration.sql.replace("\r\n", "\n").replace('\r', "\n");
        let crlf_sql = lf_sql.replace('\n', "\r\n");
        let lf_checksum = Sha384::digest(lf_sql.as_bytes());
        let crlf_checksum = Sha384::digest(crlf_sql.as_bytes());
        if stored.as_slice() != lf_checksum.as_slice()
            && stored.as_slice() != crlf_checksum.as_slice()
        {
            continue;
        }
        if stored.as_slice() == migration.checksum.as_ref() {
            continue;
        }
        normalized += sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = ? \
             WHERE version = ? AND checksum = ?",
        )
        .bind(migration.checksum.as_ref())
        .bind(migration.version)
        .bind(stored)
        .execute(&mut *transaction)
        .await?
        .rows_affected() as usize;
    }

    transaction.commit().await?;
    if normalized > 0 {
        tracing::warn!(
            migrations = normalized,
            "normalized legacy line-ending migration checksums"
        );
    }
    Ok(normalized)
}

/// Whether the database already holds anything worth protecting.
pub(super) async fn holds_user_data(pool: &SqlitePool) -> anyhow::Result<bool> {
    let user_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
           AND name <> '_sqlx_migrations' AND name <> 'instance_leases'",
    )
    .fetch_one(pool)
    .await?;
    Ok(user_tables > 0)
}

/// Writes a consistent copy of the database before pending migrations run.
///
/// Returns the backup path, or `None` when nothing was pending (the normal
/// restart case) or there is no file to protect (fresh database, in-memory
/// URL). Old backups are pruned so repeated upgrades cannot fill the disk.
pub(crate) async fn backup_before_migrations(
    pool: &SqlitePool,
    database_path: Option<&Path>,
    migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(database_path) = database_path else {
        return Ok(None);
    };
    if !database_path.exists() {
        return Ok(None);
    }
    let pending = pending_migrations(pool, migrator).await?;
    if pending.is_empty() {
        return Ok(None);
    }
    if !holds_user_data(pool).await? {
        return Ok(None);
    }

    let directory = database_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(MIGRATION_BACKUP_DIR);
    tokio::fs::create_dir_all(&directory)
        .await
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let target = if pending.len() > 1 {
        pending
            .iter()
            .map(|migration| migration.version)
            .max()
            .unwrap_or_default()
    } else {
        pending[0].version
    };
    let path = directory.join(format!(
        "{MIGRATION_BACKUP_PREFIX}{target}-{}.db",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    vault_into(pool, &path).await?;
    tracing::info!(
        backup = %path.display(),
        pending = pending.len(),
        target_version = target,
        "wrote a pre-migration backup"
    );
    prune_migration_backups(&directory).await;
    Ok(Some(path))
}

/// Keeps only the newest [`MAX_MIGRATION_BACKUPS`] pre-migration backups.
pub(super) async fn prune_migration_backups(directory: &Path) {
    prune_backups(directory, MIGRATION_BACKUP_PREFIX, MAX_MIGRATION_BACKUPS).await;
}

/// Keep only the newest `keep` backups carrying `prefix`.
pub(super) async fn prune_backups(directory: &Path, prefix: &str, keep: usize) {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return;
    };
    let mut backups: Vec<PathBuf> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(prefix) && name.ends_with(".db") {
            backups.push(entry.path());
        }
    }
    // Timestamps in the name sort chronologically, so the oldest are first.
    backups.sort();
    while backups.len() > keep {
        let oldest = backups.remove(0);
        if let Err(error) = tokio::fs::remove_file(&oldest).await {
            tracing::warn!(%error, path = %oldest.display(), "failed to prune a pre-migration backup");
        }
    }
}
