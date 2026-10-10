//! Outbound webhook delivery.
//!
//! A background dispatcher subscribes to the same in-process event stream that
//! feeds the console, so the request hot path never waits on a subscriber. Each
//! delivery is recorded so an operator can see whether an integration is
//! actually working.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::Row;
use tokio::sync::Semaphore;

use crate::models::Webhook;
use crate::state::{AppState, UsageEvent};

type HmacSha256 = Hmac<Sha256>;

/// How many delivery rows are kept per webhook. Old rows are pruned on write so
/// the table cannot grow without bound on a busy gateway.
const DELIVERY_HISTORY_PER_WEBHOOK: i64 = 200;

/// Attempts per delivery, including the first one.
const DELIVERY_ATTEMPTS: u32 = 3;

const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Event-level concurrency. Each event may fan out to several webhooks, but a
/// slow receiver must not stop the dispatcher from draining the broadcast
/// channel.
const MAX_CONCURRENT_EVENT_DELIVERIES: usize = 32;

#[derive(Debug, Clone)]
pub(crate) struct DeliveryOutcome {
    pub status_code: Option<i64>,
    pub attempts: i64,
    pub error: Option<String>,
    pub duration_ms: i64,
}

impl DeliveryOutcome {
    pub(crate) fn succeeded(&self) -> bool {
        matches!(self.status_code, Some(status) if (200..300).contains(&status))
    }
}

/// Subscribes to usage events and delivers them until the process exits.
///
/// `broadcast` drops messages for a lagging subscriber rather than blocking the
/// sender. Delivery is therefore best-effort: the dispatcher logs lag and keeps
/// going, while each completed attempt is still visible in delivery history.
pub async fn run_dispatcher(state: AppState) {
    let mut receiver = state.events.subscribe();
    let event_slots = Arc::new(Semaphore::new(MAX_CONCURRENT_EVENT_DELIVERIES));
    loop {
        match receiver.recv().await {
            Ok(event) => {
                let Ok(permit) = event_slots.clone().acquire_owned().await else {
                    break;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    deliver_usage_event(&state, event).await;
                });
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "webhook dispatcher lagged behind usage events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Delivers one completed (or failed) request to every subscribed webhook.
pub(crate) async fn deliver_usage_event(state: &AppState, event: UsageEvent) {
    let event_type = if event.success {
        "request.completed"
    } else {
        "request.failed"
    };
    let data = match usage_payload(state, &event).await {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, request_id = %event.request_id, "failed to build webhook payload");
            return;
        }
    };
    let webhooks = match enabled_webhooks(state).await {
        Ok(webhooks) => webhooks,
        Err(error) => {
            tracing::warn!(%error, "failed to load webhooks");
            return;
        }
    };
    let deliveries = webhooks
        .into_iter()
        .filter(|webhook| webhook.subscribes_to(event_type))
        .map(|webhook| {
            let state = state.clone();
            let event_type = event_type.to_string();
            let data = data.clone();
            let request_id = event.request_id.clone();
            async move {
                let outcome = deliver(&state, &webhook, &event_type, &data).await;
                record_delivery(&state, &webhook, &event_type, Some(&request_id), &outcome).await;
            }
        });
    futures_util::future::join_all(deliveries).await;
}

async fn enabled_webhooks(state: &AppState) -> Result<Vec<Webhook>, sqlx::Error> {
    sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE enabled = 1 ORDER BY id")
        .fetch_all(&state.pool)
        .await
}

async fn usage_payload(state: &AppState, event: &UsageEvent) -> Result<Value, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT u.request_id, u.session_id, u.api_key_id, u.route_id,
               u.provider_id, p.name AS provider_name, u.provider_api_key_id,
               u.requested_model, u.upstream_model, u.endpoint,
               u.prompt_tokens, u.completion_tokens, u.total_tokens,
               u.cache_read_tokens, u.cache_write_tokens,
               u.estimated_cost_micros, u.latency_ms, u.first_token_ms,
               u.status_code, u.success, u.streamed, u.error_message,
               u.warning_message, u.created_at
        FROM usage_logs u
        LEFT JOIN providers p ON p.id = u.provider_id
        WHERE u.request_id = ?
        ORDER BY u.id DESC
        LIMIT 1
        "#,
    )
    .bind(&event.request_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(json!({
        "request_id": row.get::<String, _>("request_id"),
        "session_id": row.get::<Option<String>, _>("session_id"),
        "api_key_id": row.get::<Option<i64>, _>("api_key_id"),
        "route_id": row.get::<Option<i64>, _>("route_id"),
        "provider_id": row.get::<Option<i64>, _>("provider_id"),
        "provider_name": row.get::<Option<String>, _>("provider_name"),
        "provider_api_key_id": row.get::<Option<i64>, _>("provider_api_key_id"),
        "requested_model": row.get::<String, _>("requested_model"),
        "upstream_model": row.get::<Option<String>, _>("upstream_model"),
        "endpoint": row.get::<String, _>("endpoint"),
        "prompt_tokens": row.get::<i64, _>("prompt_tokens"),
        "completion_tokens": row.get::<i64, _>("completion_tokens"),
        "total_tokens": row.get::<i64, _>("total_tokens"),
        "cache_read_tokens": row.get::<i64, _>("cache_read_tokens"),
        "cache_write_tokens": row.get::<i64, _>("cache_write_tokens"),
        "estimated_cost_micros": row.get::<Option<i64>, _>("estimated_cost_micros"),
        "latency_ms": row.get::<i64, _>("latency_ms"),
        "first_token_ms": row.get::<Option<i64>, _>("first_token_ms"),
        "status_code": row.get::<i64, _>("status_code"),
        "success": row.get::<i64, _>("success") != 0,
        "streamed": row.get::<i64, _>("streamed") != 0,
        "error_message": row.get::<Option<String>, _>("error_message"),
        "warning_message": row.get::<Option<String>, _>("warning_message"),
        "created_at": row.get::<String, _>("created_at"),
        "event_id": event.id,
    }))
}

/// Signs `timestamp.body` so the receiver can prove the payload came from this
/// gateway and was not replayed under a different body.
pub(crate) fn signature_header(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(format!("{timestamp}.{body}").as_bytes());
    let digest = mac.finalize().into_bytes();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("t={timestamp},v1={hex}")
}

/// POSTs one event payload, retrying transient failures with a short backoff.
pub(crate) async fn deliver(
    state: &AppState,
    webhook: &Webhook,
    event_type: &str,
    data: &Value,
) -> DeliveryOutcome {
    let started = Instant::now();
    let timestamp = chrono::Utc::now().timestamp();
    let body = json!({
        "event": event_type,
        "sent_at": chrono::Utc::now().to_rfc3339(),
        "webhook_id": webhook.id,
        "data": data,
    })
    .to_string();
    let signature = signature_header(&webhook.secret, timestamp, &body);
    let custom_headers = webhook_header_map(webhook);

    let mut attempts = 0u32;
    let mut last_status = None;
    let mut last_error = None;
    while attempts < DELIVERY_ATTEMPTS {
        attempts += 1;
        let mut request = state
            .client
            .post(&webhook.url)
            .timeout(DELIVERY_TIMEOUT)
            .headers(custom_headers.clone())
            .header("content-type", "application/json")
            .header("x-openllm-event", event_type)
            .header("x-openllm-delivery-attempt", attempts.to_string())
            .body(body.clone());
        if !webhook.secret.is_empty() {
            request = request.header("x-openllm-signature", signature.clone());
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16() as i64;
                last_status = Some(status);
                last_error = None;
                if (200..300).contains(&status) {
                    break;
                }
                last_error = Some(format!("webhook returned HTTP {status}"));
                // A definitive client error will not become a success by
                // retrying, so only transient statuses are retried.
                if !matches!(status, 408 | 425 | 429 | 500..=599) {
                    break;
                }
            }
            Err(error) => {
                last_status = None;
                last_error = Some(error.to_string());
            }
        }
        if attempts < DELIVERY_ATTEMPTS {
            let backoff = Duration::from_millis(250 * u64::from(attempts));
            tokio::time::sleep(backoff).await;
        }
    }

    DeliveryOutcome {
        status_code: last_status,
        attempts: i64::from(attempts),
        error: last_error,
        duration_ms: started.elapsed().as_millis() as i64,
    }
}

/// Rebuilds the stored custom headers, ignoring malformed or reserved entries
/// so a legacy row can never override gateway event or signature metadata.
fn webhook_header_map(webhook: &Webhook) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let Ok(serde_json::Value::Object(values)) = serde_json::from_str(&webhook.headers) else {
        return headers;
    };
    for (name, value) in values {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        if name.as_str().starts_with("x-openllm-")
            || matches!(name.as_str(), "host" | "content-length" | "content-type")
        {
            continue;
        }
        let Some(value) = value.as_str() else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(value) else {
            continue;
        };
        headers.insert(name, value);
    }
    headers
}

/// Persists one delivery attempt sequence and trims old history.
pub(crate) async fn record_delivery(
    state: &AppState,
    webhook: &Webhook,
    event_type: &str,
    request_id: Option<&str>,
    outcome: &DeliveryOutcome,
) {
    let insert = sqlx::query(
        "INSERT INTO webhook_deliveries (
            webhook_id, event_type, request_id, status_code, attempts, error, duration_ms
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(webhook.id)
    .bind(event_type)
    .bind(request_id)
    .bind(outcome.status_code)
    .bind(outcome.attempts)
    .bind(outcome.error.as_deref())
    .bind(outcome.duration_ms)
    .execute(&state.pool)
    .await;
    if let Err(error) = insert {
        tracing::warn!(%error, webhook_id = webhook.id, "failed to record webhook delivery");
        return;
    }
    let prune = sqlx::query(
        "DELETE FROM webhook_deliveries
         WHERE webhook_id = ?
           AND id NOT IN (
               SELECT id FROM webhook_deliveries
               WHERE webhook_id = ?
               ORDER BY id DESC
               LIMIT ?
           )",
    )
    .bind(webhook.id)
    .bind(webhook.id)
    .bind(DELIVERY_HISTORY_PER_WEBHOOK)
    .execute(&state.pool)
    .await;
    if let Err(error) = prune {
        tracing::warn!(%error, webhook_id = webhook.id, "failed to prune webhook deliveries");
    }
}
