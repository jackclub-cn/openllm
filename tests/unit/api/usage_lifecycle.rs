use super::*;

#[tokio::test]
async fn usage_lifetime_stats_tracks_completion_and_cleanup() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens,
                completion_tokens, total_tokens, cache_read_tokens,
                cache_write_tokens, latency_ms, status_code, in_flight, success
             ) VALUES (
                'pending', 'test-model', '/v1/chat/completions', 10,
                20, 30, 5, 7, 100, 0, 1, 1
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let pending: i64 = sqlx::query_scalar("SELECT requests FROM usage_lifetime_stats WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);

    sqlx::query(
        "UPDATE usage_logs
             SET in_flight = 0, status_code = 200, success = 1,
                 estimated_cost_micros = 42
             WHERE request_id = 'pending'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let completed: (i64, i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT requests, tokens, prompt_tokens, completion_tokens,
                    cache_read_tokens, cache_write_tokens, cost_micros,
                    successful_requests, latency_ms_sum
             FROM usage_lifetime_stats WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(completed, (1, 30, 10, 20, 5, 7, 42, 1, 100));

    sqlx::query("DELETE FROM usage_logs WHERE request_id = 'pending'")
        .execute(&pool)
        .await
        .unwrap();
    let cleaned: (i64, i64, i64) = sqlx::query_as(
        "SELECT requests, tokens, cost_micros
             FROM usage_lifetime_stats WHERE id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cleaned, (0, 0, 0));
}

#[tokio::test]
async fn reconciles_stale_and_interrupted_usage_requests() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let stale = (Utc::now() - Duration::days(2)).to_rfc3339();
    let recent = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, in_flight,
                success, created_at
             ) VALUES
                ('stale', 'm', '/v1/chat/completions', 0, 1, 0, ?),
                ('recent', 'm', '/v1/chat/completions', 0, 1, 0, ?),
                ('done', 'm', '/v1/chat/completions', 200, 0, 1, ?)",
    )
    .bind(stale)
    .bind(recent)
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    let cutoff = (Utc::now() - Duration::days(1)).to_rfc3339();
    let stale_updated = finish_interrupted_usage_requests(
        &state,
        Some(&cutoff),
        "request was interrupted before completion",
    )
    .await
    .unwrap();
    assert_eq!(stale_updated, 1);
    let stale_row: (i64, i64, String) = sqlx::query_as(
        "SELECT in_flight, status_code, error_message
             FROM usage_logs WHERE request_id = 'stale'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stale_row.0, 0);
    assert_eq!(stale_row.1, 499);
    assert_eq!(stale_row.2, "request was interrupted before completion");

    let startup_updated = reconcile_interrupted_usage_requests(&state).await.unwrap();
    assert_eq!(startup_updated, 1);
    let statuses = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT request_id, in_flight, status_code
             FROM usage_logs ORDER BY request_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("done".to_string(), 0, 200),
            ("recent".to_string(), 0, 499),
            ("stale".to_string(), 0, 499),
        ]
    );
}

#[tokio::test]
async fn stale_reconciliation_uses_last_activity_for_long_streams() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let old_created = (Utc::now() - Duration::hours(2)).to_rfc3339();
    let active = Utc::now().to_rfc3339();
    let stale_activity = (Utc::now() - Duration::minutes(20)).to_rfc3339();
    sqlx::query(
        "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, in_flight,
                success, created_at, last_activity_at
             ) VALUES
                ('active', 'm', '/v1/chat/completions', 0, 1, 0, ?, ?),
                ('stale', 'm', '/v1/chat/completions', 0, 1, 0, ?, ?)",
    )
    .bind(&old_created)
    .bind(active)
    .bind(&old_created)
    .bind(stale_activity)
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState::new(pool.clone(), None);
    reconcile_stale_usage_requests(state).await;
    let rows = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT request_id, in_flight, status_code
             FROM usage_logs ORDER BY request_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![("active".to_string(), 1, 0), ("stale".to_string(), 0, 499)]
    );
}
