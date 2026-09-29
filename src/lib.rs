mod api;
mod assets;
mod db;
mod error;
mod models;
mod models_dev;
mod proxy;
mod registry;
mod state;

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
    admin_auth, cleanup_usage, create_api_key, create_provider, create_route, delete_api_key,
    delete_provider, delete_route, event_stream, get_settings, get_usage_detail, health,
    list_api_keys, list_models, list_providers, list_routes, list_usage, overview,
    sync_provider_models, test_provider, update_api_key, update_provider, update_route,
};
use crate::assets::static_handler;
use crate::db::Database;
use crate::proxy::{count_tokens_anthropic, proxy_anthropic, proxy_openai, public_models};
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
    let admin = Router::new()
        .route("/overview", get(overview))
        .route("/providers", get(list_providers).post(create_provider))
        .route(
            "/providers/{id}",
            put(update_provider).delete(delete_provider),
        )
        .route("/providers/{id}/test", post(test_provider))
        .route("/providers/{id}/models/sync", post(sync_provider_models))
        .route("/routes", get(list_routes).post(create_route))
        .route("/routes/{id}", put(update_route).delete(delete_route))
        .route("/models", get(list_models))
        .route("/api-keys", get(list_api_keys).post(create_api_key))
        .route("/api-keys/{id}", put(update_api_key).delete(delete_api_key))
        .route("/usage", get(list_usage))
        .route("/usage/cleanup", post(cleanup_usage))
        .route("/usage/{request_id}", get(get_usage_detail))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth,
        ));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/settings", get(get_settings))
        .route("/api/events", get(event_stream))
        .nest("/api", admin)
        .route("/v1/models", get(public_models))
        .route("/v1/chat/completions", post(proxy_openai))
        .route("/v1/completions", post(proxy_openai))
        .route("/v1/embeddings", post(proxy_openai))
        .route("/v1/responses", post(proxy_openai))
        .route("/v1/messages", post(proxy_anthropic))
        .route("/v1/messages/count_tokens", post(count_tokens_anthropic))
        .fallback(static_handler)
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
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
