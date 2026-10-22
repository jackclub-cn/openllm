use super::*;

// -------------------------------------------------------------------------
// Backups
// -------------------------------------------------------------------------

#[tokio::test]
async fn a_backup_is_created_listed_verified_and_still_restorable() {
    let directory = tempdir().unwrap();
    let (state, database) = file_state(directory.path()).await;
    seed_gateway_key(&state, "caller").await;
    exec(&state, SEED_SQL).await;
    exec(
        &state,
        "INSERT INTO webhooks (name, url, secret) \
         VALUES ('legacy-hook', 'https://hook.example/x', 'whsec-preserved')",
    )
    .await;

    let (status, Json(info)) = create_backup(State(state.clone())).await.unwrap();
    assert_eq!(status, StatusCode::CREATED);
    assert!(info.name.starts_with("openllm-manual-"), "{}", info.name);
    assert!(info.name.ends_with(".db"));
    assert!(info.size_bytes > 0);
    assert_eq!(info.kind, "manual");
    let backup = PathBuf::from(&info.path);
    assert!(backup.exists(), "{backup:?}");

    let listed = list_backups(State(state.clone())).await.unwrap().0;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, info.name);
    assert_eq!(listed[0].kind, "manual");

    let report = verify_backup(
        State(state.clone()),
        Json(VerifyBackupRequest {
            name: info.name.clone(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(report.ok, "{:#?}", report.problems);
    assert_eq!(report.integrity, "ok");
    assert!(report.size_bytes > 0);
    assert!(
        report.missing_tables.is_empty(),
        "{:#?}",
        report.missing_tables
    );
    assert!(report.applied_migrations > 0);
    assert!(report.schema_version > 0);
    assert_eq!(report.providers, 1);
    assert_eq!(report.routes, 1);
    assert_eq!(report.provider_pools, 1);
    assert_eq!(report.file, info.path);

    // The backup is a self-contained database: opening it read-only shows the
    // same rows, and the original file still exists untouched.
    assert!(database.exists());
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        backup.to_string_lossy().replace('\\', "/")
    ))
    .await
    .unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM providers WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "alpha");
    let legacy_secret: String =
        sqlx::query_scalar("SELECT secret FROM webhooks WHERE name = 'legacy-hook'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(legacy_secret, "whsec-preserved");
    pool.close().await;
}

#[tokio::test]
async fn backup_verification_rejects_traversal_and_unknown_names() {
    let directory = tempdir().unwrap();
    let (state, _database) = file_state(directory.path()).await;
    let (_, Json(_)) = create_backup(State(state.clone())).await.unwrap();

    for name in ["../openllm.db", "backups/openllm-manual-x.db", ".."] {
        let result = verify_backup(
            State(state.clone()),
            Json(VerifyBackupRequest {
                name: name.to_string(),
            }),
        )
        .await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "{name} must be rejected"
        );
    }

    let missing = verify_backup(
        State(state.clone()),
        Json(VerifyBackupRequest {
            name: "openllm-manual-19990101-000000.db".to_string(),
        }),
    )
    .await;
    assert!(matches!(missing, Err(AppError::NotFound(_))));

    // A file that is not a database is rejected, not reported as healthy.
    std::fs::write(
        directory.path().join("backups").join("junk.db"),
        b"not a database",
    )
    .unwrap();
    let junk = verify_backup(
        State(state.clone()),
        Json(VerifyBackupRequest {
            name: "junk.db".to_string(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(!junk.ok);
    assert!(!junk.problems.is_empty());
}

#[tokio::test]
async fn database_operations_need_a_file_database() {
    let state = memory_state().await;
    let result = create_backup(State(state.clone())).await;
    assert!(
        matches!(result, Err(AppError::BadRequest(_))),
        "unexpected result: {result:?}"
    );
    assert!(matches!(
        migration_status(State(state.clone())).await,
        Err(AppError::BadRequest(_))
    ));
    assert!(matches!(
        verify_backup(
            State(state.clone()),
            Json(VerifyBackupRequest {
                name: "any.db".to_string()
            })
        )
        .await,
        Err(AppError::BadRequest(_))
    ));
}

#[tokio::test]
async fn old_backups_are_pruned_but_the_newest_are_kept() {
    let directory = tempdir().unwrap();
    let (state, database) = file_state(directory.path()).await;
    let backups = directory.path().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    for index in 0..8 {
        std::fs::write(
            backups.join(format!("openllm-manual-2020010{index}-000000.db")),
            b"old",
        )
        .unwrap();
    }
    assert!(database.exists());

    let (_, Json(_)) = create_backup(State(state.clone())).await.unwrap();
    let listed = list_backups(State(state.clone())).await.unwrap().0;
    let manual = listed.iter().filter(|info| info.kind == "manual").count();
    assert!(
        manual <= crate::startup::MAX_MANUAL_BACKUPS,
        "manual backups must be pruned: {manual}"
    );
    assert!(
        listed.iter().any(|info| info
            .name
            .contains(&chrono::Utc::now().format("%Y").to_string())),
        "the newest backup must survive: {:#?}",
        listed
    );
}
