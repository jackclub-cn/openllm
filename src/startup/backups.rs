use super::*;

/// Directory (relative to the database file) holding all backup snapshots.
pub(crate) const MIGRATION_BACKUP_DIR: &str = "backups";
/// Prefix of an operator-triggered backup, distinct from the automatic copy.
pub(crate) const MANUAL_BACKUP_PREFIX: &str = "openllm-manual-";
/// How many on-demand backups to keep.
pub(crate) const MAX_MANUAL_BACKUPS: usize = 5;

/// Extracts the on-disk path of a SQLite URL, when it points at a file.
///
/// `None` for in-memory databases, non-file URLs, and URLs with parameters such
/// as `?mode=ro`, because the pre-migration backup must never guess a wrong
/// path.
pub(crate) fn sqlite_file_path(database_url: &str) -> Option<PathBuf> {
    let rest = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?;
    // `?mode=memory` marks an in-memory database that only looks like a file,
    // so it has to be checked before the query string is stripped.
    if rest.contains("mode=memory") {
        return None;
    }
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() || rest == ":memory:" {
        return None;
    }
    Some(PathBuf::from(rest))
}

/// Resolve the SQLite file this process would serve, mirroring
/// `Database::connect` so an operator command and the server never disagree
/// about which file is "the database".
pub(crate) fn database_path(config: &crate::Config) -> PathBuf {
    config
        .database_url
        .as_deref()
        .and_then(sqlite_file_path)
        .unwrap_or_else(|| config.data_dir.join("openllm.db"))
}

/// Directory holding this database's backups.
pub(crate) fn backups_directory(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(MIGRATION_BACKUP_DIR)
}

/// `VACUUM INTO` a consistent single-file copy.
///
/// The path is escaped because it is interpolated into a SQL literal: a
/// directory name containing an apostrophe would otherwise break the statement.
pub(super) async fn vault_into(pool: &SqlitePool, path: &Path) -> anyhow::Result<()> {
    let path_string = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "''");
    sqlx::raw_sql(&format!("VACUUM INTO '{path_string}'"))
        .execute(pool)
        .await
        .with_context(|| format!("failed to back up the database to {}", path.display()))?;
    crate::secure_file::set_private_permissions(path)
        .with_context(|| format!("failed to secure backup {}", path.display()))?;
    Ok(())
}

/// Write an on-demand backup and return its path.
///
/// `VACUUM INTO` is used rather than copying the file because the live database
/// is in WAL mode: a byte copy can capture a half-checkpointed database, while
/// `VACUUM INTO` produces one consistent file with no `-wal`/`-shm` siblings.
pub(crate) async fn create_backup(
    pool: &SqlitePool,
    database_path: &Path,
) -> anyhow::Result<PathBuf> {
    let directory = backups_directory(database_path);
    tokio::fs::create_dir_all(&directory)
        .await
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let path = directory.join(format!(
        "{MANUAL_BACKUP_PREFIX}{}.db",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    vault_into(pool, &path).await?;
    prune_backups(&directory, MANUAL_BACKUP_PREFIX, MAX_MANUAL_BACKUPS).await;
    tracing::info!(backup = %path.display(), "wrote an on-demand backup");
    Ok(path)
}

/// Backups available for a restore, newest first.
///
/// Returns an empty list (not an error) when the directory does not exist yet:
/// "there are no backups" is a normal answer for the console to render.
pub(crate) async fn list_backups(
    database_path: &Path,
) -> anyhow::Result<Vec<crate::models::BackupInfo>> {
    let directory = backups_directory(database_path);
    let mut entries = match tokio::fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    let mut backups = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("db") {
            continue;
        }
        let metadata = entry.metadata().await?;
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let kind = if name.starts_with(MIGRATION_BACKUP_PREFIX) {
            "pre-migration"
        } else {
            "manual"
        };
        let created_at = metadata
            .modified()
            .map(|modified| chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339())
            .unwrap_or_default();
        backups.push(crate::models::BackupInfo {
            name: name.to_string(),
            path: path.to_string_lossy().replace('\\', "/"),
            size_bytes: metadata.len(),
            created_at,
            kind,
        });
    }
    // Names start with a sortable timestamp, so a plain reverse sort is
    // newest-first without touching the clock again.
    backups.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(backups)
}

/// `<db>-wal` / `<db>-shm`, i.e. the sidecar files a stale connection left behind.
pub(super) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Move `<db>-wal` / `<db>-shm` (when present) to the sidecar names of `target`.
///
/// Used to keep a database and its write-ahead log together: leaving a WAL
/// behind under the original name would make SQLite replay it onto whatever
/// file is moved there next.
pub(super) async fn move_sidecars(from: &Path, to: &Path) -> anyhow::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let source = sidecar(from, suffix);
        if !source.exists() {
            continue;
        }
        let destination = sidecar(to, suffix);
        if destination.exists() {
            tokio::fs::remove_file(&destination).await.ok();
        }
        tokio::fs::rename(&source, &destination)
            .await
            .with_context(|| {
                format!(
                    "failed to move the sidecar {} to {}",
                    source.display(),
                    destination.display()
                )
            })?;
    }
    Ok(())
}

/// Put a set-aside database (and its sidecars) back where it came from.
pub(super) async fn restore_previous(previous: &Path, database_path: &Path) -> anyhow::Result<()> {
    let _ = tokio::fs::remove_file(database_path).await;
    tokio::fs::rename(previous, database_path)
        .await
        .with_context(|| {
            format!(
                "failed to move {} back to {}",
                previous.display(),
                database_path.display()
            )
        })?;
    move_sidecars(previous, database_path).await
}
