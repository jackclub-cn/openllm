use super::*;

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
