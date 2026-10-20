use super::*;

/// Share of prompt tokens served from cache, as a percentage.
///
/// `prompt_tokens` must be the total input count (fresh + cached), so the ratio
/// cannot exceed 100. A zero denominator yields 0 rather than NaN so the
/// dashboard needs no special case.
pub(in crate::api) fn cache_hit_rate(prompt_tokens: i64, cache_read: i64) -> f64 {
    if prompt_tokens <= 0 {
        return 0.0;
    }
    (cache_read as f64 / prompt_tokens as f64 * 100.0).clamp(0.0, 100.0)
}

pub(in crate::api) fn parse_overview_time(
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

pub(in crate::api) fn normalize_overview_range(
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
pub(super) fn push_usage_bucket(
    buckets: &mut Vec<DailyUsage>,
    rows: &[DailyUsage],
    bucket: String,
) {
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
pub(super) struct RangeTotals {
    pub(super) requests: i64,
    pub(super) tokens: i64,
    pub(super) prompt_tokens: i64,
    pub(super) completion_tokens: i64,
    pub(super) cache_read: i64,
    pub(super) cache_write: i64,
    pub(super) cost_micros: i64,
    pub(super) unpriced: i64,
    pub(super) success_rate: f64,
    pub(super) avg_latency_ms: f64,
    pub(super) gateway_adjusted: i64,
    pub(super) tps_tokens: i64,
    pub(super) tps_elapsed_ms: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct SessionTotals {
    pub(super) sessions: i64,
    pub(super) session_requests: i64,
    pub(super) prompt_tokens: i64,
    pub(super) cache_read: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct TodayTotals {
    pub(super) requests: i64,
    pub(super) tokens: i64,
    pub(super) prompt_tokens: i64,
    pub(super) completion_tokens: i64,
    pub(super) cache_read: i64,
    pub(super) cache_write: i64,
    pub(super) cost_micros: i64,
    pub(super) unpriced: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct Percentiles {
    pub(super) p50: Option<i64>,
    pub(super) p95: Option<i64>,
    pub(super) p99: Option<i64>,
}

pub(super) fn push_range_where<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    filters: &'a UsageFilters,
    start: &'a str,
    end: &'a str,
) {
    builder.push(" AND u.created_at >= ").push_bind(start);
    builder.push(" AND u.created_at < ").push_bind(end);
    apply_usage_filters(builder, filters);
}

pub(super) fn percentile_query<'a>(
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

pub(super) struct OverviewWindow {
    pub(super) tz_offset: i64,
    pub(super) day_start: DateTime<Utc>,
    pub(super) range_start: DateTime<Utc>,
    pub(super) range_end: DateTime<Utc>,
    pub(super) granularity: ResolvedGranularity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolvedGranularity {
    Hour,
    Day,
    Week,
    Month,
}

impl ResolvedGranularity {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    pub(super) fn sql_bucket(self, tz_offset: i64) -> String {
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

pub(super) fn resolve_granularity(
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

pub(super) fn overview_window(query: &OverviewQuery) -> AppResult<OverviewWindow> {
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

pub(super) fn range_totals_query<'a>(
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
