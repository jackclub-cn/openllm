use super::*;
#[derive(Debug, sqlx::FromRow)]
pub(super) struct LifetimeMetricsRow {
    pub(super) requests: i64,
    pub(super) tokens: i64,
    pub(super) prompt_tokens: i64,
    pub(super) completion_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) cost_micros: i64,
    pub(super) unpriced_requests: i64,
    pub(super) successful_requests: i64,
    pub(super) latency_ms_sum: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct StatusMetricsRow {
    pub(super) providers_healthy: i64,
    pub(super) providers_failed: i64,
    pub(super) providers_untested: i64,
    pub(super) providers_disabled: i64,
    pub(super) provider_keys_enabled: i64,
    pub(super) provider_keys_disabled: i64,
    pub(super) provider_keys_healthy: i64,
    pub(super) provider_keys_failed: i64,
    pub(super) provider_keys_untested: i64,
    pub(super) provider_keys_runtime_error: i64,
    pub(super) models_enabled: i64,
    pub(super) models_disabled: i64,
    pub(super) routes_enabled: i64,
    pub(super) routes_disabled: i64,
    pub(super) api_keys_enabled: i64,
    pub(super) api_keys_disabled: i64,
    pub(super) audit_logs: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ProviderMetricsRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) enabled: i64,
    pub(super) max_concurrency: Option<i64>,
    pub(super) last_test_ok: Option<i64>,
    pub(super) last_test_latency_ms: Option<i64>,
    pub(super) models_enabled: i64,
    pub(super) models_disabled: i64,
    pub(super) requests: i64,
    pub(super) successes: i64,
    pub(super) latency_ms_sum: i64,
    pub(super) prompt_tokens: i64,
    pub(super) completion_tokens: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ProviderModelMetricsRow {
    pub(super) provider_id: i64,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) max_concurrency: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ProviderKeyMetricsRow {
    pub(super) id: i64,
    pub(super) provider: String,
    pub(super) name: String,
    pub(super) max_concurrency: Option<i64>,
    pub(super) requests_per_minute: Option<i64>,
}

pub(super) struct MetricsSnapshot {
    pub(super) lifetime: LifetimeMetricsRow,
    pub(super) statuses: StatusMetricsRow,
    pub(super) providers: Vec<ProviderMetricsRow>,
    pub(super) provider_models: Vec<ProviderModelMetricsRow>,
    pub(super) provider_keys: Vec<ProviderKeyMetricsRow>,
    pub(super) in_flight: i64,
    pub(super) page_count: i64,
    pub(super) page_size: i64,
    pub(super) free_pages: i64,
    pub(super) cooling: HashSet<i64>,
    pub(super) failure_streaks: HashMap<i64, u32>,
    pub(super) circuit_trips: HashMap<i64, u64>,
    pub(super) failure_classifications: HashMap<(String, String, String), u64>,
    pub(super) provider_open_threshold: u32,
    pub(super) concurrency_available: HashMap<i64, usize>,
    pub(super) model_cooling: HashSet<(i64, String)>,
    pub(super) provider_key_cooling: HashSet<i64>,
    pub(super) model_sinks: HashSet<(i64, String)>,
    pub(super) provider_key_sink_counts: HashMap<i64, usize>,
    pub(super) model_concurrency_available: HashMap<(i64, String), usize>,
    pub(super) provider_key_concurrency_available: HashMap<i64, usize>,
    pub(super) provider_key_rpm: HashMap<i64, usize>,
    pub(super) affinity_stats: crate::session_affinity::SessionAffinityStats,
    pub(super) pool_usage: Vec<crate::allocation::PoolUsageView>,
}
pub(super) async fn collect(state: &AppState) -> AppResult<MetricsSnapshot> {
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
        SELECT k.id, p.name AS provider, k.name,
               k.max_concurrency, k.requests_per_minute
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
    let failure_streaks = state.provider_failure_streak.lock().await.clone();
    let circuit_trips = state.provider_circuit_trips.lock().await.clone();
    let failure_classifications = state.failure_classifications.lock().await.clone();
    let provider_open_threshold = crate::state::provider_open_threshold();
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
    let (model_sinks, provider_key_sink_counts) = {
        let mut models = HashSet::new();
        let mut keys = HashMap::<i64, usize>::new();
        for (provider_id, provider_key_id, model) in state
            .target_sink
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| entry.until > now)
            .map(|(key, _)| key.clone())
        {
            models.insert((provider_id, model));
            if let Some(provider_key_id) = provider_key_id {
                *keys.entry(provider_key_id).or_default() += 1;
            }
        }
        (models, keys)
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
    let provider_key_concurrency_available = {
        let semaphores = state.provider_key_concurrency.lock().await;
        semaphores
            .iter()
            .map(|(key_id, semaphore)| (*key_id, semaphore.available_permits()))
            .collect::<HashMap<_, _>>()
    };
    let provider_key_rpm = {
        let windows = state.provider_key_rpm.lock().await;
        windows
            .iter()
            .map(|(key_id, starts)| {
                let used = starts
                    .iter()
                    .filter(|started| {
                        now.saturating_duration_since(**started) < Duration::from_secs(60)
                    })
                    .count();
                (*key_id, used)
            })
            .collect::<HashMap<_, _>>()
    };
    let affinity_stats = match state.session_affinity.as_ref() {
        Some(store) => store.stats().await,
        None => crate::session_affinity::SessionAffinityStats::default(),
    };
    let pool_usage = crate::allocation::all_pool_usage(state)
        .await
        .unwrap_or_default();
    Ok(MetricsSnapshot {
        lifetime,
        statuses,
        providers,
        provider_models,
        provider_keys,
        in_flight,
        page_count,
        page_size,
        free_pages,
        cooling,
        failure_streaks,
        circuit_trips,
        failure_classifications,
        provider_open_threshold,
        concurrency_available,
        model_cooling,
        provider_key_cooling,
        model_sinks,
        provider_key_sink_counts,
        model_concurrency_available,
        provider_key_concurrency_available,
        provider_key_rpm,
        affinity_stats,
        pool_usage,
    })
}
