use super::*;
use tower::ServiceExt;

async fn provider_key_test_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, None)
}

/// Every exported sample must belong to a family that declared its type.
///
/// Prometheus tolerates a missing declaration by treating the family as
/// untyped, which silently breaks `rate()` / `increase()` semantics on counters
/// and drops the family's meaning in dashboards. Keeping the whole exposition
/// self-consistent is cheap; relying on reviewers to spot one missing header is
/// not.
fn assert_every_metric_family_declares_a_type(body: &str) {
    let declared = body
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    for line in body.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let family = line.split(['{', ' ']).next().unwrap_or_default();
        assert!(
            declared.contains(family),
            "metric family '{family}' is exported without a # TYPE declaration"
        );
    }
}

fn provider_input(api_key: Option<&str>, api_keys: Vec<ProviderApiKeyInput>) -> ProviderInput {
    ProviderInput {
        name: "key-pool-test".to_string(),
        provider_type: ProviderType::Openai,
        base_url: "https://example.com/v1".to_string(),
        model_prefix: String::new(),
        api_key: api_key.map(ToOwned::to_owned),
        api_keys,
        headers: json!({}),
        enabled: true,
        auto_sync_models: false,
        models: Vec::new(),
        model_auto_include_rules: Vec::new(),
        health_check_interval_minutes: None,
        health_check_model: None,
        models_sync_interval_minutes: None,
        timeout_seconds: None,
        configured_cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
    }
}

fn provider_key_input(
    id: Option<i64>,
    name: &str,
    api_key: Option<&str>,
    enabled: bool,
) -> ProviderApiKeyInput {
    ProviderApiKeyInput {
        id,
        name: name.to_string(),
        api_key: api_key.map(ToOwned::to_owned),
        enabled,
        max_concurrency: None,
        requests_per_minute: None,
        queue_limit: None,
        queue_wait_ms: None,
    }
}

fn provider_update_with_keys(api_keys: Option<Vec<ProviderApiKeyInput>>) -> ProviderUpdate {
    ProviderUpdate {
        name: None,
        provider_type: None,
        base_url: None,
        model_prefix: None,
        api_key: None,
        clear_api_key: None,
        api_keys,
        headers: None,
        enabled: None,
        auto_sync_models: None,
        models: None,
        model_auto_include_rules: None,
        health_check_interval_minutes: None,
        health_check_model: None,
        models_sync_interval_minutes: None,
        timeout_seconds: None,
        configured_cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
    }
}
#[path = "api/metrics.rs"]
mod metrics;

#[path = "api/provider_validation.rs"]
mod provider_validation;

#[path = "api/quota_parsing.rs"]
mod quota_parsing;

#[path = "api/model_catalog.rs"]
mod model_catalog;

#[path = "api/keys.rs"]
mod keys;

#[path = "api/provider_settings.rs"]
mod provider_settings;

#[path = "api/provider_keys.rs"]
mod provider_keys;

#[path = "api/quota_snapshots.rs"]
mod quota_snapshots;

#[path = "api/provider_health.rs"]
mod provider_health;

#[path = "api/settings.rs"]
mod settings;

#[path = "api/cooldowns.rs"]
mod cooldowns;

#[path = "api/usage_logs.rs"]
mod usage_logs;

#[path = "api/maintenance.rs"]
mod maintenance;

#[path = "api/audit.rs"]
mod audit;

#[path = "api/provider_views.rs"]
mod provider_views;

#[path = "api/routes.rs"]
mod routes;

#[path = "api/analytics_window.rs"]
mod analytics_window;

#[path = "api/model_sync.rs"]
mod model_sync;

#[path = "api/model_limits.rs"]
mod model_limits;

#[path = "api/analytics_overview.rs"]
mod analytics_overview;

#[path = "api/analytics_filters.rs"]
mod analytics_filters;

#[path = "api/analytics_dimensions.rs"]
mod analytics_dimensions;

#[path = "api/analytics_routing.rs"]
mod analytics_routing;

#[path = "api/usage_lifecycle.rs"]
mod usage_lifecycle;

#[path = "api/usage_filters.rs"]
mod usage_filters;

#[path = "api/runtime.rs"]
mod runtime;

#[path = "api/activity.rs"]
mod activity;
