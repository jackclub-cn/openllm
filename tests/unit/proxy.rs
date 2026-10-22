use super::*;
use crate::state::{
    MemoryPressure, PROVIDER_PROBE_LEASE, RequestByteBudget, parse_cgroup_memory_max,
    resolve_memory_limit,
};
use tower::ServiceExt;

#[path = "proxy/activity.rs"]
mod activity;

/// Builds a page of model entries with the given ids, matching the shape
/// `anthropic_models` produces.
fn model_page(ids: &[&str]) -> Vec<Value> {
    ids.iter()
        .map(|id| json!({"type": "model", "id": id, "display_name": id, "created_at": Value::Null}))
        .collect()
}

fn ids_of(models: &[Value]) -> Vec<String> {
    models
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn endpoint_test_target(provider_type: &str, supported_endpoints: Option<&str>) -> RouteTarget {
    RouteTarget {
        id: 1,
        route_id: None,
        provider_id: 1,
        provider_name: "test".to_string(),
        provider_type: provider_type.to_string(),
        base_url: "http://upstream".to_string(),
        model_prefix: String::new(),
        api_key: None,
        provider_headers: "{}".to_string(),
        supported_endpoints: supported_endpoints.map(ToOwned::to_owned),
        cost: None,
        cost_input_override: None,
        cost_output_override: None,
        cost_cache_read_override: None,
        cost_cache_write_override: None,
        context_limit: None,
        input_limit: None,
        output_limit: None,
        reasoning_efforts: None,
        provider_enabled: None,
        model_enabled: None,
        tool_search_supported: 1,
        timeout_seconds: None,
        cooldown_seconds: None,
        max_concurrency: None,
        queue_timeout_seconds: None,
        model_max_concurrency: None,
        model_queue_timeout_seconds: None,
        provider_health: None,
        upstream_model: "model".to_string(),
        weight: 100,
        priority: 0,
        enabled: 1,
        provider_api_key_id: None,
        provider_api_key_name: None,
        provider_key_max_concurrency: None,
        provider_key_requests_per_minute: None,
        provider_key_queue_limit: None,
        provider_key_queue_wait_ms: None,
        auth_retryable: false,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "fixture mirrors quota snapshot columns"
)]
async fn insert_test_quota_snapshot(
    state: &AppState,
    provider_id: i64,
    provider_key_id: Option<i64>,
    model: &str,
    used: Option<f64>,
    limit: Option<f64>,
    remaining: Option<f64>,
    reset_in: Option<chrono::Duration>,
) {
    let now = chrono::Utc::now();
    let record = crate::models::ProviderQuotaSnapshotRecord {
        provider_id,
        provider_key_id,
        model: model.to_string(),
        window_key: "5h".to_string(),
        used,
        limit,
        remaining,
        unit: "request".to_string(),
        reset_at: reset_in.map(|duration| (now + duration).to_rfc3339()),
        source: "manual".to_string(),
        confidence: "exact".to_string(),
        observed_at: now.to_rfc3339(),
        valid_until: (now + chrono::Duration::hours(1)).to_rfc3339(),
    };
    let key = record.key();
    state.quota_snapshots.lock().await.insert(key, record);
}

async fn circuit_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, None)
}

async fn spawn_chat_upstream() -> (
    std::net::SocketAddr,
    std::sync::Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let requests = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new().route(
        OPENAI_CHAT_COMPLETIONS,
        axum::routing::post({
            let requests = requests.clone();
            move |Json(body): Json<Value>| {
                let requests = requests.clone();
                async move {
                    requests.lock().await.push(body);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "id": "chatcmpl-routing-override",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 3,
                                "completion_tokens": 1,
                                "total_tokens": 4
                            }
                        })),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, requests, server)
}

fn barrel_with_output_limit(output_limit: Option<i64>) -> BarrelEnvelope {
    BarrelEnvelope {
        capabilities: Some(crate::models::ModelCapabilities {
            output_limit,
            ..Default::default()
        }),
        incomplete: false,
        target_count: 2,
    }
}

/// A file-backed state so a second `AppState` can read what the first wrote.
///
/// Sharing the pool is what makes an in-process "restart" faithful: only the
/// in-memory maps are thrown away, exactly as a relaunch of the binary does.
async fn restartable_circuit_state() -> (tempfile::TempDir, AppState) {
    let directory = tempfile::tempdir().unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("resilience.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) VALUES
            (1, 'flaky', 'openai', 'https://flaky.example/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) \
         VALUES (11, 1, 'main', 'sk-main')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);
    (directory, state)
}

async fn count_rows(state: &AppState, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[path = "proxy/guardrails.rs"]
mod guardrails;

#[path = "proxy/sessions.rs"]
mod sessions;

#[path = "proxy/models.rs"]
mod models;

#[path = "proxy/endpoints.rs"]
mod endpoints;

#[path = "proxy/error_classification.rs"]
mod error_classification;

#[path = "proxy/routing_overrides.rs"]
mod routing_overrides;

#[path = "proxy/usage_logging.rs"]
mod usage_logging;
#[path = "proxy/usage_performance.rs"]
mod usage_performance;

#[path = "proxy/diagnostics.rs"]
mod diagnostics;

#[path = "proxy/resolution.rs"]
mod resolution;

#[path = "proxy/key_quotas.rs"]
mod key_quotas;

#[path = "proxy/model_permissions.rs"]
mod model_permissions;

#[path = "proxy/json_validation.rs"]
mod json_validation;

#[path = "proxy/retries.rs"]
mod retries;

#[path = "proxy/quota_fallback.rs"]
mod quota_fallback;

#[path = "proxy/reasoning_compat.rs"]
mod reasoning_compat;

#[path = "proxy/reasoning_effort.rs"]
mod reasoning_effort;

#[path = "proxy/stream_recovery.rs"]
mod stream_recovery;

#[path = "proxy/cancellation.rs"]
mod cancellation;

#[path = "proxy/keepalive.rs"]
mod keepalive;

#[path = "proxy/budgets.rs"]
mod budgets;

#[path = "proxy/affinity.rs"]
mod affinity;

#[path = "proxy/strategies.rs"]
mod strategies;

#[path = "proxy/quota_pacing.rs"]
mod quota_pacing;

#[path = "proxy/key_rotation.rs"]
mod key_rotation;

#[path = "proxy/cooldowns.rs"]
mod cooldowns;

#[path = "proxy/latency.rs"]
mod latency;

#[path = "proxy/timeouts.rs"]
mod timeouts;

#[path = "proxy/stream_failures.rs"]
mod stream_failures;

#[path = "proxy/concurrency.rs"]
mod concurrency;

#[path = "proxy/busy_wait.rs"]
mod busy_wait;

#[path = "proxy/busy_wait_streams.rs"]
mod busy_wait_streams;

#[path = "proxy/model_failures.rs"]
mod model_failures;

#[path = "proxy/capabilities.rs"]
mod capabilities;

#[path = "proxy/compat.rs"]
mod compat;

#[path = "proxy/tool_search.rs"]
mod tool_search;

#[path = "proxy/tool_history.rs"]
mod tool_history;

#[path = "proxy/anthropic_compat.rs"]
mod anthropic_compat;

#[path = "proxy/anthropic_translation.rs"]
mod anthropic_translation;

#[path = "proxy/usage_parser.rs"]
mod usage_parser;

#[path = "proxy/key_routing.rs"]
mod key_routing;

#[path = "proxy/responses_translation.rs"]
mod responses_translation;

#[path = "proxy/completions_translation.rs"]
mod completions_translation;

#[path = "proxy/anthropic_tools.rs"]
mod anthropic_tools;

#[path = "proxy/reasoning_translation.rs"]
mod reasoning_translation;

#[path = "proxy/responses_content.rs"]
mod responses_content;

#[path = "proxy/runtime_limits.rs"]
mod runtime_limits;

#[path = "proxy/admission.rs"]
mod admission;

#[path = "proxy/deadlines.rs"]
mod deadlines;

#[path = "proxy/memory.rs"]
mod memory;

#[path = "proxy/request_budget.rs"]
mod request_budget;

#[path = "proxy/circuits.rs"]
mod circuits;

#[path = "proxy/persistence.rs"]
mod persistence;

#[path = "proxy/probe_recovery.rs"]
mod probe_recovery;
