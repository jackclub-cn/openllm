use super::*;

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
