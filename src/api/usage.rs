use super::*;

pub async fn list_usage(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> AppResult<Json<UsagePage>> {
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 200);
    let offset = (page - 1) * page_size;
    let filters = query.filters();

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
        apply_usage_time_range(&mut count, query.from.as_deref(), query.to.as_deref());
        apply_usage_filters(&mut count, &filters);
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
               NULL AS request_preview, NULL AS response_preview,
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
    apply_usage_time_range(&mut items, query.from.as_deref(), query.to.as_deref());
    apply_usage_filters(&mut items, &filters);
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
    let filters = query.filters();
    let mut items = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms,
               u.status_code, u.in_flight, u.success, u.streamed, u.error_message,
               u.created_at, u.first_token_ms, u.session_id, u.warning_message,
               NULL AS request_preview, NULL AS response_preview,
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
    apply_usage_time_range(&mut items, query.from.as_deref(), query.to.as_deref());
    apply_usage_filters(&mut items, &filters);
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

pub async fn get_route_trace(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> AppResult<Json<RouteTraceView>> {
    let trace = sqlx::query_as::<_, RouteTraceRecord>(
        "SELECT request_id, created_at, finished_at, route_id, strategy, \
                session_id, session_decision, candidates_json, attempt_count, \
                fallback_count, final_provider_id, final_provider_api_key_id, \
                final_upstream_model, final_status_code, success \
         FROM routing_traces \
         WHERE request_id = ?",
    )
    .bind(&request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("routing trace not found".to_string()))?;
    let candidates = serde_json::from_str::<Vec<RouteTraceCandidate>>(&trace.candidates_json)
        .unwrap_or_default();
    let attempts = sqlx::query_as::<_, RouteTraceAttemptRow>(
        "SELECT attempt_index, round, candidate_order, provider_id, \
                provider_api_key_id, upstream_model, started_at, latency_ms, \
                status_code, success, retryable, reason_code, failure_scope, \
                failure_retry, failure_confidence \
         FROM routing_attempts \
         WHERE request_id = ? \
         ORDER BY attempt_index",
    )
    .bind(&request_id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();

    Ok(Json(RouteTraceView {
        request_id: trace.request_id,
        created_at: trace.created_at,
        finished_at: trace.finished_at,
        route_id: trace.route_id,
        strategy: trace.strategy,
        session_id: trace.session_id,
        session_decision: trace.session_decision,
        candidates,
        attempt_count: trace.attempt_count,
        fallback_count: trace.fallback_count,
        final_provider_id: trace.final_provider_id,
        final_provider_api_key_id: trace.final_provider_api_key_id,
        final_upstream_model: trace.final_upstream_model,
        final_status_code: trace.final_status_code,
        success: trace.success != 0,
        attempts,
    }))
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

/// Append one dashboard bucket, filling zeros for a window with no traffic.
fn push_usage_bucket(buckets: &mut Vec<DailyUsage>, rows: &[DailyUsage], bucket: String) {
    let existing = rows.iter().find(|row| row.day == bucket);
    buckets.push(DailyUsage {
        day: bucket,
        requests: existing.map_or(0, |row| row.requests),
        tokens: existing.map_or(0, |row| row.tokens),
        prompt_tokens: existing.map_or(0, |row| row.prompt_tokens),
        completion_tokens: existing.map_or(0, |row| row.completion_tokens),
        cost_micros: existing.map_or(0, |row| row.cost_micros),
        success_rate: existing.map_or(0.0, |row| row.success_rate),
        cache_read: existing.map_or(0, |row| row.cache_read),
        cache_write: existing.map_or(0, |row| row.cache_write),
        cache_hit_rate: existing.map_or(0.0, |row| row.cache_hit_rate),
    });
}

#[derive(Debug, sqlx::FromRow)]
struct RangeTotals {
    requests: i64,
    tokens: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read: i64,
    cache_write: i64,
    cost_micros: i64,
    unpriced: i64,
    success_rate: f64,
    avg_latency_ms: f64,
    gateway_adjusted: i64,
    tps_tokens: i64,
    tps_elapsed_ms: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct SessionTotals {
    sessions: i64,
    session_requests: i64,
    prompt_tokens: i64,
    cache_read: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct TodayTotals {
    requests: i64,
    tokens: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read: i64,
    cache_write: i64,
    cost_micros: i64,
    unpriced: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct Percentiles {
    p50: Option<i64>,
    p95: Option<i64>,
    p99: Option<i64>,
}

fn push_range_where<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    filters: &'a UsageFilters,
    start: &'a str,
    end: &'a str,
) {
    builder.push(" AND u.created_at >= ").push_bind(start);
    builder.push(" AND u.created_at < ").push_bind(end);
    apply_usage_filters(builder, filters);
}

fn percentile_query<'a>(
    column: &'static str,
    filters: &'a UsageFilters,
    start: &'a str,
    end: &'a str,
) -> QueryBuilder<'a, Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new(format!(
        "WITH ranked AS ( \
             SELECT u.{column} AS value, \
                    ROW_NUMBER() OVER (ORDER BY u.{column}) AS rn, \
                    COUNT(*) OVER () AS total \
             FROM usage_logs u \
             WHERE u.in_flight = 0 AND u.{column} IS NOT NULL"
    ));
    push_range_where(&mut builder, filters, start, end);
    builder.push(
        ") SELECT \
             MAX(CASE WHEN total > 0 AND rn = (total * 50 + 99) / 100 THEN value END) AS p50, \
             MAX(CASE WHEN total > 0 AND rn = (total * 95 + 99) / 100 THEN value END) AS p95, \
             MAX(CASE WHEN total > 0 AND rn = (total * 99 + 99) / 100 THEN value END) AS p99 \
         FROM ranked",
    );
    builder
}

struct OverviewWindow {
    tz_offset: i64,
    day_start: DateTime<Utc>,
    range_start: DateTime<Utc>,
    range_end: DateTime<Utc>,
    granularity: ResolvedGranularity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedGranularity {
    Hour,
    Day,
    Week,
    Month,
}

impl ResolvedGranularity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    fn sql_bucket(self, tz_offset: i64) -> String {
        match self {
            Self::Hour => {
                format!("strftime('%Y-%m-%d %H:00', datetime(u.created_at, '{tz_offset} minutes'))")
            }
            Self::Day => {
                format!("date(datetime(u.created_at, '{tz_offset} minutes'))")
            }
            Self::Week => format!(
                "date(datetime(u.created_at, '{tz_offset} minutes', 'weekday 0', '-6 days'))"
            ),
            Self::Month => {
                format!("strftime('%Y-%m-01', datetime(u.created_at, '{tz_offset} minutes'))")
            }
        }
    }
}

fn resolve_granularity(
    requested: Option<AnalyticsGranularity>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> AppResult<ResolvedGranularity> {
    let duration = end - start;
    let granularity = match requested.unwrap_or(AnalyticsGranularity::Auto) {
        AnalyticsGranularity::Auto => {
            if duration <= Duration::hours(24) {
                ResolvedGranularity::Hour
            } else {
                ResolvedGranularity::Day
            }
        }
        AnalyticsGranularity::Hour => ResolvedGranularity::Hour,
        AnalyticsGranularity::Day => ResolvedGranularity::Day,
        AnalyticsGranularity::Week => ResolvedGranularity::Week,
        AnalyticsGranularity::Month => ResolvedGranularity::Month,
    };
    if granularity == ResolvedGranularity::Hour
        && duration > Duration::days(MAX_HOURLY_OVERVIEW_DAYS)
    {
        return Err(AppError::BadRequest(format!(
            "hourly overview range cannot exceed {MAX_HOURLY_OVERVIEW_DAYS} days"
        )));
    }
    Ok(granularity)
}

fn overview_window(query: &OverviewQuery) -> AppResult<OverviewWindow> {
    let tz_offset = query.tz_offset_minutes.clamp(-14 * 60, 14 * 60);
    let local_now = Utc::now() + Duration::minutes(tz_offset);
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
    let granularity = resolve_granularity(query.granularity, range_start, range_end)?;
    Ok(OverviewWindow {
        tz_offset,
        day_start,
        range_start,
        range_end,
        granularity,
    })
}

fn range_totals_query<'a>(
    filters: &'a UsageFilters,
    start: &'a str,
    end: &'a str,
    tps_eligible: &str,
    elapsed_ms: &str,
) -> QueryBuilder<'a, Sqlite> {
    let mut builder = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT
            COUNT(*) AS requests,
            COALESCE(SUM(u.total_tokens), 0) AS tokens,
            COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
            COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
            COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
            COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
            COALESCE(SUM(u.estimated_cost_micros), 0) AS cost_micros,
            COALESCE(SUM(u.estimated_cost_micros IS NULL), 0) AS unpriced,
            COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
            COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms,
            COALESCE(SUM(CASE WHEN u.warning_message IS NOT NULL
                              AND TRIM(u.warning_message) <> '' THEN 1 ELSE 0 END), 0) AS gateway_adjusted,
            COALESCE(SUM(CASE WHEN {tps_eligible} THEN u.completion_tokens ELSE 0 END), 0)
                AS tps_tokens,
            COALESCE(SUM(CASE WHEN {tps_eligible} THEN {elapsed_ms} ELSE 0 END), 0)
                AS tps_elapsed_ms
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#,
    ));
    push_range_where(&mut builder, filters, start, end);
    builder
}

pub async fn overview(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> AppResult<Json<Overview>> {
    let OverviewWindow {
        tz_offset,
        day_start,
        range_start,
        range_end,
        granularity,
    } = overview_window(&query)?;
    let bucket = granularity.sql_bucket(tz_offset);
    let range_start_text = range_start.to_rfc3339();
    let range_end_text = range_end.to_rfc3339();
    let filters = query.filters();
    let filters = &filters;
    let elapsed_ms = "CASE WHEN u.streamed = 1 AND u.first_token_ms IS NOT NULL \
                      AND u.latency_ms > u.first_token_ms \
                      THEN u.latency_ms - u.first_token_ms ELSE u.latency_ms END";
    let tps_eligible = format!("u.completion_tokens > 0 AND ({elapsed_ms}) > 0");

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

    let mut range_totals_sql = range_totals_query(
        filters,
        &range_start_text,
        &range_end_text,
        &tps_eligible,
        elapsed_ms,
    );
    let range_totals_fut = range_totals_sql
        .build_query_as::<RangeTotals>()
        .fetch_one(&state.pool);

    let range_duration = range_end - range_start;
    let previous_range_start = range_start - range_duration;
    let previous_range_end = range_start;
    let previous_range_start_text = previous_range_start.to_rfc3339();
    let previous_range_end_text = previous_range_end.to_rfc3339();
    let mut previous_range_totals_sql = range_totals_query(
        filters,
        &previous_range_start_text,
        &previous_range_end_text,
        &tps_eligible,
        elapsed_ms,
    );
    let previous_range_totals_fut = previous_range_totals_sql
        .build_query_as::<RangeTotals>()
        .fetch_one(&state.pool);

    let session_totals_fut = async {
        if query.include_session_metrics {
            let mut session_sql = QueryBuilder::<Sqlite>::new(
                r#"
                SELECT
                    COUNT(*) AS sessions,
                    COALESCE(SUM(requests), 0) AS session_requests,
                    COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                    COALESCE(SUM(cache_read), 0) AS cache_read
                FROM (
                    SELECT u.session_id,
                           COUNT(*) AS requests,
                           COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
                           COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read
                    FROM usage_logs u
                    WHERE u.in_flight = 0
                      AND u.session_id IS NOT NULL AND TRIM(u.session_id) <> ''
                "#,
            );
            push_range_where(
                &mut session_sql,
                filters,
                &range_start_text,
                &range_end_text,
            );
            session_sql.push(" GROUP BY u.session_id )");
            session_sql
                .build_query_as::<SessionTotals>()
                .fetch_one(&state.pool)
                .await
        } else {
            sqlx::query_as::<_, SessionTotals>(
                "SELECT 0 AS sessions, 0 AS session_requests, \
                        0 AS prompt_tokens, 0 AS cache_read",
            )
            .fetch_one(&state.pool)
            .await
        }
    };

    let day_start_text = day_start.to_rfc3339();
    let day_end_text = (day_start + Duration::days(1)).to_rfc3339();
    let mut today_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT COUNT(*) AS requests, COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.estimated_cost_micros), 0) AS cost_micros,
               COALESCE(SUM(u.estimated_cost_micros IS NULL), 0) AS unpriced
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(&mut today_sql, filters, &day_start_text, &day_end_text);
    let today_fut = today_sql
        .build_query_as::<TodayTotals>()
        .fetch_one(&state.pool);

    let (totals, range_totals, previous_range_totals, session_totals, today) = tokio::try_join!(
        totals_fut,
        range_totals_fut,
        previous_range_totals_fut,
        session_totals_fut,
        today_fut
    )?;

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

    let mut recent_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms, u.status_code,
               u.in_flight, u.success, u.streamed, u.error_message, u.created_at,
               u.first_token_ms, u.session_id, u.warning_message,
               NULL AS request_preview, NULL AS response_preview,
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
    push_range_where(&mut recent_sql, filters, &range_start_text, &range_end_text);
    recent_sql.push(" ORDER BY u.created_at DESC, u.id DESC LIMIT 12");
    let recent_fut = recent_sql
        .build_query_as::<UsageLogDetailRow>()
        .fetch_all(&state.pool);

    let mut provider_usage_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT COALESCE(u.provider_id, 0) AS provider_id,
               COALESCE(p.name, '未知提供商') AS provider_name,
               COUNT(*) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               SUM(u.estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
                   AS cache_hit_rate
        FROM usage_logs u
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(
        &mut provider_usage_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    provider_usage_sql.push(" GROUP BY u.provider_id, p.name ORDER BY requests DESC, tokens DESC");
    let provider_usage_fut = provider_usage_sql
        .build_query_as::<ProviderUsage>()
        .fetch_all(&state.pool);

    let mut daily_rows_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT {bucket} AS day,
               COUNT(*) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               COALESCE(SUM(u.estimated_cost_micros), 0) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
                   AS success_rate,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
                   AS cache_hit_rate
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#,
    ));
    push_range_where(
        &mut daily_rows_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    daily_rows_sql.push(format!(" GROUP BY {bucket} ORDER BY day"));
    let daily_rows_fut = daily_rows_sql
        .build_query_as::<DailyUsage>()
        .fetch_all(&state.pool);

    let mut model_usage_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.requested_model AS model,
               COUNT(*) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               SUM(u.estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
                   AS cache_hit_rate
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(
        &mut model_usage_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    model_usage_sql.push(" GROUP BY u.requested_model ORDER BY tokens DESC, requests DESC");
    if !query.full_breakdown {
        model_usage_sql.push(" LIMIT 8");
    }
    let model_usage_fut = model_usage_sql
        .build_query_as::<ModelUsage>()
        .fetch_all(&state.pool);

    let mut daily_model_requests_sql = QueryBuilder::<Sqlite>::new(
        r#"
        WITH busiest AS (
            SELECT u.requested_model AS label
            FROM usage_logs u
            WHERE u.in_flight = 0
        "#,
    );
    push_range_where(
        &mut daily_model_requests_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    daily_model_requests_sql.push(
        r#"
            GROUP BY u.requested_model
            ORDER BY COUNT(*) DESC, u.requested_model
            LIMIT 6
        )
        SELECT "#,
    );
    daily_model_requests_sql.push(&bucket);
    daily_model_requests_sql.push(
        r#" AS day,
               CASE WHEN u.requested_model IN (SELECT label FROM busiest)
                    THEN u.requested_model ELSE '其他' END AS label,
               COUNT(*) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.estimated_cost_micros), 0) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
                   AS success_rate,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
                   AS cache_hit_rate
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(
        &mut daily_model_requests_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    daily_model_requests_sql.push(" GROUP BY day, label ORDER BY day, requests DESC");
    let daily_model_requests_fut = daily_model_requests_sql
        .build_query_as::<DailyBreakdownUsage>()
        .fetch_all(&state.pool);

    let mut daily_provider_requests_sql = QueryBuilder::<Sqlite>::new(
        r#"
        WITH busiest AS (
            SELECT u.provider_id
            FROM usage_logs u
            WHERE u.in_flight = 0 AND u.provider_id IS NOT NULL
        "#,
    );
    push_range_where(
        &mut daily_provider_requests_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    daily_provider_requests_sql.push(
        r#"
            GROUP BY u.provider_id
            ORDER BY COUNT(*) DESC, u.provider_id
            LIMIT 6
        )
        SELECT "#,
    );
    daily_provider_requests_sql.push(&bucket);
    daily_provider_requests_sql.push(
        r#" AS day,
               CASE WHEN u.provider_id IN (SELECT provider_id FROM busiest)
                    THEN COALESCE(p.name, '未知提供商') ELSE '其他' END AS label,
               COUNT(*) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.estimated_cost_micros), 0) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
                   AS success_rate,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
                   AS cache_hit_rate
        FROM usage_logs u
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(
        &mut daily_provider_requests_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    daily_provider_requests_sql.push(" GROUP BY day, label ORDER BY day, requests DESC");
    let daily_provider_requests_fut = daily_provider_requests_sql
        .build_query_as::<DailyBreakdownUsage>()
        .fetch_all(&state.pool);

    let mut latency_percentiles_sql =
        percentile_query("latency_ms", filters, &range_start_text, &range_end_text);
    let latency_percentiles_fut = latency_percentiles_sql
        .build_query_as::<Percentiles>()
        .fetch_one(&state.pool);
    let mut first_token_percentiles_sql = percentile_query(
        "first_token_ms",
        filters,
        &range_start_text,
        &range_end_text,
    );
    let first_token_percentiles_fut = first_token_percentiles_sql
        .build_query_as::<Percentiles>()
        .fetch_one(&state.pool);

    let mut status_breakdown_sql = QueryBuilder::<Sqlite>::new(
        "SELECT u.status_code, COUNT(*) AS count FROM usage_logs u WHERE u.in_flight = 0",
    );
    push_range_where(
        &mut status_breakdown_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    status_breakdown_sql.push(" GROUP BY u.status_code ORDER BY count DESC LIMIT 10");
    let status_breakdown_fut = status_breakdown_sql
        .build_query_as::<UsageStatusCount>()
        .fetch_all(&state.pool);

    let mut error_reasons_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT COALESCE(NULLIF(TRIM(u.error_message), ''), '无错误信息') AS label,
               COUNT(*) AS count
        FROM usage_logs u
        WHERE u.in_flight = 0 AND u.success = 0
        "#,
    );
    push_range_where(
        &mut error_reasons_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    error_reasons_sql.push(" GROUP BY label ORDER BY count DESC LIMIT 10");
    let error_reasons_fut = error_reasons_sql
        .build_query_as::<ErrorReason>()
        .fetch_all(&state.pool);

    let mut gateway_adjustments_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT CASE
                   WHEN u.warning_message LIKE '%incomplete tool-history%' THEN 'tool_history'
                   WHEN u.warning_message LIKE '%CommandCode compatibility normalization%' THEN 'compat'
                   WHEN u.warning_message LIKE 'budget % excluded %' THEN 'budget'
                   WHEN u.warning_message LIKE 'provider pool allocation:%' THEN 'pool'
                   ELSE 'other'
               END AS category,
               COUNT(*) AS count
        FROM usage_logs u
        WHERE u.in_flight = 0
          AND u.warning_message IS NOT NULL AND TRIM(u.warning_message) <> ''
        "#,
    );
    push_range_where(
        &mut gateway_adjustments_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    gateway_adjustments_sql.push(" GROUP BY category ORDER BY count DESC");
    let gateway_adjustments_fut = gateway_adjustments_sql
        .build_query_as::<GatewayAdjustment>()
        .fetch_all(&state.pool);

    let (
        recent,
        provider_usage,
        daily_rows,
        model_usage,
        daily_model_requests,
        daily_provider_requests,
    ) = tokio::try_join!(
        recent_fut,
        provider_usage_fut,
        daily_rows_fut,
        model_usage_fut,
        daily_model_requests_fut,
        daily_provider_requests_fut
    )?;

    let (
        latency_percentiles,
        first_token_percentiles,
        status_breakdown,
        error_reasons,
        gateway_adjustments,
    ) = tokio::try_join!(
        latency_percentiles_fut,
        first_token_percentiles_fut,
        status_breakdown_fut,
        error_reasons_fut,
        gateway_adjustments_fut
    )?;

    // Fill in buckets with no traffic so the chart has a continuous axis
    // instead of collapsing to only the buckets that saw requests.
    let local_range_start = range_start + Duration::minutes(tz_offset);
    let local_range_end = range_end + Duration::minutes(tz_offset);
    let mut daily_usage = Vec::new();
    match granularity {
        ResolvedGranularity::Hour => {
            let mut cursor = local_range_start
                .with_minute(0)
                .and_then(|value| value.with_second(0))
                .and_then(|value| value.with_nanosecond(0))
                .unwrap_or(local_range_start);
            while cursor < local_range_end {
                push_usage_bucket(
                    &mut daily_usage,
                    &daily_rows,
                    cursor.format("%Y-%m-%d %H:00").to_string(),
                );
                cursor += Duration::hours(1);
            }
        }
        ResolvedGranularity::Day => {
            let first_day = local_range_start.date_naive();
            let last_day = (local_range_end - Duration::nanoseconds(1)).date_naive();
            let day_count = (last_day - first_day).num_days() + 1;
            daily_usage.reserve(day_count.max(0) as usize);
            for offset in 0..day_count {
                push_usage_bucket(
                    &mut daily_usage,
                    &daily_rows,
                    (first_day + Duration::days(offset))
                        .format("%Y-%m-%d")
                        .to_string(),
                );
            }
        }
        ResolvedGranularity::Week => {
            let first_day = local_range_start.date_naive()
                - Duration::days(local_range_start.weekday().num_days_from_monday() as i64);
            let last_day = (local_range_end - Duration::nanoseconds(1)).date_naive();
            let mut cursor = first_day;
            while cursor <= last_day {
                push_usage_bucket(
                    &mut daily_usage,
                    &daily_rows,
                    cursor.format("%Y-%m-%d").to_string(),
                );
                cursor += Duration::days(7);
            }
        }
        ResolvedGranularity::Month => {
            let first_day = local_range_start
                .date_naive()
                .with_day(1)
                .unwrap_or(local_range_start.date_naive());
            let last_day = (local_range_end - Duration::nanoseconds(1)).date_naive();
            let mut cursor = first_day;
            while cursor <= last_day {
                push_usage_bucket(
                    &mut daily_usage,
                    &daily_rows,
                    cursor.format("%Y-%m-%d").to_string(),
                );
                cursor = cursor
                    .checked_add_months(Months::new(1))
                    .unwrap_or(last_day + Duration::days(1));
            }
        }
    }

    let range_requests = range_totals.requests;
    let range_sessions = session_totals.sessions;
    let range_session_requests = session_totals.session_requests;
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
    let output_tps = |tokens: i64, elapsed_ms: i64| {
        if tokens > 0 && elapsed_ms > 0 {
            Some(tokens as f64 / (elapsed_ms as f64 / 1000.0))
        } else {
            None
        }
    };
    let range_output_tps = output_tps(range_totals.tps_tokens, range_totals.tps_elapsed_ms);
    let previous_range = OverviewComparison {
        from: previous_range_start_text,
        to: previous_range_end_text,
        requests: previous_range_totals.requests,
        tokens: previous_range_totals.tokens,
        prompt_tokens: previous_range_totals.prompt_tokens,
        completion_tokens: previous_range_totals.completion_tokens,
        cache_read: previous_range_totals.cache_read,
        cache_write: previous_range_totals.cache_write,
        cache_hit_rate: cache_hit_rate(
            previous_range_totals.prompt_tokens,
            previous_range_totals.cache_read,
        ),
        cost_micros: previous_range_totals.cost_micros,
        unpriced: previous_range_totals.unpriced,
        success_rate: previous_range_totals.success_rate,
        avg_latency_ms: previous_range_totals.avg_latency_ms,
        output_tps: output_tps(
            previous_range_totals.tps_tokens,
            previous_range_totals.tps_elapsed_ms,
        ),
        gateway_adjusted: previous_range_totals.gateway_adjusted,
    };

    Ok(Json(Overview {
        requests_today: today.requests,
        tokens_today: today.tokens,
        prompt_tokens_today: today.prompt_tokens,
        completion_tokens_today: today.completion_tokens,
        cache_read_today: today.cache_read,
        cache_write_today: today.cache_write,
        cache_hit_rate: cache_hit_rate(today.prompt_tokens, today.cache_read),
        requests_total: totals.get("requests_total"),
        tokens_total: totals.get("tokens_total"),
        prompt_tokens_total: totals.get("prompt_tokens_total"),
        completion_tokens_total: totals.get("completion_tokens_total"),
        cache_read_total: totals.get("cache_read_total"),
        cache_write_total: totals.get("cache_write_total"),
        cost_today_micros: today.cost_micros,
        cost_total_micros: totals.get("cost_total_micros"),
        unpriced_today: today.unpriced,
        unpriced_total: totals.get("unpriced_total"),
        range_requests,
        range_tokens: range_totals.tokens,
        range_prompt_tokens: range_totals.prompt_tokens,
        range_completion_tokens: range_totals.completion_tokens,
        range_cache_read: range_totals.cache_read,
        range_cache_write: range_totals.cache_write,
        range_cache_hit_rate: cache_hit_rate(range_totals.prompt_tokens, range_totals.cache_read),
        range_sessions,
        range_session_coverage,
        range_avg_requests_per_session,
        range_session_cache_hit_rate: cache_hit_rate(
            session_totals.prompt_tokens,
            session_totals.cache_read,
        ),
        range_cost_micros: range_totals.cost_micros,
        range_unpriced: range_totals.unpriced,
        range_success_rate: range_totals.success_rate,
        range_avg_latency_ms: range_totals.avg_latency_ms,
        range_latency_p50_ms: latency_percentiles.p50,
        range_latency_p95_ms: latency_percentiles.p95,
        range_latency_p99_ms: latency_percentiles.p99,
        range_first_token_p50_ms: first_token_percentiles.p50,
        range_first_token_p95_ms: first_token_percentiles.p95,
        range_first_token_p99_ms: first_token_percentiles.p99,
        range_output_tps,
        range_gateway_adjusted: range_totals.gateway_adjusted,
        previous_range,
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
        granularity: granularity.as_str(),
        daily_usage,
        daily_model_requests,
        daily_provider_requests,
        status_breakdown,
        error_reasons,
        gateway_adjustments,
    }))
}

/// Drill-down tables for the analytics page.
///
/// Kept off `/api/overview` so opening the analytics landing page does not pay
/// for five additional GROUP BY scans. The console loads this lazily when the
/// dimension tab is opened, using exactly the same range and filters.
pub async fn analytics_dimensions(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> AppResult<Json<AnalyticsDimensions>> {
    let window = overview_window(&query)?;
    let range_start_text = window.range_start.to_rfc3339();
    let range_end_text = window.range_end.to_rfc3339();
    let filters = query.filters();
    let filters = &filters;
    let metrics = r#"
        COUNT(*) AS requests,
        COALESCE(SUM(u.total_tokens), 0) AS tokens,
        COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
        COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
        SUM(u.estimated_cost_micros) AS cost_micros,
        COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
            AS success_rate,
        COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms,
        COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
        COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
        COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
            AS cache_hit_rate
    "#;

    let mut api_key_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT u.api_key_id AS id,
               COALESCE(k.name, '未知访问密钥') AS label,
               {metrics}
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(
        &mut api_key_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    api_key_sql.push(
        " GROUP BY u.api_key_id, k.name \
          ORDER BY cost_micros DESC, tokens DESC, requests DESC LIMIT 100",
    );

    let mut provider_key_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT u.provider_api_key_id AS id,
               COALESCE(p.name, '未知提供商') || ' · ' ||
               COALESCE(NULLIF(u.provider_api_key_name, ''), pk.name, '未知密钥') AS label,
               {metrics}
        FROM usage_logs u
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(
        &mut provider_key_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    provider_key_sql.push(
        " GROUP BY u.provider_api_key_id, p.name, u.provider_api_key_name, pk.name \
          ORDER BY cost_micros DESC, tokens DESC, requests DESC LIMIT 100",
    );

    let mut route_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT u.route_id AS id,
               COALESCE(r.name, '未知路由') AS label,
               {metrics}
        FROM usage_logs u
        LEFT JOIN routes r ON r.id = u.route_id
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(&mut route_sql, filters, &range_start_text, &range_end_text);
    route_sql.push(
        " GROUP BY u.route_id, r.name \
          ORDER BY cost_micros DESC, tokens DESC, requests DESC LIMIT 100",
    );

    let mut endpoint_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT NULL AS id,
               u.endpoint AS label,
               {metrics}
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(
        &mut endpoint_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    endpoint_sql.push(
        " GROUP BY u.endpoint \
          ORDER BY cost_micros DESC, tokens DESC, requests DESC LIMIT 100",
    );

    let mut session_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.session_id AS session_id,
               COUNT(*) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               SUM(u.estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
                   AS success_rate,
               COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(u.cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(u.cache_read_tokens) * 100.0 / NULLIF(SUM(u.prompt_tokens), 0), 0.0)
                   AS cache_hit_rate,
               MAX(u.created_at) AS last_seen
        FROM usage_logs u
        WHERE u.in_flight = 0
          AND u.session_id IS NOT NULL AND TRIM(u.session_id) <> ''
        "#,
    );
    push_range_where(
        &mut session_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    session_sql.push(
        " GROUP BY u.session_id \
          ORDER BY cost_micros DESC, tokens DESC, requests DESC LIMIT 20",
    );

    let (api_key_usage, provider_api_key_usage, route_usage, endpoint_usage, session_usage) = tokio::try_join!(
        api_key_sql
            .build_query_as::<DimensionUsage>()
            .fetch_all(&state.pool),
        provider_key_sql
            .build_query_as::<DimensionUsage>()
            .fetch_all(&state.pool),
        route_sql
            .build_query_as::<DimensionUsage>()
            .fetch_all(&state.pool),
        endpoint_sql
            .build_query_as::<DimensionUsage>()
            .fetch_all(&state.pool),
        session_sql
            .build_query_as::<SessionUsage>()
            .fetch_all(&state.pool),
    )?;

    Ok(Json(AnalyticsDimensions {
        api_key_usage,
        provider_api_key_usage,
        route_usage,
        endpoint_usage,
        session_usage,
    }))
}

/// Routing quality aggregates for the analytics page.
///
/// This is deliberately separate from `/api/overview` and `/analytics/dimensions`:
/// it joins the trace tables, which are only useful when the operator opens the
/// route-quality tab.
pub async fn analytics_route_quality(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> AppResult<Json<RouteQuality>> {
    let window = overview_window(&query)?;
    let range_start_text = window.range_start.to_rfc3339();
    let range_end_text = window.range_end.to_rfc3339();
    let filters = query.filters();
    let filters = &filters;
    let from_traces = r#"
        FROM routing_traces rt
        JOIN usage_logs u ON u.request_id = rt.request_id
    "#;
    let metrics = r#"
        COUNT(*) AS traces,
        COALESCE(AVG(CASE WHEN rt.fallback_count > 0 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
            AS fallback_rate,
        COALESCE(AVG(rt.attempt_count), 0.0) AS avg_attempts,
        COALESCE(AVG(CASE WHEN rt.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
            AS success_rate
    "#;

    let mut totals_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT
            COUNT(*) AS traces,
            COALESCE(AVG(CASE WHEN rt.fallback_count > 0 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
                AS fallback_rate,
            COALESCE(AVG(rt.attempt_count), 0.0) AS avg_attempts,
            COALESCE(SUM(CASE WHEN rt.session_id IS NOT NULL
                              AND TRIM(rt.session_id) <> ''
                              AND rt.session_decision <> 'no_affinity' THEN 1 ELSE 0 END), 0)
                AS session_requests,
            COALESCE(SUM(CASE WHEN rt.session_id IS NOT NULL
                              AND TRIM(rt.session_id) <> ''
                              AND rt.session_decision = 'hit' THEN 1 ELSE 0 END), 0)
                AS session_hits,
            COALESCE(AVG(CASE WHEN rt.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0)
                AS success_rate
        {from_traces}
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(&mut totals_sql, filters, &range_start_text, &range_end_text);

    let mut strategies_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT rt.strategy AS strategy, {metrics}
        {from_traces}
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(
        &mut strategies_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    strategies_sql.push(" GROUP BY rt.strategy ORDER BY traces DESC, rt.strategy");

    let mut routes_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT rt.route_id AS route_id,
               COALESCE(r.name, '未命名路由') AS route_name,
               {metrics}
        {from_traces}
        LEFT JOIN routes r ON r.id = rt.route_id
        WHERE u.in_flight = 0
        "#
    ));
    push_range_where(&mut routes_sql, filters, &range_start_text, &range_end_text);
    routes_sql.push(" GROUP BY rt.route_id, r.name ORDER BY traces DESC, rt.route_id LIMIT 20");

    let mut decisions_sql = QueryBuilder::<Sqlite>::new(format!(
        r#"
        SELECT rt.session_decision AS decision, COUNT(*) AS count
        {from_traces}
        WHERE u.in_flight = 0
          AND rt.session_id IS NOT NULL AND TRIM(rt.session_id) <> ''
        "#
    ));
    push_range_where(
        &mut decisions_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    decisions_sql.push(" GROUP BY rt.session_decision ORDER BY count DESC, decision");

    let mut failures_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT ra.reason_code AS reason_code,
               COALESCE(ra.failure_scope, 'unknown') AS failure_scope,
               COUNT(*) AS count
        FROM routing_attempts ra
        JOIN routing_traces rt ON rt.request_id = ra.request_id
        JOIN usage_logs u ON u.request_id = rt.request_id
        WHERE u.in_flight = 0 AND ra.success = 0
        "#,
    );
    push_range_where(
        &mut failures_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    failures_sql.push(
        " GROUP BY ra.reason_code, failure_scope \
          ORDER BY count DESC, ra.reason_code LIMIT 20",
    );

    let (totals, strategies, routes, session_decisions, failure_reasons) = tokio::try_join!(
        totals_sql
            .build_query_as::<RouteQualityTotals>()
            .fetch_one(&state.pool),
        strategies_sql
            .build_query_as::<RouteQualityStrategy>()
            .fetch_all(&state.pool),
        routes_sql
            .build_query_as::<RouteQualityRoute>()
            .fetch_all(&state.pool),
        decisions_sql
            .build_query_as::<RouteQualityDecision>()
            .fetch_all(&state.pool),
        failures_sql
            .build_query_as::<RouteQualityFailure>()
            .fetch_all(&state.pool),
    )?;

    let session_hit_rate = if totals.session_requests > 0 {
        totals.session_hits as f64 * 100.0 / totals.session_requests as f64
    } else {
        0.0
    };

    Ok(Json(RouteQuality {
        traces: totals.traces,
        fallback_rate: totals.fallback_rate,
        avg_attempts: totals.avg_attempts,
        session_requests: totals.session_requests,
        session_hits: totals.session_hits,
        session_hit_rate,
        success_rate: totals.success_rate,
        strategies,
        routes,
        session_decisions,
        failure_reasons,
    }))
}
