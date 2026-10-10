use super::*;

pub async fn run_due_provider_health_checks(state: AppState) {
    let due = match sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM providers
        WHERE enabled = 1
          AND health_check_interval_minutes > 0
          AND (
              last_test_at IS NULL
              OR datetime(last_test_at) <= datetime(
                  'now',
                  '-' || health_check_interval_minutes || ' minutes'
              )
          )
        ORDER BY id
        "#,
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(%error, "failed to query providers due for health checks");
            return;
        }
    };

    futures_util::stream::iter(due.into_iter().map(|id| {
        let state = state.clone();
        async move {
            if let Err(error) = test_provider_inner(&state, id).await {
                tracing::warn!(provider_id = id, %error, "scheduled provider health check failed");
            }
        }
    }))
    .buffer_unordered(4)
    .for_each(|_| async {})
    .await;
}

pub async fn run_due_provider_model_syncs(state: AppState) {
    let due = match sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM providers
        WHERE enabled = 1
          AND models_sync_interval_minutes > 0
          AND (
              models_sync_attempted_at IS NULL
              OR datetime(models_sync_attempted_at) <= datetime(
                  'now',
                  '-' || models_sync_interval_minutes || ' minutes'
              )
          )
        ORDER BY id
        "#,
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(%error, "failed to query providers due for model sync");
            return;
        }
    };

    futures_util::stream::iter(due.into_iter().map(|id| {
        let state = state.clone();
        async move {
            if let Err(error) = sync_provider(state, id).await {
                tracing::warn!(provider_id = id, %error, "scheduled provider model sync failed");
            }
        }
    }))
    .buffer_unordered(2)
    .for_each(|_| async {})
    .await;
}

pub async fn reconcile_stale_usage_requests(state: AppState) {
    let stale_cutoff = (Utc::now() - Duration::minutes(15)).to_rfc3339();
    if let Err(error) = finish_interrupted_usage_requests(
        &state,
        Some(&stale_cutoff),
        "request was interrupted before completion",
    )
    .await
    {
        tracing::warn!(%error, "failed to reconcile stale in-flight usage logs");
    }
}

pub async fn run_due_usage_retention(state: AppState) {
    {
        let last_run = state.retention_last_run.lock().await;
        if last_run.is_some_and(|last_run| last_run.elapsed() < USAGE_RETENTION_CHECK_INTERVAL) {
            return;
        }
    }

    let raw = match sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(SETTING_USAGE_RETENTION_DAYS)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to read usage retention setting");
            return;
        }
    };

    if let Some(days) = raw.and_then(|value| value.parse::<i64>().ok())
        && days > 0
    {
        let cutoff = (Utc::now() - Duration::days(days)).to_rfc3339();
        match sqlx::query("DELETE FROM usage_logs WHERE created_at < ?")
            .bind(&cutoff)
            .execute(&state.pool)
            .await
        {
            Ok(result) => {
                tracing::info!(
                    deleted = result.rows_affected(),
                    retention_days = days,
                    %cutoff,
                    "automatic usage retention cleanup completed"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    retention_days = days,
                    "automatic usage retention cleanup failed"
                );
                return;
            }
        }
    }

    *state.retention_last_run.lock().await = Some(std::time::Instant::now());
}

pub async fn reconcile_interrupted_usage_requests(state: &AppState) -> AppResult<u64> {
    let updated = finish_interrupted_usage_requests(
        state,
        None,
        "gateway restarted before request completed",
    )
    .await?;
    if updated > 0 {
        tracing::warn!(updated, "reconciled interrupted usage logs after restart");
    }
    Ok(updated)
}

/// A process restart can leave a model sync with only its start timestamp.
/// Mark it explicitly so the console does not present a permanent "syncing"
/// state and the next scheduled run can recover normally.
pub async fn reconcile_interrupted_provider_model_syncs(state: &AppState) -> AppResult<u64> {
    let result = sqlx::query(
        "UPDATE providers \
         SET models_sync_error = 'gateway restarted before model synchronization completed', \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE models_sync_attempted_at IS NOT NULL \
           AND models_sync_error IS NULL \
           AND (models_synced_at IS NULL \
                OR datetime(models_sync_attempted_at) > datetime(models_synced_at))",
    )
    .execute(&state.pool)
    .await?;
    if result.rows_affected() > 0 {
        tracing::warn!(
            updated = result.rows_affected(),
            "reconciled interrupted provider model syncs after restart"
        );
    }
    Ok(result.rows_affected())
}

pub(crate) async fn finish_interrupted_usage_requests(
    state: &AppState,
    cutoff: Option<&str>,
    message: &str,
) -> AppResult<u64> {
    let base = r#"
        UPDATE usage_logs
        SET in_flight = 0,
            status_code = 499,
            success = 0,
            error_message = COALESCE(error_message, ?),
            latency_ms = CASE
                WHEN latency_ms > 0 THEN latency_ms
                ELSE CAST(
                    MAX(0, (julianday('now') - julianday(created_at)) * 86400000)
                    AS INTEGER
                )
            END
        WHERE in_flight = 1
    "#;
    let result = match cutoff {
        Some(cutoff) => {
            let query = format!("{base} AND COALESCE(last_activity_at, created_at) < ?");
            sqlx::query(&query)
                .bind(message)
                .bind(cutoff)
                .execute(&state.pool)
                .await?
        }
        None => sqlx::query(base).bind(message).execute(&state.pool).await?,
    };
    Ok(result.rows_affected())
}
