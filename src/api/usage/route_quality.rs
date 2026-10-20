use super::*;

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
