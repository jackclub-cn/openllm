use super::*;

pub(crate) struct UsageLogEntry<'a> {
    pub(in crate::proxy) request_id: &'a str,
    pub(in crate::proxy) api_key_id: Option<i64>,
    pub(in crate::proxy) route_id: Option<i64>,
    pub(in crate::proxy) provider_id: Option<i64>,
    pub(in crate::proxy) requested_model: &'a str,
    pub(in crate::proxy) upstream_model: Option<&'a str>,
    pub(in crate::proxy) endpoint: &'a str,
    pub(in crate::proxy) usage: Usage,
    pub(in crate::proxy) latency_ms: i64,
    pub(in crate::proxy) first_token_ms: Option<i64>,
    pub(in crate::proxy) status_code: i64,
    pub(in crate::proxy) success: bool,
    pub(in crate::proxy) streamed: bool,
    pub(in crate::proxy) error_message: Option<&'a str>,
    pub(in crate::proxy) response_preview: Option<&'a str>,
}

/// Owned mirror of [`UsageLogEntry`]; needed when the log is written from a
/// detached task that outlives the request's borrowed values.
pub(crate) struct OwnedUsageLogEntry {
    pub(in crate::proxy) request_id: String,
    pub(in crate::proxy) api_key_id: Option<i64>,
    pub(in crate::proxy) route_id: Option<i64>,
    pub(in crate::proxy) provider_id: Option<i64>,
    pub(in crate::proxy) requested_model: String,
    pub(in crate::proxy) upstream_model: Option<String>,
    pub(in crate::proxy) endpoint: String,
    pub(in crate::proxy) usage: Usage,
    pub(in crate::proxy) latency_ms: i64,
    pub(in crate::proxy) first_token_ms: Option<i64>,
    pub(in crate::proxy) status_code: i64,
    pub(in crate::proxy) success: bool,
    pub(in crate::proxy) streamed: bool,
    pub(in crate::proxy) error_message: Option<String>,
    pub(in crate::proxy) response_preview: Option<String>,
}

impl OwnedUsageLogEntry {
    pub(in crate::proxy) fn as_borrowed(&self) -> UsageLogEntry<'_> {
        UsageLogEntry {
            request_id: &self.request_id,
            api_key_id: self.api_key_id,
            route_id: self.route_id,
            provider_id: self.provider_id,
            requested_model: &self.requested_model,
            upstream_model: self.upstream_model.as_deref(),
            endpoint: &self.endpoint,
            usage: self.usage,
            latency_ms: self.latency_ms,
            first_token_ms: self.first_token_ms,
            status_code: self.status_code,
            success: self.success,
            streamed: self.streamed,
            error_message: self.error_message.as_deref(),
            response_preview: self.response_preview.as_deref(),
        }
    }
}

/// Writes the usage row off the response path so a slow SQLite write lock does
/// not inflate the latency the caller observes.
pub(crate) fn log_usage_detached(state: AppState, entry: OwnedUsageLogEntry) {
    tokio::spawn(async move {
        log_usage(&state, entry.as_borrowed()).await;
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn log_usage_started(
    state: &AppState,
    request_id: &str,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
    request_preview: Option<&str>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, session_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, request_preview, response_preview,
            last_activity_at
        ) VALUES (
            ?, ?, ?, ?, NULL, ?, NULL, ?, ?, 0, ?, 0, 0, 0, NULL, NULL, 0, 1, 0, ?,
            NULL, ?, NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        )
        ON CONFLICT(request_id) DO UPDATE SET
            session_id = COALESCE(excluded.session_id, usage_logs.session_id),
            route_id = excluded.route_id,
            requested_model = excluded.requested_model,
            endpoint = excluded.endpoint,
            prompt_tokens = excluded.prompt_tokens,
            total_tokens = excluded.total_tokens,
            streamed = excluded.streamed,
            request_preview = COALESCE(excluded.request_preview, usage_logs.request_preview),
            last_activity_at = excluded.last_activity_at,
            in_flight = 1
        WHERE usage_logs.in_flight = 1
        "#,
    )
    .bind(request_id)
    .bind(session_id)
    .bind(api_key_id)
    .bind(route_id)
    .bind(requested_model)
    .bind(endpoint)
    .bind(request_tokens)
    .bind(request_tokens)
    .bind(streamed as i64)
    .bind(request_preview)
    .execute(&state.pool)
    .await;

    match result {
        Ok(result) => {
            if result.rows_affected() > 0 {
                match sqlx::query_scalar::<_, i64>("SELECT id FROM usage_logs WHERE request_id = ?")
                    .bind(request_id)
                    .fetch_one(&state.pool)
                    .await
                {
                    Ok(id) => {
                        let _ = state.events.send(crate::state::UsageEvent {
                            id,
                            request_id: request_id.to_string(),
                            success: false,
                            streamed,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, request_id, "failed to find in-flight usage row");
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, request_id, "failed to write in-flight usage log");
        }
    }
}

pub(crate) async fn log_usage_target(
    state: &AppState,
    request_id: &str,
    provider_id: i64,
    upstream_model: &str,
    provider_api_key_id: Option<i64>,
) {
    if let Err(error) = sqlx::query(
        "UPDATE usage_logs SET provider_id = ?, provider_api_key_id = ?, \
             provider_api_key_name = (SELECT name FROM provider_api_keys WHERE id = ?), \
             upstream_model = ?, \
             last_activity_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE request_id = ? AND in_flight = 1",
    )
    .bind(provider_id)
    .bind(provider_api_key_id)
    .bind(provider_api_key_id)
    .bind(upstream_model)
    .bind(request_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(%error, request_id, "failed to update in-flight usage target");
    }
}

pub(crate) async fn log_usage_warning(state: &AppState, request_id: &str, warning: &str) {
    let warning = warning.chars().take(500).collect::<String>();
    if let Err(error) = sqlx::query(
        "UPDATE usage_logs \
         SET warning_message = CASE \
             WHEN warning_message IS NULL OR warning_message = '' THEN ? \
             WHEN instr(warning_message, ?) > 0 THEN warning_message \
             ELSE warning_message || char(10) || ? \
         END \
         WHERE request_id = ?",
    )
    .bind(&warning)
    .bind(&warning)
    .bind(&warning)
    .bind(request_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(%error, request_id, "failed to update usage warning");
    }
}

pub(crate) async fn log_usage(state: &AppState, entry: UsageLogEntry<'_>) {
    let usage = entry.usage.normalized();
    if entry.success
        && entry.streamed
        && let (Some(provider_id), Some(upstream_model)) = (entry.provider_id, entry.upstream_model)
    {
        record_target_latency_success(state, provider_id, upstream_model, entry.first_token_ms)
            .await;
    }
    // Tokens are exact only once the response is complete, so the pool is
    // charged here rather than at admission (requests are charged then).
    crate::allocation::charge_token_usage(
        state,
        entry.provider_id,
        entry.api_key_id,
        usage.total_tokens,
    )
    .await;
    let estimated_cost_micros =
        if matches!(entry.status_code, 400 | 403 | 429) && entry.provider_id.is_none() {
            Some(0)
        } else {
            match (entry.provider_id, entry.upstream_model) {
                (Some(provider_id), Some(upstream_model)) if usage.has_tokens() => {
                    estimate_usage_cost(state, provider_id, upstream_model, usage).await
                }
                _ => None,
            }
        };
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, response_preview, last_activity_at
        ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        )
        ON CONFLICT(request_id) DO UPDATE SET
            api_key_id = excluded.api_key_id,
            route_id = excluded.route_id,
            provider_id = excluded.provider_id,
            requested_model = excluded.requested_model,
            upstream_model = excluded.upstream_model,
            endpoint = excluded.endpoint,
            prompt_tokens = excluded.prompt_tokens,
            completion_tokens = excluded.completion_tokens,
            total_tokens = excluded.total_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cache_write_tokens = excluded.cache_write_tokens,
            latency_ms = excluded.latency_ms,
            estimated_cost_micros = excluded.estimated_cost_micros,
            first_token_ms = excluded.first_token_ms,
            status_code = excluded.status_code,
            in_flight = 0,
            success = excluded.success,
            streamed = excluded.streamed,
            error_message = excluded.error_message,
            response_preview = excluded.response_preview,
            last_activity_at = excluded.last_activity_at
        "#,
    )
    .bind(entry.request_id)
    .bind(entry.api_key_id)
    .bind(entry.route_id)
    .bind(entry.provider_id)
    .bind(entry.requested_model)
    .bind(entry.upstream_model)
    .bind(entry.endpoint)
    .bind(usage.prompt_tokens)
    .bind(usage.completion_tokens)
    .bind(usage.total_tokens)
    .bind(usage.cache_read_tokens)
    .bind(usage.cache_write_tokens)
    .bind(entry.latency_ms)
    .bind(estimated_cost_micros)
    .bind(entry.first_token_ms)
    .bind(entry.status_code)
    .bind(entry.success as i64)
    .bind(entry.streamed as i64)
    .bind(entry.error_message)
    .bind(entry.response_preview)
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => {
            let id = sqlx::query_scalar::<_, i64>("SELECT id FROM usage_logs WHERE request_id = ?")
                .bind(entry.request_id)
                .fetch_one(&state.pool)
                .await;
            match id {
                Ok(id) => {
                    let _ = state.events.send(crate::state::UsageEvent {
                        id,
                        request_id: entry.request_id.to_string(),
                        success: entry.success,
                        streamed: entry.streamed,
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_id = entry.request_id,
                        "failed to read usage log id after update"
                    );
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, request_id = %entry.request_id, "failed to write usage log");
        }
    }
}

pub(crate) async fn estimate_usage_cost(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
    usage: Usage,
) -> Option<i64> {
    let row = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
        ),
    >(
        "SELECT cost, cost_input_override, cost_output_override, \
                cost_cache_read_override, cost_cache_write_override \
         FROM provider_models \
         WHERE provider_id = ? AND model_name = ? AND enabled = 1",
    )
    .bind(provider_id)
    .bind(upstream_model)
    .fetch_optional(&state.pool)
    .await
    .ok()??;
    let synced = row
        .0
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let cost = effective_cost_value(synced.as_ref(), row.1, row.2, row.3, row.4)?;
    estimate_cost_micros(Some(&cost), usage)
}
