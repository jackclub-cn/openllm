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
               u.context_breakdown_json, u.cache_usage_reported,
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

pub(in crate::api) const USAGE_EXPORT_LIMIT: i64 = 100_000;

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
               u.context_breakdown_json, u.cache_usage_reported,
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

pub(in crate::api) fn usage_csv_row(item: &UsageLogView) -> String {
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

pub(in crate::api) fn csv_field(value: &str) -> String {
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
