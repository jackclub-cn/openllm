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
    admin_auth, backup_database, cleanup_usage, create_api_key, create_provider, create_route,
    create_webhook, delete_api_key, delete_provider, delete_route, delete_webhook, diagnose_route,
    event_stream, export_usage, get_runtime_settings, get_settings, get_usage_detail, health,
    list_api_keys, list_audit_logs, list_model_inventory, list_models, list_provider_model_limits,
    list_providers, list_routes, list_usage, list_webhook_deliveries, list_webhooks, overview,
    preview_provider_model_sync, prometheus_metrics, provider_quota, rotate_api_key,
    sync_provider_models, test_all_provider_keys, test_all_providers, test_provider,
    test_provider_keys, test_webhook, update_api_key, update_provider,
    update_provider_model_limits, update_route, update_runtime_settings, update_webhook,
    vacuum_database,
};
use crate::assets::static_handler;
use crate::db::Database;
use crate::proxy::{
    count_tokens_anthropic, proxy_anthropic, proxy_openai, proxy_openai_console, public_model,
    public_models,
};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub database_url: Option<String>,
    pub admin_token: Option<String>,
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
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            crate::api::run_due_provider_health_checks(health_state.clone()).await;
            crate::api::run_due_provider_model_syncs(health_state.clone()).await;
            crate::api::reconcile_stale_usage_requests(health_state.clone()).await;
            crate::api::run_due_usage_retention(health_state.clone()).await;
        }
    });
    let webhook_state = state.clone();
    tokio::spawn(async move {
        crate::webhooks::run_dispatcher(webhook_state).await;
    });

    let app = build_router(state);
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.bind))?;

    tracing::info!(
        address = %config.bind,
        console = %format!("http://{}", config.bind),
        "OpenLLM Gateway started"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")
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
        .route("/webhooks", get(list_webhooks).post(create_webhook))
        .route("/webhooks/{id}", put(update_webhook).delete(delete_webhook))
        .route("/webhooks/{id}/test", post(test_webhook))
        .route("/webhooks/{id}/deliveries", get(list_webhook_deliveries))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth,
        ));

    Router::new()
        .merge(metrics)
        .route("/api/health", get(health))
        .route("/api/settings", get(get_settings))
        .route("/api/events", get(event_stream))
        .nest("/api", admin)
        .route("/v1/models", get(public_models))
        .route("/v1/models/{*model}", get(public_model))
        .route("/v1/chat/completions", post(proxy_openai))
        .route("/v1/completions", post(proxy_openai))
        .route("/v1/embeddings", post(proxy_openai))
        .route("/v1/responses", post(proxy_openai))
        .route("/v1/messages", post(proxy_anthropic))
        .route("/v1/messages/count_tokens", post(count_tokens_anthropic))
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
