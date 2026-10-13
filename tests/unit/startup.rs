use super::*;

use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(INSTANCE_LEASE_DDL).execute(&pool).await.unwrap();
    pool
}

async fn file_pool(path: &Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
        )
        .await
        .unwrap()
}

async fn lease_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM instance_leases")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[test]
fn multi_instance_policy_defaults_to_refusing() {
    assert_eq!(parse_multi_instance_policy(None), MultiInstancePolicy::Refuse);
    assert_eq!(
        parse_multi_instance_policy(Some("")),
        MultiInstancePolicy::Refuse
    );
    assert_eq!(
        parse_multi_instance_policy(Some("refuse")),
        MultiInstancePolicy::Refuse
    );
    // A typo must not silently disable the single-writer check.
    assert_eq!(
        parse_multi_instance_policy(Some("yes-please")),
        MultiInstancePolicy::Refuse
    );
    for value in ["warn", " WARN ", "allow", "1", "true"] {
        assert_eq!(
            parse_multi_instance_policy(Some(value)),
            MultiInstancePolicy::Warn,
            "{value} must opt into the warning mode"
        );
    }
}

#[test]
fn lease_ttl_parsing_stays_in_a_sane_range() {
    assert_eq!(parse_lease_ttl_secs(None), DEFAULT_INSTANCE_LEASE_TTL_SECS);
    assert_eq!(parse_lease_ttl_secs(Some("oops")), DEFAULT_INSTANCE_LEASE_TTL_SECS);
    assert_eq!(parse_lease_ttl_secs(Some("0")), DEFAULT_INSTANCE_LEASE_TTL_SECS);
    assert_eq!(parse_lease_ttl_secs(Some("29")), DEFAULT_INSTANCE_LEASE_TTL_SECS);
    assert_eq!(parse_lease_ttl_secs(Some("600")), 600);
    assert_eq!(parse_lease_ttl_secs(Some("999999")), DEFAULT_INSTANCE_LEASE_TTL_SECS);
}

#[test]
fn sqlite_urls_resolve_to_a_file_path_only_when_they_are_one() {
    assert_eq!(
        sqlite_file_path("sqlite://C:/data/openllm.db"),
        Some(PathBuf::from("C:/data/openllm.db"))
    );
    assert_eq!(
        sqlite_file_path("sqlite:///var/lib/openllm/openllm.db"),
        Some(PathBuf::from("/var/lib/openllm/openllm.db"))
    );
    assert_eq!(
        sqlite_file_path("sqlite:/data/openllm.db"),
        Some(PathBuf::from("/data/openllm.db"))
    );
    assert_eq!(sqlite_file_path("sqlite::memory:"), None);
    assert_eq!(sqlite_file_path("sqlite://file:memdb1?mode=memory"), None);
    assert_eq!(sqlite_file_path("postgres://localhost/db"), None);
}

/// A live second instance must be refused with a message that identifies it,
/// and a clean shutdown must hand the database over.
#[tokio::test]
async fn a_live_second_instance_is_refused_until_the_first_releases() {
    let pool = memory_pool().await;
    assert!(
        claim_instance_lease(&pool, "instance-a", MultiInstancePolicy::Refuse, 300)
            .await
            .unwrap()
            .is_none(),
        "the first instance takes the database"
    );

    let error = claim_instance_lease(&pool, "instance-b", MultiInstancePolicy::Refuse, 300)
        .await
        .expect_err("a second instance must not start");
    let message = error.to_string();
    assert!(
        message.contains("instance-a") && message.contains("will not start"),
        "the refusal must name the holder and the consequence, got: {message}"
    );

    // The explicit override is tolerated but reported, so the operator can see
    // which instance is sharing the file.
    let tolerated = claim_instance_lease(&pool, "instance-b", MultiInstancePolicy::Warn, 300)
        .await
        .unwrap()
        .expect("warn mode reports the foreign lease instead of failing");
    assert_eq!(tolerated.id, "instance-a");
    assert!(tolerated.heartbeat_age_secs <= 5);
    assert!(!tolerated.pid_gone, "the first instance is running here");

    release_instance_lease(&pool, "instance-a").await;
    release_instance_lease(&pool, "instance-b").await;
    assert!(
        claim_instance_lease(&pool, "instance-c", MultiInstancePolicy::Refuse, 300)
            .await
            .unwrap()
            .is_none(),
        "after a clean release the next instance may start"
    );
}

/// A crash leaves a lease behind. It must not block the restart: a lease whose
/// pid is gone on this host is taken over, and a lease older than the TTL is
/// ignored even when its pid looks alive.
#[tokio::test]
async fn crashed_and_expired_leases_are_taken_over() {
    let pool = memory_pool().await;

    // A pid that cannot be running (out of range on every supported platform).
    sqlx::query("INSERT INTO instance_leases (id, pid, host) VALUES ('ghost', ?, ?)")
        .bind(4_294_967_290_i64)
        .bind(host_name())
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        claim_instance_lease(&pool, "restart", MultiInstancePolicy::Refuse, 300)
            .await
            .unwrap()
            .is_none(),
        "a dead pid on this host must be taken over"
    );
    let ghost: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_leases WHERE id = 'ghost'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ghost, 0, "the taken-over lease is removed");
    // Hand the database back before probing the next case, so the only lease
    // left is the one under test.
    release_instance_lease(&pool, "restart").await;

    // A lease from another host keeps its authority even if the pid means
    // nothing here: we cannot prove that host's process is gone.
    sqlx::query("INSERT INTO instance_leases (id, pid, host) VALUES ('remote', 1, 'other-host')")
        .execute(&pool)
        .await
        .unwrap();
    let error = claim_instance_lease(&pool, "restart2", MultiInstancePolicy::Refuse, 300)
        .await
        .expect_err("a lease from another host must still block");
    assert!(error.to_string().contains("remote"), "got: {error}");

    // An expired lease is ignored even though its pid is alive (this process).
    sqlx::query(
        "INSERT INTO instance_leases (id, pid, host, heartbeat_at) \
         VALUES ('expired', ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-3600 seconds'))",
    )
    .bind(i64::from(std::process::id()))
    .bind(host_name())
    .execute(&pool)
    .await
    .unwrap();
    release_instance_lease(&pool, "remote").await;
    assert!(
        claim_instance_lease(&pool, "restart3", MultiInstancePolicy::Refuse, 300)
            .await
            .unwrap()
            .is_none(),
        "a lease past its TTL must not block a restart"
    );
    assert_eq!(lease_count(&pool).await, 1, "expired rows are pruned");
}

/// An ordinary restart must not create backups; a restart that has migrations
/// to run must, and the copy has to be a usable database.
#[tokio::test]
async fn pre_migration_backup_is_written_only_when_migrations_are_pending() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let pool = file_pool(&path).await;
    let migrator = sqlx::migrate!("./migrations");
    migrator.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'p', 'openai', 'http://upstream')",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        backup_before_migrations(&pool, Some(&path), &migrator)
            .await
            .unwrap()
            .is_none(),
        "nothing is pending, so an ordinary restart takes no backup"
    );

    // Pretend the newest migration has not run yet.
    let highest: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ?")
        .bind(highest)
        .execute(&pool)
        .await
        .unwrap();
    let pending = pending_migrations(&pool, &migrator).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].version, highest);

    let backup = backup_before_migrations(&pool, Some(&path), &migrator)
        .await
        .unwrap()
        .expect("a pending migration must produce a backup");
    assert!(backup.exists());
    assert!(std::fs::read(&backup).unwrap().starts_with(b"SQLite format 3\0"));
    // The copy must be a working database with the real data in it.
    let restored = file_pool(&backup).await;
    let providers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM providers")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(providers, 1);

    // Pruning keeps the newest MAX_MIGRATION_BACKUPS files.
    let backups_dir = directory.path().join(MIGRATION_BACKUP_DIR);
    for index in 0..(MAX_MIGRATION_BACKUPS + 3) {
        tokio::fs::write(
            backups_dir.join(format!("{MIGRATION_BACKUP_PREFIX}44-2026010{index}-000000.db")),
            b"x",
        )
        .await
        .unwrap();
    }
    prune_migration_backups(&backups_dir).await;
    let mut remaining = std::fs::read_dir(&backups_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert_eq!(remaining.len(), MAX_MIGRATION_BACKUPS);
    remaining.sort();
    for pruned in 0..3 {
        assert!(
            !remaining
                .iter()
                .any(|name| name.contains(&format!("2026010{pruned}-"))),
            "the oldest backups must be pruned, still have {remaining:?}"
        );
    }
    assert!(
        remaining
            .iter()
            .any(|name| name.contains(&format!("2026010{}", MAX_MIGRATION_BACKUPS + 2))),
        "the newest backups must survive: {remaining:?}"
    );
}

/// A migration that cannot run must abort startup: no listener, no partial
/// schema, and the data still readable from the pre-migration backup.
///
/// Also: the lease row is written before migrations run, so a database that
/// only holds the lease table is still "empty" and must not trigger a
/// useless backup on the first boot of a fresh volume.
#[tokio::test]
async fn a_fresh_database_with_only_the_lease_table_needs_no_backup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let pool = file_pool(&path).await;
    sqlx::raw_sql(INSTANCE_LEASE_DDL).execute(&pool).await.unwrap();
    let migrator = sqlx::migrate!("./migrations");
    let pending = pending_migrations(&pool, &migrator).await.unwrap();
    assert!(
        !pending.is_empty(),
        "a fresh database has every migration pending"
    );

    assert!(
        backup_before_migrations(&pool, Some(&path), &migrator)
            .await
            .unwrap()
            .is_none(),
        "a fresh database has no user data to protect"
    );
    assert!(
        !directory.path().join(MIGRATION_BACKUP_DIR).exists(),
        "no backup directory should be created"
    );
}

#[tokio::test]
async fn a_failed_migration_aborts_startup_and_keeps_the_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let pool = file_pool(&path).await;
    let migrator = sqlx::migrate!("./migrations");
    migrator.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'precious', 'openai', 'http://upstream')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Migration 43 is `ALTER TABLE ... ADD COLUMN`, which fails when replayed.
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 43")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let url = format!("sqlite://{}", path.to_string_lossy().replace('\\', "/"));
    let config = crate::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: directory.path().to_path_buf(),
        database_url: Some(url.clone()),
        admin_token: None,
    };
    let message = match crate::db::Database::connect(&config).await {
        Ok(_) => panic!("a failing migration must stop startup"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        message.contains("migration failed"),
        "the error must say the migration failed, got: {message}"
    );
    assert!(
        message.contains("pre-migration backup"),
        "the error must point at the backup, got: {message}"
    );

    // The data is untouched and the failed migration is not recorded, so the
    // fixed binary can run it later.
    let pool = file_pool(&path).await;
    let name: String = sqlx::query_scalar("SELECT name FROM providers WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "precious");
    let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 43")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(recorded, 0, "a failed migration must not be recorded as applied");
    assert_eq!(
        lease_count(&pool).await,
        0,
        "the aborted instance must not keep a lease that blocks the retry"
    );

    // And the backup it promised really exists and holds the data.
    let mut backups = std::fs::read_dir(directory.path().join(MIGRATION_BACKUP_DIR)).unwrap();
    let entry = backups.next().expect("a backup was written").unwrap();
    let restored = file_pool(&entry.path()).await;
    let restored_name: String = sqlx::query_scalar("SELECT name FROM providers WHERE id = 1")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(restored_name, "precious");
}

/// A terminated process that its parent has not reaped yet must still count as
/// gone: otherwise the lease it left behind would block the restart after a
/// crash, which is the case this check exists for.
#[test]
fn an_exited_child_process_is_not_running() {
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "exit", "0"])
        .spawn()
        .unwrap();
    #[cfg(unix)]
    let mut child = std::process::Command::new("true").spawn().unwrap();

    // Wait for it to finish *without* reaping it: `try_wait` reaps, so poll the
    // pid's liveness until the process has exited, keeping the child alive.
    let pid = i64::from(child.id());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if pid_is_running(pid) == Some(false) {
            // Nothing to reap: the platform already reports it as gone.
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Sampled while the child is still an unreaped zombie, so a failure records
    // the state the platform actually reported.
    #[cfg(target_os = "macos")]
    let observed = macos_process_status(pid);
    let _ = child.kill();
    let _ = child.wait();
    #[cfg(target_os = "macos")]
    panic!("a finished child process must not look alive (macOS pbi_status={observed:?})");
    #[cfg(not(target_os = "macos"))]
    panic!("a finished child process must not look alive");
}

/// The heartbeat must keep the lease fresh, and re-create it if it was pruned.
#[tokio::test]
async fn heartbeat_keeps_and_recreates_the_lease() {
    let pool = memory_pool().await;
    claim_instance_lease(&pool, "beating", MultiInstancePolicy::Refuse, 300)
        .await
        .unwrap();
    heartbeat(&pool, "beating").await.unwrap();
    assert_eq!(lease_count(&pool).await, 1);

    // Simulate a prune (for example a clock jump) and make sure the next beat
    // restores the row instead of silently dropping the protection.
    sqlx::query("DELETE FROM instance_leases")
        .execute(&pool)
        .await
        .unwrap();
    heartbeat(&pool, "beating").await.unwrap();
    assert_eq!(lease_count(&pool).await, 1);
    let age: i64 = sqlx::query_scalar(
        "SELECT CAST(strftime('%s','now') - strftime('%s', heartbeat_at) AS INTEGER) \
         FROM instance_leases WHERE id = 'beating'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(age.abs() < 5, "the recreated lease is fresh, age={age}s");
    let _ = Duration::from_secs(1);
}

/// A restored file must be startable: the lease it carries describes the
/// process that produced the backup, not this one. Regression guard for the
/// disaster-recovery path — a backup restored on another host, or after the
/// recorded pid was reused, used to block startup for the whole lease TTL.
#[tokio::test]
async fn restoring_a_backup_clears_the_instance_lease() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let pool = file_pool(&path).await;
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'p', 'openai', 'http://upstream')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // A lease from another host is exactly what a backup taken before a machine
    // loss contains: its pid can never be probed here, so the takeover path
    // cannot rescue the start.
    sqlx::query(
        "INSERT INTO instance_leases (id, pid, host, started_at, heartbeat_at) \
         VALUES ('other', 1, 'somewhere-else', \
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(lease_count(&pool).await, 1);

    let backup = create_backup(&pool, &path).await.unwrap();
    pool.close().await;

    tokio::fs::remove_file(&path).await.unwrap();
    let outcome = restore_backup(&path, &backup).await.unwrap();
    assert!(outcome.report.ok, "{:?}", outcome.report.problems);
    assert_eq!(outcome.cleared_leases, 1);

    // The restored database keeps its data and has no lease left.
    let restored = file_pool(&path).await;
    let providers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM providers")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(providers, 1);
    assert_eq!(lease_count(&restored).await, 0);

    // And the next start claims it without waiting or refusing.
    claim_instance_lease(&restored, "fresh", MultiInstancePolicy::Refuse, 300)
        .await
        .unwrap();
    assert_eq!(lease_count(&restored).await, 1);
}

/// Regression guard for the claim statement: two instances starting at the same
/// instant used to both find an empty lease table (the read and the write were
/// separate statements) and both begin serving the same SQLite file.
#[tokio::test]
async fn two_instances_cannot_claim_the_lease_at_the_same_instant() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let first = file_pool(&path).await;
    sqlx::migrate!("./migrations").run(&first).await.unwrap();
    sqlx::raw_sql(INSTANCE_LEASE_DDL).execute(&first).await.unwrap();
    // A second connection to the same file, with a busy timeout so the loser
    // waits for the winner's write lock instead of failing on SQLITE_BUSY.
    let second = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap();

    let (a, b) = tokio::join!(
        claim_instance_lease(&first, "instance-a", MultiInstancePolicy::Refuse, 300),
        claim_instance_lease(&second, "instance-b", MultiInstancePolicy::Refuse, 300),
    );
    let winners = [&a, &b].into_iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1, "exactly one instance may take the database");
    let loser = if a.is_err() { &a } else { &b };
    let message = loser.as_ref().unwrap_err().to_string();
    assert!(
        message.contains("instance-") && message.contains("will not start"),
        "the loser must be told who holds the database, got: {message}"
    );
    let held: Vec<String> = sqlx::query_scalar("SELECT id FROM instance_leases")
        .fetch_all(&first)
        .await
        .unwrap();
    assert_eq!(held.len(), 1, "only the winner leaves a lease behind: {held:?}");
}

/// A restore must not run against a live instance. A lease from another host is
/// the case that used to be skipped: its pid cannot be probed here, so treating
/// it as dead let `--restore-backup` write while the other host served the file.
#[tokio::test]
async fn a_recent_lease_from_another_host_still_counts_as_serving() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let pool = file_pool(&path).await;
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO instance_leases (id, pid, host, started_at, heartbeat_at) \
         VALUES ('remote', 4242, 'another-machine', \
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    assert!(
        database_is_served(&path).await.unwrap(),
        "a fresh lease from another host cannot be probed, so it counts as alive"
    );

    // An expired lease from another host is free to be taken over.
    let pool = file_pool(&path).await;
    sqlx::query("UPDATE instance_leases SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(!database_is_served(&path).await.unwrap());
}

/// The heartbeat must stay comfortably inside the lease TTL: a shorter TTL than
/// the interval would let a healthy instance's lease lapse between two beats and
/// be taken over as crashed.
#[test]
fn the_heartbeat_interval_always_stays_inside_the_lease_ttl() {
    for ttl in [30, 45, 60, 120, 300, 3_600, 86_400] {
        let interval = heartbeat_interval_secs(ttl);
        assert!(
            interval < ttl as u64,
            "heartbeat {interval}s must fit inside a {ttl}s lease"
        );
        assert!(interval >= 5, "a too-frequent heartbeat just wastes writes");
    }
    assert_eq!(heartbeat_interval_secs(30), 10);
    assert_eq!(heartbeat_interval_secs(300), 60);
    assert_eq!(heartbeat_interval_secs(86_400), 60);
}

/// Regression guard for the restore step order: the database and its WAL travel
/// together. Deleting the sidecars before the rename lost committed-but-not-yet-
/// checkpointed pages of the previous database, and anything left behind would
/// be replayed over the freshly restored file.
#[tokio::test]
async fn restoring_moves_the_previous_database_and_its_wal_together() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("openllm.db");
    let pool = file_pool(&path).await;
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'one', 'openai', 'http://upstream')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let backup = create_backup(&pool, &path).await.unwrap();
    pool.close().await;

    // A WAL left behind by a killed process. Its content does not matter here:
    // what matters is that it is not dropped or replayed over the restored file.
    let wal = path.with_file_name("openllm.db-wal");
    tokio::fs::write(&wal, b"stale wal").await.unwrap();

    let outcome = restore_backup(&path, &backup).await.unwrap();
    assert!(outcome.report.ok, "{:?}", outcome.report.problems);
    let previous = outcome.previous.clone().expect("the old file is kept");
    assert!(previous.exists(), "the previous database is set aside");
    assert!(
        previous.with_file_name("openllm.db.pre-restore-wal").exists(),
        "the previous database keeps its WAL next to it"
    );
    assert!(
        !wal.exists(),
        "no stale WAL may sit next to the restored database"
    );

    let restored = file_pool(&path).await;
    let providers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM providers")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(providers, 1, "the restored file is the backup");
}
