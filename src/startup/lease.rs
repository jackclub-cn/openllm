use super::*;

/// Default lease TTL, in seconds. A crashed instance's lease is ignored after
/// this long even if the pid check cannot decide.
pub(crate) const DEFAULT_INSTANCE_LEASE_TTL_SECS: i64 = 300;
/// How often the running instance refreshes its lease.
pub(crate) const INSTANCE_HEARTBEAT_INTERVAL_SECS: u64 = 60;

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
    parse_lease_ttl_secs(
        std::env::var("OPENLLM_INSTANCE_LEASE_TTL_SECS")
            .ok()
            .as_deref(),
    )
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
/// after a crash. Linux reads the state from `/proc` and Windows waits on the
/// process handle; macOS has no equivalent cheap probe — `proc_pidinfo` returns
/// nothing at all for a zombie — so there an unreaped instance counts as alive
/// and its lease is freed by the TTL instead.
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
/// Read from `/proc` on Linux, where the state field is available.
#[cfg(target_os = "linux")]
pub(super) fn is_zombie(pid: i64) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // Format: `pid (comm) state ...`; `comm` may contain spaces and brackets, so
    // the state is the field after the last ')'.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|state| state == "Z")
}

/// Without `/proc` (macOS, the other BSDs) the answer stays unknown, so the pid
/// counts as alive.
///
/// That errs toward "alive" on purpose: calling a live instance gone would let a
/// second process serve the same SQLite file. The cost is only that a crashed
/// instance which is still an unreaped zombie keeps its lease until the TTL
/// expires, which `claim_instance_lease` already handles.
#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn is_zombie(_pid: i64) -> bool {
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
pub(super) fn multi_instance_refusal(lease: &ForeignLease) -> anyhow::Error {
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
    sqlx::query(
        "DELETE FROM instance_leases WHERE heartbeat_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
    )
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
