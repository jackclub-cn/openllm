mod allocation;
mod api;
mod assets;
mod capabilities;
pub mod cli;
mod db;
mod error;
mod models;
mod models_dev;
mod proxy;
mod quota;
mod registry;
mod release_feed;
mod resilience_state;
mod secure_file;
mod session_affinity;
mod startup;
mod state;

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderValue;
use axum::routing::{get, post, put};
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::api::{
    admin_auth, admin_origin_guard, analytics_context, analytics_dimensions,
    analytics_route_quality, backup_database, cleanup_usage, clear_cooldowns, create_api_key,
    create_backup, create_provider, create_provider_pool, create_route, delete_api_key,
    delete_provider, delete_provider_pool, delete_route, diagnose_route, diagnostic_snapshot,
    event_stream, event_stream_ticket, export_audit_logs, export_config, export_usage,
    get_guardrails_settings, get_inspector_settings, get_request_activity, get_resilience_settings,
    get_route_trace, get_runtime_settings, get_settings, get_status, get_usage_detail, health,
    import_config, list_api_keys, list_audit_logs, list_backups, list_cooldowns,
    list_model_inventory, list_models, list_provider_model_limits, list_provider_pools,
    list_provider_quota_snapshots, list_providers, list_routes, list_usage, migration_status,
    overview, preview_provider_model_sync, prometheus_metrics, provider_pool_usage, provider_quota,
    provider_quota_overview, provider_quota_refresh_interval, ready, recent_activity,
    rotate_api_key, run_maintenance, run_provider_quota_refresh, self_check, sync_provider_models,
    test_all_provider_keys, test_all_providers, test_provider, test_provider_keys, update_api_key,
    update_guardrails_settings, update_inspector_settings, update_provider,
    update_provider_model_limits, update_provider_pool, update_provider_quota_snapshots,
    update_resilience_settings, update_route, update_runtime_settings, upgrade_check,
    vacuum_database, verify_backup,
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
/// Request routing reads a precomputed one-day P95 snapshot; this worker owns
/// the window-function query and keeps the request path off SQLite sorting.
const ROUTING_P95_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs a long-lived background worker, restarting it if it ever stops.
///
/// Maintenance and scheduled refreshes are spawned once at startup and loop
/// forever. Without supervision a single panic would take a worker down for
/// the rest of the process's life, with only a one-off panic message as a clue. This
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
    validate_runtime_security(&config)?;
    let database = Database::connect(&config).await?;
    let instance_id = database.instance_id.clone();
    let state = AppState::new(database.pool.clone(), config.admin_token.clone());
    if let Err(error) = crate::proxy::refresh_runtime_target_p95(&state).await {
        tracing::warn!(%error, "failed to initialize routing P95 snapshot");
    }
    let p95_state = state.clone();
    spawn_supervised("routing-p95-refresh", move || {
        let state = p95_state.clone();
        async move {
            loop {
                tokio::time::sleep(ROUTING_P95_REFRESH_INTERVAL).await;
                if let Err(error) = crate::proxy::refresh_runtime_target_p95(&state).await {
                    tracing::warn!(%error, "failed to refresh routing P95 snapshot");
                }
            }
        }
    });
    if let Err(error) = crate::resilience_state::restore(&state).await {
        tracing::warn!(%error, "failed to restore persisted resilience state");
    }
    if let Err(error) = crate::quota::restore(&state).await {
        tracing::warn!(%error, "failed to restore provider quota snapshots");
    }
    if let Err(error) = crate::capabilities::restore(&state).await {
        tracing::warn!(%error, "failed to restore provider capability profiles");
    }
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
    let quota_refresh_interval = provider_quota_refresh_interval();
    if let Some(interval) = quota_refresh_interval {
        let quota_state = state.clone();
        spawn_supervised("provider-quota-refresh", move || {
            let state = quota_state.clone();
            async move {
                loop {
                    run_provider_quota_refresh(&state).await;
                    tokio::time::sleep(interval).await;
                }
            }
        });
    } else {
        tracing::info!("scheduled provider quota refresh is disabled");
    }
    // Keep the instance lease fresh so a second process on the same database
    // can tell a live instance from a crashed one.
    let lease_pool = state.pool.clone();
    let lease_instance = instance_id.clone();
    spawn_supervised("instance-lease", move || {
        let pool = lease_pool.clone();
        let instance = lease_instance.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(
                    crate::startup::lease_heartbeat_interval_secs(),
                ))
                .await;
                if let Err(error) = crate::startup::heartbeat(&pool, &instance).await {
                    tracing::warn!(%error, "failed to refresh the instance lease");
                }
            }
        }
    });
    if state.resilience_state_persist {
        let resilience_state = state.clone();
        spawn_supervised("resilience-state-writer", move || {
            crate::resilience_state::run_writer(resilience_state.clone())
        });
    } else {
        tracing::warn!(
            "OPENLLM_RESILIENCE_STATE_PERSIST is off; circuits and cooldowns will be \
             forgotten on restart"
        );
    }
    let lag_state = state.clone();
    tokio::spawn(async move {
        monitor_runtime_lag(lag_state).await;
    });

    let limits = state.runtime_limits();
    let shutting_down = state.shutting_down.clone();
    // Kept outside the router so the final flush below can still reach the
    // writer's shared state after `build_router` consumes the other handle.
    let persist_state = state.clone();
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
        request_timeout_secs = ?limits.request_timeout_secs,
        provider_open_threshold = %limits.provider_open_threshold,
        resilience_state_persist = %limits.resilience_state_persist,
        instance_id = %instance_id,
        multi_instance_policy = %crate::startup::multi_instance_policy().as_str(),
        instance_lease_ttl_secs = crate::startup::lease_ttl_secs(),
        memory_limit_mib = %limits.memory_limit_mib,
        memory_limit_source = %limits.memory_limit_source,
        memory_shed_ratio_pct = %limits.memory_shed_ratio_pct,
        provider_quota_refresh_secs = ?quota_refresh_interval.map(|duration| duration.as_secs()),
        db_max_connections = %limits.db_max_connections,
        db_busy_timeout_secs = %limits.db_busy_timeout_secs,
        db_acquire_timeout_secs = %limits.db_acquire_timeout_secs,
        "OpenLLM Gateway started"
    );

    let grace_secs =
        parse_shutdown_grace_secs(std::env::var("OPENLLM_SHUTDOWN_GRACE_SECS").ok().as_deref());
    let shutdown = async move {
        shutdown_signal().await;
        // Flip readiness first: a load balancer then stops sending new requests
        // here while the drain below lets in-flight work finish.
        shutting_down.store(true, std::sync::atomic::Ordering::Relaxed);
    };
    let result = serve_until_shutdown(listener, app, grace_secs, shutdown).await;
    // A restart right after a deploy must not hand traffic straight back to a
    // provider that was cooling when the process stopped, so flush what is
    // still marked dirty before exiting.
    if let Err(error) = crate::resilience_state::flush(&persist_state).await {
        tracing::warn!(%error, "failed to persist resilience state during shutdown");
    }
    crate::startup::release_instance_lease(&persist_state.pool, &instance_id).await;
    result
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
        tracing::info!(
            grace_secs,
            "shutdown signal received; draining in-flight requests"
        );
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
    // Retired API paths must not fall through to the console's HTML fallback.
    let retired_webhooks = axum::routing::any(|| async { axum::http::StatusCode::NOT_FOUND });
    let metrics = Router::new()
        .route("/metrics", get(prometheus_metrics))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_origin_guard,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth,
        ));
    let admin = Router::new()
        .route("/status", get(get_status))
        .route("/events/ticket", post(event_stream_ticket))
        .route("/activity/recent", get(recent_activity))
        .route("/activity/{request_id}", get(get_request_activity))
        .route("/diagnostics/self-check", get(self_check))
        .route("/diagnostics/snapshot", get(diagnostic_snapshot))
        .route("/overview", get(overview))
        .route("/analytics/dimensions", get(analytics_dimensions))
        .route("/analytics/context", get(analytics_context))
        .route("/analytics/route-quality", get(analytics_route_quality))
        .route("/audit-logs", get(list_audit_logs))
        .route("/audit-logs/export", get(export_audit_logs))
        .route("/database/backup", get(backup_database))
        .route("/database/vacuum", post(vacuum_database))
        .route("/maintenance/run", post(run_maintenance))
        .route("/maintenance/backup", post(create_backup))
        .route("/maintenance/backups", get(list_backups))
        .route("/maintenance/backups/verify", post(verify_backup))
        .route("/maintenance/migrations", get(migration_status))
        .route("/maintenance/upgrade-check", get(upgrade_check))
        .route("/config/export", get(export_config))
        .route("/config/import", post(import_config))
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/quota-overview", get(provider_quota_overview))
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
        .route(
            "/providers/{id}/quota-snapshots",
            get(list_provider_quota_snapshots).post(update_provider_quota_snapshots),
        )
        .route(
            "/provider-pools",
            get(list_provider_pools).post(create_provider_pool),
        )
        .route(
            "/provider-pools/{id}",
            put(update_provider_pool).delete(delete_provider_pool),
        )
        .route("/providers/{id}/pool-usage", get(provider_pool_usage))
        .route("/resilience/cooldowns", get(list_cooldowns))
        .route("/resilience/cooldowns/clear", post(clear_cooldowns))
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
        .route("/usage/{request_id}/route-trace", get(get_route_trace))
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
        .route("/webhooks", retired_webhooks.clone())
        .route("/webhooks/{*path}", retired_webhooks)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_origin_guard,
        ))
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

    let cors = configured_cors_layer(state.cors_origins.as_ref());
    let app = Router::new()
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
        .layer(DefaultBodyLimit::max(crate::state::max_request_body_bytes()));
    let app = match cors {
        Some(cors) => app.layer(cors),
        None => app,
    };
    app.layer(TraceLayer::new_for_http()).with_state(state)
}

fn configured_cors_layer(origins: &[String]) -> Option<CorsLayer> {
    let origins = origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect::<Vec<_>>();
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(Any)
            .allow_headers(Any)
            // Let a cross-origin console read the export-truncation header so
            // a capped download is never silent.
            .expose_headers(Any),
    )
}

fn validate_runtime_security(config: &Config) -> anyhow::Result<()> {
    let has_admin_token = config
        .admin_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    if !config.bind.ip().is_loopback() && !has_admin_token {
        bail!(
            "refusing to serve {bind} without OPENLLM_ADMIN_TOKEN; bind to a loopback address or configure a token",
            bind = config.bind
        );
    }
    Ok(())
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
