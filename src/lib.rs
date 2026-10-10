mod api;
mod assets;
mod db;
mod error;
mod models;
mod models_dev;
mod proxy;
mod registry;
mod state;
mod webhooks;

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::api::{
    admin_auth, backup_database, cleanup_usage, clear_cooldowns, create_api_key, create_provider,
    create_route,
    create_webhook, delete_api_key, delete_provider, delete_route, delete_webhook, diagnose_route,
    event_stream, export_usage, get_guardrails_settings, get_inspector_settings,
    get_resilience_settings, get_runtime_settings, get_settings, get_usage_detail, health,
    list_api_keys, list_audit_logs, list_cooldowns, list_model_inventory, list_models,
    list_provider_model_limits,
    list_providers, list_routes, list_usage, list_webhook_deliveries, list_webhooks, overview,
    preview_provider_model_sync, prometheus_metrics, provider_quota, ready, rotate_api_key,
    run_maintenance,
    sync_provider_models, test_all_provider_keys, test_all_providers, test_provider,
    test_provider_keys, test_webhook, update_api_key, update_guardrails_settings,
    update_inspector_settings, update_provider, update_provider_model_limits,
    update_resilience_settings, update_route, update_runtime_settings, update_webhook,
    vacuum_database,
};
use crate::assets::static_handler;
use crate::db::Database;
use crate::proxy::{
    admission, count_tokens_anthropic, proxy_anthropic, proxy_openai, proxy_openai_console,
    public_model, public_models,
};
use crate::state::{AppState, parse_shutdown_grace_secs};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub database_url: Option<String>,
    pub admin_token: Option<String>,
}

/// Measures how late the async runtime runs a 1-second timer and records it.
///
/// A healthy runtime wakes a timer close to its deadline. A large value means
/// a blocking call or a CPU-bound task is occupying the reactor thread, which
/// delays *every* in-flight request; exposing it as a gauge lets an operator
/// alert on it instead of guessing from request latency.
async fn monitor_runtime_lag(state: AppState) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const INTERVAL: Duration = Duration::from_secs(1);
    const WARN_AFTER_MS: u64 = 5_000;

    let mut expected = tokio::time::Instant::now() + INTERVAL;
    loop {
        tokio::time::sleep_until(expected).await;
        let now = tokio::time::Instant::now();
        let lag_ms = now.saturating_duration_since(expected).as_millis() as u64;
        state.runtime_lag_millis.store(lag_ms, Ordering::Relaxed);
        if lag_ms >= WARN_AFTER_MS {
            tracing::warn!(
                lag_ms,
                "async runtime lag detected; a blocking call may be stalling requests"
            );
        }
        expected += INTERVAL;
        // After a long stall, resync instead of replaying every missed tick.
        if expected <= now {
            expected = now + INTERVAL;
        }
    }
}

/// How long to wait before restarting a background worker that stopped.
const WORKER_RESTART_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);

/// Runs a long-lived background worker, restarting it if it ever stops.
///
/// Maintenance and webhook delivery are spawned once at startup and loop
/// forever. Without supervision a single panic would take that worker down for
/// the rest of the process's life — maintenance would stop running and webhooks
/// would stop delivering, with only a one-off panic message as a clue. This
/// catches the panic (or an unexpected early return), logs it, and starts the
/// worker again after a short backoff. `make` must build a fresh future per
/// attempt because a future cannot be polled after it completes.
pub(crate) fn spawn_supervised<F, Fut>(name: &'static str, make: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match tokio::spawn(make()).await {
                Ok(()) => tracing::warn!(worker = name, "background worker exited; restarting"),
                Err(error) => {
                    tracing::error!(worker = name, %error, "background worker panicked; restarting")
                }
            }
            tokio::time::sleep(WORKER_RESTART_BACKOFF).await;
        }
    });
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let database = Database::connect(&config).await?;
    let state = AppState::new(database.pool.clone(), config.admin_token.clone());
    if let Err(error) = state.load_provider_key_error_state().await {
        tracing::warn!(%error, "failed to load provider key error state");
    }
    if let Err(error) = crate::api::reconcile_interrupted_usage_requests(&state).await {
        tracing::warn!(%error, "failed to reconcile interrupted usage logs on startup");
    }
    if let Err(error) = crate::api::reconcile_interrupted_provider_model_syncs(&state).await {
        tracing::warn!(%error, "failed to reconcile interrupted provider model syncs on startup");
    }
    let health_state = state.clone();
    spawn_supervised("maintenance", move || {
        let state = health_state.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                crate::api::run_maintenance_cycle(&state).await;
            }
        }
    });
    let webhook_state = state.clone();
    spawn_supervised("webhook-dispatcher", move || {
        crate::webhooks::run_dispatcher(webhook_state.clone())
    });
    let lag_state = state.clone();
    tokio::spawn(async move {
        monitor_runtime_lag(lag_state).await;
    });

    let limits = state.runtime_limits();
    let app = build_router(state);
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.bind))?;

    tracing::info!(
        address = %config.bind,
        console = %format!("http://{}", config.bind),
        upstream_idle_timeout_secs = limits.upstream_idle_timeout_secs,
        shutdown_grace_secs = limits.shutdown_grace_secs,
        max_request_body_mib = %limits.max_request_body_mib,
        max_upstream_body_mib = %limits.max_upstream_body_mib,
        sse_keepalive_secs = ?limits.sse_keepalive_secs,
        max_concurrent_requests = %limits.max_concurrent_requests,
        max_inflight_request_mib = %limits.max_inflight_request_mib,
        admission_wait_ms = %limits.admission_wait_ms,
        stream_max_secs = ?limits.stream_max_secs,
        body_read_timeout_secs = ?limits.body_read_timeout_secs,
        memory_limit_mib = %limits.memory_limit_mib,
        memory_limit_source = %limits.memory_limit_source,
        memory_shed_ratio_pct = %limits.memory_shed_ratio_pct,
        db_max_connections = %limits.db_max_connections,
        db_busy_timeout_secs = %limits.db_busy_timeout_secs,
        db_acquire_timeout_secs = %limits.db_acquire_timeout_secs,
        "OpenLLM Gateway started"
    );

    let grace_secs =
        parse_shutdown_grace_secs(std::env::var("OPENLLM_SHUTDOWN_GRACE_SECS").ok().as_deref());
    serve_until_shutdown(listener, app, grace_secs, shutdown_signal()).await
}

/// Serves until a shutdown signal, then drains in-flight work for a bounded
/// grace period before closing whatever is left.
///
/// A bare graceful shutdown waits for every connection, so one long-lived
/// stream can hold a restart open indefinitely. The deadline keeps deploys and
/// restarts predictable while still giving healthy requests a chance to finish.
async fn serve_until_shutdown<F>(
    listener: TcpListener,
    app: Router,
    grace_secs: u64,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if grace_secs == 0 {
        return axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .context("server error");
    }

    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        shutdown.await;
        tracing::info!(grace_secs, "shutdown signal received; draining in-flight requests");
        let _ = signal_tx.send(());
    };
    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .into_future();
    tokio::pin!(serve);
    tokio::select! {
        result = &mut serve => result.context("server error"),
        _ = async move {
            // The countdown only starts once the signal actually arrives.
            let _ = signal_rx.await;
            tokio::time::sleep(std::time::Duration::from_secs(grace_secs)).await;
        } => {
            tracing::warn!(
                grace_secs,
                "shutdown grace period elapsed; closing remaining connections"
            );
            Ok(())
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    let metrics = Router::new()
        .route("/metrics", get(prometheus_metrics))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth,
        ));
    let admin = Router::new()
        .route("/overview", get(overview))
        .route("/audit-logs", get(list_audit_logs))
        .route("/database/backup", get(backup_database))
        .route("/database/vacuum", post(vacuum_database))
        .route("/maintenance/run", post(run_maintenance))
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/test-all", post(test_all_providers))
        .route("/providers/test-all-keys", post(test_all_provider_keys))
        .route(
            "/providers/{id}",
            put(update_provider).delete(delete_provider),
        )
        .route("/providers/{id}/test", post(test_provider))
        .route("/providers/{id}/keys/test", post(test_provider_keys))
        .route("/providers/{id}/models/sync", post(sync_provider_models))
        .route(
            "/providers/{id}/models/preview",
            post(preview_provider_model_sync),
        )
        .route(
            "/providers/{id}/model-limits",
            get(list_provider_model_limits).put(update_provider_model_limits),
        )
        .route("/providers/{id}/quota", get(provider_quota))
        .route("/resilience/cooldowns", get(list_cooldowns))
        .route(
            "/resilience/cooldowns/clear",
            post(clear_cooldowns),
        )
        .route("/model-inventory", get(list_model_inventory))
        .route("/routes", get(list_routes).post(create_route))
        .route("/routes/diagnose", post(diagnose_route))
        .route("/routes/{id}", put(update_route).delete(delete_route))
        .route("/models", get(list_models))
        .route("/api-keys", get(list_api_keys).post(create_api_key))
        .route("/api-keys/{id}/rotate", post(rotate_api_key))
        .route("/api-keys/{id}", put(update_api_key).delete(delete_api_key))
        .route("/playground/chat/completions", post(proxy_openai_console))
        .route("/usage", get(list_usage))
        .route("/usage/export", get(export_usage))
        .route("/usage/cleanup", post(cleanup_usage))
        .route("/usage/{request_id}", get(get_usage_detail))
        .route(
            "/settings/runtime",
            get(get_runtime_settings).put(update_runtime_settings),
        )
        .route(
            "/settings/guardrails",
            get(get_guardrails_settings).put(update_guardrails_settings),
        )
        .route(
            "/settings/inspector",
            get(get_inspector_settings).put(update_inspector_settings),
        )
        .route(
            "/settings/resilience",
            get(get_resilience_settings).put(update_resilience_settings),
        )
        .route("/webhooks", get(list_webhooks).post(create_webhook))
        .route("/webhooks/{id}", put(update_webhook).delete(delete_webhook))
        .route("/webhooks/{id}/test", post(test_webhook))
        .route("/webhooks/{id}/deliveries", get(list_webhook_deliveries))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth,
        ));
    // The public proxy routes get the admission check before the body is read;
    // the model-list routes stay uncapped because they are cheap metadata.
    let proxy = Router::new()
        .route("/v1/chat/completions", post(proxy_openai))
        .route("/v1/completions", post(proxy_openai))
        .route("/v1/embeddings", post(proxy_openai))
        .route("/v1/responses", post(proxy_openai))
        .route("/v1/messages", post(proxy_anthropic))
        .route("/v1/messages/count_tokens", post(count_tokens_anthropic))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admission,
        ));

    Router::new()
        .merge(metrics)
        .merge(proxy)
        .route("/api/health", get(health))
        .route("/api/ready", get(ready))
        .route("/api/settings", get(get_settings))
        .route("/api/events", get(event_stream))
        .nest("/api", admin)
        .route("/v1/models", get(public_models))
        .route("/v1/models/{*model}", get(public_model))
        .fallback(static_handler)
        .layer(DefaultBodyLimit::max(crate::state::max_request_body_bytes()))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
