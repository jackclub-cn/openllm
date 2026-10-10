use super::*;

pub async fn list_usage(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> AppResult<Json<UsagePage>> {
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 200);
    let offset = (page - 1) * page_size;

    let total = if usage_query_is_unfiltered(&query) {
        sqlx::query_scalar(
            "SELECT requests + (SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1) \
             FROM usage_lifetime_stats WHERE id = 1",
        )
        .fetch_one(&state.pool)
        .await?
    } else {
        let mut count =
            QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM usage_logs u WHERE 1 = 1");
        apply_usage_filters(&mut count, &query);
        count.build_query_scalar().fetch_one(&state.pool).await?
    };

    let mut items = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms, u.status_code,
               u.in_flight, u.success, u.streamed, u.error_message, u.created_at,
               u.first_token_ms, u.session_id, u.warning_message,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name,
               COALESCE(u.provider_api_key_name, pk.name) AS provider_api_key_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
        WHERE 1 = 1
        "#,
    );
    apply_usage_filters(&mut items, &query);
    items
        .push(" ORDER BY u.created_at DESC, u.id DESC LIMIT ")
        .push_bind(page_size)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows = items
        .build_query_as::<UsageLogDetailRow>()
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(UsagePage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
    }))
}

pub(super) const USAGE_EXPORT_LIMIT: i64 = 100_000;

pub async fn export_usage(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> AppResult<Response> {
    let mut items = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms,
               u.status_code, u.in_flight, u.success, u.streamed, u.error_message,
               u.created_at, u.first_token_ms, u.session_id, u.warning_message,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name,
               COALESCE(u.provider_api_key_name, pk.name) AS provider_api_key_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
        WHERE 1 = 1
        "#,
    );
    apply_usage_filters(&mut items, &query);
    items
        .push(" ORDER BY u.created_at DESC, u.id DESC LIMIT ")
        .push_bind(USAGE_EXPORT_LIMIT);
    let rows = items
        .build_query_as::<UsageLogDetailRow>()
        .fetch_all(&state.pool)
        .await?;
    let truncated = rows.len() as i64 == USAGE_EXPORT_LIMIT;

    let mut csv = String::from(
        "\u{feff}created_at,request_id,session_id,api_key,provider,provider_api_key,route,requested_model,upstream_model,endpoint,prompt_tokens,completion_tokens,total_tokens,cache_read_tokens,cache_write_tokens,estimated_cost_usd,latency_ms,first_token_ms,output_tps,status_code,in_flight,success,streamed,error_message,gateway_warning\r\n",
    );
    for row in rows {
        let item = UsageLogView::from(row);
        csv.push_str(&usage_csv_row(&item));
        csv.push_str("\r\n");
    }

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"openllm-usage.csv\"",
        );
    if truncated {
        response = response.header("x-openllm-export-truncated", "true");
    }
    Ok(response
        .body(Body::from(csv))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

pub(super) fn usage_csv_row(item: &UsageLogView) -> String {
    let text = |value: Option<&str>| value.unwrap_or_default().to_string();
    let number = |value: Option<i64>| value.map(|value| value.to_string()).unwrap_or_default();
    let cost = item
        .estimated_cost_micros
        .map(|value| format!("{:.6}", value as f64 / 1_000_000.0))
        .unwrap_or_default();
    let tps = item
        .output_tps
        .map(|value| format!("{value:.2}"))
        .unwrap_or_default();
    [
        item.created_at.clone(),
        item.request_id.clone(),
        text(item.session_id.as_deref()),
        text(item.api_key_name.as_deref()),
        text(item.provider_name.as_deref()),
        text(item.provider_api_key_name.as_deref()),
        text(item.route_name.as_deref()),
        item.requested_model.clone(),
        text(item.upstream_model.as_deref()),
        item.endpoint.clone(),
        item.prompt_tokens.to_string(),
        item.completion_tokens.to_string(),
        item.total_tokens.to_string(),
        item.cache_read_tokens.to_string(),
        item.cache_write_tokens.to_string(),
        cost,
        item.latency_ms.to_string(),
        number(item.first_token_ms),
        tps,
        item.status_code.to_string(),
        if item.in_flight { "true" } else { "false" }.to_string(),
        if item.success { "true" } else { "false" }.to_string(),
        if item.streamed { "true" } else { "false" }.to_string(),
        text(item.error_message.as_deref()),
        text(item.warning_message.as_deref()),
    ]
    .into_iter()
    .map(|value| csv_field(&value))
    .collect::<Vec<_>>()
    .join(",")
}

pub(super) fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\r') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub async fn get_usage_detail(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> AppResult<Json<UsageLogView>> {
    let row = sqlx::query_as::<_, UsageLogDetailRow>(
        r#"
        SELECT u.*, k.name AS api_key_name, r.name AS route_name, p.name AS provider_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE u.request_id = ?
        "#,
    )
    .bind(&request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("usage log not found".to_string()))?;
    Ok(Json(row.into()))
}

pub async fn cleanup_usage(
    State(state): State<AppState>,
    Json(input): Json<UsageCleanup>,
) -> AppResult<Json<UsageCleanupResult>> {
    if input.older_than_days < 1 {
        return Err(AppError::BadRequest(
            "older_than_days must be at least 1".to_string(),
        ));
    }
    let cutoff = Utc::now() - Duration::days(input.older_than_days);
    let cutoff = cutoff.to_rfc3339();
    let result = sqlx::query("DELETE FROM usage_logs WHERE created_at < ?")
        .bind(&cutoff)
        .execute(&state.pool)
        .await?;
    record_audit(
        &state,
        "delete",
        "usage",
        None,
        &format!(
            "cleaned {} usage log(s) older than {} day(s)",
            result.rows_affected(),
            input.older_than_days
        ),
        Some(json!({
            "older_than_days": input.older_than_days,
            "deleted": result.rows_affected(),
            "cutoff": cutoff,
        })),
    )
    .await;
    Ok(Json(UsageCleanupResult {
        deleted: result.rows_affected(),
        older_than_days: input.older_than_days,
        cutoff,
    }))
}

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

/// Share of prompt tokens served from cache, as a percentage.
///
/// `prompt_tokens` must be the total input count (fresh + cached), so the ratio
/// cannot exceed 100. A zero denominator yields 0 rather than NaN so the
/// dashboard needs no special case.
pub(super) fn cache_hit_rate(prompt_tokens: i64, cache_read: i64) -> f64 {
    if prompt_tokens <= 0 {
        return 0.0;
    }
    (cache_read as f64 / prompt_tokens as f64 * 100.0).clamp(0.0, 100.0)
}

pub(super) fn parse_overview_time(
    value: Option<&str>,
    fallback: DateTime<Utc>,
    name: &str,
) -> AppResult<DateTime<Utc>> {
    match value {
        Some(value) => DateTime::parse_from_rfc3339(value)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| {
                AppError::BadRequest(format!("{name} must be an RFC3339 timestamp: {error}"))
            }),
        None => Ok(fallback),
    }
}

pub(super) fn normalize_overview_range(
    from: Option<&str>,
    to: Option<&str>,
    default_start: DateTime<Utc>,
    default_end: DateTime<Utc>,
) -> AppResult<(DateTime<Utc>, DateTime<Utc>)> {
    let start = parse_overview_time(from, default_start, "from")?;
    let end = parse_overview_time(to, default_end, "to")?;
    if start >= end {
        return Err(AppError::BadRequest(
            "overview range end must be after its start".to_string(),
        ));
    }
    if end - start > Duration::days(MAX_OVERVIEW_RANGE_DAYS) {
        return Err(AppError::BadRequest(format!(
            "overview range cannot exceed {MAX_OVERVIEW_RANGE_DAYS} days"
        )));
    }
    Ok((start, end))
}

pub async fn overview(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> AppResult<Json<Overview>> {
    let now = Utc::now();
    // Clamp to real-world offsets so a stray value cannot shift the window far.
    let tz_offset = query.tz_offset_minutes.clamp(-14 * 60, 14 * 60);
    let local_now = now + Duration::minutes(tz_offset);
    // Start of the caller's local day, converted back to UTC for comparison
    // against the stored UTC timestamps.
    let day_start = local_now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        - Duration::minutes(tz_offset);
    let (range_start, range_end) = normalize_overview_range(
        query.from.as_deref(),
        query.to.as_deref(),
        day_start - Duration::days(13),
        day_start + Duration::days(1),
    )?;

    let totals_fut = sqlx::query(
        r#"
        SELECT
            requests AS requests_total,
            tokens AS tokens_total,
            prompt_tokens AS prompt_tokens_total,
            completion_tokens AS completion_tokens_total,
            prompt_tokens AS prompt_total,
            cache_read_tokens AS cache_read_total,
            cache_write_tokens AS cache_write_total,
            cost_micros AS cost_total_micros,
            unpriced_requests AS unpriced_total,
            CASE
                WHEN requests = 0 THEN 0.0
                ELSE successful_requests * 100.0 / requests
            END AS success_rate,
            CASE
                WHEN requests = 0 THEN 0.0
                ELSE latency_ms_sum * 1.0 / requests
            END AS avg_latency_ms
        FROM usage_lifetime_stats
        WHERE id = 1
        "#,
    )
    .fetch_one(&state.pool);

    let range_totals_fut = sqlx::query(
        r#"
        SELECT
            COUNT(*) AS requests,
            COALESCE(SUM(total_tokens), 0) AS tokens,
            COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
            COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
            COALESCE(SUM(prompt_tokens), 0) AS prompt,
            COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
            COALESCE(SUM(cache_write_tokens), 0) AS cache_write,
            COALESCE(SUM(estimated_cost_micros), 0) AS cost_micros,
            COALESCE(SUM(estimated_cost_micros IS NULL), 0) AS unpriced,
            COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
            COALESCE(AVG(latency_ms), 0.0) AS avg_latency_ms,
            COALESCE(SUM(CASE WHEN warning_message IS NOT NULL
                              AND TRIM(warning_message) <> '' THEN 1 ELSE 0 END), 0) AS gateway_adjusted
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_one(&state.pool);

    let session_totals_fut = if query.include_session_metrics {
        sqlx::query(
            r#"
            SELECT
                COUNT(*) AS sessions,
                COALESCE(SUM(requests), 0) AS session_requests,
                COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                COALESCE(SUM(cache_read), 0) AS cache_read
            FROM (
                SELECT session_id,
                       COUNT(*) AS requests,
                       COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                       COALESCE(SUM(cache_read_tokens), 0) AS cache_read
                FROM usage_logs
                WHERE created_at >= ? AND created_at < ? AND in_flight = 0
                  AND session_id IS NOT NULL AND TRIM(session_id) <> ''
                GROUP BY session_id
            )
            "#,
        )
        .bind(range_start.to_rfc3339())
        .bind(range_end.to_rfc3339())
        .fetch_one(&state.pool)
    } else {
        sqlx::query(
            "SELECT 0 AS sessions, 0 AS session_requests, \
                    0 AS prompt_tokens, 0 AS cache_read",
        )
        .fetch_one(&state.pool)
    };

    let today_fut = sqlx::query(
        r#"
        SELECT COUNT(*) AS requests, COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt,
               COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(estimated_cost_micros), 0) AS cost_micros,
               COALESCE(SUM(estimated_cost_micros IS NULL), 0) AS unpriced
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        "#,
    )
    .bind(day_start.to_rfc3339())
    .bind((day_start + Duration::days(1)).to_rfc3339())
    .fetch_one(&state.pool);

    let (totals, range_totals, session_totals, today) =
        tokio::try_join!(totals_fut, range_totals_fut, session_totals_fut, today_fut)?;

    let active_providers_fut =
        sqlx::query_scalar("SELECT COUNT(*) FROM providers WHERE enabled = 1")
            .fetch_one(&state.pool);
    let active_routes_fut =
        sqlx::query_scalar("SELECT COUNT(*) FROM routes WHERE enabled = 1").fetch_one(&state.pool);
    let provider_health_fut = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT \
                COALESCE(SUM(CASE WHEN enabled = 1 AND last_test_ok = 1 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN enabled = 1 AND last_test_ok = 0 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN enabled = 1 AND last_test_ok IS NULL THEN 1 ELSE 0 END), 0) \
             FROM providers",
    )
    .fetch_one(&state.pool);
    let provider_key_health_fut = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
        "SELECT \
            COUNT(*), \
            COALESCE(SUM(CASE WHEN k.last_test_ok = 1 THEN 1 ELSE 0 END), 0), \
            COALESCE(SUM(CASE WHEN k.last_test_ok = 0 THEN 1 ELSE 0 END), 0), \
            COALESCE(SUM(CASE WHEN k.last_test_ok IS NULL THEN 1 ELSE 0 END), 0), \
            COALESCE(SUM(CASE WHEN k.last_error IS NOT NULL THEN 1 ELSE 0 END), 0) \
         FROM provider_api_keys k \
         JOIN providers p ON p.id = k.provider_id \
         WHERE p.enabled = 1 AND k.enabled = 1",
    )
    .fetch_one(&state.pool);
    let in_flight_requests_fut =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
            .fetch_one(&state.pool);
    let (
        active_providers,
        active_routes,
        (healthy_providers, failed_providers, untested_providers),
        (
            provider_keys_total,
            healthy_provider_keys,
            failed_provider_keys,
            untested_provider_keys,
            runtime_error_provider_keys,
        ),
        in_flight_requests,
    ) = tokio::try_join!(
        active_providers_fut,
        active_routes_fut,
        provider_health_fut,
        provider_key_health_fut,
        in_flight_requests_fut,
    )?;
    let now = std::time::Instant::now();
    let cooling_providers = {
        let provider_cooldowns = state.provider_cooldown.lock().await;
        provider_cooldowns
            .values()
            .filter(|until| **until > now)
            .count() as i64
    };
    let cooling_provider_keys = state
        .provider_key_cooldown
        .lock()
        .await
        .values()
        .filter(|until| **until > now)
        .count() as i64;

    let recent_fut = sqlx::query_as::<_, UsageLogDetailRow>(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms, u.status_code,
               u.in_flight, u.success, u.streamed, u.error_message, u.created_at,
               u.first_token_ms, u.session_id, u.warning_message,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name,
               COALESCE(u.provider_api_key_name, pk.name) AS provider_api_key_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
        WHERE u.created_at >= ? AND u.created_at < ?
        ORDER BY u.created_at DESC, u.id DESC
        LIMIT 12
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_all(&state.pool);

    let provider_usage_fut = sqlx::query_as::<_, ProviderUsage>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               COUNT(u.id) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               SUM(u.estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms
        FROM providers p
        LEFT JOIN usage_logs u
          ON u.provider_id = p.id AND u.created_at >= ? AND u.created_at < ?
             AND u.in_flight = 0
        GROUP BY p.id, p.name
        ORDER BY requests DESC, tokens DESC
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_all(&state.pool);

    let daily_rows_fut = sqlx::query_as::<_, DailyUsage>(
        r#"
        SELECT date(datetime(created_at), ? || ' minutes') AS day,
               COUNT(*) AS requests,
               COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(completion_tokens), 0) AS completion_tokens
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        GROUP BY date(datetime(created_at), ? || ' minutes')
        ORDER BY day
        "#,
    )
    .bind(tz_offset)
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .bind(tz_offset)
    .fetch_all(&state.pool);

    let model_usage_fut = sqlx::query_as::<_, ModelUsage>(
        r#"
        SELECT requested_model AS model,
               COUNT(*) AS requests,
               COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
               SUM(estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(latency_ms), 0.0) AS avg_latency_ms
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        GROUP BY requested_model
        ORDER BY tokens DESC, requests DESC
        LIMIT 8
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_all(&state.pool);

    let (recent, provider_usage, daily_rows, model_usage) = tokio::try_join!(
        recent_fut,
        provider_usage_fut,
        daily_rows_fut,
        model_usage_fut
    )?;

    // Fill in days with no traffic so the chart has a continuous axis
    // instead of collapsing to only the days that happened to have requests.
    let local_range_start = range_start + Duration::minutes(tz_offset);
    let local_range_end = range_end + Duration::minutes(tz_offset);
    let first_day = local_range_start.date_naive();
    let last_day = (local_range_end - Duration::nanoseconds(1)).date_naive();
    let day_count = (last_day - first_day).num_days() + 1;
    let mut daily_usage = Vec::with_capacity(day_count.max(0) as usize);
    for offset in 0..day_count {
        let day = (first_day + Duration::days(offset))
            .format("%Y-%m-%d")
            .to_string();
        let existing = daily_rows.iter().find(|row| row.day == day);
        daily_usage.push(DailyUsage {
            day,
            requests: existing.map_or(0, |row| row.requests),
            tokens: existing.map_or(0, |row| row.tokens),
            prompt_tokens: existing.map_or(0, |row| row.prompt_tokens),
            completion_tokens: existing.map_or(0, |row| row.completion_tokens),
        });
    }

    let range_requests: i64 = range_totals.get("requests");
    let range_sessions: i64 = session_totals.get("sessions");
    let range_session_requests: i64 = session_totals.get("session_requests");
    let range_session_coverage = if range_requests > 0 {
        (range_session_requests as f64 / range_requests as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let range_avg_requests_per_session = if range_sessions > 0 {
        range_session_requests as f64 / range_sessions as f64
    } else {
        0.0
    };

    Ok(Json(Overview {
        requests_today: today.get("requests"),
        tokens_today: today.get("tokens"),
        prompt_tokens_today: today.get("prompt_tokens"),
        completion_tokens_today: today.get("completion_tokens"),
        cache_read_today: today.get("cache_read"),
        cache_write_today: today.get("cache_write"),
        cache_hit_rate: cache_hit_rate(today.get("prompt"), today.get("cache_read")),
        requests_total: totals.get("requests_total"),
        tokens_total: totals.get("tokens_total"),
        prompt_tokens_total: totals.get("prompt_tokens_total"),
        completion_tokens_total: totals.get("completion_tokens_total"),
        cache_read_total: totals.get("cache_read_total"),
        cache_write_total: totals.get("cache_write_total"),
        cost_today_micros: today.get("cost_micros"),
        cost_total_micros: totals.get("cost_total_micros"),
        unpriced_today: today.get("unpriced"),
        unpriced_total: totals.get("unpriced_total"),
        range_requests: range_totals.get("requests"),
        range_tokens: range_totals.get("tokens"),
        range_prompt_tokens: range_totals.get("prompt_tokens"),
        range_completion_tokens: range_totals.get("completion_tokens"),
        range_cache_read: range_totals.get("cache_read"),
        range_cache_write: range_totals.get("cache_write"),
        range_cache_hit_rate: cache_hit_rate(
            range_totals.get("prompt"),
            range_totals.get("cache_read"),
        ),
        range_sessions,
        range_session_coverage,
        range_avg_requests_per_session,
        range_session_cache_hit_rate: cache_hit_rate(
            session_totals.get("prompt_tokens"),
            session_totals.get("cache_read"),
        ),
        range_cost_micros: range_totals.get("cost_micros"),
        range_unpriced: range_totals.get("unpriced"),
        range_success_rate: range_totals.get("success_rate"),
        range_avg_latency_ms: range_totals.get("avg_latency_ms"),
        range_gateway_adjusted: range_totals.get("gateway_adjusted"),
        success_rate: totals.get("success_rate"),
        avg_latency_ms: totals.get("avg_latency_ms"),
        active_providers,
        active_routes,
        healthy_providers,
        failed_providers,
        untested_providers,
        provider_keys_total,
        healthy_provider_keys,
        failed_provider_keys,
        untested_provider_keys,
        runtime_error_provider_keys,
        cooling_providers,
        cooling_provider_keys,
        in_flight_requests,
        recent_requests: recent.into_iter().map(Into::into).collect(),
        provider_usage,
        model_usage,
        daily_usage,
    }))
}
