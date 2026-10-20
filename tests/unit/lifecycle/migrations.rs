use super::*;

// -------------------------------------------------------------------------
// Migration pre-check
// -------------------------------------------------------------------------

#[tokio::test]
async fn the_migration_precheck_reports_pending_migrations_without_touching_the_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("openllm.db");

    // A file that exists but was never migrated: every migration is pending.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    pool.close().await;
    let before = std::fs::read(&path).unwrap();

    let config = crate::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: directory.path().to_path_buf(),
        database_url: Some(format!(
            "sqlite://{}",
            path.to_string_lossy().replace('\\', "/")
        )),
        admin_token: None,
    };
    let report = crate::startup::migration_report(&config).await.unwrap();
    assert!(!report.up_to_date);
    assert!(report.applied.is_empty());
    assert!(!report.pending.is_empty());
    assert!(!report.data_present);
    assert!(
        !report.backup_would_be_written,
        "an empty file needs no backup"
    );
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    assert!(
        report.pending.iter().any(|entry| entry.version == 1),
        "the first migration must be listed"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the pre-check must not modify the database file"
    );

    // A missing file is reported the same way instead of being created.
    let missing = directory.path().join("nope.db");
    let config = crate::Config {
        database_url: Some(format!(
            "sqlite://{}",
            missing.to_string_lossy().replace('\\', "/")
        )),
        ..config
    };
    let report = crate::startup::migration_report(&config).await.unwrap();
    assert!(!report.up_to_date);
    assert!(!report.pending.is_empty());
    assert!(!missing.exists());

    // The in-memory state is a fully migrated database, so the API reports it
    // as up to date when pointed at a real file.
    let (state, database) = file_state(directory.path()).await;
    let report = migration_status(State(state.clone())).await.unwrap().0;
    assert!(report.up_to_date, "{:#?}", report.problems);
    assert!(report.pending.is_empty());
    assert!(!report.applied.is_empty());
    assert!(report.unknown_applied.is_empty());
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    assert!(database.exists());
    assert!(
        !report.data_present,
        "a migrated but empty database holds no user data"
    );
    assert!(!report.backup_would_be_written);

    // With rows in it, the same report says a backup would be written before
    // any pending migration runs.
    exec(
        &state,
        "INSERT INTO providers (name, provider_type, base_url, enabled) \
         VALUES ('alpha', 'openai', 'https://alpha.example/v1', 1)",
    )
    .await;
    let report = migration_status(State(state.clone())).await.unwrap().0;
    assert!(report.data_present);
    assert!(report.up_to_date);
    assert!(
        !report.backup_would_be_written,
        "an up-to-date database needs no pre-migration backup"
    );
    drop(state);
}
