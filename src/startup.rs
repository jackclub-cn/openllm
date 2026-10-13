//! Startup safety: single-writer protection and pre-migration backups.
//!
//! The gateway stores everything in one SQLite file and is designed as a single
//! instance (SQLite serialises writers, so two processes on the same file do
//! not fail loudly — they corrupt each other's expectations, for example by
//! each keeping its own in-memory cooldowns and pool windows). The schema
//! comment in the README said "do not run two instances"; this module turns
//! that note into a startup check that names the other instance instead of
//! leaving an operator to discover it through duplicated webhook deliveries.
//!
//! Two mechanisms live here:
//!
//! * a **lease** row per running instance, refreshed by a heartbeat and
//!   expiring on a TTL. A fresh foreign lease makes the new process refuse to
//!   start unless the operator explicitly allows it; a lease left behind by a
//!   crashed process on the same host is taken over after checking that its pid
//!   is gone.
//! * a **pre-migration backup**: when a newer binary has pending migrations and
//!   the database already holds data, a consistent `VACUUM INTO` copy is written
//!   next to it first. A failed migration then aborts startup with the backup
//!   path in the error instead of leaving an operator to recover by hand.

use std::path::{Path, PathBuf};

use anyhow::Context;
use sqlx::SqlitePool;

/// Default lease TTL, in seconds. A crashed instance's lease is ignored after
/// this long even if the pid check cannot decide.
pub(crate) const DEFAULT_INSTANCE_LEASE_TTL_SECS: i64 = 300;
/// How often the running instance refreshes its lease.
pub(crate) const INSTANCE_HEARTBEAT_INTERVAL_SECS: u64 = 60;
/// Directory (relative to the database file) holding pre-migration backups.
pub(crate) const MIGRATION_BACKUP_DIR: &str = "backups";
/// Prefix of a pre-migration backup file, used for pruning.
pub(crate) const MIGRATION_BACKUP_PREFIX: &str = "openllm-pre-migration-";
/// How many pre-migration backups to keep.
pub(crate) const MAX_MIGRATION_BACKUPS: usize = 5;
/// Prefix of an operator-triggered backup, distinct from the automatic
/// pre-migration copy so pruning can treat them independently.
pub(crate) const MANUAL_BACKUP_PREFIX: &str = "openllm-manual-";
/// How many on-demand backups to keep.
pub(crate) const MAX_MANUAL_BACKUPS: usize = 5;
/// Suffix of the file the previous database is moved to during a restore.
pub(crate) const PRE_RESTORE_SUFFIX: &str = "db.pre-restore";
/// Tables a restorable OpenLLM database must contain. A file missing any of
/// them is either not an OpenLLM database or too old to swap in silently.
pub(crate) const REQUIRED_TABLES: &[&str] = &[
    "_sqlx_migrations",
    "api_keys",
    "provider_models",
    "providers",
    "route_targets",
    "routes",
    "webhooks",
];

/// Bootstraps the lease table.
///
/// Run before migrations on purpose: a second instance must be detected before
/// it is allowed to migrate the schema, and the check needs the table to exist.
/// The same DDL is repeated in a migration so the schema also appears in the
/// migration lineage of a database created by an older binary.
pub(crate) const INSTANCE_LEASE_DDL: &str = "\
CREATE TABLE IF NOT EXISTS instance_leases (
    id TEXT PRIMARY KEY,
    pid INTEGER NOT NULL,
    host TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    heartbeat_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
)";

/// What to do when another live instance already holds the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MultiInstancePolicy {
    /// Refuse to start (default). Two writers on one SQLite file is not a
    /// supported deployment, and failing fast is the only safe answer.
    Refuse,
    /// Start anyway with a warning. For the operator who knows the other
    /// lease is wrong (for example a stale row from a host that was rebuilt).
    Warn,
}

impl MultiInstancePolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::Warn => "warn",
        }
    }
}

/// Parses `OPENLLM_MULTI_INSTANCE`.
///
/// Anything other than an explicit `warn`/`allow`/`1` keeps the safe default,
/// so a typo cannot silently disable the check.
pub(crate) fn parse_multi_instance_policy(value: Option<&str>) -> MultiInstancePolicy {
    match value.map(str::trim) {
        Some(raw)
            if matches!(
                raw.to_ascii_lowercase().as_str(),
                "warn" | "allow" | "1" | "true" | "yes"
            ) =>
        {
            MultiInstancePolicy::Warn
        }
        _ => MultiInstancePolicy::Refuse,
    }
}

pub(crate) fn multi_instance_policy() -> MultiInstancePolicy {
    parse_multi_instance_policy(std::env::var("OPENLLM_MULTI_INSTANCE").ok().as_deref())
}

/// Parses the lease TTL, clamped to a sane range.
pub(crate) fn parse_lease_ttl_secs(value: Option<&str>) -> i64 {
    value
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|secs| (30..=86_400).contains(secs))
        .unwrap_or(DEFAULT_INSTANCE_LEASE_TTL_SECS)
}

pub(crate) fn lease_ttl_secs() -> i64 {
    parse_lease_ttl_secs(std::env::var("OPENLLM_INSTANCE_LEASE_TTL_SECS").ok().as_deref())
}

/// How often to refresh the lease.
///
/// Derived from the TTL instead of being a fixed 60s: an operator may lower the
/// TTL below the heartbeat interval, and a lease that expires between two
/// heartbeats would make a healthy instance look crashed to the next starter.
pub(crate) fn heartbeat_interval_secs(ttl_secs: i64) -> u64 {
    (ttl_secs / 3).clamp(5, INSTANCE_HEARTBEAT_INTERVAL_SECS as i64) as u64
}

pub(crate) fn lease_heartbeat_interval_secs() -> u64 {
    heartbeat_interval_secs(lease_ttl_secs())
}

/// A lease held by another instance that is still inside its TTL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignLease {
    pub(crate) id: String,
    pub(crate) pid: i64,
    pub(crate) host: String,
    /// Age of the last heartbeat, in seconds.
    pub(crate) heartbeat_age_secs: i64,
    /// Whether the recorded pid is provably gone on this host. A stale pid on
    /// the same host is taken over instead of blocking a restart.
    pub(crate) pid_gone: bool,
}

impl ForeignLease {
    /// Readable description used in logs and the startup error.
    pub(crate) fn describe(&self) -> String {
        format!(
            "instance {} (pid {} on {}) last checked in {}s ago",
            self.id, self.pid, self.host, self.heartbeat_age_secs
        )
    }
}

/// This machine's name, as recorded in the lease.
pub(crate) fn host_name() -> String {
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return value;
        }
    }
    "unknown".to_string()
}

/// Checks whether a pid is running, where the platform allows it cheaply.
///
/// `None` means "cannot tell", which callers must treat as *alive*: refusing to
/// start is recoverable, corrupting a shared database is not.
///
/// Both platforms have the same trap: a terminated process is not gone from the
/// kernel's point of view until its parent reaps it (a zombie). Existence alone
/// therefore reports a killed instance as alive, which would block the restart
/// after a crash. On Linux the process state is checked instead, and on Windows
/// the handle is waited on.
#[cfg(unix)]
pub(crate) fn pid_is_running(pid: i64) -> Option<bool> {
    if pid <= 0 {
        return Some(false);
    }
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering anything; the arguments are plain integers.
    let alive = unsafe { libc_kill(pid as i32, 0) } == 0;
    if !alive {
        return Some(false);
    }
    Some(!is_zombie(pid))
}

/// Whether a live pid is a zombie (exited but not reaped by its parent).
///
/// Read from `/proc` on Linux, where the state field is available; elsewhere
/// the answer stays unknown, so the pid counts as alive.
#[cfg(target_os = "linux")]
fn is_zombie(pid: i64) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // Format: `pid (comm) state ...`; `comm` may contain spaces and brackets, so
    // the state is the field after the last ')'.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state == "Z")
}

#[cfg(all(unix, not(target_os = "linux")))]
fn is_zombie(_pid: i64) -> bool {
    false
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, signal: i32) -> i32;
}

#[cfg(windows)]
pub(crate) fn pid_is_running(pid: i64) -> Option<bool> {
    if pid <= 0 {
        return Some(false);
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    // SAFETY: `OpenProcess` only inspects the process table; a null handle
    // means the pid does not exist (or is not accessible). The returned handle
    // is closed on every path, and waiting with a zero timeout never blocks.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid as u32);
        if handle.is_null() {
            return Some(false);
        }
        let status = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        match status {
            // Signalled: the process has exited, even if a handle (for example
            // this one) keeps its object alive.
            WAIT_OBJECT_0 => Some(false),
            WAIT_TIMEOUT => Some(true),
            _ => None,
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn pid_is_running(_pid: i64) -> Option<bool> {
    None
}

/// The refusal shown when another live instance holds the database.
fn multi_instance_refusal(lease: &ForeignLease) -> anyhow::Error {
    anyhow::anyhow!(
        "another OpenLLM instance is already serving this database: {}. \
         SQLite supports a single writer, so this process will not start. \
         Stop the other instance, or set OPENLLM_MULTI_INSTANCE=warn to \
         start anyway (unsupported).",
        lease.describe()
    )
}

/// Claims this database for `instance_id`, refusing when another live instance
/// already holds it.
///
/// Returns the foreign lease that was found but tolerated (only possible under
/// [`MultiInstancePolicy::Warn`]) so the caller can log it. Expired leases, and
/// leases whose pid is provably gone on this host, are taken over and removed.
pub(crate) async fn claim_instance_lease(
    pool: &SqlitePool,
    instance_id: &str,
    policy: MultiInstancePolicy,
    ttl_secs: i64,
) -> anyhow::Result<Option<ForeignLease>> {
    sqlx::raw_sql(INSTANCE_LEASE_DDL)
        .execute(pool)
        .await
        .context("failed to create the instance lease table")?;

    let ttl_secs = ttl_secs.max(1);
    let cutoff = format!("-{ttl_secs} seconds");
    let rows = sqlx::query_as::<_, (String, i64, String, i64)>(
        "SELECT id, pid, host, \
                CAST(strftime('%s', 'now') - strftime('%s', heartbeat_at) AS INTEGER) AS age \
         FROM instance_leases \
         WHERE id <> ? AND heartbeat_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
    )
    .bind(instance_id)
    .bind(&cutoff)
    .fetch_all(pool)
    .await
    .context("failed to read the instance lease table")?;

    let host = host_name();
    let mut blocking: Option<ForeignLease> = None;
    for (id, pid, lease_host, age) in rows {
        let pid_gone = lease_host == host && pid_is_running(pid) == Some(false);
        if pid_gone {
            tracing::warn!(
                stale_instance = %id,
                pid,
                host = %lease_host,
                "taking over the database from an instance whose process is gone"
            );
            sqlx::query("DELETE FROM instance_leases WHERE id = ?")
                .bind(&id)
                .execute(pool)
                .await?;
            continue;
        }
        let candidate = ForeignLease {
            id,
            pid,
            host: lease_host,
            heartbeat_age_secs: age.max(0),
            pid_gone,
        };
        if blocking.is_none() {
            blocking = Some(candidate);
        }
    }

    if let Some(foreign) = &blocking
        && policy == MultiInstancePolicy::Refuse
    {
        return Err(multi_instance_refusal(foreign));
    }

    // The advisory read above settles the common case; this statement is what
    // makes the claim atomic. Two processes starting at the same instant can
    // both find an empty table, and a plain SELECT-then-INSERT would let both
    // serve the same file: `NOT EXISTS` is evaluated inside the statement that
    // takes the write lock, so only one of them can win. Under `Warn` the
    // insert is unconditional, which is the point of that policy.
    let claimed = sqlx::query(
        "INSERT INTO instance_leases (id, pid, host) \
         SELECT ?, ?, ? WHERE ? = 1 OR NOT EXISTS ( \
             SELECT 1 FROM instance_leases \
             WHERE id <> ? AND heartbeat_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?) \
         ) \
         ON CONFLICT(id) DO UPDATE SET pid = excluded.pid, host = excluded.host, \
             heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(instance_id)
    .bind(i64::from(std::process::id()))
    .bind(&host)
    .bind(i64::from(policy == MultiInstancePolicy::Warn))
    .bind(instance_id)
    .bind(&cutoff)
    .execute(pool)
    .await
    .context("failed to record the instance lease")?
    .rows_affected();

    if claimed == 0 {
        // Another process took the lease in the window between the read above
        // and this insert.
        let holder = sqlx::query_as::<_, (String, i64, String, i64)>(
            "SELECT id, pid, host, \
                    CAST(strftime('%s', 'now') - strftime('%s', heartbeat_at) AS INTEGER) AS age \
             FROM instance_leases \
             WHERE id <> ? AND heartbeat_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
        )
        .bind(instance_id)
        .bind(&cutoff)
        .fetch_optional(pool)
        .await
        .context("failed to read the instance lease that won the race")?;
        let lease = holder.map(|(id, pid, host, age)| ForeignLease {
            id,
            pid,
            host,
            heartbeat_age_secs: age.max(0),
            pid_gone: false,
        });
        return Err(match lease {
            Some(lease) => multi_instance_refusal(&lease),
            None => anyhow::anyhow!(
                "another OpenLLM instance claimed this database while starting; retry the start"
            ),
        });
    }

    // Remove anything that has already expired so the table stays tiny.
    sqlx::query("DELETE FROM instance_leases WHERE heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)")
        .bind(&cutoff)
        .execute(pool)
        .await?;

    Ok(blocking)
}

/// Refreshes this instance's lease.
pub(crate) async fn heartbeat(pool: &SqlitePool, instance_id: &str) -> anyhow::Result<()> {
    let result = sqlx::query(
        "UPDATE instance_leases \
         SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
    .bind(instance_id)
    .execute(pool)
    .await
    .context("failed to refresh the instance lease")?;
    if result.rows_affected() == 0 {
        // The row was pruned (for example the clock jumped); claim it again so
        // the next start does not see a stale lease and refuse.
        sqlx::query("INSERT INTO instance_leases (id, pid, host) VALUES (?, ?, ?)")
            .bind(instance_id)
            .bind(i64::from(std::process::id()))
            .bind(host_name())
            .execute(pool)
            .await
            .context("failed to re-record the instance lease")?;
    }
    Ok(())
}

/// Drops this instance's lease on a clean shutdown.
pub(crate) async fn release_instance_lease(pool: &SqlitePool, instance_id: &str) {
    if let Err(error) = sqlx::query("DELETE FROM instance_leases WHERE id = ?")
        .bind(instance_id)
        .execute(pool)
        .await
    {
        // A stale lease is harmless (it expires), so this never fails shutdown.
        tracing::warn!(%error, "failed to release the instance lease");
    }
}

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

async fn applied_versions(pool: &SqlitePool) -> anyhow::Result<Vec<i64>> {
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

/// Whether the database already holds anything worth protecting.
async fn holds_user_data(pool: &SqlitePool) -> anyhow::Result<bool> {
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
async fn prune_migration_backups(directory: &Path) {
    prune_backups(directory, MIGRATION_BACKUP_PREFIX, MAX_MIGRATION_BACKUPS).await;
}

/// Keep only the newest `keep` backups carrying `prefix`.
async fn prune_backups(directory: &Path, prefix: &str, keep: usize) {
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
async fn vault_into(pool: &SqlitePool, path: &Path) -> anyhow::Result<()> {
    let path_string = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "''");
    sqlx::raw_sql(&format!("VACUUM INTO '{path_string}'"))
        .execute(pool)
        .await
        .with_context(|| format!("failed to back up the database to {}", path.display()))?;
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
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Move `<db>-wal` / `<db>-shm` (when present) to the sidecar names of `target`.
///
/// Used to keep a database and its write-ahead log together: leaving a WAL
/// behind under the original name would make SQLite replay it onto whatever
/// file is moved there next.
async fn move_sidecars(from: &Path, to: &Path) -> anyhow::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let source = sidecar(from, suffix);
        if !source.exists() {
            continue;
        }
        let destination = sidecar(to, suffix);
        if destination.exists() {
            tokio::fs::remove_file(&destination).await.ok();
        }
        tokio::fs::rename(&source, &destination).await.with_context(|| {
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
async fn restore_previous(previous: &Path, database_path: &Path) -> anyhow::Result<()> {
    let _ = tokio::fs::remove_file(database_path).await;
    tokio::fs::rename(previous, database_path).await.with_context(|| {
        format!(
            "failed to move {} back to {}",
            previous.display(),
            database_path.display()
        )
    })?;
    move_sidecars(previous, database_path).await
}

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
            if has("_sqlx_migrations") {                report.applied_migrations = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM _sqlx_migrations",
                )
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
async fn table_count(pool: &SqlitePool, table: &str) -> i64 {
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
    let looks_like_a_path = trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..");
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
async fn clear_instance_leases(database_path: &Path) -> anyhow::Result<u64> {
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
    tokio::fs::copy(backup, &staging)
        .await
        .with_context(|| format!("failed to copy {} to {}", backup.display(), staging.display()))?;

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
                format!("failed to set the current database aside at {}", previous.display())
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
    report.pending.retain(|migration| !applied.contains(&migration.version));
    report.unknown_applied = applied
        .iter()
        .filter(|version| !migrator.iter().any(|migration| migration.version == **version))
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

#[cfg(test)]
#[path = "../tests/unit/startup.rs"]
mod tests;
