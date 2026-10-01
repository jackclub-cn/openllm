use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Timelike, Utc};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::models_dev;
use crate::proxy::{apply_custom_headers, join_upstream_url, upstream_rejects_tool_search};
use crate::state::AppState;

const SETTING_USAGE_RETENTION_DAYS: &str = "usage_retention_days";
const USAGE_RETENTION_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const MAX_OVERVIEW_RANGE_DAYS: i64 = 366;

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

pub async fn get_settings(State(state): State<AppState>) -> AppResult<Json<SettingsView>> {
    Ok(Json(SettingsView {
        admin_auth_enabled: state.admin_token.is_some(),
        database: "sqlite",
        version: env!("CARGO_PKG_VERSION"),
        database_stats: load_database_stats(&state).await?,
    }))
}

async fn load_database_stats(state: &AppState) -> AppResult<DatabaseStats> {
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&state.pool)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&state.pool)
        .await?;
    let free_pages: i64 = sqlx::query_scalar("PRAGMA freelist_count")
        .fetch_one(&state.pool)
        .await?;
    let (_, _, path) = sqlx::query_as::<_, (i64, String, String)>("PRAGMA database_list")
        .fetch_one(&state.pool)
        .await?;
    let (
        providers,
        provider_models,
        provider_api_keys,
        routes,
        access_keys,
        usage_logs,
        in_flight_requests,
    ) = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64)>(
        "SELECT \
            (SELECT COUNT(*) FROM providers), \
            (SELECT COUNT(*) FROM provider_models), \
            (SELECT COUNT(*) FROM provider_api_keys), \
            (SELECT COUNT(*) FROM routes), \
            (SELECT COUNT(*) FROM api_keys), \
            (SELECT requests FROM usage_lifetime_stats WHERE id = 1) \
                + (SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1), \
            (SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1)",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(DatabaseStats {
        path: (!path.is_empty()).then_some(path),
        size_bytes: page_count.saturating_mul(page_size),
        free_bytes: free_pages.saturating_mul(page_size),
        providers,
        provider_models,
        provider_api_keys,
        routes,
        access_keys,
        usage_logs,
        in_flight_requests,
    })
}

pub async fn get_runtime_settings(
    State(state): State<AppState>,
) -> AppResult<Json<RuntimeSettingsView>> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(SETTING_USAGE_RETENTION_DAYS)
        .fetch_optional(&state.pool)
        .await?;
    let usage_retention_days = raw.and_then(|value| value.parse::<i64>().ok());
    Ok(Json(RuntimeSettingsView {
        usage_retention_days,
    }))
}

pub async fn update_runtime_settings(
    State(state): State<AppState>,
    Json(input): Json<RuntimeSettingsUpdate>,
) -> AppResult<Json<RuntimeSettingsView>> {
    let usage_retention_days = normalize_retention_days(input.usage_retention_days)?;
    if let Some(days) = usage_retention_days {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTING_USAGE_RETENTION_DAYS)
        .bind(days.to_string())
        .execute(&state.pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(SETTING_USAGE_RETENTION_DAYS)
            .execute(&state.pool)
            .await?;
    }
    *state.retention_last_run.lock().await = None;
    Ok(Json(RuntimeSettingsView {
        usage_retention_days,
    }))
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
        hydrate_provider_view(&state, &mut view).await?;
        views.push(view);
    }
    Ok(Json(views))
}

pub async fn provider_quota(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<ProviderQuotaQuery>,
) -> AppResult<Json<ProviderQuotaView>> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let kind = provider_quota_kind(&provider.base_url)
        .ok_or_else(|| AppError::BadRequest("this provider has no quota API".to_string()))?;
    let credential = provider_quota_credential(&state, &provider, query.key_id).await?;
    let prices = provider_prices(&state, provider.id).await?;
    let mut view = match kind {
        "command_code" => fetch_command_code_quota(&state, &credential.secret).await?,
        "opencode_go" => fetch_opencode_go_quota(&state, &credential.secret).await?,
        "deepseek" => fetch_deepseek_quota(&state, &credential.secret).await?,
        _ => {
            return Err(AppError::BadRequest(
                "this provider has no quota API".to_string(),
            ));
        }
    };
    view.key_id = credential.key_id;
    view.key_name = Some(credential.key_name);
    view.key_suffix = Some(credential.key_suffix);
    view.prices = prices;
    Ok(Json(view))
}

struct ProviderQuotaCredential {
    key_id: Option<i64>,
    key_name: String,
    key_suffix: String,
    secret: String,
}

async fn provider_quota_credential(
    state: &AppState,
    provider: &Provider,
    key_id: Option<i64>,
) -> AppResult<ProviderQuotaCredential> {
    let records = provider_api_key_records(&state.pool, provider.id).await?;
    if let Some(key_id) = key_id {
        let record = records
            .into_iter()
            .find(|record| record.id == key_id)
            .ok_or_else(|| AppError::NotFound("provider API key not found".to_string()))?;
        return quota_credential_from_record(record);
    }
    if let Some(record) = records
        .iter()
        .find(|record| record.enabled != 0 && !record.secret.is_empty())
        .or_else(|| records.iter().find(|record| !record.secret.is_empty()))
        .cloned()
    {
        return quota_credential_from_record(record);
    }
    let secret = normalize_optional(provider.api_key.clone())
        .ok_or_else(|| AppError::BadRequest("provider has no credentials to query".to_string()))?;
    Ok(ProviderQuotaCredential {
        key_id: None,
        key_name: "Default".to_string(),
        key_suffix: api_key_suffix(&secret),
        secret,
    })
}

fn quota_credential_from_record(
    record: ProviderApiKeyRecord,
) -> AppResult<ProviderQuotaCredential> {
    let secret = normalize_optional(Some(record.secret))
        .ok_or_else(|| AppError::BadRequest("provider API key is empty".to_string()))?;
    let key_name = if record.name.trim().is_empty() {
        format!("Key {}", record.id)
    } else {
        record.name
    };
    Ok(ProviderQuotaCredential {
        key_id: Some(record.id),
        key_name,
        key_suffix: api_key_suffix(&secret),
        secret,
    })
}

pub(crate) fn provider_quota_kind(base_url: &str) -> Option<&'static str> {
    let base_url = base_url.to_ascii_lowercase();
    if base_url.contains("api.commandcode.ai") {
        Some("command_code")
    } else if base_url.contains("opencode.ai/zen/go") {
        Some("opencode_go")
    } else if base_url.contains("api.deepseek.com") {
        Some("deepseek")
    } else {
        None
    }
}

fn quota_title(kind: &str) -> &'static str {
    match kind {
        "command_code" => "Command Code 额度",
        "opencode_go" => "OpenCode Go 额度",
        "deepseek" => "DeepSeek 余额与价格",
        _ => "提供商额度",
    }
}

async fn fetch_quota_json(
    state: &AppState,
    url: &str,
    api_key: &str,
    headers: &[(&str, &str)],
) -> AppResult<Value> {
    let mut request = state
        .client
        .get(url)
        .bearer_auth(api_key)
        .header(reqwest::header::ACCEPT, "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::Upstream(format!("quota request failed: {error}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| AppError::Upstream(format!("quota response failed: {error}")))?;
    if !status.is_success() {
        let summary = body.chars().take(300).collect::<String>();
        return Err(AppError::Upstream(format!(
            "quota endpoint returned {status}: {summary}"
        )));
    }
    serde_json::from_str(&body)
        .map_err(|error| AppError::Upstream(format!("invalid quota response: {error}")))
}

async fn fetch_command_code_quota(state: &AppState, api_key: &str) -> AppResult<ProviderQuotaView> {
    let headers = [("User-Agent", "cli"), ("x-cli-environment", "cli")];
    let (credits, subscription) = tokio::join!(
        fetch_quota_json(
            state,
            "https://api.commandcode.ai/alpha/billing/credits",
            api_key,
            &headers,
        ),
        fetch_quota_json(
            state,
            "https://api.commandcode.ai/alpha/billing/subscriptions",
            api_key,
            &headers,
        ),
    );
    let credits = credits?;
    let subscription = subscription.ok();
    let plan_id = subscription
        .as_ref()
        .and_then(|value| value.pointer("/data/planId"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let plan_name = plan_id.as_deref().map(command_code_plan_name);
    let monthly_remaining = credits
        .pointer("/credits/monthlyCredits")
        .and_then(Value::as_f64);
    let monthly_total = plan_id.as_deref().and_then(command_code_monthly_total);
    let monthly_reset = subscription
        .as_ref()
        .and_then(|value| value.pointer("/data/currentPeriodEnd"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut items = Vec::new();
    for (key, label, path) in [
        ("five_hour", "5 小时", "/windowLimits/fiveHour"),
        ("weekly", "周", "/windowLimits/weekly"),
    ] {
        let Some(window) = credits.pointer(path) else {
            continue;
        };
        let used = window.get("used").and_then(Value::as_f64);
        let limit = window.get("cap").and_then(Value::as_f64);
        items.push(ProviderQuotaItem {
            key: key.to_string(),
            label: label.to_string(),
            used,
            limit,
            remaining: used.zip(limit).map(|(used, limit)| (limit - used).max(0.0)),
            unit: "USD".to_string(),
            percent: used
                .zip(limit)
                .filter(|(_, limit)| *limit > 0.0)
                .map(|(used, limit)| (used / limit * 100.0).clamp(0.0, 100.0)),
            reset_at: window
                .get("resetAt")
                .and_then(Value::as_i64)
                .and_then(epoch_millis_to_rfc3339),
        });
    }
    if let Some(remaining) = monthly_remaining {
        let used = monthly_total.map(|total| (total - remaining).max(0.0));
        items.push(ProviderQuotaItem {
            key: "monthly".to_string(),
            label: "月".to_string(),
            used,
            limit: monthly_total,
            remaining: Some(remaining),
            unit: "USD".to_string(),
            percent: monthly_total
                .filter(|total| *total > 0.0)
                .map(|total| (used.unwrap_or(0.0) / total * 100.0).clamp(0.0, 100.0)),
            reset_at: monthly_reset,
        });
    }
    let details = [
        ("已购买额度", credits.pointer("/credits/purchasedCredits")),
        ("赠送额度", credits.pointer("/credits/freeCredits")),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        value
            .and_then(Value::as_f64)
            .map(|value| ProviderQuotaDetail {
                label: label.to_string(),
                value: format!("${value:.4}"),
            })
    })
    .chain(plan_id.as_ref().map(|plan_id| ProviderQuotaDetail {
        label: "套餐 ID".to_string(),
        value: plan_id.clone(),
    }))
    .collect();
    Ok(ProviderQuotaView {
        kind: "command_code".to_string(),
        title: quota_title("command_code").to_string(),
        plan_name,
        key_id: None,
        key_name: None,
        key_suffix: None,
        source_url: Some("https://commandcode.ai/".to_string()),
        items,
        details,
        prices: Vec::new(),
        fetched_at: Utc::now().to_rfc3339(),
    })
}

async fn fetch_opencode_go_quota(state: &AppState, api_key: &str) -> AppResult<ProviderQuotaView> {
    let body = fetch_quota_json(state, "https://opencode.ai/zen/go/v1/usage", api_key, &[]).await?;
    let mut items = Vec::new();
    for (key, label) in [("rolling", "5 小时"), ("weekly", "周"), ("monthly", "月")] {
        let Some(window) = body.pointer(&format!("/usage/{key}")) else {
            continue;
        };
        let percent = window
            .get("percent")
            .and_then(Value::as_f64)
            .map(|value| value.clamp(0.0, 100.0));
        items.push(ProviderQuotaItem {
            key: key.to_string(),
            label: label.to_string(),
            used: None,
            limit: None,
            remaining: None,
            unit: "%".to_string(),
            percent,
            reset_at: quota_reset_at(window.get("resetsAt")),
        });
    }
    Ok(ProviderQuotaView {
        kind: "opencode_go".to_string(),
        title: quota_title("opencode_go").to_string(),
        plan_name: Some("Go".to_string()),
        key_id: None,
        key_name: None,
        key_suffix: None,
        source_url: Some("https://opencode.ai/docs/go".to_string()),
        items,
        details: Vec::new(),
        prices: Vec::new(),
        fetched_at: Utc::now().to_rfc3339(),
    })
}

async fn fetch_deepseek_quota(state: &AppState, api_key: &str) -> AppResult<ProviderQuotaView> {
    let body =
        fetch_quota_json(state, "https://api.deepseek.com/user/balance", api_key, &[]).await?;
    let available = body
        .get("is_available")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut items = Vec::new();
    let mut details = vec![ProviderQuotaDetail {
        label: "账户可用".to_string(),
        value: if available { "是" } else { "否" }.to_string(),
    }];
    if let Some(balances) = body.get("balance_infos").and_then(Value::as_array) {
        for balance in balances {
            let Some(currency) = balance.get("currency").and_then(Value::as_str) else {
                continue;
            };
            let Some(remaining) = balance
                .get("total_balance")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<f64>().ok())
            else {
                continue;
            };
            items.push(ProviderQuotaItem {
                key: format!("balance_{}", currency.to_ascii_lowercase()),
                label: format!("{currency} 余额"),
                used: None,
                limit: None,
                remaining: Some(remaining),
                unit: currency.to_string(),
                percent: None,
                reset_at: None,
            });
            for (key, label) in [
                ("granted_balance", "赠送余额"),
                ("topped_up_balance", "充值余额"),
            ] {
                if let Some(value) = balance.get(key).and_then(Value::as_str) {
                    details.push(ProviderQuotaDetail {
                        label: format!("{currency} {label}"),
                        value: value.to_string(),
                    });
                }
            }
        }
    }
    Ok(ProviderQuotaView {
        kind: "deepseek".to_string(),
        title: quota_title("deepseek").to_string(),
        plan_name: None,
        key_id: None,
        key_name: None,
        key_suffix: None,
        source_url: Some("https://api-docs.deepseek.com/quick_start/pricing".to_string()),
        items,
        details,
        prices: Vec::new(),
        fetched_at: Utc::now().to_rfc3339(),
    })
}

fn epoch_millis_to_rfc3339(value: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(value).map(|value| value.to_rfc3339())
}

fn quota_reset_at(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => {
            let value = value.as_i64()?;
            let millis = if value >= 10_000_000_000 {
                value
            } else {
                value.saturating_mul(1000)
            };
            epoch_millis_to_rfc3339(millis).or_else(|| Some(value.to_string()))
        }
        _ => None,
    }
}

fn command_code_plan_name(plan_id: &str) -> String {
    match plan_id {
        "individual-go" => "Go".to_string(),
        "individual-goat" => "GOAT".to_string(),
        "individual-pro" | "individual-pro-v1" => "Pro".to_string(),
        "individual-provider" => "Provider".to_string(),
        "individual-max" => "Max".to_string(),
        "individual-ultra" => "Ultra".to_string(),
        "teams-pro" => "Team Pro".to_string(),
        _ => plan_id.to_string(),
    }
}

fn command_code_monthly_total(plan_id: &str) -> Option<f64> {
    match plan_id {
        "individual-go" => Some(10.0),
        "individual-goat" => Some(70.0),
        "individual-pro" => Some(80.0),
        "teams-pro" => Some(40.0),
        _ => None,
    }
}

async fn provider_prices(state: &AppState, provider_id: i64) -> AppResult<Vec<ProviderPriceView>> {
    Ok(provider_model_limits(state, provider_id)
        .await?
        .into_iter()
        .filter(|model| model.enabled)
        .filter_map(|model| {
            let has_price = model.cost_input.is_some()
                || model.cost_output.is_some()
                || model.cost_cache_read.is_some()
                || model.cost_cache_write.is_some();
            has_price.then_some(ProviderPriceView {
                model_name: model.model_name,
                input: model.cost_input,
                output: model.cost_output,
                cache_read: model.cost_cache_read,
                cache_write: model.cost_cache_write,
            })
        })
        .collect())
}

pub async fn create_provider(
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> AppResult<(StatusCode, Json<ProviderView>)> {
    validate_provider_input(&input)?;
    let headers = serde_json::to_string(&input.headers).unwrap_or_else(|_| "{}".to_string());
    let model_prefix = normalize_model_prefix(&input.model_prefix)?;
    let api_key = normalize_optional(input.api_key.clone());
    let mut api_keys = input.api_keys.clone();
    if api_keys.is_empty()
        && let Some(secret) = api_key.clone()
    {
        api_keys.push(provider_api_key_input(None, "Default", Some(secret), true));
    }
    let base_url = normalize_base_url(&input.base_url);
    let health_check_interval_minutes =
        normalize_health_interval(input.health_check_interval_minutes)?;
    let health_check_model = normalize_health_check_model(input.health_check_model.as_deref());
    let models_sync_interval_minutes =
        normalize_health_interval(input.models_sync_interval_minutes)?;
    // Resolve metadata before opening the transaction: the catalog fetch may
    // hit the network, and holding a SQLite write transaction across it would
    // block every other writer.
    let catalog = models_dev::try_load(&state).await;
    let models_dev_id = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(input.name.trim(), &base_url));

    let mut tx = state.pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO providers (
            name, provider_type, base_url, model_prefix, models_dev_id,
            api_key, headers, enabled, tool_search_supported,
            health_check_interval_minutes, health_check_model,
            models_sync_interval_minutes
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(input.name.trim())
    .bind(input.provider_type.as_str())
    .bind(base_url)
    .bind(model_prefix)
    .bind(models_dev_id.as_deref())
    .bind(api_key)
    .bind(headers)
    .bind(input.enabled as i64)
    .bind(health_check_interval_minutes)
    .bind(health_check_model)
    .bind(models_sync_interval_minutes)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    let id = result.last_insert_rowid();
    replace_provider_api_keys(&mut tx, id, &api_keys).await?;
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

    let name = input
        .name
        .unwrap_or_else(|| current.name.clone())
        .trim()
        .to_string();
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
    let health_check_interval_minutes = match input.health_check_interval_minutes {
        Some(value) => normalize_health_interval(Some(value))?,
        None => current.health_check_interval_minutes,
    };
    let health_check_model = match input.health_check_model.as_deref() {
        Some(value) => normalize_health_check_model(Some(value)),
        None => current.health_check_model,
    };
    let models_sync_interval_minutes = match input.models_sync_interval_minutes {
        Some(value) => normalize_health_interval(Some(value))?,
        None => current.models_sync_interval_minutes,
    };
    let api_key_update = normalize_optional(input.api_key.clone());
    let api_keys_update = if let Some(api_keys) = input.api_keys.clone() {
        Some(api_keys)
    } else if input.clear_api_key.unwrap_or(false) {
        Some(Vec::new())
    } else {
        api_key_update
            .map(|secret| vec![provider_api_key_input(None, "Default", Some(secret), true)])
    };
    let api_keys_changed = api_keys_update.is_some();
    let previous_key_ids = if api_keys_changed {
        provider_api_key_records(&state.pool, id)
            .await?
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
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
    // A status-only update must not wait on catalog refreshes.
    let identity_changed = name != current.name || base_url != current.base_url;
    let catalog = if identity_changed || input.models.is_some() {
        models_dev::try_load(&state).await
    } else {
        None
    };
    let models_dev_id = if identity_changed {
        catalog
            .as_ref()
            .and_then(|catalog| catalog.match_provider(&name, &base_url))
    } else {
        current.models_dev_id.clone()
    };

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE providers SET name = ?, provider_type = ?, base_url = ?, model_prefix = ?, models_dev_id = ?, headers = ?, enabled = ?, health_check_interval_minutes = ?, health_check_model = ?, models_sync_interval_minutes = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name)
    .bind(provider_type.as_str())
    .bind(base_url)
    .bind(model_prefix)
    .bind(models_dev_id.as_deref())
    .bind(headers)
    .bind(enabled as i64)
    .bind(health_check_interval_minutes)
    .bind(health_check_model)
    .bind(models_sync_interval_minutes)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlite_conflict)?;

    if let Some(api_keys) = api_keys_update {
        replace_provider_api_keys(&mut tx, id, &api_keys).await?;
    }

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

    if api_keys_changed {
        let current_key_ids =
            sqlx::query_scalar::<_, i64>("SELECT id FROM provider_api_keys WHERE provider_id = ?")
                .bind(id)
                .fetch_all(&state.pool)
                .await?;
        clear_provider_key_cooldowns(&state, previous_key_ids.into_iter().chain(current_key_ids))
            .await;
    }

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
    Ok(Json(test_provider_inner(&state, id).await?))
}

pub async fn test_provider_keys(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ProviderKeyTestResult>> {
    Ok(Json(test_provider_keys_inner(&state, id).await?))
}

pub async fn test_all_provider_keys(
    State(state): State<AppState>,
) -> AppResult<Json<ProviderKeyTestAllResult>> {
    let providers = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, name FROM providers WHERE enabled = 1 ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;

    let results = futures_util::stream::iter(providers.into_iter().map(|(id, name)| {
        let state = state.clone();
        async move {
            match test_provider_keys_inner(&state, id).await {
                Ok(result) => ProviderKeyTestSummary {
                    provider_id: id,
                    provider_name: name,
                    total: result.total,
                    ok: result.ok,
                    failed: result.failed,
                    model: result.model,
                    message: if result.total == 0 {
                        "provider has no enabled keys".to_string()
                    } else {
                        format!("{} healthy, {} failed", result.ok, result.failed)
                    },
                },
                Err(error) => ProviderKeyTestSummary {
                    provider_id: id,
                    provider_name: name,
                    total: 0,
                    ok: 0,
                    failed: 1,
                    model: None,
                    message: error.to_string(),
                },
            }
        }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;

    let tested_providers = results.iter().filter(|result| result.total > 0).count();
    let healthy_providers = results
        .iter()
        .filter(|result| result.total > 0 && result.failed == 0)
        .count();
    let failed_providers = results.iter().filter(|result| result.failed > 0).count();
    let total_keys = results.iter().map(|result| result.total).sum();
    let healthy_keys = results.iter().map(|result| result.ok).sum();
    let failed_keys = results.iter().map(|result| result.failed).sum();
    Ok(Json(ProviderKeyTestAllResult {
        total_providers: results.len(),
        tested_providers,
        healthy_providers,
        failed_providers,
        total_keys,
        healthy_keys,
        failed_keys,
        results,
    }))
}

#[derive(Debug)]
struct ProviderProbeModel {
    name: String,
    supported_endpoints: Vec<String>,
}

#[derive(Debug)]
struct ProviderProbeKey {
    id: Option<i64>,
    name: String,
    api_key_suffix: String,
    secret: Option<String>,
}

async fn test_provider_inner(state: &AppState, id: i64) -> AppResult<ProviderTestResult> {
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
    let model = resolve_provider_probe_model(state, &provider).await?;

    // A provider may have several credentials. Test every enabled key and
    // consider the provider healthy when any one of them succeeds.
    let mut result = None;
    for (key_id, key) in provider_key_candidates(state, &provider).await? {
        let checked = if model.is_some() {
            "inference"
        } else {
            "models"
        };
        let request = build_provider_probe_request(
            state,
            &provider,
            provider_type,
            model.as_ref(),
            key.as_deref(),
            false,
        )?;
        let attempt = probe_provider(
            request,
            std::time::Instant::now(),
            checked,
            model
                .as_ref()
                .map(|model| model.name.as_str())
                .unwrap_or(""),
        )
        .await;
        persist_provider_key_result(
            state,
            key_id,
            attempt.ok,
            attempt.latency_ms,
            &attempt.checked,
            &attempt.message,
        )
        .await?;
        if attempt.ok {
            if let Some(supported) = probe_tool_search_support(
                state,
                &provider,
                provider_type,
                model.as_ref(),
                key.as_deref(),
            )
            .await?
            {
                persist_tool_search_support(state, provider.id, supported).await?;
            }
            if let Some(key_id) = key_id {
                state.provider_key_cooldown.lock().await.remove(&key_id);
            }
            result = Some(attempt);
            break;
        }
        result = Some(attempt);
    }
    let result = result.unwrap_or(ProviderTestResult {
        ok: false,
        latency_ms: started.elapsed().as_millis() as i64,
        message: "provider has no credentials to test".to_string(),
        checked: "none".to_string(),
    });
    persist_provider_test(state, id, &result).await?;
    Ok(result)
}

async fn test_provider_keys_inner(state: &AppState, id: i64) -> AppResult<ProviderKeyTestResult> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let provider_type =
        ProviderType::from_str(&provider.provider_type).map_err(AppError::BadRequest)?;
    let model = resolve_provider_probe_model(state, &provider).await?;
    let model_name = model
        .as_ref()
        .map(|model| model.name.as_str())
        .unwrap_or("");
    let checked = if model.is_some() {
        "inference"
    } else {
        "models"
    };

    let mut results = Vec::new();
    for key in provider_probe_keys(state, &provider).await? {
        let request = build_provider_probe_request(
            state,
            &provider,
            provider_type,
            model.as_ref(),
            key.secret.as_deref(),
            false,
        )?;
        let attempt = probe_provider(request, std::time::Instant::now(), checked, model_name).await;
        let result = ProviderKeyTestItem {
            key_id: key.id,
            key_name: key.name,
            api_key_suffix: key.api_key_suffix,
            ok: attempt.ok,
            latency_ms: attempt.latency_ms,
            message: attempt.message,
            checked: attempt.checked,
        };
        persist_provider_key_result(
            state,
            result.key_id,
            result.ok,
            result.latency_ms,
            &result.checked,
            &result.message,
        )
        .await?;
        results.push(result);
    }
    let ok = results.iter().filter(|result| result.ok).count();
    let total = results.len();
    Ok(ProviderKeyTestResult {
        provider_id: provider.id,
        provider_name: provider.name,
        total,
        ok,
        failed: total - ok,
        model: model.map(|model| model.name),
        results,
    })
}

async fn resolve_provider_probe_model(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Option<ProviderProbeModel>> {
    match normalize_health_check_model(provider.health_check_model.as_deref()) {
        Some(name) => {
            let endpoints = sqlx::query_scalar::<_, Option<String>>(
                "SELECT COALESCE(supported_endpoints_override, supported_endpoints) \
                 FROM provider_models \
                 WHERE provider_id = ? AND model_name = ? AND enabled = 1",
            )
            .bind(provider.id)
            .bind(&name)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
            Ok(Some(ProviderProbeModel {
                name,
                supported_endpoints: parse_provider_probe_endpoints(endpoints.as_deref()),
            }))
        }
        None => Ok(sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT model_name, \
                    COALESCE(supported_endpoints_override, supported_endpoints) \
             FROM provider_models WHERE provider_id = ? AND enabled = 1 \
             ORDER BY model_name LIMIT 1",
        )
        .bind(provider.id)
        .fetch_optional(&state.pool)
        .await?
        .map(|(name, endpoints)| ProviderProbeModel {
            name,
            supported_endpoints: parse_provider_probe_endpoints(endpoints.as_deref()),
        })),
    }
}

fn build_provider_probe_request(
    state: &AppState,
    provider: &Provider,
    provider_type: ProviderType,
    model: Option<&ProviderProbeModel>,
    key: Option<&str>,
    include_tool_search: bool,
) -> AppResult<reqwest::RequestBuilder> {
    let mut request = if let Some(model) = model {
        let (url, mut body) = match provider_type {
            ProviderType::Anthropic => (
                join_upstream_url(&provider.base_url, "/v1/messages"),
                json!({
                    "model": model.name.as_str(),
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "ping"}]
                }),
            ),
            ProviderType::Openai | ProviderType::Custom
                if !provider_probe_supports(&model.supported_endpoints, "/v1/chat/completions")
                    && provider_probe_supports(&model.supported_endpoints, "/v1/responses") =>
            {
                // Some compatibility gateways reject max_output_tokens when the
                // underlying provider cannot enforce it. A health check owns
                // this budget, so omit it rather than making the probe fail.
                (
                    join_upstream_url(&provider.base_url, "/v1/responses"),
                    json!({
                        "model": model.name.as_str(),
                        "input": "ping"
                    }),
                )
            }
            // Ollama's native tags endpoint needs no auth either, so probe its
            // chat endpoint for the same reason.
            _ => (
                join_upstream_url(&provider.base_url, "/v1/chat/completions"),
                json!({
                    "model": model.name.as_str(),
                    "messages": [{"role": "user", "content": "ping"}],
                    "max_tokens": 1
                }),
            ),
        };
        if include_tool_search {
            body["tools"] = json!([{"type": "tool_search", "execution": "client"}]);
        }
        state
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body)
    } else {
        // No model synced yet, so fall back to listing. This only proves the
        // host is reachable, which the result message says explicitly.
        let url = match provider_type {
            ProviderType::Anthropic => {
                format!("{}/v1/models", provider.base_url.trim_end_matches('/'))
            }
            ProviderType::Ollama => format!("{}/api/tags", ollama_root(&provider.base_url)),
            _ => format!("{}/models", provider.base_url.trim_end_matches('/')),
        };
        state.client.get(url)
    };

    if let Some(key) = key {
        request = match provider_type {
            ProviderType::Anthropic => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.bearer_auth(key),
        };
    }
    apply_custom_headers(request, &provider.headers)
}

async fn probe_tool_search_support(
    state: &AppState,
    provider: &Provider,
    provider_type: ProviderType,
    model: Option<&ProviderProbeModel>,
    key: Option<&str>,
) -> AppResult<Option<bool>> {
    if model.is_none() || !matches!(provider_type, ProviderType::Openai | ProviderType::Custom) {
        return Ok(None);
    }
    let request = build_provider_probe_request(state, provider, provider_type, model, key, true)?;
    let Ok(response) = request.send().await else {
        return Ok(None);
    };
    if response.status().is_success() {
        return Ok(Some(true));
    }
    let body = response.bytes().await.unwrap_or_default();
    Ok(upstream_rejects_tool_search(&body).then_some(false))
}

async fn persist_tool_search_support(
    state: &AppState,
    provider_id: i64,
    supported: bool,
) -> AppResult<()> {
    sqlx::query("UPDATE providers SET tool_search_supported = ? WHERE id = ?")
        .bind(supported as i64)
        .bind(provider_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn test_all_providers(
    State(state): State<AppState>,
) -> AppResult<Json<ProviderTestAllResult>> {
    let providers = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, name FROM providers WHERE enabled = 1 ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;

    let results = futures_util::stream::iter(providers.into_iter().map(|(id, name)| {
        let state = state.clone();
        async move {
            let result = test_provider_inner(&state, id).await;
            ProviderTestSummary {
                provider_id: id,
                provider_name: name,
                ok: result.as_ref().map(|result| result.ok).unwrap_or(false),
                latency_ms: result.as_ref().map(|result| result.latency_ms).unwrap_or(0),
                message: result
                    .as_ref()
                    .map(|result| result.message.clone())
                    .unwrap_or_else(|error| error.to_string()),
                checked: result
                    .as_ref()
                    .map(|result| result.checked.clone())
                    .unwrap_or_else(|_| "none".to_string()),
            }
        }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;

    let ok = results.iter().filter(|result| result.ok).count();
    Ok(Json(ProviderTestAllResult {
        total: results.len(),
        ok,
        failed: results.len() - ok,
        results,
    }))
}

pub async fn run_due_provider_health_checks(state: AppState) {
    let due = match sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM providers
        WHERE enabled = 1
          AND health_check_interval_minutes > 0
          AND (
              last_test_at IS NULL
              OR datetime(last_test_at) <= datetime(
                  'now',
                  '-' || health_check_interval_minutes || ' minutes'
              )
          )
        ORDER BY id
        "#,
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(%error, "failed to query providers due for health checks");
            return;
        }
    };

    futures_util::stream::iter(due.into_iter().map(|id| {
        let state = state.clone();
        async move {
            if let Err(error) = test_provider_inner(&state, id).await {
                tracing::warn!(provider_id = id, %error, "scheduled provider health check failed");
            }
        }
    }))
    .buffer_unordered(4)
    .for_each(|_| async {})
    .await;
}

pub async fn run_due_provider_model_syncs(state: AppState) {
    let due = match sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM providers
        WHERE enabled = 1
          AND models_sync_interval_minutes > 0
          AND (
              models_sync_attempted_at IS NULL
              OR datetime(models_sync_attempted_at) <= datetime(
                  'now',
                  '-' || models_sync_interval_minutes || ' minutes'
              )
          )
        ORDER BY id
        "#,
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(%error, "failed to query providers due for model sync");
            return;
        }
    };

    futures_util::stream::iter(due.into_iter().map(|id| {
        let state = state.clone();
        async move {
            if let Err(error) = sync_provider(state, id).await {
                tracing::warn!(provider_id = id, %error, "scheduled provider model sync failed");
            }
        }
    }))
    .buffer_unordered(2)
    .for_each(|_| async {})
    .await;
}

pub async fn reconcile_stale_usage_requests(state: AppState) {
    let stale_cutoff = (Utc::now() - Duration::minutes(15)).to_rfc3339();
    if let Err(error) = finish_interrupted_usage_requests(
        &state,
        Some(&stale_cutoff),
        "request was interrupted before completion",
    )
    .await
    {
        tracing::warn!(%error, "failed to reconcile stale in-flight usage logs");
    }
}

pub async fn run_due_usage_retention(state: AppState) {
    {
        let last_run = state.retention_last_run.lock().await;
        if last_run.is_some_and(|last_run| last_run.elapsed() < USAGE_RETENTION_CHECK_INTERVAL) {
            return;
        }
    }

    let raw = match sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(SETTING_USAGE_RETENTION_DAYS)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to read usage retention setting");
            return;
        }
    };

    if let Some(days) = raw.and_then(|value| value.parse::<i64>().ok())
        && days > 0
    {
        let cutoff = (Utc::now() - Duration::days(days)).to_rfc3339();
        match sqlx::query("DELETE FROM usage_logs WHERE created_at < ?")
            .bind(&cutoff)
            .execute(&state.pool)
            .await
        {
            Ok(result) => {
                tracing::info!(
                    deleted = result.rows_affected(),
                    retention_days = days,
                    %cutoff,
                    "automatic usage retention cleanup completed"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    retention_days = days,
                    "automatic usage retention cleanup failed"
                );
                return;
            }
        }
    }

    *state.retention_last_run.lock().await = Some(std::time::Instant::now());
}

pub async fn reconcile_interrupted_usage_requests(state: &AppState) -> AppResult<u64> {
    let updated = finish_interrupted_usage_requests(
        state,
        None,
        "gateway restarted before request completed",
    )
    .await?;
    if updated > 0 {
        tracing::warn!(updated, "reconciled interrupted usage logs after restart");
    }
    Ok(updated)
}

async fn finish_interrupted_usage_requests(
    state: &AppState,
    cutoff: Option<&str>,
    message: &str,
) -> AppResult<u64> {
    let base = r#"
        UPDATE usage_logs
        SET in_flight = 0,
            status_code = 499,
            success = 0,
            error_message = COALESCE(error_message, ?),
            latency_ms = CASE
                WHEN latency_ms > 0 THEN latency_ms
                ELSE CAST(
                    MAX(0, (julianday('now') - julianday(created_at)) * 86400000)
                    AS INTEGER
                )
            END
        WHERE in_flight = 1
    "#;
    let result = match cutoff {
        Some(cutoff) => {
            let query = format!("{base} AND COALESCE(last_activity_at, created_at) < ?");
            sqlx::query(&query)
                .bind(message)
                .bind(cutoff)
                .execute(&state.pool)
                .await?
        }
        None => sqlx::query(base).bind(message).execute(&state.pool).await?,
    };
    Ok(result.rows_affected())
}

async fn persist_provider_test(
    state: &AppState,
    provider_id: i64,
    result: &ProviderTestResult,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE providers \
         SET last_test_at = ?, last_test_ok = ?, last_test_latency_ms = ?, \
             last_test_checked = ?, last_test_message = ? \
         WHERE id = ?",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(result.ok as i64)
    .bind(result.latency_ms)
    .bind(&result.checked)
    .bind(&result.message)
    .bind(provider_id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn persist_provider_key_result(
    state: &AppState,
    key_id: Option<i64>,
    ok: bool,
    latency_ms: i64,
    checked: &str,
    message: &str,
) -> AppResult<()> {
    let Some(key_id) = key_id else {
        return Ok(());
    };
    if ok {
        state.provider_key_error_state.lock().await.remove(&key_id);
        sqlx::query(
            "UPDATE provider_api_keys \
             SET last_test_at = ?, last_test_ok = 1, last_test_latency_ms = ?, \
                 last_test_checked = ?, last_test_message = ?, \
                 last_error_at = NULL, last_error = NULL \
             WHERE id = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(latency_ms)
        .bind(checked)
        .bind(message)
        .bind(key_id)
        .execute(&state.pool)
        .await?;
    } else {
        sqlx::query(
            "UPDATE provider_api_keys \
             SET last_test_at = ?, last_test_ok = 0, last_test_latency_ms = ?, \
                 last_test_checked = ?, last_test_message = ? \
             WHERE id = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(latency_ms)
        .bind(checked)
        .bind(message)
        .bind(key_id)
        .execute(&state.pool)
        .await?;
    }
    Ok(())
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

pub async fn preview_provider_model_sync(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ModelSyncPreview>> {
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("provider not found".to_string()))?;
    let entries = fetch_provider_entries(&state, &provider).await?;
    let existing = sqlx::query_as::<_, ProviderModelPreviewRow>(
        "SELECT model_name, enabled, context_limit, input_limit, output_limit, \
                supported_endpoints, cost, display_name \
         FROM provider_models WHERE provider_id = ?",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| (row.model_name.clone(), row))
    .collect::<HashMap<_, _>>();
    let catalog = models_dev::try_load(&state).await;
    let provider_hint = catalog
        .as_ref()
        .and_then(|catalog| catalog.match_provider(&provider.name, &provider.base_url));
    let changed = detect_model_sync_changes(
        &entries,
        &existing,
        catalog.as_deref(),
        provider_hint.as_deref(),
    );

    Ok(Json(build_model_sync_preview(
        id, &entries, &existing, changed,
    )))
}

fn build_model_sync_preview(
    provider_id: i64,
    entries: &[(String, UpstreamModelInfo)],
    existing: &HashMap<String, ProviderModelPreviewRow>,
    changed: Vec<ModelSyncChange>,
) -> ModelSyncPreview {
    let upstream = entries
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<HashSet<_>>();
    let added = entries
        .iter()
        .filter(|(name, _)| !existing.contains_key(name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let removed = existing
        .keys()
        .filter(|name| !upstream.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    let retained = entries
        .iter()
        .filter(|(name, _)| existing.contains_key(name))
        .count();
    let disabled_retained = existing
        .iter()
        .filter(|(name, row)| row.enabled == 0 && upstream.contains(*name))
        .count();
    ModelSyncPreview {
        provider_id,
        added,
        removed,
        changed,
        retained,
        disabled_retained,
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ProviderModelPreviewRow {
    model_name: String,
    enabled: i64,
    context_limit: Option<i64>,
    input_limit: Option<i64>,
    output_limit: Option<i64>,
    supported_endpoints: Option<String>,
    cost: Option<String>,
    display_name: Option<String>,
}

fn detect_model_sync_changes(
    entries: &[(String, UpstreamModelInfo)],
    existing: &HashMap<String, ProviderModelPreviewRow>,
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
) -> Vec<ModelSyncChange> {
    let mut changed = Vec::new();
    for (model, upstream) in entries {
        let Some(current) = existing.get(model) else {
            continue;
        };
        let capabilities = catalog
            .and_then(|catalog| catalog.lookup(provider_hint, model))
            .unwrap_or_default()
            .with_effective_input_limit();
        let context_limit = min_known(upstream.context_limit, capabilities.context_limit);
        let input_limit = min_known(context_limit, capabilities.input_limit);
        let supported_endpoints = (!upstream.supported_endpoints.is_empty())
            .then(|| serde_json::to_string(&upstream.supported_endpoints).ok())
            .flatten();
        let cost = capabilities
            .cost
            .as_ref()
            .and_then(|cost| serde_json::to_string(cost).ok());

        let mut fields = Vec::new();
        if current.context_limit != context_limit {
            fields.push("context_limit");
        }
        if current.input_limit != input_limit {
            fields.push("input_limit");
        }
        if current.output_limit != capabilities.output_limit {
            fields.push("output_limit");
        }
        if current.supported_endpoints != supported_endpoints {
            fields.push("supported_endpoints");
        }
        if current.cost != cost {
            fields.push("cost");
        }
        if current.display_name != upstream.display_name {
            fields.push("display_name");
        }
        if !fields.is_empty() {
            changed.push(ModelSyncChange {
                model_name: model.clone(),
                fields: fields.into_iter().map(ToOwned::to_owned).collect(),
            });
        }
    }
    changed
}

pub async fn list_provider_model_limits(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<ProviderModelLimitView>>> {
    ensure_provider_exists(&state, id).await?;
    Ok(Json(provider_model_limits(&state, id).await?))
}

pub async fn list_model_inventory(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<ModelInventoryView>>> {
    let rows = sqlx::query_as::<_, ModelInventoryRow>(
        r#"
        SELECT pm.provider_id,
               p.name AS provider_name,
               p.enabled AS provider_enabled,
               p.model_prefix,
               pm.model_name,
               pm.enabled,
               COALESCE(pm.context_override, pm.context_limit) AS context_limit,
               CASE
                   WHEN COALESCE(pm.input_override, pm.input_limit) IS NULL
                       THEN COALESCE(pm.context_override, pm.context_limit)
                   WHEN COALESCE(pm.context_override, pm.context_limit) IS NULL
                       THEN COALESCE(pm.input_override, pm.input_limit)
                   ELSE MIN(
                       COALESCE(pm.input_override, pm.input_limit),
                       COALESCE(pm.context_override, pm.context_limit)
                   )
               END AS input_limit,
               COALESCE(pm.output_override, pm.output_limit) AS output_limit,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               pm.cost,
               pm.cost_input_override,
               pm.cost_output_override,
               pm.cost_cache_read_override,
               pm.cost_cache_write_override
        FROM provider_models pm
        JOIN providers p ON p.id = pm.provider_id
        ORDER BY p.name COLLATE NOCASE, pm.model_name COLLATE NOCASE
        "#,
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn update_provider_model_limits(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProviderModelLimitsUpdate>,
) -> AppResult<Json<Vec<ProviderModelLimitView>>> {
    ensure_provider_exists(&state, id).await?;
    let mut seen = HashSet::new();
    let mut endpoint_overrides = Vec::with_capacity(input.models.len());
    for model in &input.models {
        let name = model.model_name.trim();
        if name.is_empty() {
            return Err(AppError::BadRequest("model name is required".to_string()));
        }
        if !seen.insert(name.to_string()) {
            return Err(AppError::BadRequest(format!(
                "model '{name}' appears more than once"
            )));
        }
        validate_limit("context", model.context_limit)?;
        validate_limit("input", model.input_limit)?;
        validate_limit("output", model.output_limit)?;
        validate_cost_override("input cost", model.cost_input_override)?;
        validate_cost_override("output cost", model.cost_output_override)?;
        validate_cost_override("cache read cost", model.cost_cache_read_override)?;
        validate_cost_override("cache write cost", model.cost_cache_write_override)?;
        if let (Some(context), Some(input)) = (model.context_limit, model.input_limit)
            && input > context
        {
            return Err(AppError::BadRequest(format!(
                "model '{name}' input limit cannot exceed its context limit"
            )));
        }
        endpoint_overrides.push(serialize_endpoint_override(
            model.supported_endpoints_override.as_deref(),
        )?);
    }

    let mut tx = state.pool.begin().await?;
    for (model, endpoint_override) in input.models.iter().zip(&endpoint_overrides) {
        let result = sqlx::query(
            "UPDATE provider_models \
             SET enabled = ?, context_override = ?, input_override = ?, output_override = ?, \
                 supported_endpoints_override = ?, cost_input_override = ?, \
                 cost_output_override = ?, cost_cache_read_override = ?, \
                 cost_cache_write_override = ? \
             WHERE provider_id = ? AND model_name = ?",
        )
        .bind(model.enabled as i64)
        .bind(model.context_limit)
        .bind(model.input_limit)
        .bind(model.output_limit)
        .bind(endpoint_override)
        .bind(model.cost_input_override)
        .bind(model.cost_output_override)
        .bind(model.cost_cache_read_override)
        .bind(model.cost_cache_write_override)
        .bind(id)
        .bind(model.model_name.trim())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!(
                "model '{}' was not found for this provider",
                model.model_name.trim()
            )));
        }
    }
    tx.commit().await?;

    Ok(Json(provider_model_limits(&state, id).await?))
}

async fn ensure_provider_exists(state: &AppState, id: i64) -> AppResult<()> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM providers WHERE id = ?)")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound("provider not found".to_string()))
    }
}

async fn provider_model_limits(
    state: &AppState,
    provider_id: i64,
) -> AppResult<Vec<ProviderModelLimitView>> {
    let rows = sqlx::query_as::<_, ProviderModelLimitRow>(
        r#"
        SELECT model_name, enabled,
               COALESCE(context_override, context_limit) AS context_limit,
               CASE
                   WHEN COALESCE(input_override, input_limit) IS NULL
                       THEN COALESCE(context_override, context_limit)
                   WHEN COALESCE(context_override, context_limit) IS NULL
                       THEN COALESCE(input_override, input_limit)
                   ELSE MIN(
                       COALESCE(input_override, input_limit),
                       COALESCE(context_override, context_limit)
                   )
               END AS input_limit,
               COALESCE(output_override, output_limit) AS output_limit,
               context_override,
               input_override,
               output_override,
               COALESCE(supported_endpoints_override, supported_endpoints)
                   AS supported_endpoints,
               supported_endpoints_override,
               cost,
               cost_input_override,
               cost_output_override,
               cost_cache_read_override,
               cost_cache_write_override
        FROM provider_models
        WHERE provider_id = ?
        ORDER BY model_name COLLATE NOCASE
        "#,
    )
    .bind(provider_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(Debug, sqlx::FromRow)]
struct ProviderModelLimitRow {
    model_name: String,
    enabled: bool,
    context_limit: Option<i64>,
    input_limit: Option<i64>,
    output_limit: Option<i64>,
    context_override: Option<i64>,
    input_override: Option<i64>,
    output_override: Option<i64>,
    supported_endpoints: Option<String>,
    supported_endpoints_override: Option<String>,
    cost: Option<String>,
    cost_input_override: Option<f64>,
    cost_output_override: Option<f64>,
    cost_cache_read_override: Option<f64>,
    cost_cache_write_override: Option<f64>,
}

impl From<ProviderModelLimitRow> for ProviderModelLimitView {
    fn from(value: ProviderModelLimitRow) -> Self {
        let cost = value
            .cost
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let effective_cost = crate::models::effective_cost_value(
            cost.as_ref(),
            value.cost_input_override,
            value.cost_output_override,
            value.cost_cache_read_override,
            value.cost_cache_write_override,
        );
        Self {
            model_name: value.model_name,
            enabled: value.enabled,
            supported_endpoints: value
                .supported_endpoints
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                .unwrap_or_default(),
            supported_endpoints_override: value
                .supported_endpoints_override
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok()),
            context_limit: value.context_limit,
            input_limit: value.input_limit,
            output_limit: value.output_limit,
            context_override: value.context_override,
            input_override: value.input_override,
            output_override: value.output_override,
            cost_input: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "input")),
            cost_output: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "output")),
            cost_cache_read: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "cache_read")),
            cost_cache_write: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "cache_write")),
            cost_input_override: value.cost_input_override,
            cost_output_override: value.cost_output_override,
            cost_cache_read_override: value.cost_cache_read_override,
            cost_cache_write_override: value.cost_cache_write_override,
        }
    }
}

fn validate_limit(name: &str, value: Option<i64>) -> AppResult<()> {
    if value.is_some_and(|value| value <= 0) {
        return Err(AppError::BadRequest(format!(
            "{name} limit must be a positive integer"
        )));
    }
    Ok(())
}

fn validate_cost_override(name: &str, value: Option<f64>) -> AppResult<()> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(AppError::BadRequest(format!(
            "{name} override must be a non-negative number"
        )));
    }
    Ok(())
}

fn serialize_endpoint_override(value: Option<&[String]>) -> AppResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut endpoints = Vec::new();
    for raw in value {
        let endpoint = raw.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            continue;
        }
        if !endpoint.starts_with('/') {
            return Err(AppError::BadRequest(format!(
                "endpoint '{raw}' must start with '/'"
            )));
        }
        if endpoint.len() > 200 {
            return Err(AppError::BadRequest(format!(
                "endpoint '{raw}' is too long"
            )));
        }
        if !endpoints.iter().any(|existing| existing == endpoint) {
            endpoints.push(endpoint.to_string());
        }
    }
    if endpoints.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&endpoints)
        .map(Some)
        .map_err(|error| AppError::Internal(error.into()))
}

async fn fetch_provider_entries(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<(String, UpstreamModelInfo)>> {
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

    let mut last_error = None;
    for (_, key) in provider_key_candidates(state, provider).await? {
        let mut request = state.client.get(&url);
        if let Some(key) = key {
            request = match provider_type {
                ProviderType::Anthropic => request
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01"),
                _ => request.bearer_auth(key),
            };
        }
        request = apply_custom_headers(request, &provider.headers)?;

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(format!(
                    "failed to fetch models from {}: {error}",
                    provider.name
                ));
                continue;
            }
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes)
                .chars()
                .take(400)
                .collect::<String>();
            last_error = Some(format!("{} returned {}: {}", provider.name, status, body));
            if matches!(status.as_u16(), 401 | 403 | 408 | 409 | 425 | 429)
                || status.is_server_error()
            {
                continue;
            }
            break;
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
        return Ok(entries);
    }

    Err(AppError::Upstream(last_error.unwrap_or_else(|| {
        format!("no credentials available for {}", provider.name)
    })))
}

async fn sync_provider(state: AppState, id: i64) -> AppResult<ModelSyncResult> {
    let result = sync_provider_inner(&state, id).await;
    let attempted_at = Utc::now().to_rfc3339();
    if let Err(error) = &result {
        let _ = sqlx::query(
            "UPDATE providers \
             SET models_sync_error = ?, models_sync_attempted_at = ?, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(error.to_string())
        .bind(&attempted_at)
        .bind(id)
        .execute(&state.pool)
        .await;
    } else {
        let _ = sqlx::query("UPDATE providers SET models_sync_attempted_at = ? WHERE id = ?")
            .bind(&attempted_at)
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
    let entries = fetch_provider_entries(state, &provider).await?;
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

pub async fn diagnose_route(
    State(state): State<AppState>,
    Json(input): Json<RouteDiagnoseInput>,
) -> AppResult<Json<RouteDiagnoseView>> {
    let model = input.model.trim();
    if model.is_empty() {
        return Err(AppError::BadRequest("model is required".to_string()));
    }
    let endpoint = input.endpoint.trim().trim_end_matches('/');
    if !endpoint.starts_with('/') {
        return Err(AppError::BadRequest(
            "endpoint must start with '/'".to_string(),
        ));
    }
    let session_id = input
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    Ok(Json(
        crate::proxy::diagnose_route(&state, model, endpoint, session_id).await?,
    ))
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
                display_name: model.display_name,
                supported_endpoints: model.supported_endpoints,
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
                supported_endpoints: model.supported_endpoints,
            }
            .with_flat_limits(),
        );
    }
    Ok(Json(by_id.into_values().collect()))
}

pub async fn list_api_keys(State(state): State<AppState>) -> AppResult<Json<Vec<ApiKeyView>>> {
    let now = Utc::now();
    let day_start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
        .to_rfc3339();
    let minute_start = now
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("current minute start is valid")
        .to_rfc3339();
    let keys = sqlx::query_as::<_, ApiKeyStatsRow>(
        r#"
        SELECT
            k.id, k.name, k.key_prefix, k.key_suffix, k.enabled,
            k.last_used_at, k.created_at,
            k.daily_token_limit, k.daily_cost_limit_micros,
            k.requests_per_minute, k.max_concurrency,
            k.allowed_models, k.expires_at,
            (
                SELECT COUNT(*) FROM usage_logs rate
                WHERE rate.api_key_id = k.id AND rate.created_at >= ?
            ) AS requests_this_minute,
            (
                SELECT COUNT(*) FROM usage_logs active
                WHERE active.api_key_id = k.id AND active.in_flight = 1
            ) AS current_in_flight,
            COUNT(CASE WHEN u.id IS NOT NULL AND u.created_at >= ? THEN 1 END) AS today_requests,
            COALESCE(SUM(
                CASE WHEN u.id IS NOT NULL AND u.created_at >= ?
                     THEN u.total_tokens ELSE 0 END
            ), 0) AS today_tokens,
            COALESCE(SUM(
                CASE WHEN u.id IS NOT NULL AND u.created_at >= ?
                     THEN u.prompt_tokens ELSE 0 END
            ), 0) AS today_prompt_tokens,
            COALESCE(SUM(
                CASE WHEN u.id IS NOT NULL AND u.created_at >= ?
                     THEN u.completion_tokens ELSE 0 END
            ), 0) AS today_completion_tokens,
            SUM(
                CASE WHEN u.id IS NOT NULL AND u.created_at >= ?
                     THEN u.estimated_cost_micros END
            ) AS today_cost_micros,
            COUNT(u.id) AS requests,
            COALESCE(SUM(u.total_tokens), 0) AS tokens,
            COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
            COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
            SUM(u.estimated_cost_micros) AS cost_micros,
            COALESCE(SUM(
                CASE WHEN u.id IS NOT NULL AND u.estimated_cost_micros IS NULL
                     THEN 1 ELSE 0 END
            ), 0) AS unpriced_requests
        FROM api_keys k
        LEFT JOIN usage_logs u ON u.api_key_id = k.id AND u.in_flight = 0
        GROUP BY k.id, k.name, k.key_prefix, k.key_suffix, k.enabled,
                 k.last_used_at, k.created_at, k.daily_token_limit,
                 k.daily_cost_limit_micros, k.requests_per_minute,
                 k.max_concurrency, k.allowed_models, k.expires_at
        ORDER BY k.enabled DESC, k.created_at DESC
        "#,
    )
    .bind(&minute_start)
    .bind(&day_start)
    .bind(&day_start)
    .bind(&day_start)
    .bind(&day_start)
    .bind(&day_start)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(keys.into_iter().map(Into::into).collect()))
}

fn generate_api_key_material() -> (String, String, String, String) {
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
    (raw, key_hash, key_prefix, key_suffix)
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Json(input): Json<ApiKeyInput>,
) -> AppResult<(StatusCode, Json<ApiKeyCreated>)> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("key name is required".to_string()));
    }
    let daily_token_limit = normalize_api_key_limit("daily token", input.daily_token_limit)?;
    let daily_cost_limit_micros =
        normalize_api_key_limit("daily cost", input.daily_cost_limit_micros)?;
    let requests_per_minute =
        normalize_api_key_limit("requests per minute", input.requests_per_minute)?;
    let max_concurrency = normalize_api_key_limit("max concurrency", input.max_concurrency)?;
    let allowed_models = normalize_allowed_models(input.allowed_models)?;
    let expires_at = normalize_expiration(input.expires_at)?;

    let (raw, key_hash, key_prefix, key_suffix) = generate_api_key_material();

    let result = sqlx::query(
        "INSERT INTO api_keys (
            name, key_hash, key_prefix, key_suffix, enabled,
            daily_token_limit, daily_cost_limit_micros, requests_per_minute,
            max_concurrency, allowed_models, expires_at
         ) VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(key_hash)
    .bind(key_prefix)
    .bind(key_suffix)
    .bind(daily_token_limit)
    .bind(daily_cost_limit_micros)
    .bind(requests_per_minute)
    .bind(max_concurrency)
    .bind(allowed_models)
    .bind(expires_at)
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

pub async fn rotate_api_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ApiKeyCreated>> {
    let (raw, key_hash, key_prefix, key_suffix) = generate_api_key_material();
    let result = sqlx::query(
        "UPDATE api_keys \
         SET key_hash = ?, key_prefix = ?, key_suffix = ?, last_used_at = NULL \
         WHERE id = ?",
    )
    .bind(key_hash)
    .bind(key_prefix)
    .bind(key_suffix)
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
    Ok(Json(ApiKeyCreated {
        key: raw,
        item: record.into(),
    }))
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
    let current = sqlx::query_as::<_, ApiKeyRecord>("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("API key not found".to_string()))?;
    let daily_token_limit = match input.daily_token_limit {
        Some(value) => normalize_api_key_limit("daily token", Some(value))?,
        None => current.daily_token_limit,
    };
    let daily_cost_limit_micros = match input.daily_cost_limit_micros {
        Some(value) => normalize_api_key_limit("daily cost", Some(value))?,
        None => current.daily_cost_limit_micros,
    };
    let requests_per_minute = match input.requests_per_minute {
        Some(value) => normalize_api_key_limit("requests per minute", Some(value))?,
        None => current.requests_per_minute,
    };
    let max_concurrency = match input.max_concurrency {
        Some(value) => normalize_api_key_limit("max concurrency", Some(value))?,
        None => current.max_concurrency,
    };
    let allowed_models = match input.allowed_models {
        Some(value) => normalize_allowed_models(Some(value))?,
        None => current.allowed_models,
    };
    let expires_at = match input.expires_at {
        Some(value) => normalize_expiration(Some(value))?,
        None => current.expires_at,
    };
    let result = sqlx::query(
        "UPDATE api_keys \
             SET enabled = ?, daily_token_limit = ?, daily_cost_limit_micros = ?, \
             requests_per_minute = ?, max_concurrency = ?, allowed_models = ?, \
             expires_at = ? \
         WHERE id = ?",
    )
    .bind(input.enabled as i64)
    .bind(daily_token_limit)
    .bind(daily_cost_limit_micros)
    .bind(requests_per_minute)
    .bind(max_concurrency)
    .bind(allowed_models)
    .bind(expires_at)
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

fn normalize_api_key_limit(name: &str, value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(format!(
            "{name} limit must be zero or a positive integer"
        ))),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

fn parse_allowed_models(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|model| !model.trim().is_empty())
        .collect()
}

fn normalize_allowed_models(value: Option<Vec<String>>) -> AppResult<Option<String>> {
    let Some(values) = value else {
        return Ok(None);
    };
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for value in values {
        let model = value.trim();
        if model.is_empty() {
            continue;
        }
        if model.chars().count() > 500 {
            return Err(AppError::BadRequest(
                "model permission entries must be at most 500 characters".to_string(),
            ));
        }
        if let Err(error) = globset::Glob::new(model) {
            return Err(AppError::BadRequest(format!(
                "invalid model permission pattern '{model}': {error}"
            )));
        }
        if seen.insert(model.to_string()) {
            models.push(model.to_string());
        }
    }
    if models.len() > 200 {
        return Err(AppError::BadRequest(
            "an API key can allow at most 200 model patterns".to_string(),
        ));
    }
    if models.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&models).unwrap_or_default()))
}

fn normalize_expiration(value: Option<String>) -> AppResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value).map_err(|error| {
        AppError::BadRequest(format!("expiration must be an RFC3339 timestamp: {error}"))
    })?;
    if parsed <= Utc::now() {
        return Err(AppError::BadRequest(
            "expiration must be in the future".to_string(),
        ));
    }
    Ok(Some(parsed.with_timezone(&Utc).to_rfc3339()))
}

fn normalize_retention_days(value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if !(0..=3650).contains(&value) => Err(AppError::BadRequest(
            "usage retention must be between 0 and 3650 days".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
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
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms, u.status_code,
               u.in_flight, u.success, u.streamed, u.error_message, u.created_at,
               u.first_token_ms, u.session_id,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name,
               COALESCE(u.provider_api_key_name, pk.name) AS provider_api_key_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
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

const USAGE_EXPORT_LIMIT: i64 = 100_000;

pub async fn export_usage(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> AppResult<Response> {
    let mut items = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms,
               u.status_code, u.in_flight, u.success, u.streamed, u.error_message,
               u.created_at, u.first_token_ms, u.session_id, NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name,
               COALESCE(u.provider_api_key_name, pk.name) AS provider_api_key_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
        WHERE 1 = 1
        "#,
    );
    apply_usage_filters(&mut items, &query);
    items
        .push(" ORDER BY u.created_at DESC, u.id DESC LIMIT ")
        .push_bind(USAGE_EXPORT_LIMIT);
    let rows = items
        .build_query_as::<UsageLogDetailRow>()
        .fetch_all(&state.pool)
        .await?;
    let truncated = rows.len() as i64 == USAGE_EXPORT_LIMIT;

    let mut csv = String::from(
        "\u{feff}created_at,request_id,session_id,api_key,provider,provider_api_key,route,requested_model,upstream_model,endpoint,prompt_tokens,completion_tokens,total_tokens,cache_read_tokens,cache_write_tokens,estimated_cost_usd,latency_ms,first_token_ms,output_tps,status_code,in_flight,success,streamed,error_message\r\n",
    );
    for row in rows {
        let item = UsageLogView::from(row);
        csv.push_str(&usage_csv_row(&item));
        csv.push_str("\r\n");
    }

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"openllm-usage.csv\"",
        );
    if truncated {
        response = response.header("x-openllm-export-truncated", "true");
    }
    Ok(response
        .body(Body::from(csv))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

fn usage_csv_row(item: &UsageLogView) -> String {
    let text = |value: Option<&str>| value.unwrap_or_default().to_string();
    let number = |value: Option<i64>| value.map(|value| value.to_string()).unwrap_or_default();
    let cost = item
        .estimated_cost_micros
        .map(|value| format!("{:.6}", value as f64 / 1_000_000.0))
        .unwrap_or_default();
    let tps = item
        .output_tps
        .map(|value| format!("{value:.2}"))
        .unwrap_or_default();
    [
        item.created_at.clone(),
        item.request_id.clone(),
        text(item.session_id.as_deref()),
        text(item.api_key_name.as_deref()),
        text(item.provider_name.as_deref()),
        text(item.provider_api_key_name.as_deref()),
        text(item.route_name.as_deref()),
        item.requested_model.clone(),
        text(item.upstream_model.as_deref()),
        item.endpoint.clone(),
        item.prompt_tokens.to_string(),
        item.completion_tokens.to_string(),
        item.total_tokens.to_string(),
        item.cache_read_tokens.to_string(),
        item.cache_write_tokens.to_string(),
        cost,
        item.latency_ms.to_string(),
        number(item.first_token_ms),
        tps,
        item.status_code.to_string(),
        if item.in_flight { "true" } else { "false" }.to_string(),
        if item.success { "true" } else { "false" }.to_string(),
        if item.streamed { "true" } else { "false" }.to_string(),
        text(item.error_message.as_deref()),
    ]
    .into_iter()
    .map(|value| csv_field(&value))
    .collect::<Vec<_>>()
    .join(",")
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\r') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
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

pub async fn backup_database(State(state): State<AppState>) -> AppResult<Response> {
    let filename = format!("openllm-backup-{}.db", Utc::now().format("%Y%m%d-%H%M%S"));
    let path = std::env::temp_dir().join(format!(
        "openllm-backup-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let path_string = path.to_string_lossy().to_string();
    if let Err(error) = sqlx::query("VACUUM INTO ?")
        .bind(&path_string)
        .execute(&state.pool)
        .await
    {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(AppError::Database(error));
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AppError::Internal(error.into()));
        }
    };
    let _ = tokio::fs::remove_file(&path).await;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.sqlite3")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
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

fn parse_overview_time(
    value: Option<&str>,
    fallback: DateTime<Utc>,
    name: &str,
) -> AppResult<DateTime<Utc>> {
    match value {
        Some(value) => DateTime::parse_from_rfc3339(value)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| {
                AppError::BadRequest(format!("{name} must be an RFC3339 timestamp: {error}"))
            }),
        None => Ok(fallback),
    }
}

fn normalize_overview_range(
    from: Option<&str>,
    to: Option<&str>,
    default_start: DateTime<Utc>,
    default_end: DateTime<Utc>,
) -> AppResult<(DateTime<Utc>, DateTime<Utc>)> {
    let start = parse_overview_time(from, default_start, "from")?;
    let end = parse_overview_time(to, default_end, "to")?;
    if start >= end {
        return Err(AppError::BadRequest(
            "overview range end must be after its start".to_string(),
        ));
    }
    if end - start > Duration::days(MAX_OVERVIEW_RANGE_DAYS) {
        return Err(AppError::BadRequest(format!(
            "overview range cannot exceed {MAX_OVERVIEW_RANGE_DAYS} days"
        )));
    }
    Ok((start, end))
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
    let (range_start, range_end) = normalize_overview_range(
        query.from.as_deref(),
        query.to.as_deref(),
        day_start - Duration::days(13),
        day_start + Duration::days(1),
    )?;

    let totals = sqlx::query(
        r#"
        SELECT
            requests AS requests_total,
            tokens AS tokens_total,
            prompt_tokens AS prompt_tokens_total,
            completion_tokens AS completion_tokens_total,
            prompt_tokens AS prompt_total,
            cache_read_tokens AS cache_read_total,
            cache_write_tokens AS cache_write_total,
            cost_micros AS cost_total_micros,
            unpriced_requests AS unpriced_total,
            CASE
                WHEN requests = 0 THEN 0.0
                ELSE successful_requests * 100.0 / requests
            END AS success_rate,
            CASE
                WHEN requests = 0 THEN 0.0
                ELSE latency_ms_sum * 1.0 / requests
            END AS avg_latency_ms
        FROM usage_lifetime_stats
        WHERE id = 1
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    let range_totals = sqlx::query(
        r#"
        SELECT
            COUNT(*) AS requests,
            COALESCE(SUM(total_tokens), 0) AS tokens,
            COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
            COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
            COALESCE(SUM(prompt_tokens), 0) AS prompt,
            COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
            COALESCE(SUM(cache_write_tokens), 0) AS cache_write,
            COALESCE(SUM(estimated_cost_micros), 0) AS cost_micros,
            COALESCE(SUM(estimated_cost_micros IS NULL), 0) AS unpriced,
            COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
            COALESCE(AVG(latency_ms), 0.0) AS avg_latency_ms
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_one(&state.pool)
    .await?;

    let session_totals = sqlx::query(
        r#"
        SELECT
            COUNT(*) AS sessions,
            COALESCE(SUM(requests), 0) AS session_requests,
            COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
            COALESCE(SUM(cache_read), 0) AS cache_read
        FROM (
            SELECT session_id,
                   COUNT(*) AS requests,
                   COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
                   COALESCE(SUM(cache_read_tokens), 0) AS cache_read
            FROM usage_logs
            WHERE created_at >= ? AND created_at < ? AND in_flight = 0
              AND session_id IS NOT NULL AND TRIM(session_id) <> ''
            GROUP BY session_id
        )
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_one(&state.pool)
    .await?;

    let today = sqlx::query(
        r#"
        SELECT COUNT(*) AS requests, COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt,
               COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
               COALESCE(SUM(cache_write_tokens), 0) AS cache_write,
               COALESCE(SUM(estimated_cost_micros), 0) AS cost_micros,
               COALESCE(SUM(estimated_cost_micros IS NULL), 0) AS unpriced
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
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
    let (healthy_providers, failed_providers, untested_providers) =
        sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT \
                COALESCE(SUM(CASE WHEN enabled = 1 AND last_test_ok = 1 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN enabled = 1 AND last_test_ok = 0 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN enabled = 1 AND last_test_ok IS NULL THEN 1 ELSE 0 END), 0) \
             FROM providers",
        )
        .fetch_one(&state.pool)
        .await?;
    let (
        provider_keys_total,
        healthy_provider_keys,
        failed_provider_keys,
        untested_provider_keys,
        runtime_error_provider_keys,
    ) = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
        "SELECT \
            COUNT(*), \
            COALESCE(SUM(CASE WHEN k.last_test_ok = 1 THEN 1 ELSE 0 END), 0), \
            COALESCE(SUM(CASE WHEN k.last_test_ok = 0 THEN 1 ELSE 0 END), 0), \
            COALESCE(SUM(CASE WHEN k.last_test_ok IS NULL THEN 1 ELSE 0 END), 0), \
            COALESCE(SUM(CASE WHEN k.last_error IS NOT NULL THEN 1 ELSE 0 END), 0) \
         FROM provider_api_keys k \
         JOIN providers p ON p.id = k.provider_id \
         WHERE p.enabled = 1 AND k.enabled = 1",
    )
    .fetch_one(&state.pool)
    .await?;
    let in_flight_requests: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
            .fetch_one(&state.pool)
            .await?;
    let now = std::time::Instant::now();
    let cooling_provider_keys = state
        .provider_key_cooldown
        .lock()
        .await
        .values()
        .filter(|until| **until > now)
        .count() as i64;

    let recent = sqlx::query_as::<_, UsageLogDetailRow>(
        r#"
        SELECT u.id, u.request_id, u.api_key_id, u.route_id, u.provider_id,
               u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint, u.prompt_tokens,
               u.completion_tokens, u.total_tokens, u.cache_read_tokens,
               u.cache_write_tokens, u.estimated_cost_micros, u.latency_ms, u.status_code,
               u.in_flight, u.success, u.streamed, u.error_message, u.created_at,
               u.first_token_ms, u.session_id,
               NULL AS response_preview,
               k.name AS api_key_name, r.name AS route_name, p.name AS provider_name,
               COALESCE(u.provider_api_key_name, pk.name) AS provider_api_key_name
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        LEFT JOIN routes r ON r.id = u.route_id
        LEFT JOIN providers p ON p.id = u.provider_id
        LEFT JOIN provider_api_keys pk ON pk.id = u.provider_api_key_id
        WHERE u.created_at >= ? AND u.created_at < ?
        ORDER BY u.created_at DESC, u.id DESC
        LIMIT 12
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_all(&state.pool)
    .await?;

    let provider_usage = sqlx::query_as::<_, ProviderUsage>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               COUNT(u.id) AS requests,
               COALESCE(SUM(u.total_tokens), 0) AS tokens,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(u.completion_tokens), 0) AS completion_tokens,
               SUM(u.estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN u.success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(u.latency_ms), 0.0) AS avg_latency_ms
        FROM providers p
        LEFT JOIN usage_logs u
          ON u.provider_id = p.id AND u.created_at >= ? AND u.created_at < ?
             AND u.in_flight = 0
        GROUP BY p.id, p.name
        ORDER BY requests DESC, tokens DESC
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_all(&state.pool)
    .await?;

    let daily_rows = sqlx::query_as::<_, DailyUsage>(
        r#"
        SELECT date(datetime(created_at), ? || ' minutes') AS day,
               COUNT(*) AS requests,
               COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(completion_tokens), 0) AS completion_tokens
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        GROUP BY date(datetime(created_at), ? || ' minutes')
        ORDER BY day
        "#,
    )
    .bind(tz_offset)
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .bind(tz_offset)
    .fetch_all(&state.pool)
    .await?;

    // Fill in days with no traffic so the chart has a continuous axis
    // instead of collapsing to only the days that happened to have requests.
    let local_range_start = range_start + Duration::minutes(tz_offset);
    let local_range_end = range_end + Duration::minutes(tz_offset);
    let first_day = local_range_start.date_naive();
    let last_day = (local_range_end - Duration::nanoseconds(1)).date_naive();
    let day_count = (last_day - first_day).num_days() + 1;
    let mut daily_usage = Vec::with_capacity(day_count.max(0) as usize);
    for offset in 0..day_count {
        let day = (first_day + Duration::days(offset))
            .format("%Y-%m-%d")
            .to_string();
        let existing = daily_rows.iter().find(|row| row.day == day);
        daily_usage.push(DailyUsage {
            day,
            requests: existing.map_or(0, |row| row.requests),
            tokens: existing.map_or(0, |row| row.tokens),
            prompt_tokens: existing.map_or(0, |row| row.prompt_tokens),
            completion_tokens: existing.map_or(0, |row| row.completion_tokens),
        });
    }

    let model_usage = sqlx::query_as::<_, ModelUsage>(
        r#"
        SELECT requested_model AS model,
               COUNT(*) AS requests,
               COALESCE(SUM(total_tokens), 0) AS tokens,
               COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
               COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
               SUM(estimated_cost_micros) AS cost_micros,
               COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0) AS success_rate,
               COALESCE(AVG(latency_ms), 0.0) AS avg_latency_ms
        FROM usage_logs
        WHERE created_at >= ? AND created_at < ? AND in_flight = 0
        GROUP BY requested_model
        ORDER BY tokens DESC, requests DESC
        LIMIT 8
        "#,
    )
    .bind(range_start.to_rfc3339())
    .bind(range_end.to_rfc3339())
    .fetch_all(&state.pool)
    .await?;

    let range_requests: i64 = range_totals.get("requests");
    let range_sessions: i64 = session_totals.get("sessions");
    let range_session_requests: i64 = session_totals.get("session_requests");
    let range_session_coverage = if range_requests > 0 {
        (range_session_requests as f64 / range_requests as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let range_avg_requests_per_session = if range_sessions > 0 {
        range_session_requests as f64 / range_sessions as f64
    } else {
        0.0
    };

    Ok(Json(Overview {
        requests_today: today.get("requests"),
        tokens_today: today.get("tokens"),
        prompt_tokens_today: today.get("prompt_tokens"),
        completion_tokens_today: today.get("completion_tokens"),
        cache_read_today: today.get("cache_read"),
        cache_write_today: today.get("cache_write"),
        cache_hit_rate: cache_hit_rate(today.get("prompt"), today.get("cache_read")),
        requests_total: totals.get("requests_total"),
        tokens_total: totals.get("tokens_total"),
        prompt_tokens_total: totals.get("prompt_tokens_total"),
        completion_tokens_total: totals.get("completion_tokens_total"),
        cache_read_total: totals.get("cache_read_total"),
        cache_write_total: totals.get("cache_write_total"),
        cost_today_micros: today.get("cost_micros"),
        cost_total_micros: totals.get("cost_total_micros"),
        unpriced_today: today.get("unpriced"),
        unpriced_total: totals.get("unpriced_total"),
        range_requests: range_totals.get("requests"),
        range_tokens: range_totals.get("tokens"),
        range_prompt_tokens: range_totals.get("prompt_tokens"),
        range_completion_tokens: range_totals.get("completion_tokens"),
        range_cache_read: range_totals.get("cache_read"),
        range_cache_write: range_totals.get("cache_write"),
        range_cache_hit_rate: cache_hit_rate(
            range_totals.get("prompt"),
            range_totals.get("cache_read"),
        ),
        range_sessions,
        range_session_coverage,
        range_avg_requests_per_session,
        range_session_cache_hit_rate: cache_hit_rate(
            session_totals.get("prompt_tokens"),
            session_totals.get("cache_read"),
        ),
        range_cost_micros: range_totals.get("cost_micros"),
        range_unpriced: range_totals.get("unpriced"),
        range_success_rate: range_totals.get("success_rate"),
        range_avg_latency_ms: range_totals.get("avg_latency_ms"),
        success_rate: totals.get("success_rate"),
        avg_latency_ms: totals.get("avg_latency_ms"),
        active_providers,
        active_routes,
        healthy_providers,
        failed_providers,
        untested_providers,
        provider_keys_total,
        healthy_provider_keys,
        failed_provider_keys,
        untested_provider_keys,
        runtime_error_provider_keys,
        cooling_provider_keys,
        in_flight_requests,
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
    hydrate_provider_view(state, &mut view).await?;
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
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.tool_search_supported,
               p.last_test_ok AS provider_health,
               p.enabled AS provider_enabled
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        LEFT JOIN provider_models pm
          ON pm.provider_id = rt.provider_id
         AND pm.model_name = rt.upstream_model
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
                supported_endpoints: target
                    .supported_endpoints
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                    .unwrap_or_default(),
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
    let existing_overrides = sqlx::query_as::<_, ProviderModelOverride>(
        "SELECT model_name, enabled, context_override, input_override, output_override, \
                supported_endpoints_override, cost_input_override, cost_output_override, \
                cost_cache_read_override, cost_cache_write_override \
         FROM provider_models WHERE provider_id = ?",
    )
    .bind(provider_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| (row.model_name.clone(), row))
    .collect::<HashMap<_, _>>();
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
        let overrides = existing_overrides.get(model).cloned().unwrap_or_default();
        sqlx::query(
            r#"
            INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, output_limit,
                input_limit, attachment, reasoning, tool_call, structured_output,
                temperature, open_weights, modalities, cost, family, knowledge,
                release_date, last_updated, canonical_model_id, capabilities_synced_at,
                upstream_context_limit, supported_endpoints, display_name,
                context_override, input_override, output_override,
                supported_endpoints_override, cost_input_override, cost_output_override,
                cost_cache_read_override, cost_cache_write_override
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(provider_id)
        .bind(model)
        .bind(overrides.enabled.unwrap_or(1))
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
        .bind(overrides.context_override)
        .bind(overrides.input_override)
        .bind(overrides.output_override)
        .bind(overrides.supported_endpoints_override)
        .bind(overrides.cost_input_override)
        .bind(overrides.cost_output_override)
        .bind(overrides.cost_cache_read_override)
        .bind(overrides.cost_cache_write_override)
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

#[derive(Debug, Clone, Default, sqlx::FromRow)]
struct ProviderModelOverride {
    model_name: String,
    enabled: Option<i64>,
    context_override: Option<i64>,
    input_override: Option<i64>,
    output_override: Option<i64>,
    supported_endpoints_override: Option<String>,
    cost_input_override: Option<f64>,
    cost_output_override: Option<f64>,
    cost_cache_read_override: Option<f64>,
    cost_cache_write_override: Option<f64>,
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

impl From<ProviderApiKeyRecord> for ProviderApiKeyView {
    fn from(value: ProviderApiKeyRecord) -> Self {
        let api_key_suffix = api_key_suffix(&value.secret);
        Self {
            id: value.id,
            name: value.name,
            api_key_set: !value.secret.is_empty(),
            api_key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            last_error_at: value.last_error_at,
            last_error: value.last_error,
            last_test_at: value.last_test_at,
            last_test_ok: value.last_test_ok.map(|value| value != 0),
            last_test_latency_ms: value.last_test_latency_ms,
            last_test_checked: value.last_test_checked,
            last_test_message: value.last_test_message,
            requests: 0,
            success_rate: 0.0,
            avg_latency_ms: 0.0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cooldown_seconds: None,
            created_at: value.created_at,
        }
    }
}

fn api_key_suffix(secret: &str) -> String {
    let suffix = secret.chars().rev().take(4).collect::<String>();
    suffix.chars().rev().collect()
}

async fn provider_api_key_records(
    pool: &sqlx::SqlitePool,
    provider_id: i64,
) -> AppResult<Vec<ProviderApiKeyRecord>> {
    Ok(sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

async fn clear_provider_key_cooldowns(
    state: &AppState,
    provider_api_key_ids: impl IntoIterator<Item = i64>,
) {
    let mut cooldowns = state.provider_key_cooldown.lock().await;
    for provider_api_key_id in provider_api_key_ids {
        cooldowns.remove(&provider_api_key_id);
    }
}

async fn provider_key_candidates(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<(Option<i64>, Option<String>)>> {
    let mut candidates = provider_api_key_records(&state.pool, provider.id)
        .await?
        .into_iter()
        .filter(|record| record.enabled != 0)
        .map(|record| (Some(record.id), Some(record.secret)))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates.push((None, provider.api_key.clone()));
    }
    Ok(candidates)
}

async fn provider_probe_keys(
    state: &AppState,
    provider: &Provider,
) -> AppResult<Vec<ProviderProbeKey>> {
    let mut keys = provider_api_key_records(&state.pool, provider.id)
        .await?
        .into_iter()
        .filter(|record| record.enabled != 0)
        .map(|record| ProviderProbeKey {
            id: Some(record.id),
            name: record.name,
            api_key_suffix: api_key_suffix(&record.secret),
            secret: Some(record.secret),
        })
        .collect::<Vec<_>>();
    if keys.is_empty()
        && let Some(secret) = normalize_optional(provider.api_key.clone())
    {
        keys.push(ProviderProbeKey {
            id: None,
            name: "Default".to_string(),
            api_key_suffix: api_key_suffix(&secret),
            secret: Some(secret),
        });
    }
    Ok(keys)
}

async fn hydrate_provider_view(state: &AppState, view: &mut ProviderView) -> AppResult<()> {
    view.api_keys = provider_api_key_records(&state.pool, view.id)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    let stats = sqlx::query_as::<_, (i64, i64, f64, f64, i64, i64)>(
        "SELECT provider_api_key_id, COUNT(*), \
                COALESCE(AVG(CASE WHEN success = 1 THEN 1.0 ELSE 0.0 END) * 100.0, 0.0), \
                COALESCE(AVG(latency_ms), 0.0), \
                COALESCE(SUM(prompt_tokens), 0), \
                COALESCE(SUM(completion_tokens), 0) \
         FROM usage_logs \
         WHERE provider_id = ? AND provider_api_key_id IS NOT NULL AND in_flight = 0 \
         GROUP BY provider_api_key_id",
    )
    .bind(view.id)
    .fetch_all(&state.pool)
    .await?;
    let stats = stats
        .into_iter()
        .map(
            |(id, requests, success_rate, avg_latency_ms, prompt_tokens, completion_tokens)| {
                (
                    id,
                    (
                        requests,
                        success_rate,
                        avg_latency_ms,
                        prompt_tokens,
                        completion_tokens,
                    ),
                )
            },
        )
        .collect::<HashMap<_, _>>();
    for key in &mut view.api_keys {
        if let Some((requests, success_rate, avg_latency_ms, prompt_tokens, completion_tokens)) =
            stats.get(&key.id)
        {
            key.requests = *requests;
            key.success_rate = *success_rate;
            key.avg_latency_ms = *avg_latency_ms;
            key.prompt_tokens = *prompt_tokens;
            key.completion_tokens = *completion_tokens;
        }
    }
    let now = std::time::Instant::now();
    let cooldowns = state.provider_key_cooldown.lock().await;
    for key in &mut view.api_keys {
        if let Some(until) = cooldowns.get(&key.id) {
            let remaining = until.saturating_duration_since(now);
            if !remaining.is_zero() {
                key.cooldown_seconds = Some((remaining.as_secs_f64().ceil() as i64).max(1));
            }
        }
    }
    view.api_key_set = !view.api_keys.is_empty();
    Ok(())
}

fn provider_api_key_input(
    id: Option<i64>,
    name: impl Into<String>,
    api_key: Option<String>,
    enabled: bool,
) -> ProviderApiKeyInput {
    ProviderApiKeyInput {
        id,
        name: name.into(),
        api_key,
        enabled,
    }
}

async fn replace_provider_api_keys(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    inputs: &[ProviderApiKeyInput],
) -> AppResult<()> {
    let existing = sqlx::query_as::<_, ProviderApiKeyRecord>(
        "SELECT * FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|record| (record.id, record))
    .collect::<HashMap<_, _>>();
    let mut retained = HashSet::new();
    let mut secrets = HashSet::new();

    for (index, input) in inputs.iter().enumerate() {
        let current = match input.id {
            Some(id) => Some(
                existing
                    .get(&id)
                    .ok_or_else(|| {
                        AppError::BadRequest("API key does not belong to this provider".to_string())
                    })?
                    .clone(),
            ),
            None => None,
        };
        let requested_secret = normalize_optional(input.api_key.clone());
        let secret = match (requested_secret, current.as_ref()) {
            (Some(secret), _) => secret,
            (None, Some(current)) => current.secret.clone(),
            (None, None) => {
                return Err(AppError::BadRequest(
                    "new provider API keys must include a secret".to_string(),
                ));
            }
        };
        if !secrets.insert(secret.clone()) {
            return Err(AppError::BadRequest(
                "provider API keys must be unique".to_string(),
            ));
        }
        let name = match input.name.trim() {
            "" => current
                .as_ref()
                .map(|current| current.name.clone())
                .unwrap_or_else(|| format!("Key {}", index + 1)),
            name => name.to_string(),
        };
        if name.chars().count() > 80 {
            return Err(AppError::BadRequest(
                "provider API key name must be at most 80 characters".to_string(),
            ));
        }

        if let Some(current) = current {
            retained.insert(current.id);
            if current.secret != secret {
                sqlx::query(
                    "UPDATE provider_api_keys \
                     SET name = ?, secret = ?, enabled = ?, \
                         last_used_at = NULL, last_error_at = NULL, last_error = NULL, \
                         last_test_at = NULL, last_test_ok = NULL, \
                         last_test_latency_ms = NULL, last_test_checked = NULL, \
                         last_test_message = NULL, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE id = ? AND provider_id = ?",
                )
                .bind(name)
                .bind(secret)
                .bind(input.enabled as i64)
                .bind(current.id)
                .bind(provider_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlite_conflict)?;
            } else {
                sqlx::query(
                    "UPDATE provider_api_keys \
                     SET name = ?, secret = ?, enabled = ?, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE id = ? AND provider_id = ?",
                )
                .bind(name)
                .bind(secret)
                .bind(input.enabled as i64)
                .bind(current.id)
                .bind(provider_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlite_conflict)?;
            }
        } else {
            let result = sqlx::query(
                "INSERT INTO provider_api_keys (provider_id, name, secret, enabled) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(provider_id)
            .bind(name)
            .bind(secret)
            .bind(input.enabled as i64)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlite_conflict)?;
            retained.insert(result.last_insert_rowid());
        }
    }

    for id in existing.keys().filter(|id| !retained.contains(id)) {
        sqlx::query("DELETE FROM provider_api_keys WHERE id = ? AND provider_id = ?")
            .bind(id)
            .bind(provider_id)
            .execute(&mut **tx)
            .await?;
    }

    let first_secret = sqlx::query_scalar::<_, String>(
        "SELECT secret FROM provider_api_keys \
         WHERE provider_id = ? AND enabled = 1 ORDER BY id LIMIT 1",
    )
    .bind(provider_id)
    .fetch_optional(&mut **tx)
    .await?;
    sqlx::query("UPDATE providers SET api_key = ? WHERE id = ?")
        .bind(first_secret)
        .bind(provider_id)
        .execute(&mut **tx)
        .await?;
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

fn normalize_health_check_model(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_provider_probe_endpoints(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
}

fn provider_probe_supports(endpoints: &[String], expected: &str) -> bool {
    let expected = expected.trim_end_matches('/');
    endpoints.iter().any(|endpoint| {
        let endpoint = endpoint.trim_end_matches('/');
        !endpoint.is_empty() && (expected == endpoint || expected.ends_with(endpoint))
    })
}

fn normalize_health_interval(value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "health check interval must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
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
    if let Some(provider_api_key_id) = query.provider_api_key_id {
        builder
            .push(" AND u.provider_api_key_id = ")
            .push_bind(provider_api_key_id);
    }
    if let Some(api_key_id) = query.api_key_id {
        builder.push(" AND u.api_key_id = ").push_bind(api_key_id);
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
    if let Some(session_id) = &query.session_id
        && !session_id.trim().is_empty()
    {
        builder
            .push(" AND u.session_id LIKE ")
            .push_bind(format!("%{}%", session_id.trim()));
    }
    if let Some(endpoint) = &query.endpoint
        && !endpoint.trim().is_empty()
    {
        builder
            .push(" AND u.endpoint LIKE ")
            .push_bind(format!("%{}%", endpoint.trim()));
    }
    if let Some(success) = query.success {
        builder.push(" AND u.success = ").push_bind(success as i64);
        if !success {
            builder.push(" AND u.in_flight = 0");
        }
    }
    if let Some(in_flight) = query.in_flight {
        builder
            .push(" AND u.in_flight = ")
            .push_bind(in_flight as i64);
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
            requests: 0,
            tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_micros: None,
            unpriced_requests: 0,
            daily_token_limit: value.daily_token_limit,
            daily_cost_limit_micros: value.daily_cost_limit_micros,
            requests_per_minute: value.requests_per_minute,
            max_concurrency: value.max_concurrency,
            today_requests: 0,
            today_tokens: 0,
            today_prompt_tokens: 0,
            today_completion_tokens: 0,
            today_cost_micros: None,
            requests_this_minute: 0,
            current_in_flight: 0,
            allowed_models: parse_allowed_models(value.allowed_models.as_deref()),
            expires_at: value.expires_at,
        }
    }
}

impl From<ApiKeyStatsRow> for ApiKeyView {
    fn from(value: ApiKeyStatsRow) -> Self {
        Self {
            id: value.id,
            name: value.name,
            key_prefix: value.key_prefix,
            key_suffix: value.key_suffix,
            enabled: value.enabled != 0,
            last_used_at: value.last_used_at,
            created_at: value.created_at,
            requests: value.requests,
            tokens: value.tokens,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            cost_micros: value.cost_micros,
            unpriced_requests: value.unpriced_requests,
            daily_token_limit: value.daily_token_limit,
            daily_cost_limit_micros: value.daily_cost_limit_micros,
            requests_per_minute: value.requests_per_minute,
            max_concurrency: value.max_concurrency,
            today_requests: value.today_requests,
            today_tokens: value.today_tokens,
            today_prompt_tokens: value.today_prompt_tokens,
            today_completion_tokens: value.today_completion_tokens,
            today_cost_micros: value.today_cost_micros,
            requests_this_minute: value.requests_this_minute,
            current_in_flight: value.current_in_flight,
            allowed_models: parse_allowed_models(value.allowed_models.as_deref()),
            expires_at: value.expires_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn provider_key_test_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        AppState::new(pool, None)
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
            health_check_interval_minutes: None,
            health_check_model: None,
            models_sync_interval_minutes: None,
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
            health_check_interval_minutes: None,
            health_check_model: None,
            models_sync_interval_minutes: None,
        }
    }

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
    fn recognizes_provider_quota_endpoints() {
        assert_eq!(
            provider_quota_kind("https://api.commandcode.ai/provider/v1"),
            Some("command_code")
        );
        assert_eq!(
            provider_quota_kind("https://opencode.ai/zen/go/v1"),
            Some("opencode_go")
        );
        assert_eq!(
            provider_quota_kind("https://api.deepseek.com/v1"),
            Some("deepseek")
        );
        assert_eq!(provider_quota_kind("https://api.openai.com/v1"), None);
    }

    #[test]
    fn maps_command_code_plans() {
        assert_eq!(command_code_plan_name("individual-goat"), "GOAT");
        assert_eq!(command_code_monthly_total("individual-goat"), Some(70.0));
        assert_eq!(command_code_plan_name("individual-pro"), "Pro");
        assert_eq!(command_code_monthly_total("individual-pro"), Some(80.0));
        assert_eq!(command_code_plan_name("unknown"), "unknown");
        assert_eq!(
            quota_reset_at(Some(&json!(1790774552836_i64))),
            epoch_millis_to_rfc3339(1790774552836)
        );
        assert_eq!(
            quota_reset_at(Some(&json!("2026-10-01T00:00:00Z"))).as_deref(),
            Some("2026-10-01T00:00:00Z")
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
    fn validates_manual_model_limits() {
        assert!(validate_limit("context", None).is_ok());
        assert!(validate_limit("context", Some(400_000)).is_ok());
        assert!(validate_limit("context", Some(0)).is_err());
        assert!(validate_limit("input", Some(-1)).is_err());

        assert_eq!(serialize_endpoint_override(None).unwrap(), None);
        assert_eq!(
            serialize_endpoint_override(Some(&[
                "/v1/responses/".to_string(),
                "/v1/responses".to_string()
            ]))
            .unwrap()
            .as_deref(),
            Some(r#"["/v1/responses"]"#)
        );
        assert!(serialize_endpoint_override(Some(&["responses".to_string()])).is_err());

        assert!(validate_cost_override("input cost", None).is_ok());
        assert!(validate_cost_override("input cost", Some(0.0)).is_ok());
        assert!(validate_cost_override("input cost", Some(1.25)).is_ok());
        assert!(validate_cost_override("input cost", Some(-0.1)).is_err());
    }

    #[test]
    fn validates_api_key_daily_limits() {
        assert_eq!(normalize_api_key_limit("token", None).unwrap(), None);
        assert_eq!(normalize_api_key_limit("token", Some(0)).unwrap(), None);
        assert_eq!(
            normalize_api_key_limit("token", Some(10_000)).unwrap(),
            Some(10_000)
        );
        assert!(normalize_api_key_limit("token", Some(-1)).is_err());
    }

    #[tokio::test]
    async fn persists_and_reports_api_key_rate_limits() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let state = AppState::new(pool.clone(), None);

        let (_, Json(created)) = create_api_key(
            State(state.clone()),
            Json(ApiKeyInput {
                name: "limited".to_string(),
                daily_token_limit: None,
                daily_cost_limit_micros: None,
                requests_per_minute: Some(120),
                max_concurrency: Some(5),
                allowed_models: None,
                expires_at: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(created.item.requests_per_minute, Some(120));
        assert_eq!(created.item.max_concurrency, Some(5));

        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                status_code, in_flight, success, created_at
             ) VALUES (
                'active', ?, 'model', '/v1/chat/completions',
                0, 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
        )
        .bind(created.item.id)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                prompt_tokens, completion_tokens, total_tokens,
                status_code, in_flight, success, created_at
             ) VALUES (
                'completed', ?, 'model', '/v1/chat/completions',
                100, 25, 125, 200, 0, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
        )
        .bind(created.item.id)
        .execute(&pool)
        .await
        .unwrap();

        let Json(items) = list_api_keys(State(state.clone())).await.unwrap();
        assert_eq!(items[0].requests_per_minute, Some(120));
        assert_eq!(items[0].max_concurrency, Some(5));
        assert_eq!(items[0].requests_this_minute, 2);
        assert_eq!(items[0].current_in_flight, 1);
        assert_eq!(items[0].prompt_tokens, 100);
        assert_eq!(items[0].completion_tokens, 25);
        assert_eq!(items[0].today_prompt_tokens, 100);
        assert_eq!(items[0].today_completion_tokens, 25);

        let Json(updated) = update_api_key(
            State(state),
            Path(created.item.id),
            Json(ApiKeyUpdate {
                enabled: true,
                daily_token_limit: None,
                daily_cost_limit_micros: None,
                requests_per_minute: Some(0),
                max_concurrency: Some(2),
                allowed_models: None,
                expires_at: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(updated.requests_per_minute, None);
        assert_eq!(updated.max_concurrency, Some(2));
    }

    #[test]
    fn validates_provider_health_interval() {
        assert_eq!(normalize_health_interval(None).unwrap(), None);
        assert_eq!(normalize_health_interval(Some(0)).unwrap(), None);
        assert_eq!(normalize_health_interval(Some(30)).unwrap(), Some(30));
        assert!(normalize_health_interval(Some(-1)).is_err());
    }

    #[tokio::test]
    async fn provider_api_key_pool_supports_create_update_and_clear() {
        let state = provider_key_test_state().await;
        let (_, Json(created)) = create_provider(
            State(state.clone()),
            Json(provider_input(
                None,
                vec![
                    provider_key_input(None, "Primary", Some("sk-one"), true),
                    provider_key_input(None, "Backup", Some("sk-two"), true),
                ],
            )),
        )
        .await
        .unwrap();
        assert_eq!(created.api_keys.len(), 2);
        assert_eq!(created.api_keys[0].name, "Primary");
        assert_eq!(created.api_keys[0].api_key_suffix, "-one");
        assert_eq!(created.api_keys[1].api_key_suffix, "-two");

        let first_id = created.api_keys[0].id;
        state.provider_key_cooldown.lock().await.insert(
            first_id,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let Json(updated) = update_provider(
            State(state.clone()),
            Path(created.id),
            Json(provider_update_with_keys(Some(vec![
                provider_key_input(Some(first_id), "Primary", None, false),
                provider_key_input(None, "Replacement", Some("sk-three"), true),
            ]))),
        )
        .await
        .unwrap();
        assert_eq!(updated.api_keys.len(), 2);
        assert!(!updated.api_keys[0].enabled);
        assert_eq!(updated.api_keys[1].name, "Replacement");
        assert!(
            !state
                .provider_key_cooldown
                .lock()
                .await
                .contains_key(&first_id)
        );

        let retained_secret: String =
            sqlx::query_scalar("SELECT secret FROM provider_api_keys WHERE id = ?")
                .bind(first_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(retained_secret, "sk-one");
        let mirrored: Option<String> =
            sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
                .bind(created.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mirrored.as_deref(), Some("sk-three"));

        let Json(cleared) = update_provider(
            State(state.clone()),
            Path(created.id),
            Json(provider_update_with_keys(Some(Vec::new()))),
        )
        .await
        .unwrap();
        assert!(cleared.api_keys.is_empty());
        assert!(!cleared.api_key_set);
        let mirrored: Option<String> =
            sqlx::query_scalar("SELECT api_key FROM providers WHERE id = ?")
                .bind(created.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mirrored, None);
    }

    #[tokio::test]
    async fn provider_quota_credential_uses_selected_or_first_enabled_key() {
        let state = provider_key_test_state().await;
        let (_, Json(created)) = create_provider(
            State(state.clone()),
            Json(provider_input(
                None,
                vec![
                    provider_key_input(None, "Primary", Some("sk-one"), true),
                    provider_key_input(None, "Backup", Some("sk-two"), true),
                ],
            )),
        )
        .await
        .unwrap();
        let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
            .bind(created.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let selected = provider_quota_credential(&state, &provider, Some(created.api_keys[1].id))
            .await
            .unwrap();
        assert_eq!(selected.key_id, Some(created.api_keys[1].id));
        assert_eq!(selected.key_name, "Backup");
        assert_eq!(selected.secret, "sk-two");

        let default = provider_quota_credential(&state, &provider, None)
            .await
            .unwrap();
        assert_eq!(default.key_id, Some(created.api_keys[0].id));
        assert_eq!(default.key_name, "Primary");

        sqlx::query("UPDATE provider_api_keys SET enabled = 0 WHERE id = ?")
            .bind(created.api_keys[0].id)
            .execute(&state.pool)
            .await
            .unwrap();
        let fallback = provider_quota_credential(&state, &provider, None)
            .await
            .unwrap();
        assert_eq!(fallback.key_id, Some(created.api_keys[1].id));
        assert_eq!(fallback.key_name, "Backup");

        assert!(
            provider_quota_credential(&state, &provider, Some(999_999))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn provider_api_key_rotation_resets_stale_health_state() {
        let state = provider_key_test_state().await;
        let (_, Json(created)) = create_provider(
            State(state.clone()),
            Json(provider_input(
                None,
                vec![provider_key_input(None, "Primary", Some("sk-old"), true)],
            )),
        )
        .await
        .unwrap();
        let key_id = created.api_keys[0].id;
        sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = '2026-01-01T00:00:00Z', \
                 last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale 401', \
                 last_test_at = '2026-01-01T00:00:00Z', last_test_ok = 0, \
                 last_test_latency_ms = 42, last_test_checked = 'inference', \
                 last_test_message = 'old failure' \
             WHERE id = ?",
        )
        .bind(key_id)
        .execute(&state.pool)
        .await
        .unwrap();
        state.provider_key_cooldown.lock().await.insert(
            key_id,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let Json(updated) = update_provider(
            State(state.clone()),
            Path(created.id),
            Json(provider_update_with_keys(Some(vec![provider_key_input(
                Some(key_id),
                "Primary",
                Some("sk-rotated"),
                true,
            )]))),
        )
        .await
        .unwrap();

        assert_eq!(updated.api_keys[0].api_key_suffix, "ated");
        assert_eq!(updated.api_keys[0].last_test_ok, None);
        assert_eq!(updated.api_keys[0].last_error, None);
        assert!(
            !state
                .provider_key_cooldown
                .lock()
                .await
                .contains_key(&key_id)
        );
        let stale_fields: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_api_keys \
             WHERE id = ? AND ( \
                 last_used_at IS NOT NULL OR last_error_at IS NOT NULL OR last_error IS NOT NULL \
                 OR last_test_at IS NOT NULL OR last_test_ok IS NOT NULL \
                 OR last_test_latency_ms IS NOT NULL OR last_test_checked IS NOT NULL \
                 OR last_test_message IS NOT NULL \
             )",
        )
        .bind(key_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(stale_fields, 0);
    }

    #[tokio::test]
    async fn provider_api_key_pool_rejects_duplicate_secrets() {
        let state = provider_key_test_state().await;
        let error = create_provider(
            State(state),
            Json(provider_input(
                None,
                vec![
                    provider_key_input(None, "One", Some("sk-same"), true),
                    provider_key_input(None, "Two", Some("sk-same"), true),
                ],
            )),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("must be unique"));
    }

    #[tokio::test]
    async fn provider_api_key_pool_accepts_legacy_single_key_input() {
        let state = provider_key_test_state().await;
        let (_, Json(created)) = create_provider(
            State(state),
            Json(provider_input(Some("sk-legacy"), Vec::new())),
        )
        .await
        .unwrap();
        assert_eq!(created.api_keys.len(), 1);
        assert_eq!(created.api_keys[0].name, "Default");
        assert_eq!(created.api_keys[0].api_key_suffix, "gacy");
    }

    #[tokio::test]
    async fn persists_provider_health_check_model() {
        let state = provider_key_test_state().await;
        let (_, Json(created)) =
            create_provider(State(state.clone()), Json(provider_input(None, Vec::new())))
                .await
                .unwrap();
        assert_eq!(created.health_check_model, None);

        let mut update = provider_update_with_keys(None);
        update.health_check_model = Some("  claude-haiku-4-5  ".to_string());
        let Json(updated) = update_provider(State(state.clone()), Path(created.id), Json(update))
            .await
            .unwrap();
        assert_eq!(
            updated.health_check_model.as_deref(),
            Some("claude-haiku-4-5")
        );

        let mut clear = provider_update_with_keys(None);
        clear.health_check_model = Some(String::new());
        let Json(cleared) = update_provider(State(state), Path(created.id), Json(clear))
            .await
            .unwrap();
        assert_eq!(cleared.health_check_model, None);
    }

    #[tokio::test]
    async fn provider_health_check_uses_configured_model() {
        let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post({
                let seen = seen.clone();
                move |Json(body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        *seen.lock().await = Some(body);
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "health",
                                "choices": [],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 0,
                                    "total_tokens": 1
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

        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, health_check_model
             ) VALUES (1, 'mock', 'openai', ?, 'configured-model')",
        )
        .bind(format!("http://{address}"))
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'fallback-model', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let result = test_provider_inner(&state, 1).await.unwrap();
        assert!(result.ok);
        assert_eq!(result.checked, "inference");
        assert!(result.message.contains("configured-model"));
        assert_eq!(
            seen.lock()
                .await
                .as_ref()
                .and_then(|body| body.get("model"))
                .and_then(Value::as_str),
            Some("configured-model")
        );
        server.abort();
    }

    #[tokio::test]
    async fn provider_health_probe_learns_tool_search_compatibility() {
        let reject_tool_search = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post({
                let reject_tool_search = reject_tool_search.clone();
                move |Json(body): Json<Value>| {
                    let reject_tool_search = reject_tool_search.clone();
                    async move {
                        let includes_tool_search = body
                            .get("tools")
                            .and_then(Value::as_array)
                            .is_some_and(|tools| {
                                tools.iter().any(|tool| tool["type"] == "tool_search")
                            });
                        if includes_tool_search
                            && reject_tool_search.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "error": {
                                        "message": "unknown tool type: tool_search"
                                    }
                                })),
                            );
                        }
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "health",
                                "choices": [],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 0,
                                    "total_tokens": 1
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

        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, tool_search_supported
             ) VALUES (1, 'mock', 'openai', ?, 1)",
        )
        .bind(format!("http://{address}"))
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
             VALUES (1, 'Primary', 'sk-test', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'probe-model', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let result = test_provider_inner(&state, 1).await.unwrap();
        assert!(result.ok);
        let unsupported: i64 =
            sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(unsupported, 0);

        reject_tool_search.store(false, std::sync::atomic::Ordering::SeqCst);
        let result = test_provider_inner(&state, 1).await.unwrap();
        assert!(result.ok);
        let supported: i64 =
            sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(supported, 1);
        server.abort();
    }

    #[tokio::test]
    async fn provider_health_check_uses_responses_for_responses_only_model() {
        let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post({
                let seen = seen.clone();
                move |Json(body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        *seen.lock().await = Some(body);
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": "resp_health",
                                "object": "response",
                                "usage": {
                                    "input_tokens": 1,
                                    "output_tokens": 0,
                                    "total_tokens": 1
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

        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'mock', 'openai', ?)",
        )
        .bind(format!("http://{address}"))
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (
                provider_id, model_name, enabled, supported_endpoints
             ) VALUES (1, 'responses-model', 1, '[\"/responses\"]')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let result = test_provider_inner(&state, 1).await.unwrap();
        assert!(result.ok);
        assert_eq!(result.checked, "inference");
        let body = seen.lock().await;
        let body = body.as_ref().unwrap();
        assert_eq!(body["model"], "responses-model");
        assert_eq!(body["input"], "ping");
        assert!(body.get("max_output_tokens").is_none());
        server.abort();
    }

    #[tokio::test]
    async fn provider_key_health_check_tests_every_enabled_key() {
        let seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post({
                let seen = seen.clone();
                move |headers: HeaderMap, Json(_body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        let authorization = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        seen.lock().await.push(authorization.clone());
                        let ok = authorization == "Bearer sk-good";
                        (
                            if ok {
                                StatusCode::OK
                            } else {
                                StatusCode::UNAUTHORIZED
                            },
                            Json(json!({
                                "id": "key-health",
                                "choices": [],
                                "usage": {
                                    "prompt_tokens": 1,
                                    "completion_tokens": 0,
                                    "total_tokens": 1
                                },
                                "error": if ok {
                                    Value::Null
                                } else {
                                    json!({"message": "invalid API key"})
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

        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'multi-key', 'openai', ?)",
        )
        .bind(format!("http://{address}"))
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'probe-model', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) VALUES
                (11, 1, 'Broken', 'sk-bad', 1),
                (12, 1, 'Healthy', 'sk-good', 1),
                (13, 1, 'Disabled', 'sk-disabled', 0)",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE provider_api_keys \
             SET last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale unauthorized' \
             WHERE id = 12",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        state.provider_key_cooldown.lock().await.insert(
            11,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let result = test_provider_keys_inner(&state, 1).await.unwrap();

        assert_eq!(result.provider_name, "multi-key");
        assert_eq!(result.model.as_deref(), Some("probe-model"));
        assert_eq!(result.total, 2);
        assert_eq!(result.ok, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.results[0].key_id, Some(11));
        assert_eq!(result.results[0].key_name, "Broken");
        assert_eq!(result.results[0].api_key_suffix, "-bad");
        assert!(!result.results[0].ok);
        assert!(result.results[0].message.contains("401"));
        assert_eq!(result.results[1].key_id, Some(12));
        assert_eq!(result.results[1].key_name, "Healthy");
        assert_eq!(result.results[1].api_key_suffix, "good");
        assert!(result.results[1].ok);
        assert_eq!(result.results[1].checked, "inference");
        assert_eq!(
            seen.lock().await.as_slice(),
            &["Bearer sk-bad".to_string(), "Bearer sk-good".to_string()]
        );
        assert!(state.provider_key_cooldown.lock().await.contains_key(&11));
        let failed: (Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT last_test_ok, last_test_checked, last_test_message \
             FROM provider_api_keys WHERE id = 11",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(failed.0, Some(0));
        assert_eq!(failed.1.as_deref(), Some("inference"));
        assert!(failed.2.unwrap().contains("401"));
        let recovered: (Option<i64>, Option<String>, Option<String>, Option<String>) =
            sqlx::query_as(
                "SELECT last_test_ok, last_test_checked, last_test_message, last_error \
                 FROM provider_api_keys WHERE id = 12",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(recovered.0, Some(1));
        assert_eq!(recovered.1.as_deref(), Some("inference"));
        assert!(recovered.2.is_some());
        assert_eq!(recovered.3, None);
        let provider = get_provider(&state, 1).await.unwrap();
        assert_eq!(provider.api_keys[0].last_test_ok, Some(false));
        assert_eq!(provider.api_keys[1].last_test_ok, Some(true));
        assert!(provider.api_keys[1].last_test_at.is_some());

        sqlx::query(
            "UPDATE provider_api_keys \
             SET last_test_at = NULL, last_test_ok = NULL, last_test_latency_ms = NULL, \
                 last_test_checked = NULL, last_test_message = NULL, \
                 last_error_at = NULL, last_error = NULL \
             WHERE id IN (11, 12)",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE provider_api_keys \
             SET last_error_at = '2026-01-01T00:00:00Z', last_error = 'stale unauthorized' \
             WHERE id = 12",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let aggregate = test_provider_inner(&state, 1).await.unwrap();
        assert!(aggregate.ok);
        let persisted: (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT \
                COALESCE(SUM(CASE WHEN id IN (11, 12) AND last_test_ok = 0 THEN 1 ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN id IN (11, 12) AND last_test_ok = 1 THEN 1 ELSE 0 END), 0) \
             FROM provider_api_keys",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(persisted, (Some(1), Some(1)));
        let recovered_error: Option<String> =
            sqlx::query_scalar("SELECT last_error FROM provider_api_keys WHERE id = 12")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recovered_error, None);

        let Json(all) = test_all_provider_keys(State(state.clone())).await.unwrap();
        assert_eq!(all.total_providers, 1);
        assert_eq!(all.tested_providers, 1);
        assert_eq!(all.healthy_providers, 0);
        assert_eq!(all.failed_providers, 1);
        assert_eq!(all.total_keys, 2);
        assert_eq!(all.healthy_keys, 1);
        assert_eq!(all.failed_keys, 1);
        server.abort();
    }

    #[tokio::test]
    async fn usage_views_expose_provider_api_key_name() {
        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'Primary', 'sk-primary', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, provider_api_key_name,
                requested_model,
                endpoint, prompt_tokens, completion_tokens, total_tokens,
                latency_ms, status_code, success
             ) VALUES (
                'request-with-provider-key', 1, 11, 'Primary', 'gpt-test',
                '/v1/chat/completions', 10, 4, 14, 123, 200, 1
             )",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let query: UsageQuery = serde_json::from_value(json!({})).unwrap();
        let Json(page) = list_usage(State(state.clone()), Query(query))
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].provider_api_key_id, Some(11));
        assert_eq!(
            page.items[0].provider_api_key_name.as_deref(),
            Some("Primary")
        );

        let Json(detail) = get_usage_detail(
            State(state.clone()),
            Path("request-with-provider-key".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(detail.provider_api_key_id, Some(11));
        assert_eq!(detail.provider_api_key_name.as_deref(), Some("Primary"));

        let provider = get_provider(&state, 1).await.unwrap();
        assert_eq!(provider.api_keys[0].requests, 1);
        assert_eq!(provider.api_keys[0].success_rate, 100.0);
        assert_eq!(provider.api_keys[0].avg_latency_ms, 123.0);
        assert_eq!(provider.api_keys[0].prompt_tokens, 10);
        assert_eq!(provider.api_keys[0].completion_tokens, 4);
        state.provider_key_cooldown.lock().await.insert(
            11,
            std::time::Instant::now() + std::time::Duration::from_secs(120),
        );
        let provider = get_provider(&state, 1).await.unwrap();
        assert!(provider.api_keys[0].cooldown_seconds.is_some());
        assert!(provider.api_keys[0].cooldown_seconds.unwrap() > 0);
    }

    #[tokio::test]
    async fn usage_filter_can_select_provider_api_key() {
        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'Primary', 'sk-primary', 1),
                    (12, 1, 'Backup', 'sk-backup', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, provider_id, provider_api_key_id, provider_api_key_name,
                requested_model, endpoint, status_code, success
             ) VALUES
                ('primary', 1, 11, 'Primary', 'gpt-test', '/v1/chat/completions', 200, 1),
                ('backup', 1, 12, 'Backup', 'gpt-test', '/v1/chat/completions', 200, 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let query: UsageQuery = serde_json::from_value(json!({
            "provider_api_key_id": 12
        }))
        .unwrap();
        let Json(page) = list_usage(State(state), Query(query)).await.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].request_id, "backup");
        assert_eq!(
            page.items[0].provider_api_key_name.as_deref(),
            Some("Backup")
        );
    }

    #[test]
    fn validates_usage_retention_days() {
        assert_eq!(normalize_retention_days(None).unwrap(), None);
        assert_eq!(normalize_retention_days(Some(0)).unwrap(), None);
        assert_eq!(normalize_retention_days(Some(30)).unwrap(), Some(30));
        assert_eq!(normalize_retention_days(Some(3650)).unwrap(), Some(3650));
        assert!(normalize_retention_days(Some(-1)).is_err());
        assert!(normalize_retention_days(Some(3651)).is_err());
    }

    #[tokio::test]
    async fn settings_expose_database_stats() {
        let state = provider_key_test_state().await;
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Provider', 'openai', 'https://example.com/v1')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'Primary', 'sk-primary', 1)",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let Json(settings) = get_settings(State(state)).await.unwrap();

        assert_eq!(settings.database, "sqlite");
        assert!(settings.database_stats.size_bytes > 0);
        assert_eq!(settings.database_stats.providers, 1);
        assert_eq!(settings.database_stats.provider_api_keys, 1);
        assert_eq!(settings.database_stats.in_flight_requests, 0);
    }

    #[test]
    fn validates_overview_ranges() {
        let default_start = Utc::now() - Duration::days(13);
        let default_end = Utc::now() + Duration::days(1);
        assert_eq!(
            normalize_overview_range(None, None, default_start, default_end).unwrap(),
            (default_start, default_end)
        );

        let start = Utc::now() - Duration::days(7);
        let end = Utc::now();
        assert_eq!(
            normalize_overview_range(
                Some(&start.to_rfc3339()),
                Some(&end.to_rfc3339()),
                default_start,
                default_end,
            )
            .unwrap(),
            (start, end)
        );
        assert!(
            normalize_overview_range(Some("not-a-date"), None, default_start, default_end).is_err()
        );
        assert!(
            normalize_overview_range(
                Some(&end.to_rfc3339()),
                Some(&start.to_rfc3339()),
                default_start,
                default_end,
            )
            .is_err()
        );
        assert!(
            normalize_overview_range(
                Some(&(Utc::now() - Duration::days(367)).to_rfc3339()),
                Some(&Utc::now().to_rfc3339()),
                default_start,
                default_end,
            )
            .is_err()
        );
    }

    #[test]
    fn generates_api_key_material() {
        let (raw, hash, prefix, suffix) = generate_api_key_material();
        assert!(raw.starts_with("sk-openllm-"));
        assert_eq!(hash.len(), 64);
        assert_eq!(prefix, raw[..12]);
        assert_eq!(suffix, raw[raw.len() - 4..]);
    }

    #[test]
    fn validates_api_key_expiration() {
        assert_eq!(normalize_expiration(None).unwrap(), None);
        assert_eq!(normalize_expiration(Some("  ".to_string())).unwrap(), None);
        let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
        assert!(normalize_expiration(Some(future)).unwrap().is_some());
        assert!(normalize_expiration(Some("not-a-date".to_string())).is_err());
        assert!(
            normalize_expiration(Some((Utc::now() - Duration::hours(1)).to_rfc3339())).is_err()
        );
    }

    #[test]
    fn normalizes_api_key_model_permissions() {
        assert_eq!(normalize_allowed_models(None).unwrap(), None);
        assert_eq!(
            normalize_allowed_models(Some(vec!["  ".to_string()])).unwrap(),
            None
        );
        let stored = normalize_allowed_models(Some(vec![
            " gpt-* ".to_string(),
            "gpt-*".to_string(),
            "claude-*".to_string(),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(stored, r#"["gpt-*","claude-*"]"#);
        assert!(normalize_allowed_models(Some(vec!["unclosed[".to_string()])).is_err());
    }

    #[test]
    fn builds_model_sync_preview() {
        let row = |enabled: i64| ProviderModelPreviewRow {
            model_name: String::new(),
            enabled,
            context_limit: None,
            input_limit: None,
            output_limit: None,
            supported_endpoints: None,
            cost: None,
            display_name: None,
        };
        let entries = vec![
            ("new-model".to_string(), UpstreamModelInfo::default()),
            ("kept-model".to_string(), UpstreamModelInfo::default()),
            ("disabled-model".to_string(), UpstreamModelInfo::default()),
        ];
        let existing = HashMap::from([
            ("kept-model".to_string(), row(1)),
            ("disabled-model".to_string(), row(0)),
            ("removed-model".to_string(), row(1)),
        ]);
        let changed = vec![ModelSyncChange {
            model_name: "kept-model".to_string(),
            fields: vec!["context_limit".to_string()],
        }];
        let preview = build_model_sync_preview(7, &entries, &existing, changed);
        assert_eq!(preview.provider_id, 7);
        assert_eq!(preview.added, vec!["new-model"]);
        assert_eq!(preview.removed, vec!["removed-model"]);
        assert_eq!(preview.changed[0].model_name, "kept-model");
        assert_eq!(preview.retained, 2);
        assert_eq!(preview.disabled_retained, 1);
    }

    #[test]
    fn detects_metadata_changes_during_sync_preview() {
        let entries = vec![(
            "model".to_string(),
            UpstreamModelInfo {
                context_limit: Some(128_000),
                supported_endpoints: vec!["/responses".to_string()],
                display_name: Some("Model".to_string()),
            },
        )];
        let existing = HashMap::from([(
            "model".to_string(),
            ProviderModelPreviewRow {
                model_name: "model".to_string(),
                enabled: 1,
                context_limit: Some(64_000),
                input_limit: Some(64_000),
                output_limit: Some(8_000),
                supported_endpoints: Some(r#"["/chat/completions"]"#.to_string()),
                cost: Some(r#"{"input":1.0}"#.to_string()),
                display_name: None,
            },
        )]);

        let changed = detect_model_sync_changes(&entries, &existing, None, None);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].model_name, "model");
        assert!(changed[0].fields.contains(&"context_limit".to_string()));
        assert!(
            changed[0]
                .fields
                .contains(&"supported_endpoints".to_string())
        );
        assert!(changed[0].fields.contains(&"cost".to_string()));
        assert!(changed[0].fields.contains(&"display_name".to_string()));
    }

    #[test]
    fn escapes_csv_fields() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
    }

    #[tokio::test]
    async fn persists_provider_test_result() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                last_test_at TEXT,
                last_test_ok INTEGER,
                last_test_latency_ms INTEGER,
                last_test_checked TEXT,
                last_test_message TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO providers (id) VALUES (1)")
            .execute(&pool)
            .await
            .unwrap();
        let state = AppState::new(pool.clone(), None);
        persist_provider_test(
            &state,
            1,
            &ProviderTestResult {
                ok: true,
                latency_ms: 42,
                message: "ok".to_string(),
                checked: "inference".to_string(),
            },
        )
        .await
        .unwrap();
        let row: (Option<i64>, Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT last_test_ok, last_test_latency_ms, last_test_checked, last_test_message \
             FROM providers WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                Some(1),
                Some(42),
                Some("inference".to_string()),
                Some("ok".to_string())
            )
        );
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

    #[tokio::test]
    async fn model_limit_overrides_survive_resync() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url) \
             VALUES (1, 'CallAI', 'openai', 'https://sub.callai.one/v1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, input_limit,
                output_limit, context_override, input_override, output_override,
                supported_endpoints_override, cost, cost_input_override,
                cost_output_override
             ) VALUES
                (1, 'gpt-6-astra', 0, 1050000, 922000, 128000, 400000, NULL, 64000,
                 '[\"/responses\"]', '{\"input\":2,\"output\":10,\"cache_read\":0.2}', 1.5, 12.0),
                (1, 'gpt-6-luna', 1, 1050000, 922000, 128000, 400000, 400000, 64000,
                 NULL, NULL, NULL, NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        replace_provider_models(
            &mut tx,
            1,
            &[
                (
                    "gpt-6-astra".to_string(),
                    UpstreamModelInfo {
                        context_limit: Some(1_050_000),
                        supported_endpoints: vec!["/chat/completions".to_string()],
                        ..Default::default()
                    },
                ),
                (
                    "gpt-6-luna".to_string(),
                    UpstreamModelInfo {
                        context_limit: Some(1_050_000),
                        supported_endpoints: vec![
                            "/chat/completions".to_string(),
                            "/responses".to_string(),
                        ],
                        ..Default::default()
                    },
                ),
            ],
            None,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let state = AppState::new(pool, None);
        let limits = provider_model_limits(&state, 1).await.unwrap();
        assert_eq!(limits.len(), 2);
        let astra = limits
            .iter()
            .find(|model| model.model_name == "gpt-6-astra")
            .unwrap();
        assert!(!astra.enabled);
        assert_eq!(astra.context_limit, Some(400_000));
        assert_eq!(astra.input_limit, Some(400_000));
        assert_eq!(astra.output_limit, Some(64_000));
        assert_eq!(astra.context_override, Some(400_000));
        assert_eq!(astra.supported_endpoints, vec!["/responses"]);
        assert_eq!(
            astra.supported_endpoints_override,
            Some(vec!["/responses".to_string()])
        );
        assert_eq!(astra.cost_input, Some(1.5));
        assert_eq!(astra.cost_output, Some(12.0));
        assert_eq!(astra.cost_input_override, Some(1.5));
        assert_eq!(astra.cost_output_override, Some(12.0));

        let luna = limits
            .iter()
            .find(|model| model.model_name == "gpt-6-luna")
            .unwrap();
        assert_eq!(
            luna.supported_endpoints,
            vec!["/chat/completions", "/responses"]
        );
        assert_eq!(luna.supported_endpoints_override, None);

        let models = crate::registry::synced_models(&state.pool).await.unwrap();
        let model = models
            .iter()
            .find(|model| model.upstream_model == "gpt-6-luna")
            .unwrap();
        assert_eq!(models.len(), 1);
        let capabilities = model.capabilities.as_ref().unwrap();
        assert_eq!(capabilities.context_limit, Some(400_000));
        assert_eq!(capabilities.input_limit, Some(400_000));
        assert_eq!(capabilities.output_limit, Some(64_000));
        assert_eq!(
            model.supported_endpoints,
            Some(vec![
                "/chat/completions".to_string(),
                "/responses".to_string()
            ])
        );
    }

    #[tokio::test]
    async fn model_inventory_exposes_effective_limits_and_prices() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url, model_prefix, enabled) \
             VALUES (1, 'Inventory', 'openai', 'https://inventory.example/v1', 'inv/', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, input_limit, output_limit,
                context_override, input_override, output_override, supported_endpoints,
                supported_endpoints_override, cost, cost_input_override
             ) VALUES
                (1, 'model-a', 1, 100000, 90000, 8000, 50000, NULL, 4000,
                 '[\"/chat/completions\"]', NULL, '{\"input\":1,\"output\":2}', 1.5)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);

        let Json(rows) = list_model_inventory(State(state)).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.provider_id, 1);
        assert_eq!(row.provider_name, "Inventory");
        assert!(row.provider_enabled);
        assert_eq!(row.model_prefix, "inv/");
        assert_eq!(row.model_name, "model-a");
        assert_eq!(row.context_limit, Some(50_000));
        assert_eq!(row.input_limit, Some(50_000));
        assert_eq!(row.output_limit, Some(4_000));
        assert_eq!(row.supported_endpoints, vec!["/chat/completions"]);
        assert_eq!(row.cost_input, Some(1.5));
        assert_eq!(row.cost_output, Some(2.0));
    }

    #[tokio::test]
    async fn updates_model_cost_overrides() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'Priced', 'openai', 'https://priced.example/v1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);

        let Json(rows) = update_provider_model_limits(
            State(state),
            Path(1),
            Json(ProviderModelLimitsUpdate {
                models: vec![ProviderModelLimitInput {
                    model_name: "model".to_string(),
                    enabled: true,
                    supported_endpoints_override: None,
                    context_limit: None,
                    input_limit: None,
                    output_limit: None,
                    cost_input_override: Some(1.25),
                    cost_output_override: Some(5.0),
                    cost_cache_read_override: Some(0.1),
                    cost_cache_write_override: Some(2.0),
                }],
            }),
        )
        .await
        .unwrap();

        assert_eq!(rows[0].cost_input, Some(1.25));
        assert_eq!(rows[0].cost_output, Some(5.0));
        assert_eq!(rows[0].cost_cache_read, Some(0.1));
        assert_eq!(rows[0].cost_cache_write, Some(2.0));
        assert_eq!(rows[0].cost_input_override, Some(1.25));
    }

    #[tokio::test]
    async fn automatic_usage_retention_deletes_only_expired_logs() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('usage_retention_days', '30')")
            .execute(&pool)
            .await
            .unwrap();

        let old = (Utc::now() - Duration::days(31)).to_rfc3339();
        let recent = (Utc::now() - Duration::days(2)).to_rfc3339();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, success, created_at
             ) VALUES (?, ?, '/v1/chat/completions', 200, 1, ?)",
        )
        .bind("expired")
        .bind("test-model")
        .bind(old)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, success, created_at
             ) VALUES (?, ?, '/v1/chat/completions', 200, 1, ?)",
        )
        .bind("recent")
        .bind("test-model")
        .bind(recent)
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool.clone(), None);
        run_due_usage_retention(state).await;

        let request_ids =
            sqlx::query_scalar::<_, String>("SELECT request_id FROM usage_logs ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(request_ids, vec!["recent"]);
    }

    #[tokio::test]
    async fn overview_range_filters_dashboard_usage() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let old = (Utc::now() - Duration::days(20)).to_rfc3339();
        let recent = (Utc::now() - Duration::days(1)).to_rfc3339();
        let today = Utc::now().to_rfc3339();
        for (
            request_id,
            session_id,
            prompt,
            completion,
            cache_read,
            latency_ms,
            success,
            created_at,
        ) in [
            (
                "old",
                Some("old-session"),
                700_i64,
                200_i64,
                0_i64,
                500_i64,
                1_i64,
                old,
            ),
            (
                "recent",
                Some("shared-session"),
                60,
                40,
                20,
                100,
                1,
                recent.clone(),
            ),
            (
                "recent-2",
                Some("shared-session"),
                30,
                10,
                5,
                200,
                1,
                recent,
            ),
            ("today", None, 50, 0, 0, 300, 0, today),
        ] {
            sqlx::query(
                "INSERT INTO usage_logs (
                    request_id, session_id, requested_model, endpoint, prompt_tokens,
                    completion_tokens, total_tokens, cache_read_tokens, latency_ms,
                    status_code, success, created_at
                 ) VALUES (?, ?, 'test-model', '/v1/chat/completions', ?, ?, ?, ?, ?, 200, ?, ?)",
            )
            .bind(request_id)
            .bind(session_id)
            .bind(prompt)
            .bind(completion)
            .bind(prompt + completion)
            .bind(cache_read)
            .bind(latency_ms)
            .bind(success)
            .bind(created_at)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens,
                total_tokens, status_code, in_flight, success, created_at
             ) VALUES (
                'pending', 'test-model', '/v1/chat/completions', 25, 25, 0, 1, 0,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO providers (name, provider_type, base_url, enabled, last_test_ok)
             VALUES
                ('healthy', 'openai', 'https://healthy.example/v1', 1, 1),
                ('failed', 'openai', 'https://failed.example/v1', 1, 0),
                ('untested', 'openai', 'https://untested.example/v1', 1, NULL),
                ('disabled', 'openai', 'https://disabled.example/v1', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (
                provider_id, name, secret, enabled, last_test_ok, last_error
             ) VALUES
                (1, 'Healthy', 'sk-healthy', 1, 1, NULL),
                (2, 'Failed', 'sk-failed', 1, 0, 'upstream 401'),
                (3, 'Untested', 'sk-untested', 1, NULL, NULL),
                (4, 'Disabled provider', 'sk-disabled-provider', 1, 1, NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        state.provider_key_cooldown.lock().await.insert(
            99,
            std::time::Instant::now() + std::time::Duration::from_secs(120),
        );
        let range_start = Utc::now() - Duration::days(2);
        let range_end = Utc::now() + Duration::days(1);
        let Json(view) = overview(
            State(state),
            Query(OverviewQuery {
                tz_offset_minutes: 0,
                from: Some(range_start.to_rfc3339()),
                to: Some(range_end.to_rfc3339()),
            }),
        )
        .await
        .unwrap();

        assert_eq!(view.requests_today, 1);
        assert_eq!(view.requests_total, 4);
        assert_eq!(view.prompt_tokens_today, 50);
        assert_eq!(view.completion_tokens_today, 0);
        assert_eq!(view.prompt_tokens_total, 840);
        assert_eq!(view.completion_tokens_total, 250);
        assert_eq!(view.range_requests, 3);
        assert_eq!(view.range_tokens, 190);
        assert_eq!(view.range_prompt_tokens, 140);
        assert_eq!(view.range_completion_tokens, 50);
        assert_eq!(view.range_cache_read, 25);
        assert_eq!(view.range_sessions, 1);
        assert!((view.range_session_coverage - 66.666_666).abs() < 0.001);
        assert_eq!(view.range_avg_requests_per_session, 2.0);
        assert!((view.range_session_cache_hit_rate - 27.777_777).abs() < 0.001);
        assert!((view.range_success_rate - 66.666_666).abs() < 0.001);
        assert_eq!(view.range_avg_latency_ms, 200.0);
        assert_eq!(view.active_providers, 3);
        assert_eq!(view.healthy_providers, 1);
        assert_eq!(view.failed_providers, 1);
        assert_eq!(view.untested_providers, 1);
        assert_eq!(view.provider_keys_total, 3);
        assert_eq!(view.healthy_provider_keys, 1);
        assert_eq!(view.failed_provider_keys, 1);
        assert_eq!(view.untested_provider_keys, 1);
        assert_eq!(view.runtime_error_provider_keys, 1);
        assert_eq!(view.cooling_provider_keys, 1);
        assert_eq!(view.in_flight_requests, 1);
        assert_eq!(
            view.daily_usage
                .iter()
                .map(|row| row.prompt_tokens)
                .sum::<i64>(),
            view.range_prompt_tokens
        );
        assert_eq!(
            view.daily_usage
                .iter()
                .map(|row| row.completion_tokens)
                .sum::<i64>(),
            view.range_completion_tokens
        );
        assert_eq!(view.recent_requests.len(), 4);
        assert!(view.recent_requests.iter().any(|row| row.in_flight));
        assert!(
            view.recent_requests
                .iter()
                .all(|row| row.request_id != "old")
        );
        assert_eq!(view.model_usage[0].requests, 3);
        assert_eq!(view.model_usage[0].tokens, 190);
        assert_eq!(view.model_usage[0].prompt_tokens, 140);
        assert_eq!(view.model_usage[0].completion_tokens, 50);
    }

    #[tokio::test]
    async fn usage_lifetime_stats_tracks_completion_and_cleanup() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens,
                completion_tokens, total_tokens, cache_read_tokens,
                cache_write_tokens, latency_ms, status_code, in_flight, success
             ) VALUES (
                'pending', 'test-model', '/v1/chat/completions', 10,
                20, 30, 5, 7, 100, 0, 1, 1
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let pending: i64 =
            sqlx::query_scalar("SELECT requests FROM usage_lifetime_stats WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pending, 0);

        sqlx::query(
            "UPDATE usage_logs
             SET in_flight = 0, status_code = 200, success = 1,
                 estimated_cost_micros = 42
             WHERE request_id = 'pending'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let completed: (i64, i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT requests, tokens, prompt_tokens, completion_tokens,
                    cache_read_tokens, cache_write_tokens, cost_micros,
                    successful_requests, latency_ms_sum
             FROM usage_lifetime_stats WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(completed, (1, 30, 10, 20, 5, 7, 42, 1, 100));

        sqlx::query("DELETE FROM usage_logs WHERE request_id = 'pending'")
            .execute(&pool)
            .await
            .unwrap();
        let cleaned: (i64, i64, i64) = sqlx::query_as(
            "SELECT requests, tokens, cost_micros
             FROM usage_lifetime_stats WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cleaned, (0, 0, 0));
    }

    #[tokio::test]
    async fn usage_filter_can_select_in_flight_requests() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, prompt_tokens, total_tokens,
                latency_ms, status_code, in_flight, success, created_at
             ) VALUES
                ('pending', 'm', '/v1/chat/completions', 10, 10, 0, 0, 1, 0,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('finished', 'm', '/v1/chat/completions', 10, 20, 100, 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let Json(page) = list_usage(
            State(state),
            Query(UsageQuery {
                page: 1,
                page_size: 20,
                provider_id: None,
                provider_api_key_id: None,
                api_key_id: None,
                route_id: None,
                model: None,
                request_id: None,
                session_id: None,
                endpoint: None,
                success: None,
                in_flight: Some(true),
                from: None,
                to: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].request_id, "pending");
        assert!(page.items[0].in_flight);
    }

    #[tokio::test]
    async fn usage_filter_can_select_endpoint() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code,
                in_flight, success, created_at
             ) VALUES
                ('chat', 'm', '/v1/chat/completions', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('responses', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let Json(page) = list_usage(
            State(state),
            Query(UsageQuery {
                page: 1,
                page_size: 20,
                provider_id: None,
                provider_api_key_id: None,
                api_key_id: None,
                route_id: None,
                model: None,
                request_id: None,
                session_id: None,
                endpoint: Some("/v1/responses".to_string()),
                success: None,
                in_flight: None,
                from: None,
                to: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].request_id, "responses");
    }

    #[tokio::test]
    async fn usage_filter_can_select_session() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, session_id, requested_model, endpoint, status_code,
                in_flight, success, created_at
             ) VALUES
                ('session-a', 'codex-session-a', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                ('session-b', 'codex-session-b', 'm', '/v1/responses', 200, 0, 1,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let Json(page) = list_usage(
            State(state),
            Query(UsageQuery {
                page: 1,
                page_size: 20,
                provider_id: None,
                provider_api_key_id: None,
                api_key_id: None,
                route_id: None,
                model: None,
                request_id: None,
                session_id: Some("session-a".to_string()),
                endpoint: None,
                success: None,
                in_flight: None,
                from: None,
                to: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].request_id, "session-a");
        assert_eq!(page.items[0].session_id.as_deref(), Some("codex-session-a"));
    }

    #[tokio::test]
    async fn reconciles_stale_and_interrupted_usage_requests() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let stale = (Utc::now() - Duration::days(2)).to_rfc3339();
        let recent = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, in_flight,
                success, created_at
             ) VALUES
                ('stale', 'm', '/v1/chat/completions', 0, 1, 0, ?),
                ('recent', 'm', '/v1/chat/completions', 0, 1, 0, ?),
                ('done', 'm', '/v1/chat/completions', 200, 0, 1, ?)",
        )
        .bind(stale)
        .bind(recent)
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool.clone(), None);
        let cutoff = (Utc::now() - Duration::days(1)).to_rfc3339();
        let stale_updated = finish_interrupted_usage_requests(
            &state,
            Some(&cutoff),
            "request was interrupted before completion",
        )
        .await
        .unwrap();
        assert_eq!(stale_updated, 1);
        let stale_row: (i64, i64, String) = sqlx::query_as(
            "SELECT in_flight, status_code, error_message
             FROM usage_logs WHERE request_id = 'stale'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stale_row.0, 0);
        assert_eq!(stale_row.1, 499);
        assert_eq!(stale_row.2, "request was interrupted before completion");

        let startup_updated = reconcile_interrupted_usage_requests(&state).await.unwrap();
        assert_eq!(startup_updated, 1);
        let statuses = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT request_id, in_flight, status_code
             FROM usage_logs ORDER BY request_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            statuses,
            vec![
                ("done".to_string(), 0, 200),
                ("recent".to_string(), 0, 499),
                ("stale".to_string(), 0, 499),
            ]
        );
    }

    #[tokio::test]
    async fn stale_reconciliation_uses_last_activity_for_long_streams() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let old_created = (Utc::now() - Duration::hours(2)).to_rfc3339();
        let active = Utc::now().to_rfc3339();
        let stale_activity = (Utc::now() - Duration::minutes(20)).to_rfc3339();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, requested_model, endpoint, status_code, in_flight,
                success, created_at, last_activity_at
             ) VALUES
                ('active', 'm', '/v1/chat/completions', 0, 1, 0, ?, ?),
                ('stale', 'm', '/v1/chat/completions', 0, 1, 0, ?, ?)",
        )
        .bind(&old_created)
        .bind(active)
        .bind(&old_created)
        .bind(stale_activity)
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool.clone(), None);
        reconcile_stale_usage_requests(state).await;
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT request_id, in_flight, status_code
             FROM usage_logs ORDER BY request_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![("active".to_string(), 1, 0), ("stale".to_string(), 0, 499)]
        );
    }
}
