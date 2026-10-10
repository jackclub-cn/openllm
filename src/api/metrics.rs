use super::*;
use std::fmt::Write as _;
use std::time::Instant;

#[derive(Debug, sqlx::FromRow)]
struct LifetimeMetricsRow {
    requests: i64,
    tokens: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cost_micros: i64,
    unpriced_requests: i64,
    successful_requests: i64,
    latency_ms_sum: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct StatusMetricsRow {
    providers_healthy: i64,
    providers_failed: i64,
    providers_untested: i64,
    providers_disabled: i64,
    provider_keys_enabled: i64,
    provider_keys_disabled: i64,
    provider_keys_healthy: i64,
    provider_keys_failed: i64,
    provider_keys_untested: i64,
    provider_keys_runtime_error: i64,
    models_enabled: i64,
    models_disabled: i64,
    routes_enabled: i64,
    routes_disabled: i64,
    api_keys_enabled: i64,
    api_keys_disabled: i64,
    webhooks_enabled: i64,
    webhooks_disabled: i64,
    webhook_deliveries_succeeded: i64,
    webhook_deliveries_failed: i64,
    audit_logs: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ProviderMetricsRow {
    id: i64,
    name: String,
    enabled: i64,
    max_concurrency: Option<i64>,
    last_test_ok: Option<i64>,
    last_test_latency_ms: Option<i64>,
    models_enabled: i64,
    models_disabled: i64,
    requests: i64,
    successes: i64,
    latency_ms_sum: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ProviderModelMetricsRow {
    provider_id: i64,
    provider: String,
    model: String,
    max_concurrency: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct ProviderKeyMetricsRow {
    id: i64,
    provider: String,
    name: String,
}

pub async fn prometheus_metrics(State(state): State<AppState>) -> AppResult<Response> {
    let lifetime_fut = sqlx::query_as::<_, LifetimeMetricsRow>(
        r#"
        SELECT requests, tokens, prompt_tokens, completion_tokens,
               cache_read_tokens, cache_write_tokens, cost_micros,
               unpriced_requests, successful_requests, latency_ms_sum
        FROM usage_lifetime_stats
        WHERE id = 1
        "#,
    )
    .fetch_one(&state.pool);
    let statuses_fut = sqlx::query_as::<_, StatusMetricsRow>(
        r#"
        SELECT
            (SELECT COUNT(*) FROM providers WHERE enabled = 1 AND last_test_ok = 1) AS providers_healthy,
            (SELECT COUNT(*) FROM providers WHERE enabled = 1 AND last_test_ok = 0) AS providers_failed,
            (SELECT COUNT(*) FROM providers WHERE enabled = 1 AND last_test_ok IS NULL) AS providers_untested,
            (SELECT COUNT(*) FROM providers WHERE enabled = 0) AS providers_disabled,

            (SELECT COUNT(*) FROM provider_api_keys WHERE enabled = 1) AS provider_keys_enabled,
            (SELECT COUNT(*) FROM provider_api_keys WHERE enabled = 0) AS provider_keys_disabled,
            (SELECT COUNT(*) FROM provider_api_keys
              WHERE enabled = 1 AND last_test_ok = 1) AS provider_keys_healthy,
            (SELECT COUNT(*) FROM provider_api_keys
              WHERE enabled = 1 AND last_test_ok = 0) AS provider_keys_failed,
            (SELECT COUNT(*) FROM provider_api_keys
              WHERE enabled = 1 AND last_test_ok IS NULL) AS provider_keys_untested,
            (SELECT COUNT(*) FROM provider_api_keys
              WHERE enabled = 1 AND last_error IS NOT NULL) AS provider_keys_runtime_error,

            (SELECT COUNT(*) FROM provider_models WHERE enabled = 1) AS models_enabled,
            (SELECT COUNT(*) FROM provider_models WHERE enabled = 0) AS models_disabled,
            (SELECT COUNT(*) FROM routes WHERE enabled = 1) AS routes_enabled,
            (SELECT COUNT(*) FROM routes WHERE enabled = 0) AS routes_disabled,
            (SELECT COUNT(*) FROM api_keys WHERE enabled = 1) AS api_keys_enabled,
            (SELECT COUNT(*) FROM api_keys WHERE enabled = 0) AS api_keys_disabled,

            (SELECT COUNT(*) FROM webhooks WHERE enabled = 1) AS webhooks_enabled,
            (SELECT COUNT(*) FROM webhooks WHERE enabled = 0) AS webhooks_disabled,
            (SELECT COUNT(*) FROM webhook_deliveries
              WHERE status_code >= 200 AND status_code < 300) AS webhook_deliveries_succeeded,
            (SELECT COUNT(*) FROM webhook_deliveries
              WHERE status_code IS NULL OR status_code < 200 OR status_code >= 300)
              AS webhook_deliveries_failed,

            (SELECT COUNT(*) FROM audit_logs) AS audit_logs
        "#,
    )
    .fetch_one(&state.pool);
    let providers_fut = sqlx::query_as::<_, ProviderMetricsRow>(
        r#"
        WITH model_counts AS (
            SELECT provider_id,
                   COALESCE(SUM(CASE WHEN enabled = 1 THEN 1 ELSE 0 END), 0) AS models_enabled,
                   COALESCE(SUM(CASE WHEN enabled = 0 THEN 1 ELSE 0 END), 0) AS models_disabled
            FROM provider_models
            GROUP BY provider_id
        ),
        key_stats AS (
            SELECT provider_id,
                   COALESCE(SUM(lifetime_requests), 0) AS requests,
                   COALESCE(SUM(lifetime_successes), 0) AS successes,
                   COALESCE(SUM(lifetime_latency_ms), 0) AS latency_ms_sum,
                   COALESCE(SUM(lifetime_prompt_tokens), 0) AS prompt_tokens,
                   COALESCE(SUM(lifetime_completion_tokens), 0) AS completion_tokens
            FROM provider_api_keys
            GROUP BY provider_id
        )
        SELECT p.id, p.name, p.enabled, p.max_concurrency,
               p.last_test_ok, p.last_test_latency_ms,
               COALESCE(m.models_enabled, 0) AS models_enabled,
               COALESCE(m.models_disabled, 0) AS models_disabled,
               COALESCE(k.requests, 0) AS requests,
               COALESCE(k.successes, 0) AS successes,
               COALESCE(k.latency_ms_sum, 0) AS latency_ms_sum,
               COALESCE(k.prompt_tokens, 0) AS prompt_tokens,
               COALESCE(k.completion_tokens, 0) AS completion_tokens
        FROM providers p
        LEFT JOIN model_counts m ON m.provider_id = p.id
        LEFT JOIN key_stats k ON k.provider_id = p.id
        ORDER BY p.name COLLATE NOCASE, p.id
        "#,
    )
    .fetch_all(&state.pool);
    let provider_models_fut = sqlx::query_as::<_, ProviderModelMetricsRow>(
        r#"
        SELECT pm.provider_id, p.name AS provider, pm.model_name AS model,
               pm.max_concurrency
        FROM provider_models pm
        JOIN providers p ON p.id = pm.provider_id
        ORDER BY p.name COLLATE NOCASE, pm.model_name COLLATE NOCASE, pm.id
        "#,
    )
    .fetch_all(&state.pool);
    let provider_keys_fut = sqlx::query_as::<_, ProviderKeyMetricsRow>(
        r#"
        SELECT k.id, p.name AS provider, k.name
        FROM provider_api_keys k
        JOIN providers p ON p.id = k.provider_id
        ORDER BY p.name COLLATE NOCASE, k.name COLLATE NOCASE, k.id
        "#,
    )
    .fetch_all(&state.pool);
    let in_flight_fut =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
            .fetch_one(&state.pool);
    let page_count_fut = sqlx::query_scalar::<_, i64>("PRAGMA page_count").fetch_one(&state.pool);
    let page_size_fut = sqlx::query_scalar::<_, i64>("PRAGMA page_size").fetch_one(&state.pool);
    let free_pages_fut =
        sqlx::query_scalar::<_, i64>("PRAGMA freelist_count").fetch_one(&state.pool);

    let (
        lifetime,
        statuses,
        providers,
        provider_models,
        provider_keys,
        in_flight,
        page_count,
        page_size,
        free_pages,
    ) = tokio::try_join!(
        lifetime_fut,
        statuses_fut,
        providers_fut,
        provider_models_fut,
        provider_keys_fut,
        in_flight_fut,
        page_count_fut,
        page_size_fut,
        free_pages_fut,
    )?;

    let now = Instant::now();
    let cooling = {
        let cooldowns = state.provider_cooldown.lock().await;
        cooldowns
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(provider_id, _)| *provider_id)
            .collect::<HashSet<_>>()
    };
    let concurrency_available = {
        let semaphores = state.provider_concurrency.lock().await;
        semaphores
            .iter()
            .map(|(provider_id, semaphore)| (*provider_id, semaphore.available_permits()))
            .collect::<HashMap<_, _>>()
    };
    let model_cooling = {
        let cooldowns = state.target_cooldown.lock().await;
        cooldowns
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|((provider_id, model), _)| (*provider_id, model.clone()))
            .collect::<HashSet<_>>()
    };
    let provider_key_cooling = {
        let cooldowns = state.provider_key_cooldown.lock().await;
        cooldowns
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(key_id, _)| *key_id)
            .collect::<HashSet<_>>()
    };
    let model_concurrency_available = {
        let semaphores = state.model_concurrency.lock().await;
        semaphores
            .iter()
            .map(|((provider_id, model), semaphore)| {
                ((*provider_id, model.clone()), semaphore.available_permits())
            })
            .collect::<HashMap<_, _>>()
    };

    let mut body = String::with_capacity(8 * 1024);
    metric_header(&mut body, "openllm_info", "Build information");
    push_metric(
        &mut body,
        "openllm_info",
        &[("version", env!("CARGO_PKG_VERSION"))],
        1,
    );
    metric_header(
        &mut body,
        "openllm_requests_total",
        "Completed requests retained by the gateway",
    );
    push_metric(&mut body, "openllm_requests_total", &[], lifetime.requests);
    metric_header(
        &mut body,
        "openllm_requests_successful_total",
        "Completed requests that reached the upstream successfully",
    );
    push_metric(
        &mut body,
        "openllm_requests_successful_total",
        &[],
        lifetime.successful_requests,
    );
    metric_header(
        &mut body,
        "openllm_requests_failed_total",
        "Completed requests that failed",
    );
    push_metric(
        &mut body,
        "openllm_requests_failed_total",
        &[],
        (lifetime.requests - lifetime.successful_requests).max(0),
    );
    metric_header(
        &mut body,
        "openllm_requests_in_flight",
        "Requests currently being processed",
    );
    push_metric(&mut body, "openllm_requests_in_flight", &[], in_flight);
    metric_header(
        &mut body,
        "openllm_tokens_total",
        "Retained token counts by type",
    );
    for (kind, value) in [
        ("total", lifetime.tokens),
        ("prompt", lifetime.prompt_tokens),
        ("completion", lifetime.completion_tokens),
        ("cache_read", lifetime.cache_read_tokens),
        ("cache_write", lifetime.cache_write_tokens),
    ] {
        push_metric(&mut body, "openllm_tokens_total", &[("type", kind)], value);
    }
    metric_header(
        &mut body,
        "openllm_estimated_cost_usd_total",
        "Estimated request cost in USD",
    );
    write!(
        &mut body,
        "openllm_estimated_cost_usd_total {:.6}\n",
        lifetime.cost_micros as f64 / 1_000_000.0
    )
    .expect("writing to a String cannot fail");
    metric_header(
        &mut body,
        "openllm_unpriced_requests_total",
        "Completed requests without known pricing",
    );
    push_metric(
        &mut body,
        "openllm_unpriced_requests_total",
        &[],
        lifetime.unpriced_requests,
    );
    metric_header(
        &mut body,
        "openllm_average_latency_ms",
        "Average completed request latency in milliseconds",
    );
    let avg_latency = if lifetime.requests > 0 {
        lifetime.latency_ms_sum as f64 / lifetime.requests as f64
    } else {
        0.0
    };
    write!(&mut body, "openllm_average_latency_ms {avg_latency:.3}\n")
        .expect("writing to a String cannot fail");

    metric_header(
        &mut body,
        "openllm_providers",
        "Provider count by lifecycle and health state",
    );
    for (state, value) in [
        ("healthy", statuses.providers_healthy),
        ("failed", statuses.providers_failed),
        ("untested", statuses.providers_untested),
        ("disabled", statuses.providers_disabled),
    ] {
        push_metric(&mut body, "openllm_providers", &[("state", state)], value);
    }
    metric_header(
        &mut body,
        "openllm_provider_keys",
        "Provider credential count by lifecycle and health state",
    );
    for (state, value) in [
        ("enabled", statuses.provider_keys_enabled),
        ("disabled", statuses.provider_keys_disabled),
        ("healthy", statuses.provider_keys_healthy),
        ("failed", statuses.provider_keys_failed),
        ("untested", statuses.provider_keys_untested),
        ("runtime_error", statuses.provider_keys_runtime_error),
    ] {
        push_metric(
            &mut body,
            "openllm_provider_keys",
            &[("state", state)],
            value,
        );
    }
    metric_header(
        &mut body,
        "openllm_models",
        "Provider model count by enabled state",
    );
    for (state, value) in [
        ("enabled", statuses.models_enabled),
        ("disabled", statuses.models_disabled),
    ] {
        push_metric(&mut body, "openllm_models", &[("state", state)], value);
    }
    metric_header(&mut body, "openllm_routes", "Route count by enabled state");
    for (state, value) in [
        ("enabled", statuses.routes_enabled),
        ("disabled", statuses.routes_disabled),
    ] {
        push_metric(&mut body, "openllm_routes", &[("state", state)], value);
    }
    metric_header(
        &mut body,
        "openllm_api_keys",
        "Gateway API key count by enabled state",
    );
    for (state, value) in [
        ("enabled", statuses.api_keys_enabled),
        ("disabled", statuses.api_keys_disabled),
    ] {
        push_metric(&mut body, "openllm_api_keys", &[("state", state)], value);
    }
    metric_header(
        &mut body,
        "openllm_webhooks",
        "Webhook subscription count by enabled state",
    );
    for (state, value) in [
        ("enabled", statuses.webhooks_enabled),
        ("disabled", statuses.webhooks_disabled),
    ] {
        push_metric(&mut body, "openllm_webhooks", &[("state", state)], value);
    }
    metric_header(
        &mut body,
        "openllm_webhook_deliveries",
        "Retained webhook delivery records by outcome",
    );
    for (state, value) in [
        ("succeeded", statuses.webhook_deliveries_succeeded),
        ("failed", statuses.webhook_deliveries_failed),
    ] {
        push_metric(
            &mut body,
            "openllm_webhook_deliveries",
            &[("state", state)],
            value,
        );
    }
    metric_header(
        &mut body,
        "openllm_audit_logs",
        "Retained configuration change audit records",
    );
    push_metric(&mut body, "openllm_audit_logs", &[], statuses.audit_logs);

    metric_header(
        &mut body,
        "openllm_provider_health",
        "Current provider health as a one-hot status label",
    );
    metric_header(
        &mut body,
        "openllm_provider_concurrency_limit",
        "Configured per-provider upstream concurrency limit; zero means unlimited",
    );
    metric_header(
        &mut body,
        "openllm_provider_inflight",
        "Current upstream requests holding a per-provider concurrency slot",
    );
    metric_header(
        &mut body,
        "openllm_provider_cooling",
        "Whether a provider is currently excluded from routing by a runtime cooldown",
    );
    metric_header(
        &mut body,
        "openllm_provider_models",
        "Enabled upstream models per provider",
    );
    metric_header(
        &mut body,
        "openllm_provider_models_disabled",
        "Disabled upstream models per provider",
    );
    metric_header(
        &mut body,
        "openllm_provider_requests_total",
        "Lifetime upstream requests per provider",
    );
    metric_header(
        &mut body,
        "openllm_provider_successes_total",
        "Lifetime successful upstream requests per provider",
    );
    metric_header(
        &mut body,
        "openllm_provider_prompt_tokens_total",
        "Lifetime prompt tokens attributed to each provider",
    );
    metric_header(
        &mut body,
        "openllm_provider_completion_tokens_total",
        "Lifetime completion tokens attributed to each provider",
    );
    metric_header(
        &mut body,
        "openllm_provider_average_latency_ms",
        "Mean upstream latency per provider over retained requests",
    );
    metric_header(
        &mut body,
        "openllm_provider_last_test_latency_ms",
        "Latency of the most recent provider health probe",
    );
    for provider in &providers {
        let state = if provider.enabled == 0 {
            "disabled"
        } else {
            match provider.last_test_ok {
                Some(1) => "healthy",
                Some(_) => "failed",
                None => "untested",
            }
        };
        push_metric(
            &mut body,
            "openllm_provider_health",
            &[("provider", &provider.name), ("state", state)],
            1,
        );
        let label = [("provider", provider.name.as_str())];
        let limit = provider.max_concurrency.unwrap_or(0).max(0);
        let in_flight = concurrency_available
            .get(&provider.id)
            .map(|available| limit.saturating_sub(*available as i64))
            .unwrap_or(0);
        push_metric(&mut body, "openllm_provider_concurrency_limit", &label, limit);
        push_metric(&mut body, "openllm_provider_inflight", &label, in_flight);
        push_metric(
            &mut body,
            "openllm_provider_cooling",
            &label,
            cooling.contains(&provider.id) as u8,
        );
        push_metric(
            &mut body,
            "openllm_provider_models",
            &label,
            provider.models_enabled,
        );
        push_metric(
            &mut body,
            "openllm_provider_models_disabled",
            &label,
            provider.models_disabled,
        );
        push_metric(
            &mut body,
            "openllm_provider_requests_total",
            &label,
            provider.requests,
        );
        push_metric(
            &mut body,
            "openllm_provider_successes_total",
            &label,
            provider.successes,
        );
        push_metric(
            &mut body,
            "openllm_provider_prompt_tokens_total",
            &label,
            provider.prompt_tokens,
        );
        push_metric(
            &mut body,
            "openllm_provider_completion_tokens_total",
            &label,
            provider.completion_tokens,
        );
        let provider_avg_latency = if provider.requests > 0 {
            provider.latency_ms_sum as f64 / provider.requests as f64
        } else {
            0.0
        };
        write!(
            &mut body,
            "openllm_provider_average_latency_ms{{provider=\"{}\"}} {:.3}\n",
            escape_label(&provider.name),
            provider_avg_latency
        )
        .expect("writing to a String cannot fail");
        if let Some(latency) = provider.last_test_latency_ms {
            push_metric(
                &mut body,
                "openllm_provider_last_test_latency_ms",
                &label,
                latency,
            );
        }
    }

    metric_header(
        &mut body,
        "openllm_model_cooling",
        "Current model cooldown state",
    );
    metric_header(
        &mut body,
        "openllm_model_concurrency_limit",
        "Configured per-model upstream concurrency limit; zero means unlimited",
    );
    metric_header(
        &mut body,
        "openllm_model_inflight",
        "Current upstream requests holding a per-model concurrency slot",
    );
    for model in &provider_models {
        let label = [
            ("provider", model.provider.as_str()),
            ("model", model.model.as_str()),
        ];
        let limit = model.max_concurrency.unwrap_or(0).max(0);
        let in_flight = model_concurrency_available
            .get(&(model.provider_id, model.model.clone()))
            .map(|available| limit.saturating_sub(*available as i64))
            .unwrap_or(0);
        push_metric(
            &mut body,
            "openllm_model_cooling",
            &label,
            model_cooling.contains(&(model.provider_id, model.model.clone())) as u8,
        );
        push_metric(
            &mut body,
            "openllm_model_concurrency_limit",
            &label,
            limit,
        );
        push_metric(&mut body, "openllm_model_inflight", &label, in_flight);
    }

    metric_header(
        &mut body,
        "openllm_provider_key_cooling",
        "Current provider key cooldown state",
    );
    for key in &provider_keys {
        let key_id = key.id.to_string();
        let label = [
            ("provider", key.provider.as_str()),
            ("key", key.name.as_str()),
            ("key_id", key_id.as_str()),
        ];
        push_metric(
            &mut body,
            "openllm_provider_key_cooling",
            &label,
            provider_key_cooling.contains(&key.id) as u8,
        );
    }

    metric_header(
        &mut body,
        "openllm_database_size_bytes",
        "SQLite database size in bytes including free pages",
    );
    push_metric(
        &mut body,
        "openllm_database_size_bytes",
        &[],
        page_count.saturating_mul(page_size),
    );
    metric_header(
        &mut body,
        "openllm_database_free_bytes",
        "Reusable free space in the SQLite database",
    );
    push_metric(
        &mut body,
        "openllm_database_free_bytes",
        &[],
        free_pages.saturating_mul(page_size),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .map_err(|error| AppError::Internal(error.into()))
}

/// Prometheus exposition type. Monotonic totals must be declared as counters
/// so `rate()` / `increase()` behave correctly; point-in-time state stays a
/// gauge.
#[derive(Debug, Clone, Copy)]
enum MetricKind {
    Counter,
    Gauge,
}

impl MetricKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }
}

/// Emits the `# HELP` / `# TYPE` pair for a metric family.
///
/// The exposition type is derived from the name: monotonic `*_total` families
/// are counters, everything else is a point-in-time gauge.
fn metric_header(output: &mut String, name: &str, help: &str) {
    let kind = if name.ends_with("_total") {
        MetricKind::Counter
    } else {
        MetricKind::Gauge
    };
    write!(
        output,
        "# HELP {name} {help}\n# TYPE {name} {}\n",
        kind.as_str()
    )
    .expect("writing to a String cannot fail");
}

fn push_metric(
    output: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    value: impl std::fmt::Display,
) {
    write!(output, "{name}").expect("writing to a String cannot fail");
    if !labels.is_empty() {
        output.push('{');
        for (index, (key, value)) in labels.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(output, "{key}=\"{}\"", escape_label(value))
                .expect("writing to a String cannot fail");
        }
        output.push('}');
    }
    writeln!(output, " {value}").expect("writing to a String cannot fail");
}

fn escape_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}
