use super::*;

// -------------------------------------------------------------------------
// Backups
// -------------------------------------------------------------------------

/// The file behind the live pool, or an error for an in-memory database.
pub(super) fn database_file(state: &AppState) -> AppResult<PathBuf> {
    let options = state.pool.connect_options();
    let path = options.get_filename().to_path_buf();
    // sqlx has no `in_memory` getter, so the marker it writes into the
    // filename is what identifies a `sqlite::memory:` pool; a real database
    // never has a `file:` filename.
    let filename = options.get_filename().to_path_buf();
    let name = filename.to_string_lossy();
    if name.is_empty() || name == ":memory:" || name.starts_with("file:sqlx-in-memory-") {
        return Err(AppError::BadRequest(
            "该实例使用内存数据库，没有可备份的文件".to_string(),
        ));
    }
    Ok(path)
}

pub async fn create_backup(
    State(state): State<AppState>,
) -> AppResult<(StatusCode, Json<BackupInfo>)> {
    let path = database_file(&state)?;
    let written = crate::startup::create_backup(&state.pool, &path)
        .await
        .map_err(AppError::Internal)?;
    let name = written
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let info = crate::startup::list_backups(&path)
        .await
        .map_err(AppError::Internal)?
        .into_iter()
        .find(|backup| backup.name == name)
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("备份文件写入后未能列出")))?;
    record_audit(
        &state,
        "backup",
        "database",
        Some(&info.name),
        "created a database backup",
        Some(json!({ "size_bytes": info.size_bytes })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(info)))
}

pub async fn list_backups(State(state): State<AppState>) -> AppResult<Json<Vec<BackupInfo>>> {
    let path = database_file(&state)?;
    Ok(Json(
        crate::startup::list_backups(&path)
            .await
            .map_err(AppError::Internal)?,
    ))
}

/// Verify a backup without touching it: read-only open, integrity check and
/// row counts, so an operator knows the file is restorable before stopping the
/// gateway to restore it.
pub async fn verify_backup(
    State(state): State<AppState>,
    Json(input): Json<VerifyBackupRequest>,
) -> AppResult<Json<DatabaseFileReport>> {
    let path = database_file(&state)?;
    let backup = crate::startup::resolve_backup_path(&path, &input.name, false)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    if !backup.is_file() {
        return Err(AppError::NotFound(format!(
            "备份文件 {} 不存在",
            input.name
        )));
    }
    let report = crate::startup::verify_database_file(&backup)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(report))
}

// -------------------------------------------------------------------------
// Migration pre-check and upgrade check
// -------------------------------------------------------------------------

/// What a start would do to the schema, answered from the live connection.
pub async fn migration_status(State(state): State<AppState>) -> AppResult<Json<MigrationReport>> {
    let path = database_file(&state)?;
    let file = path.to_string_lossy().replace('\\', "/");
    let migrator = sqlx::migrate!("./migrations");
    let mut problems = Vec::new();

    let applied_versions = sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_else(|error| {
            problems.push(format!("读取迁移记录失败：{error}"));
            Vec::new()
        });

    let pending = crate::startup::pending_migrations(&state.pool, &migrator)
        .await
        .map_err(AppError::Internal)?
        .into_iter()
        .map(|migration| MigrationEntry {
            version: migration.version,
            description: migration.description.clone(),
        })
        .collect::<Vec<_>>();

    let unknown_applied = {
        let mut unknown = applied_versions
            .iter()
            .filter(|version| !migrator.iter().any(|m| m.version == **version))
            .copied()
            .collect::<Vec<_>>();
        unknown.sort_unstable();
        unknown
    };
    if !unknown_applied.is_empty() {
        problems.push(format!(
            "数据库中记录了本程序不认识的迁移 {unknown_applied:?}，说明该库由更新的版本写入",
        ));
    }

    let applied = migrator
        .iter()
        .filter(|migration| applied_versions.contains(&migration.version))
        .map(|migration| MigrationEntry {
            version: migration.version,
            description: migration.description.to_string(),
        })
        .collect::<Vec<_>>();

    let data_present = sqlx::query_scalar::<_, i64>(
        "SELECT (SELECT COUNT(*) FROM providers) + (SELECT COUNT(*) FROM routes) \
                + (SELECT COUNT(*) FROM api_keys)",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0)
        > 0;

    Ok(Json(MigrationReport {
        database: file,
        up_to_date: pending.is_empty(),
        applied,
        backup_would_be_written: data_present && !pending.is_empty(),
        pending,
        unknown_applied,
        data_present,
        problems,
    }))
}
