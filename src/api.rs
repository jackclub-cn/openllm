use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::models_dev;
use crate::proxy::{apply_custom_headers, join_upstream_url};
use crate::state::AppState;

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "openllm",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

pub async fn admin_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> AppResult<Response> {
    if let Some(expected) = &state.admin_token {
        let token = request
            .headers()
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .or_else(|| bearer_token(request.headers()));

        if token != Some(expected.as_str()) {
            return Err(AppError::Unauthorized(
                "a valid admin token is required".to_string(),
            ));
        }
    }

    Ok(next.run(request).await)
}

pub async fn get_settings(State(state): State<AppState>) -> Json<SettingsView> {
    Json(SettingsView {
        admin_auth_enabled: state.admin_token.is_some(),
        database: "sqlite",
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub async fn event_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AdminTokenQuery>,
) -> Response {
    if let Some(expected) = &state.admin_token {
        let query_token = query.admin_token.as_deref();
        let header_token = headers
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .or_else(|| bearer_token(&headers));
        if query_token != Some(expected.as_str()) && header_token != Some(expected.as_str()) {
            return AppError::Unauthorized("a valid admin token is required".to_string())
                .into_response();
        }
    }
    let mut receiver = state.events.subscribe();
    let stream = async_stream::stream! {
        yield Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(b"event: ready\ndata: {}\n\n"));
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    yield Ok(Bytes::from_static(b": keep-alive\n\n"));
                }
                event = receiver.recv() => {
                    match event {
                        Ok(event) => {
                            let payload = json!({
                                "id": event.id,
                                "request_id": event.request_id,
                                "success": event.success,
                                "streamed": event.streamed
                            });
                            yield Ok(Bytes::from(format!("event: usage\ndata: {payload}\n\n")));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub async fn list_providers(State(state): State<AppState>) -> AppResult<Json<Vec<ProviderView>>> {
    let providers = sqlx::query_as::<_, Provider>(
        "SELECT * FROM providers ORDER BY enabled DESC, name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut views = Vec::with_capacity(providers.len());
    for provider in providers {
        let models: Vec<String> = sqlx::query_scalar(
            "SELECT model_name FROM provider_models WHERE provider_id = ? AND enabled = 1 ORDER BY model_name COLLATE NOCASE",
        )
        .bind(provider.id)
        .fetch_all(&state.pool)
        .await?;
        let mut view = ProviderView::from(provider);
        view.models = models;
        views.push(view);
    }
    Ok(Json(views))
}

pub async fn create_provider(
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> AppResult<(StatusCode, Json<ProviderView>)> {
    validate_provider_input(&input)?;
    let headers = serde_json::to_string(&input.headers).unwrap_or_else(|_| "{}".to_string());
    let model_prefix = normalize_model_prefix(&input.model_prefix)?;
    let api_key = normalize_optional(input.api_key);
    let base_url = normalize_base_url(&input.base_url);
    // Resolve metadata before opening the transaction: the catalog fetch may
    // hit the network, and holding a SQLite write transaction across it would
    // block every other writer.
    let catalog = models_dev::try_load(&state).await;
    let models_dev_id = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(input.name.trim(), &base_url));

    let mut tx = state.pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO providers (name, provider_type, base_url, model_prefix, models_dev_id, api_key, headers, enabled) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.name.trim())
    .bind(input.provider_type.as_str())
    .bind(base_url)
    .bind(model_prefix)
    .bind(models_dev_id.as_deref())
    .bind(api_key)
    .bind(headers)
    .bind(input.enabled as i64)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    let id = result.last_insert_rowid();
    let entries = names_to_entries(&input.models);
    replace_provider_models(
        &mut tx,
        id,
        &entries,
        catalog.as_deref(),
        models_dev_id.as_deref(),
    )
    .await?;
    tx.commit().await?;

    if input.auto_sync_models
        && let Err(error) = sync_provider(state.clone(), id).await
    {
        tracing::warn!(provider_id = id, %error, "automatic model sync failed");
    }
    Ok((StatusCode::CREATED, Json(get_provider(&state, id).await?)))
}

pub async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProviderUpdate>,
) -> AppResult<Json<ProviderView>> {
    let current = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;

    let name = input.name.unwrap_or(current.name).trim().to_string();
    let provider_type = input
        .provider_type
        .unwrap_or(ProviderType::from_str(&current.provider_type).map_err(AppError::BadRequest)?);
    let base_url = normalize_base_url(
        input
            .base_url
            .as_deref()
            .unwrap_or(current.base_url.as_str()),
    );
    let model_prefix = normalize_model_prefix(
        input
            .model_prefix
            .as_deref()
            .unwrap_or(current.model_prefix.as_str()),
    )?;
    let enabled = input.enabled.unwrap_or(current.enabled != 0);
    let api_key = match input.api_key {
        Some(key) if key.trim().is_empty() => current.api_key,
        Some(key) => Some(key.trim().to_string()),
        None => current.api_key,
    };
    let headers = match input.headers {
        Some(value) => serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
        None => current.headers,
    };

    if name.is_empty() || base_url.is_empty() {
        return Err(AppError::BadRequest(
            "provider name and base URL are required".to_string(),
        ));
    }

    // Name or base URL may have changed, which can change the models.dev match.
    let catalog = models_dev::try_load(&state).await;
    let models_dev_id = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(&name, &base_url));

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE providers SET name = ?, provider_type = ?, base_url = ?, model_prefix = ?, models_dev_id = ?, api_key = ?, headers = ?, enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name)
    .bind(provider_type.as_str())
    .bind(base_url)
    .bind(model_prefix)
    .bind(models_dev_id.as_deref())
    .bind(api_key)
    .bind(headers)
    .bind(enabled as i64)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    if let Some(models) = input.models {
        let entries = names_to_entries(&models);
        replace_provider_models(
            &mut tx,
            id,
            &entries,
            catalog.as_deref(),
            models_dev_id.as_deref(),
        )
        .await?;
    }
    tx.commit().await?;

    if input.auto_sync_models == Some(true)
        && let Err(error) = sync_provider(state.clone(), id).await
    {
        tracing::warn!(provider_id = id, %error, "automatic model sync failed");
    }
    Ok(Json(get_provider(&state, id).await?))
}

pub async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let in_use: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM route_targets WHERE provider_id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await?;
    if in_use > 0 {
        return Err(AppError::Conflict(
            "provider is still used by one or more routes".to_string(),
        ));
    }

    let result = sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("provider not found".to_string()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_provider(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ProviderTestResult>> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let provider_type =
        ProviderType::from_str(&provider.provider_type).map_err(AppError::BadRequest)?;
    let started = std::time::Instant::now();

    // A model listing is often reachable without credentials (verified against
    // a live provider whose /models returns 200 for a bogus key), so on its own
    // it cannot tell the operator whether their key works. When a model is
    // known, probe the inference endpoint instead: it is the one that actually
    // enforces auth, so a bad key fails the test instead of looking healthy.
    let model = sqlx::query_scalar::<_, String>(
        "SELECT model_name FROM provider_models WHERE provider_id = ? AND enabled = 1 \
         ORDER BY model_name LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(model) = model {
        let (url, body) = match provider_type {
            ProviderType::Anthropic => (
                join_upstream_url(&provider.base_url, "/v1/messages"),
                json!({
                    "model": model,
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "ping"}]
                }),
            ),
            // Ollama's native tags endpoint needs no auth either, so probe its
            // chat endpoint for the same reason.
            _ => (
                join_upstream_url(&provider.base_url, "/v1/chat/completions"),
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "ping"}],
                    "max_tokens": 1
                }),
            ),
        };
        let mut request = state
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        request = match provider_type {
            ProviderType::Anthropic => {
                let request = request.header("anthropic-version", "2023-06-01");
                match &provider.api_key {
                    Some(key) => request.header("x-api-key", key),
                    None => request,
                }
            }
            _ => match &provider.api_key {
                Some(key) => request.bearer_auth(key),
                None => request,
            },
        };
        request = apply_custom_headers(request, &provider.headers)?;
        return Ok(Json(
            probe_provider(request, started, "inference", &model).await,
        ));
    }

    // No model synced yet, so fall back to listing. This only proves the host
    // is reachable, which the message says explicitly.
    let url = match provider_type {
        ProviderType::Anthropic => format!("{}/v1/models", provider.base_url.trim_end_matches('/')),
        ProviderType::Ollama => format!("{}/api/tags", ollama_root(&provider.base_url)),
        _ => format!("{}/models", provider.base_url.trim_end_matches('/')),
    };

    let mut request = state.client.get(url);
    if let Some(key) = &provider.api_key {
        request = match provider_type {
            ProviderType::Anthropic => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.bearer_auth(key),
        };
    }
    request = apply_custom_headers(request, &provider.headers)?;

    Ok(Json(probe_provider(request, started, "models", "").await))
}

/// Sends the probe and turns the outcome into a test result.
async fn probe_provider(
    request: reqwest::RequestBuilder,
    started: std::time::Instant,
    checked: &str,
    model: &str,
) -> ProviderTestResult {
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let latency_ms = started.elapsed().as_millis() as i64;
            let message = if status.is_success() {
                if checked == "inference" {
                    format!("上游已接受请求（模型 {model}），凭证有效")
                } else {
                    "上游可达，但尚未同步模型，未校验调用凭证".to_string()
                }
            } else {
                let body = response.text().await.unwrap_or_default();
                let summary = body.chars().take(300).collect::<String>();
                format!("upstream returned {status}: {summary}")
            };
            ProviderTestResult {
                ok: status.is_success(),
                latency_ms,
                message,
                checked: checked.to_string(),
            }
        }
        Err(error) => ProviderTestResult {
            ok: false,
            latency_ms: started.elapsed().as_millis() as i64,
            message: error.to_string(),
            checked: checked.to_string(),
        },
    }
}

pub async fn sync_provider_models(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ModelSyncResult>> {
    Ok(Json(sync_provider(state, id).await?))
}

async fn sync_provider(state: AppState, id: i64) -> AppResult<ModelSyncResult> {
    let result = sync_provider_inner(&state, id).await;
    if let Err(error) = &result {
        let _ = sqlx::query(
            "UPDATE providers SET models_sync_error = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
        )
        .bind(error.to_string())
        .bind(id)
        .execute(&state.pool)
        .await;
    }
    result
}

async fn sync_provider_inner(state: &AppState, id: i64) -> AppResult<ModelSyncResult> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let provider_type =
        ProviderType::from_str(&provider.provider_type).map_err(AppError::BadRequest)?;
    let (url, ollama_style) = match provider_type {
        ProviderType::Anthropic => (
            format!("{}/v1/models", provider.base_url.trim_end_matches('/')),
            false,
        ),
        ProviderType::Ollama => (
            format!("{}/api/tags", ollama_root(&provider.base_url)),
            true,
        ),
        ProviderType::Openai | ProviderType::Custom => (
            format!("{}/models", provider.base_url.trim_end_matches('/')),
            false,
        ),
    };

    let mut request = state.client.get(url);
    if let Some(key) = &provider.api_key {
        request = match provider_type {
            ProviderType::Anthropic => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.bearer_auth(key),
        };
    }
    request = apply_custom_headers(request, &provider.headers)?;

    let response = request.send().await.map_err(|error| {
        AppError::Upstream(format!(
            "failed to fetch models from {}: {error}",
            provider.name
        ))
    })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes)
            .chars()
            .take(400)
            .collect::<String>();
        return Err(AppError::Upstream(format!(
            "{} returned {}: {}",
            provider.name, status, body
        )));
    }

    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::Upstream(format!("invalid model list response: {error}")))?;
    let entries = if ollama_style {
        names_to_entries(&parse_ollama_models(&value))
    } else {
        parse_openai_model_entries(&value)
    };
    if entries.is_empty() {
        return Err(AppError::Upstream(
            "upstream model list did not contain any recognizable models".to_string(),
        ));
    }
    let models = entries
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();

    let catalog = models_dev::try_load(state).await;
    // Re-resolve the provider match on every sync: the stored id may be stale
    // (for example a provider renamed after a catalog update).
    let models_dev_id = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(&provider.name, &provider.base_url));

    let mut tx = state.pool.begin().await?;
    replace_provider_models(
        &mut tx,
        id,
        &entries,
        catalog.as_deref(),
        models_dev_id.as_deref(),
    )
    .await?;
    let synced_at = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE providers SET models_dev_id = ?, models_synced_at = ?, models_sync_error = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(models_dev_id.as_deref())
    .bind(&synced_at)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ModelSyncResult {
        ok: true,
        provider_id: id,
        count: models.len(),
        models,
        synced_at,
        message: "models synchronized".to_string(),
    })
}

pub async fn list_routes(State(state): State<AppState>) -> AppResult<Json<Vec<RouteView>>> {
    let routes = sqlx::query_as::<_, Route>(
        "SELECT * FROM routes ORDER BY enabled DESC, model_pattern COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut views = Vec::with_capacity(routes.len());
    for route in routes {
        views.push(route_view(&state, route).await?);
    }
    Ok(Json(views))
}

pub async fn create_route(
    State(state): State<AppState>,
    Json(input): Json<RouteInput>,
) -> AppResult<(StatusCode, Json<RouteView>)> {
    validate_route_input(&input)?;
    let mut tx = state.pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO routes (name, model_pattern, strategy, enabled) VALUES (?, ?, ?, ?)",
    )
    .bind(input.name.trim())
    .bind(input.model_pattern.trim())
    .bind(input.strategy.as_str())
    .bind(input.enabled as i64)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    let id = result.last_insert_rowid();
    replace_route_targets(&mut tx, id, &input.targets).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(get_route(&state, id).await?)))
}

pub async fn update_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<RouteUpdate>,
) -> AppResult<Json<RouteView>> {
    let current = sqlx::query_as::<_, Route>("SELECT * FROM routes WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("route not found".to_string()))?;

    let name = input.name.unwrap_or(current.name).trim().to_string();
    let model_pattern = input
        .model_pattern
        .unwrap_or(current.model_pattern)
        .trim()
        .to_string();
    let strategy = input
        .strategy
        .unwrap_or(RouteStrategy::from_str(&current.strategy).map_err(AppError::BadRequest)?);
    let enabled = input.enabled.unwrap_or(current.enabled != 0);

    if name.is_empty() || model_pattern.is_empty() {
        return Err(AppError::BadRequest(
            "route name and model pattern are required".to_string(),
        ));
    }
    validate_model_pattern(&model_pattern)?;

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE routes SET name = ?, model_pattern = ?, strategy = ?, enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name)
    .bind(model_pattern)
    .bind(strategy.as_str())
    .bind(enabled as i64)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    if let Some(targets) = input.targets {
        validate_targets(&targets)?;
        replace_route_targets(&mut tx, id, &targets).await?;
    }
    tx.commit().await?;

    Ok(Json(get_route(&state, id).await?))
}

pub async fn delete_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let result = sqlx::query("DELETE FROM routes WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("route not found".to_string()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_models(State(state): State<AppState>) -> AppResult<Json<Vec<PublicModel>>> {
    let routes = crate::registry::route_models(&state.pool).await?;
    let synced = crate::registry::synced_models(&state.pool).await?;
    let from = std::time::UNIX_EPOCH
        .elapsed()
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let mut by_id = BTreeMap::new();
    for model in routes {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created: from,
                owned_by: "openllm",
                provider: None,
                upstream_model: None,
                capabilities: model.capabilities,
                target_count: Some(model.target_count),
                limits_verified: Some(!model.incomplete),
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_completion_tokens: None,
                display_name: None,
            }
            .with_flat_limits(),
        );
    }
    for model in synced {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created: from,
                owned_by: "openllm",
                provider: Some(model.provider_name),
                upstream_model: Some(model.upstream_model),
                capabilities: model.capabilities,
                target_count: None,
                limits_verified: None,
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_completion_tokens: None,
                display_name: model.display_name,
            }
            .with_flat_limits(),
        );
    }
    Ok(Json(by_id.into_values().collect()))
}

pub async fn list_api_keys(State(state): State<AppState>) -> AppResult<Json<Vec<ApiKeyView>>> {
    let keys = sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT * FROM api_keys ORDER BY enabled DESC, created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Json(input): Json<ApiKeyInput>,
) -> AppResult<(StatusCode, Json<ApiKeyCreated>)> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("key name is required".to_string()));
    }

    let raw = format!("sk-openllm-{}", uuid::Uuid::new_v4().simple());
    let key_hash = hash_secret(&raw);
    let key_prefix = raw.chars().take(12).collect::<String>();
    let key_suffix = raw
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();

    let result = sqlx::query(
        "INSERT INTO api_keys (name, key_hash, key_prefix, key_suffix, enabled) VALUES (?, ?, ?, ?, 1)",
    )
    .bind(name)
    .bind(key_hash)
    .bind(key_prefix)
    .bind(key_suffix)
    .execute(&state.pool)
    .await?;

    let id = result.last_insert_rowid();
    let record = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    *state.auth_required.write().await = None;
    Ok((
        StatusCode::CREATED,
        Json(ApiKeyCreated {
            key: raw,
            item: record.into(),
        }),
    ))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let result = sqlx::query("DELETE FROM api_keys WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    *state.auth_required.write().await = None;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_api_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ApiKeyUpdate>,
) -> AppResult<Json<ApiKeyView>> {
    let result = sqlx::query("UPDATE api_keys SET enabled = ? WHERE id = ?")
        .bind(input.enabled as i64)
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    let record = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(record.into()))
}

pub async fn list_usage(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> AppResult<Json<UsagePage>> {
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 200);
    let offset = (page - 1) * page_size;

    let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM usage_logs u WHERE 1 = 1");
    apply_usage_filters(&mut count, &query);
    let total: i64 = count.build_query_scalar().fetch_one(&state.pool).await?;

    let mut items = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.latency_ms, u.status_code,
               u.success, u.streamed, u.error_message, u.created_at, u.first_token_ms,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE 1 = 1
        "#,
    );
    apply_usage_filters(&mut items, &query);
    items
        .push(" ORDER BY u.created_at DESC, u.id DESC LIMIT ")
        .push_bind(page_size)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows = items
        .build_query_as::<UsageLogDetailRow>()
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(UsagePage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
    }))
}

pub async fn get_usage_detail(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> AppResult<Json<UsageLogView>> {
    let row = sqlx::query_as::<_, UsageLogDetailRow>(
        r#"
        SELECT u.*, k.name AS api_key_name, r.name AS route_name, p.name AS provider_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE u.request_id = ?
        "#,
    )
    .bind(&request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("usage log not found".to_string()))?;
    Ok(Json(row.into()))
}

pub async fn cleanup_usage(
    State(state): State<AppState>,
    Json(input): Json<UsageCleanup>,
) -> AppResult<Json<UsageCleanupResult>> {
    if input.older_than_days < 1 {
        return Err(AppError::BadRequest(
            "older_than_days must be at least 1".to_string(),
        ));
    }
    let cutoff = Utc::now() - Duration::days(input.older_than_days);
    let cutoff = cutoff.to_rfc3339();
    let result = sqlx::query("DELETE FROM usage_logs WHERE created_at < ?")
        .bind(&cutoff)
        .execute(&state.pool)
        .await?;
    Ok(Json(UsageCleanupResult {
        deleted: result.rows_affected(),
        older_than_days: input.older_than_days,
        cutoff,
    }))
}

/// Share of prompt tokens served from cache, as a percentage.
///
/// `prompt_tokens` must be the total input count (fresh + cached), so the ratio
/// cannot exceed 100. A zero denominator yields 0 rather than NaN so the
/// dashboard needs no special case.
fn cache_hit_rate(prompt_tokens: i64, cache_read: i64) -> f64 {
    if prompt_tokens <= 0 {
        return 0.0;
    }
    (cache_read as f64 / prompt_tokens as f64 * 100.0).clamp(0.0, 100.0)
}

pub async fn overview(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> AppResult<Json<Overview>> {
    let now = Utc::now();
    // Clamp to real-world offsets so a stray value cannot shift the window far.
    let tz_offset = query.tz_offset_minutes.clamp(-14 * 60, 14 * 60);
    let local_now = now + Duration::minutes(tz_offset);
    // Start of the caller's local day, converted back to UTC for comparison
    // against the stored UTC timestamps.
    let day_start = local_now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        - Duration::minutes(tz_offset);
    let history_start = day_start - Duration::days(13);

    let totals = sqlx::query(
        r#"
        SELECT
            COUNT(*) AS requests_total,
            COALESCE(SUM(total_tokens), 0) AS tokens_total,
            COALESCE(SUM(prompt_tokens), 0) AS prompt_total,
            COALESCE(SUM(cache_read_tokens), 0) AS cache_read_total,
            COALESCE(SUM(cache_write_tokens), 0) AS cache_write_total,
            COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
            COALESCE(AVG(latency_ms), 0.0) AS avg_latency_ms
        FROM usage_logs
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    let today = sqlx::query(
        r#"
        SELECT COUNT(*) AS requests, COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt,
               COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(cache_write_tokens), 0) AS cache_write
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ?
        "#,
    )
    .bind(day_start.to_rfc3339())
    .bind((day_start + Duration::days(1)).to_rfc3339())
    .fetch_one(&state.pool)
    .await?;

    let active_providers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM providers WHERE enabled = 1")
            .fetch_one(&state.pool)
            .await?;
    let active_routes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routes WHERE enabled = 1")
        .fetch_one(&state.pool)
        .await?;

    let recent = sqlx::query_as::<_, UsageLogDetailRow>(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.latency_ms, u.status_code,
               u.success, u.streamed, u.error_message, u.created_at, u.first_token_ms,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        ORDER BY u.created_at DESC, u.id DESC
        LIMIT 8
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let provider_usage = sqlx::query_as::<_, ProviderUsage>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               COUNT(u.id) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms
        FROM providers p
        LEFT JOIN usage_logs u ON u.provider_id = p.id AND u.created_at >= ?
        GROUP BY p.id, p.name
        ORDER BY requests DESC, tokens DESC
        "#,
    )
    .bind(history_start.to_rfc3339())
    .fetch_all(&state.pool)
    .await?;

    let daily_rows = sqlx::query_as::<_, DailyUsage>(
        r#"
        SELECT date(datetime(created_at), ? || ' minutes') AS day,
               COUNT(*) AS requests,
               COALESCE(SUM(total_tokens), 0) AS tokens
        FROM usage_logs
        WHERE created_at >= ?
        GROUP BY date(datetime(created_at), ? || ' minutes')
        ORDER BY day
        "#,
    )
    .bind(tz_offset)
    .bind(history_start.to_rfc3339())
    .bind(tz_offset)
    .fetch_all(&state.pool)
    .await?;

    // Fill in days with no traffic so the 14-day chart has a continuous axis
    // instead of collapsing to only the days that happened to have requests.
    let mut daily_usage = Vec::with_capacity(14);
    for offset in 0..14 {
        // `history_start` is the first day's UTC instant; shift it into the
        // caller's local day before formatting.
        let day = (history_start + Duration::days(offset) + Duration::minutes(tz_offset))
            .format("%Y-%m-%d")
            .to_string();
        let existing = daily_rows.iter().find(|row| row.day == day);
        daily_usage.push(DailyUsage {
            day,
            requests: existing.map_or(0, |row| row.requests),
            tokens: existing.map_or(0, |row| row.tokens),
        });
    }

    let model_usage = sqlx::query_as::<_, ModelUsage>(
        r#"
        SELECT requested_model AS model,
               COUNT(*) AS requests,
               COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(latency_ms), 0.0) AS avg_latency_ms
        FROM usage_logs
        WHERE created_at >= ?
        GROUP BY requested_model
        ORDER BY tokens DESC, requests DESC
        LIMIT 8
        "#,
    )
    .bind(history_start.to_rfc3339())
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(Overview {
        requests_today: today.get("requests"),
        tokens_today: today.get("tokens"),
        cache_read_today: today.get("cache_read"),
        cache_write_today: today.get("cache_write"),
        cache_hit_rate: cache_hit_rate(today.get("prompt"), today.get("cache_read")),
        requests_total: totals.get("requests_total"),
        tokens_total: totals.get("tokens_total"),
        cache_read_total: totals.get("cache_read_total"),
        cache_write_total: totals.get("cache_write_total"),
        success_rate: totals.get("success_rate"),
        avg_latency_ms: totals.get("avg_latency_ms"),
        active_providers,
        active_routes,
        recent_requests: recent.into_iter().map(Into::into).collect(),
        provider_usage,
        model_usage,
        daily_usage,
    }))
}

async fn get_provider(state: &AppState, id: i64) -> AppResult<ProviderView> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    let models: Vec<String> = sqlx::query_scalar(
        "SELECT model_name FROM provider_models WHERE provider_id = ? AND enabled = 1 ORDER BY model_name COLLATE NOCASE",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let mut view = ProviderView::from(provider);
    view.models = models;
    Ok(view)
}

async fn get_route(state: &AppState, id: i64) -> AppResult<RouteView> {
    let route = sqlx::query_as::<_, Route>("SELECT * FROM routes WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    route_view(state, route).await
}

async fn route_view(state: &AppState, route: Route) -> AppResult<RouteView> {
    let targets = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT rt.*, p.name AS provider_name, p.provider_type,
               p.base_url, p.model_prefix, p.api_key, p.headers AS provider_headers,
               p.enabled AS provider_enabled
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        WHERE rt.route_id = ?
        ORDER BY rt.priority ASC, rt.id
        "#,
    )
    .bind(route.id)
    .fetch_all(&state.pool)
    .await?;

    Ok(RouteView {
        id: route.id,
        name: route.name,
        model_pattern: route.model_pattern,
        strategy: route.strategy,
        enabled: route.enabled != 0,
        targets: targets
            .into_iter()
            .map(|target| RouteTargetView {
                id: target.id,
                provider_id: target.provider_id,
                provider_name: target.provider_name,
                provider_type: target.provider_type,
                upstream_model: target.upstream_model,
                model_prefix: target.model_prefix,
                weight: target.weight,
                priority: target.priority,
                enabled: target.enabled != 0,
            })
            .collect(),
        created_at: route.created_at,
        updated_at: route.updated_at,
    })
}

/// Wraps plain model names (manually curated lists, Ollama) with empty upstream
/// metadata so they share one insert path with synced entries.
fn names_to_entries(models: &[String]) -> Vec<(String, UpstreamModelInfo)> {
    models
        .iter()
        .map(|name| (name.clone(), UpstreamModelInfo::default()))
        .collect()
}

/// Serialises the split modality fields back into the nested shape used for
/// storage, keeping one canonical on-disk representation.
fn modalities_to_storage(capabilities: &ModelCapabilities) -> Option<String> {
    if capabilities.input_modalities.is_none() && capabilities.output_modalities.is_none() {
        return None;
    }
    let mut value = serde_json::Map::new();
    if let Some(input) = &capabilities.input_modalities {
        value.insert("input".to_string(), json!(input));
    }
    if let Some(output) = &capabilities.output_modalities {
        value.insert("output".to_string(), json!(output));
    }
    serde_json::to_string(&Value::Object(value)).ok()
}

async fn replace_provider_models(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    models: &[(String, UpstreamModelInfo)],
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
) -> AppResult<()> {
    sqlx::query("DELETE FROM provider_models WHERE provider_id = ?")
        .bind(provider_id)
        .execute(&mut **tx)
        .await?;
    let mut seen = HashSet::new();
    let synced_at = Utc::now().to_rfc3339();
    for (model, upstream) in models {
        let model = model.trim();
        if model.is_empty() || !seen.insert(model.to_string()) {
            continue;
        }
        let found = catalog.and_then(|catalog| catalog.lookup(provider_hint, model));
        let has_capabilities = found.is_some();
        let capabilities = found.unwrap_or_default().with_effective_input_limit();
        // The provider's own context window is more trustworthy than a
        // models.dev guess, and is the only source for models models.dev lacks.
        // A provider may publish both a total window and a smaller accepted
        // input ceiling; keep the strictest value so clients never see an
        // optimistic limit.
        let context_limit = min_known(upstream.context_limit, capabilities.context_limit);
        let input_limit = min_known(context_limit, capabilities.input_limit);
        sqlx::query(
            r#"
            INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, output_limit,
                input_limit, attachment, reasoning, tool_call, structured_output,
                temperature, open_weights, modalities, cost, family, knowledge,
                release_date, last_updated, canonical_model_id, capabilities_synced_at,
                upstream_context_limit, supported_endpoints, display_name
            ) VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(provider_id)
        .bind(model)
        .bind(context_limit)
        .bind(capabilities.output_limit)
        .bind(input_limit)
        .bind(capabilities.attachment.map(i64::from))
        .bind(capabilities.reasoning.map(i64::from))
        .bind(capabilities.tool_call.map(i64::from))
        .bind(capabilities.structured_output.map(i64::from))
        .bind(capabilities.temperature.map(i64::from))
        .bind(capabilities.open_weights.map(i64::from))
        // Persisted in models.dev's nested shape; the read path flattens it.
        .bind(modalities_to_storage(&capabilities))
        .bind(
            capabilities
                .cost
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .unwrap_or_default(),
        )
        .bind(capabilities.family)
        .bind(capabilities.knowledge)
        .bind(capabilities.release_date)
        .bind(capabilities.last_updated)
        .bind(capabilities.canonical_model_id)
        .bind((has_capabilities || upstream.context_limit.is_some()).then_some(synced_at.clone()))
        .bind(upstream.context_limit)
        .bind(
            (!upstream.supported_endpoints.is_empty())
                .then(|| serde_json::to_string(&upstream.supported_endpoints))
                .transpose()
                .unwrap_or_default(),
        )
        .bind(upstream.display_name.clone())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Extra facts a provider reports about a model in its own `/models` response.
///
/// These are kept separate from models.dev metadata so the upstream's own
/// numbers can win: a provider knows its real context window even when
/// models.dev has never heard of the model.
#[derive(Debug, Clone, Default)]
struct UpstreamModelInfo {
    context_limit: Option<i64>,
    supported_endpoints: Vec<String>,
    /// Provider-supplied label, e.g. "DeepSeek V4.1 Flash".
    display_name: Option<String>,
}

/// Reads every common context/input spelling and keeps the strictest value.
///
/// OpenAI-compatible providers are inconsistent here: some expose
/// `context_length`, others `context_window`, and several publish both a total
/// window and a smaller `max_input_tokens`. Taking the minimum keeps the
/// gateway conservative when those fields disagree.
fn upstream_context_limit(model: &Value) -> Option<i64> {
    [
        "context_length",
        "context_window",
        "context_size",
        "max_input_tokens",
        "max_context_window",
        "max_context_tokens",
    ]
    .iter()
    .filter_map(|key| model.get(*key).and_then(Value::as_i64))
    .filter(|value| *value > 0)
    .min()
}

fn min_known(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// Parses an OpenAI-style model list, preserving each entry's name and any
/// upstream-reported limits.
fn parse_openai_model_entries(value: &Value) -> Vec<(String, UpstreamModelInfo)> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model
                .get("id")
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)?
                .trim();
            if name.is_empty() {
                return None;
            }
            let context_limit = upstream_context_limit(model);
            let supported_endpoints = model
                .get("supported_endpoints")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            // Providers commonly send the friendly label as `name`. Only keep
            // it when it adds information beyond the id itself.
            let display_name = model
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty() && *label != name)
                .map(ToOwned::to_owned);
            Some((
                name.to_string(),
                UpstreamModelInfo {
                    context_limit,
                    supported_endpoints,
                    display_name,
                },
            ))
        })
        .collect()
}

fn parse_ollama_models(value: &Value) -> Vec<String> {
    value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("name").or_else(|| model.get("model")))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

async fn replace_route_targets(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    route_id: i64,
    targets: &[RouteTargetInput],
) -> AppResult<()> {
    validate_targets(targets)?;
    sqlx::query("DELETE FROM route_targets WHERE route_id = ?")
        .bind(route_id)
        .execute(&mut **tx)
        .await?;

    for target in targets {
        sqlx::query(
            "INSERT INTO route_targets (route_id, provider_id, upstream_model, weight, priority, enabled) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(route_id)
        .bind(target.provider_id)
        .bind(target.upstream_model.trim())
        .bind(target.weight)
        .bind(target.priority)
        .bind(target.enabled as i64)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlite_conflict)?;
    }
    Ok(())
}

fn validate_provider_input(input: &ProviderInput) -> AppResult<()> {
    if input.name.trim().is_empty() || normalize_base_url(&input.base_url).is_empty() {
        return Err(AppError::BadRequest(
            "provider name and base URL are required".to_string(),
        ));
    }
    if !input.headers.is_object() && !input.headers.is_null() {
        return Err(AppError::BadRequest(
            "provider headers must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

fn validate_route_input(input: &RouteInput) -> AppResult<()> {
    if input.name.trim().is_empty() || input.model_pattern.trim().is_empty() {
        return Err(AppError::BadRequest(
            "route name and model pattern are required".to_string(),
        ));
    }
    validate_model_pattern(&input.model_pattern)?;
    validate_targets(&input.targets)
}

/// Reject patterns that `globset` cannot compile. Otherwise the route would be
/// stored successfully but silently skipped at request time, surfacing as a
/// confusing "no route matches" error much later.
fn validate_model_pattern(pattern: &str) -> AppResult<()> {
    let pattern = pattern.trim();
    globset::Glob::new(pattern).map(|_| ()).map_err(|error| {
        AppError::BadRequest(format!("invalid model pattern '{pattern}': {error}"))
    })
}

fn validate_targets(targets: &[RouteTargetInput]) -> AppResult<()> {
    if targets.is_empty() {
        return Err(AppError::BadRequest(
            "a route must have at least one enabled target".to_string(),
        ));
    }
    // A route whose targets are all disabled would still match incoming
    // requests, then fail every one with "no enabled provider targets". Reject
    // it at write time; disable the route itself instead.
    if !targets.iter().any(|target| target.enabled) {
        return Err(AppError::BadRequest(
            "a route must have at least one enabled target; disable the route instead of all of its targets".to_string(),
        ));
    }
    if targets
        .iter()
        .any(|target| target.provider_id <= 0 || target.upstream_model.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "every route target needs a provider and upstream model".to_string(),
        ));
    }
    if targets.iter().any(|target| target.weight <= 0) {
        return Err(AppError::BadRequest(
            "route target weight must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn ollama_root(base_url: &str) -> &str {
    base_url
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or_else(|| base_url.trim_end_matches('/'))
}

fn normalize_model_prefix(value: &str) -> AppResult<String> {
    let value = value.trim().trim_matches('/').trim().to_string();
    if value.is_empty() {
        return Ok(value);
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(AppError::BadRequest(
            "model prefix may only contain letters, numbers, '-', '_' and '.'".to_string(),
        ));
    }
    Ok(format!("{value}/"))
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn map_sqlite_conflict(error: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(database_error) = &error {
        if database_error.is_unique_violation() {
            // SQLite reports the violated index/column in the message; use it
            // to tell the operator exactly which field collided instead of a
            // generic "something already exists".
            let detail = database_error.message().to_ascii_lowercase();
            let message = if detail.contains("model_prefix") {
                "another provider already uses this model prefix"
            } else if detail.contains("providers.name") || detail.contains("idx_providers_name") {
                "a provider with this name already exists"
            } else if detail.contains("idx_routes_model_pattern")
                || detail.contains("routes.model_pattern")
            {
                "a route with this model pattern already exists"
            } else if detail.contains("routes.name") {
                "a route with this name already exists"
            } else if detail.contains("route_targets") {
                "this provider and upstream model are already used by the route"
            } else {
                "an item with the same name/pattern already exists"
            };
            return AppError::Conflict(message.to_string());
        }
        if database_error.is_foreign_key_violation() {
            return AppError::BadRequest("referenced provider does not exist".to_string());
        }
    }
    AppError::Database(error)
}

fn hash_secret(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn apply_usage_filters<'a>(builder: &mut QueryBuilder<'a, Sqlite>, query: &'a UsageQuery) {
    if let Some(provider_id) = query.provider_id {
        builder.push(" AND u.provider_id = ").push_bind(provider_id);
    }
    if let Some(route_id) = query.route_id {
        builder.push(" AND u.route_id = ").push_bind(route_id);
    }
    if let Some(model) = &query.model
        && !model.trim().is_empty()
    {
        builder
            .push(" AND u.requested_model LIKE ")
            .push_bind(format!("%{}%", model.trim()));
    }
    if let Some(request_id) = &query.request_id
        && !request_id.trim().is_empty()
    {
        builder
            .push(" AND u.request_id LIKE ")
            .push_bind(format!("%{}%", request_id.trim()));
    }
    if let Some(success) = query.success {
        builder.push(" AND u.success = ").push_bind(success as i64);
    }
    if let Some(from) = &query.from {
        builder.push(" AND u.created_at >= ").push_bind(from);
    }
    if let Some(to) = &query.to {
        builder.push(" AND u.created_at <= ").push_bind(to);
    }
}

impl From<ApiKeyRecord> for ApiKeyView {
    fn from(value: ApiKeyRecord) -> Self {
        Self {
            id: value.id,
            name: value.name,
            key_prefix: value.key_prefix,
            key_suffix: value.key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            created_at: value.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_model_prefix_with_trailing_slash() {
        assert_eq!(normalize_model_prefix("openai").unwrap(), "openai/");
        assert_eq!(normalize_model_prefix("openai/").unwrap(), "openai/");
        assert_eq!(normalize_model_prefix("  /local/  ").unwrap(), "local/");
        assert_eq!(normalize_model_prefix("").unwrap(), "");
    }

    #[test]
    fn provider_test_urls_reuse_the_shared_joiner() {
        // The probe must hit the inference endpoint, which is what enforces
        // credentials, and must not double up `/v1`.
        assert_eq!(
            join_upstream_url(
                "https://api.commandcode.ai/provider/v1",
                "/v1/chat/completions"
            ),
            "https://api.commandcode.ai/provider/v1/chat/completions"
        );
        assert_eq!(
            join_upstream_url("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        // A base without `/v1` keeps the full path.
        assert_eq!(
            join_upstream_url("http://localhost:8000", "/v1/chat/completions"),
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_model_prefix_with_invalid_characters() {
        assert!(normalize_model_prefix("bad prefix").is_err());
        assert!(normalize_model_prefix("spaces/and").is_err());
    }

    #[test]
    fn derives_ollama_root_from_root_and_v1_base_urls() {
        assert_eq!(
            ollama_root("http://localhost:11434"),
            "http://localhost:11434"
        );
        assert_eq!(
            ollama_root("http://localhost:11434/"),
            "http://localhost:11434"
        );
        assert_eq!(
            ollama_root("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
    }

    #[test]
    fn trims_trailing_slash_from_base_url() {
        assert_eq!(
            normalize_base_url("https://api.example.com/v1/"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            normalize_base_url("  https://api.example.com/v1  "),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn parses_openai_style_model_lists() {
        let value = json!({ "object": "list", "data": [
            { "id": "gpt-4.1" },
            { "id": "gpt-4.1-mini" },
            { "name": "fallback-name" }
        ]});
        let names = parse_openai_model_entries(&value)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["gpt-4.1", "gpt-4.1-mini", "fallback-name"]);
    }

    #[test]
    fn captures_upstream_context_length_and_endpoints() {
        // CommandCode reports a flat `context_length` for every model, even ones
        // models.dev has never heard of.
        let value = json!({ "data": [
            { "id": "deepseek/deepseek-v4.1-flash", "context_length": 1000000,
              "supported_endpoints": ["/chat/completions", "/responses"] }
        ]});
        let entries = parse_openai_model_entries(&value);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.context_limit, Some(1000000));
        assert_eq!(
            entries[0].1.supported_endpoints,
            vec!["/chat/completions", "/responses"]
        );
    }

    #[test]
    fn prefers_stricter_upstream_input_limit() {
        // The live CallAI catalog advertises 400K input, while models.dev
        // reports a 1.05M window. Keeping the smaller value prevents Codex from
        // compacting too late and hitting an upstream 400.
        let value = json!({ "data": [
            {
                "id": "gpt-6-astra",
                "context_length": 1050000,
                "max_input_tokens": 400000
            }
        ]});
        let entries = parse_openai_model_entries(&value);
        assert_eq!(entries[0].1.context_limit, Some(400_000));
    }

    #[test]
    fn accepts_max_input_tokens_without_context_length() {
        let value = json!({ "data": [
            { "id": "codex-auto-review", "max_input_tokens": 400000 }
        ]});
        let entries = parse_openai_model_entries(&value);
        assert_eq!(entries[0].1.context_limit, Some(400_000));
    }

    #[test]
    fn parses_ollama_style_model_lists() {
        let value = json!({ "models": [
            { "name": "llama3:8b", "model": "llama3:8b" },
            { "model": "qwen2:7b" }
        ]});
        assert_eq!(parse_ollama_models(&value), vec!["llama3:8b", "qwen2:7b"]);
    }

    #[test]
    fn ignores_empty_model_entries() {
        let value = json!({ "data": [{ "id": "  " }, { "id": "real-model" }] });
        let names = parse_openai_model_entries(&value)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["real-model"]);
    }

    #[test]
    fn accepts_valid_glob_model_patterns() {
        assert!(validate_model_pattern("gpt-*").is_ok());
        assert!(validate_model_pattern("*").is_ok());
        assert!(validate_model_pattern("vendor/model-?").is_ok());
        assert!(validate_model_pattern("exact-model").is_ok());
    }

    #[test]
    fn rejects_uncompilable_glob_model_patterns() {
        assert!(validate_model_pattern("unclosed[").is_err());
    }

    #[test]
    fn rejects_route_when_every_target_is_disabled() {
        let disabled = vec![RouteTargetInput {
            id: None,
            provider_id: 1,
            upstream_model: "m".to_string(),
            weight: 100,
            priority: 0,
            enabled: false,
        }];
        assert!(validate_targets(&disabled).is_err());

        let enabled = vec![RouteTargetInput {
            id: None,
            provider_id: 1,
            upstream_model: "m".to_string(),
            weight: 100,
            priority: 0,
            enabled: true,
        }];
        assert!(validate_targets(&enabled).is_ok());
    }
}
