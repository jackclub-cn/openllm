use super::*;

pub async fn backup_database(State(state): State<AppState>) -> AppResult<Response> {
    struct TemporaryFile(PathBuf);

    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    let filename = format!("openllm-backup-{}.db", Utc::now().format("%Y%m%d-%H%M%S"));
    let path = std::env::temp_dir().join(format!(
        "openllm-backup-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let temporary_file = TemporaryFile(path.clone());
    let path_string = path.to_string_lossy().replace('\\', "/");
    let vacuum_sql = format!("VACUUM INTO '{}'", path_string.replace('\'', "''"));
    if let Err(error) = sqlx::raw_sql(&vacuum_sql).execute(&state.pool).await {
        return Err(AppError::Database(error));
    }
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) => {
            return Err(AppError::Internal(error.into()));
        }
    };
    let content_length = file.metadata().await.ok().map(|metadata| metadata.len());
    let stream = async_stream::stream! {
        let _temporary_file = temporary_file;
        let mut file = file;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            match file.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => yield Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buffer[..read])),
                Err(error) => {
                    yield Err(error);
                    break;
                }
            }
        }
    };

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.sqlite3")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(content_length) = content_length {
        response = response.header(header::CONTENT_LENGTH, content_length.to_string());
    }
    Ok(response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

pub async fn vacuum_database(
    State(state): State<AppState>,
) -> AppResult<Json<DatabaseVacuumResult>> {
    let before = load_database_stats(&state).await?;
    sqlx::query("VACUUM").execute(&state.pool).await?;
    let database_stats = load_database_stats(&state).await?;
    let reclaimed_bytes = before
        .size_bytes
        .saturating_sub(database_stats.size_bytes)
        .max(0);
    record_audit(
        &state,
        "action",
        "database",
        None,
        &format!("vacuumed database, reclaimed {reclaimed_bytes} byte(s)"),
        Some(json!({ "reclaimed_bytes": reclaimed_bytes })),
    )
    .await;
    Ok(Json(DatabaseVacuumResult {
        reclaimed_bytes,
        database_stats,
    }))
}
