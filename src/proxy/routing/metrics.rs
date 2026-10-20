use super::*;

#[derive(Debug, sqlx::FromRow)]
pub(super) struct RuntimeTargetMetricRow {
    pub(super) provider_id: i64,
    pub(super) upstream_model: String,
    pub(super) requests: i64,
    pub(super) prompt_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) avg_latency_ms: Option<f64>,
    /// Nearest-rank 95th percentile first-token (or total) latency.
    pub(super) p95_latency_ms: Option<f64>,
    /// Rows that finished successfully in the window.
    pub(super) successes: i64,
    /// Finished rows that failed because of the upstream (5xx, rate limit or a
    /// transport error). Client-side rejections are not counted, so a caller
    /// sending broken requests cannot make a healthy provider look unreliable.
    pub(super) upstream_faults: i64,
}

impl RuntimeTargetMetricRow {
    /// Recent success rate over the upstream-caused outcomes, or `None` when
    /// there is not enough evidence to judge.
    pub(super) fn success_rate(&self) -> Option<f64> {
        let sample = self.successes + self.upstream_faults;
        (sample > 0).then(|| self.successes as f64 / sample as f64)
    }

    pub(super) fn has_cache_evidence(&self) -> bool {
        self.cache_roi().is_some()
    }

    pub(super) fn cache_hit_rate(&self) -> Option<f64> {
        (self.prompt_tokens > 0 && self.cache_read_tokens > 0)
            .then(|| (self.cache_read_tokens as f64 / self.prompt_tokens as f64).clamp(0.0, 1.0))
    }

    /// Estimates whether recent prompt-cache traffic is likely to be useful.
    ///
    /// A high hit rate means more prompt tokens were served from cache, while
    /// the read/write ratio keeps a target from looking attractive when it
    /// mostly paid to establish a cache that was rarely reused.
    pub(super) fn cache_roi(&self) -> Option<f64> {
        let reads = self.cache_read_tokens.max(0) as f64;
        if reads <= 0.0 {
            return None;
        }
        let writes = self.cache_write_tokens.max(0) as f64;
        let hit_rate = self.cache_hit_rate()?;
        let reuse_ratio = reads / (reads + writes).max(1.0);
        Some((hit_rate * 0.7 + reuse_ratio * 0.3).clamp(0.0, 1.0))
    }
}

pub(super) async fn runtime_target_metrics(
    state: &AppState,
    targets: &[RouteTarget],
    include_p95: bool,
) -> AppResult<HashMap<(i64, String), RuntimeTargetMetricRow>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let provider_ids = targets
        .iter()
        .map(|target| target.provider_id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        r#"
        SELECT provider_id,
               upstream_model,
               COUNT(*) AS requests,
               SUM(prompt_tokens) AS prompt_tokens,
               SUM(cache_read_tokens) AS cache_read_tokens,
               SUM(cache_write_tokens) AS cache_write_tokens,
               AVG(
                   CASE
                       WHEN success = 1
                            AND first_token_ms IS NOT NULL
                            AND first_token_ms > 0
                           THEN first_token_ms
                       WHEN success = 1 AND latency_ms > 0
                           THEN latency_ms
                       ELSE NULL
                   END
               ) AS avg_latency_ms,
               NULL AS p95_latency_ms,
               SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS successes,
               SUM(
                   CASE
                       WHEN success = 0
                            AND (status_code >= 500 OR status_code = 429 OR status_code = 0)
                           THEN 1
                       ELSE 0
                   END
               ) AS upstream_faults
        FROM usage_logs
        WHERE in_flight = 0
          AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')
          AND provider_id IN (
        "#,
    );
    {
        let mut separated = query.separated(", ");
        for provider_id in provider_ids {
            separated.push_bind(provider_id);
        }
    }
    query.push(
        r#")
        GROUP BY provider_id, upstream_model
        "#,
    );
    let rows = query
        .build_query_as::<RuntimeTargetMetricRow>()
        .fetch_all(&state.pool)
        .await?;
    let mut metrics = rows
        .into_iter()
        .map(|row| ((row.provider_id, row.upstream_model.clone()), row))
        .collect::<HashMap<_, _>>();
    if include_p95 {
        let p95 = runtime_target_p95_latency(state, targets).await?;
        for (key, value) in p95 {
            if let Some(metric) = metrics.get_mut(&key) {
                metric.p95_latency_ms = Some(value);
            }
        }
    }
    Ok(metrics)
}

/// Nearest-rank P95 for the same one-day success window as the average metric.
///
/// The query only runs for the P95 strategy, so the extra window-function pass
/// never enters the hot path of the other strategies.
pub(super) async fn runtime_target_p95_latency(
    state: &AppState,
    targets: &[RouteTarget],
) -> AppResult<HashMap<(i64, String), f64>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let provider_ids = targets
        .iter()
        .map(|target| target.provider_id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        r#"
        WITH samples AS (
            SELECT provider_id,
                   upstream_model,
                   CASE
                       WHEN first_token_ms IS NOT NULL AND first_token_ms > 0
                           THEN first_token_ms
                       ELSE latency_ms
                   END AS sample
            FROM usage_logs
            WHERE in_flight = 0
              AND success = 1
              AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')
              AND (first_token_ms > 0 OR latency_ms > 0)
              AND provider_id IN (
        "#,
    );
    {
        let mut separated = query.separated(", ");
        for provider_id in provider_ids {
            separated.push_bind(provider_id);
        }
    }
    query.push(
        r#")
        ),
        ranked AS (
            SELECT provider_id,
                   upstream_model,
                   sample,
                   ROW_NUMBER() OVER (
                       PARTITION BY provider_id, upstream_model ORDER BY sample
                   ) AS rank,
                   COUNT(*) OVER (
                       PARTITION BY provider_id, upstream_model
                   ) AS total
            FROM samples
        )
        SELECT provider_id,
               upstream_model,
               CAST(MAX(sample) AS REAL) AS p95_latency_ms
        FROM ranked
        WHERE rank >= (total * 95 + 99) / 100
        GROUP BY provider_id, upstream_model
        "#,
    );
    let rows = query
        .build_query_as::<(i64, String, f64)>()
        .fetch_all(&state.pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(provider_id, model, latency)| ((provider_id, model), latency))
        .collect())
}

pub(super) fn target_cost_prices(target: &RouteTarget) -> (Option<f64>, Option<f64>) {
    let cost = target
        .cost
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let Some(cost) = effective_cost_value(
        cost.as_ref(),
        target.cost_input_override,
        target.cost_output_override,
        target.cost_cache_read_override,
        target.cost_cache_write_override,
    ) else {
        return (None, None);
    };
    (
        crate::models::cost_base_price(&cost, "input"),
        crate::models::cost_base_price(&cost, "output"),
    )
}

pub(super) fn target_cost_score(target: &RouteTarget) -> Option<f64> {
    let (input, output) = target_cost_prices(target);
    match (input, output) {
        (Some(input), Some(output)) => Some(input * 0.7 + output * 0.3),
        (Some(input), None) => Some(input),
        (None, Some(output)) => Some(output),
        (None, None) => None,
    }
}
