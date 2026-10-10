use super::*;
use axum::http::{HeaderName, HeaderValue};

const RESERVED_WEBHOOK_HEADERS: [&str; 5] = [
    "host",
    "content-length",
    "content-type",
    "x-openllm-event",
    "x-openllm-delivery-attempt",
];

/// Rejects URLs the gateway cannot or should not POST to.
fn validate_webhook_url(raw: &str) -> AppResult<String> {
    let url = raw.trim().to_string();
    if url.is_empty() {
        return Err(AppError::BadRequest("webhook URL is required".to_string()));
    }
    let parsed = url
        .parse::<reqwest::Url>()
        .map_err(|error| AppError::BadRequest(format!("invalid webhook URL '{raw}': {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "webhook URL must use http or https".to_string(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(AppError::BadRequest(
            "webhook URL must include a host".to_string(),
        ));
    }
    Ok(url)
}

/// Normalizes and validates the subscribed event types.
fn normalize_event_types(values: Option<Vec<String>>) -> AppResult<(Vec<String>, String)> {
    let mut event_types = Vec::new();
    for value in values.unwrap_or_else(|| {
        WEBHOOK_EVENT_TYPES
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    }) {
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        if !WEBHOOK_EVENT_TYPES.contains(&value.as_str()) {
            return Err(AppError::BadRequest(format!(
                "unsupported webhook event '{value}'; expected one of {}",
                WEBHOOK_EVENT_TYPES.join(", ")
            )));
        }
        if !event_types.contains(&value) {
            event_types.push(value);
        }
    }
    if event_types.is_empty() {
        return Err(AppError::BadRequest(
            "a webhook must subscribe to at least one event".to_string(),
        ));
    }
    let stored =
        serde_json::to_string(&event_types).map_err(|error| AppError::Internal(error.into()))?;
    Ok((event_types, stored))
}

/// Validates operator-provided headers and stores them as a stable JSON object.
fn normalize_webhook_headers(value: Option<&serde_json::Value>) -> AppResult<String> {
    let Some(value) = value else {
        return Ok("{}".to_string());
    };
    if value.is_null() {
        return Ok("{}".to_string());
    }
    let object = value
        .as_object()
        .ok_or_else(|| AppError::BadRequest("webhook headers must be a JSON object".to_string()))?;
    if object.len() > 32 {
        return Err(AppError::BadRequest(
            "webhook headers are limited to 32 entries".to_string(),
        ));
    }
    let mut normalized = serde_json::Map::new();
    for (name, value) in object {
        let name = name.trim();
        let parsed_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            AppError::BadRequest(format!("invalid webhook header name '{name}': {error}"))
        })?;
        if RESERVED_WEBHOOK_HEADERS.contains(&parsed_name.as_str())
            || parsed_name.as_str().starts_with("x-openllm-")
        {
            return Err(AppError::BadRequest(format!(
                "webhook header '{name}' is managed by the gateway"
            )));
        }
        let value = value.as_str().ok_or_else(|| {
            AppError::BadRequest(format!("webhook header '{name}' must have a string value"))
        })?;
        let parsed_value = HeaderValue::from_str(value).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid value for webhook header '{name}': {error}"
            ))
        })?;
        normalized.insert(
            parsed_name.as_str().to_string(),
            serde_json::Value::String(
                parsed_value
                    .to_str()
                    .map_err(|error| AppError::BadRequest(error.to_string()))?
                    .to_string(),
            ),
        );
    }
    serde_json::to_string(&normalized).map_err(|error| AppError::Internal(error.into()))
}

fn webhook_headers(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}))
}

async fn webhook_view(state: &AppState, id: i64) -> AppResult<WebhookView> {
    let webhook = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(into_view(state, webhook).await?)
}

async fn into_view(state: &AppState, webhook: Webhook) -> AppResult<WebhookView> {
    let event_types = webhook.event_type_list();
    let headers = webhook_headers(&webhook.headers);
    let (last_delivery_at, last_delivery_status, recent_failures) =
        sqlx::query_as::<_, (Option<String>, Option<i64>, i64)>(
            "SELECT \
                (SELECT created_at FROM webhook_deliveries d \
                  WHERE d.webhook_id = w.id ORDER BY d.id DESC LIMIT 1), \
                (SELECT status_code FROM webhook_deliveries d \
                  WHERE d.webhook_id = w.id ORDER BY d.id DESC LIMIT 1), \
                (SELECT COUNT(*) FROM webhook_deliveries d \
                  WHERE d.webhook_id = w.id \
                    AND d.created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-24 hours') \
                    AND (d.status_code IS NULL OR d.status_code >= 400)) \
             FROM webhooks w WHERE w.id = ?",
        )
        .bind(webhook.id)
        .fetch_one(&state.pool)
        .await?;
    Ok(WebhookView {
        id: webhook.id,
        name: webhook.name,
        url: webhook.url,
        secret_set: !webhook.secret.is_empty(),
        headers,
        event_types,
        enabled: webhook.enabled != 0,
        created_at: webhook.created_at,
        updated_at: webhook.updated_at,
        last_delivery_at,
        last_delivery_status,
        recent_failures,
    })
}

pub async fn list_webhooks(State(state): State<AppState>) -> AppResult<Json<Vec<WebhookView>>> {
    let webhooks = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks ORDER BY id")
        .fetch_all(&state.pool)
        .await?;
    let mut views = Vec::with_capacity(webhooks.len());
    for webhook in webhooks {
        views.push(into_view(&state, webhook).await?);
    }
    Ok(Json(views))
}

pub async fn create_webhook(
    State(state): State<AppState>,
    Json(input): Json<WebhookInput>,
) -> AppResult<(StatusCode, Json<WebhookView>)> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("webhook name is required".to_string()));
    }
    let url = validate_webhook_url(&input.url)?;
    let headers = normalize_webhook_headers(input.headers.as_ref())?;
    let (_, stored_events) = normalize_event_types(input.event_types)?;
    let secret = input.secret.as_deref().map(str::trim).unwrap_or("");
    let result = sqlx::query(
        "INSERT INTO webhooks (name, url, secret, headers, event_types, enabled) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(url)
    .bind(secret)
    .bind(headers)
    .bind(stored_events)
    .bind(input.enabled as i64)
    .execute(&state.pool)
    .await
    .map_err(map_sqlite_conflict)?;
    let id = result.last_insert_rowid();
    Ok((StatusCode::CREATED, Json(webhook_view(&state, id).await?)))
}

pub async fn update_webhook(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<WebhookUpdate>,
) -> AppResult<Json<WebhookView>> {
    let current = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("webhook not found".to_string()))?;

    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or(current.name.as_str())
        .to_string();
    if name.is_empty() {
        return Err(AppError::BadRequest("webhook name is required".to_string()));
    }
    let url = match input.url.as_deref() {
        Some(url) => validate_webhook_url(url)?,
        None => current.url.clone(),
    };
    let headers = match input.headers.as_ref() {
        Some(value) => normalize_webhook_headers(Some(value))?,
        None => current.headers.clone(),
    };
    let stored_events = match input.event_types {
        Some(values) => normalize_event_types(Some(values))?.1,
        None => current.event_types.clone(),
    };
    let secret = if input.clear_secret.unwrap_or(false) {
        String::new()
    } else {
        match input.secret.as_deref().map(str::trim) {
            // An omitted secret keeps the stored one; an explicit empty string
            // is treated as "leave unchanged" too, so the UI can submit a blank
            // field without silently disabling signing.
            Some(value) if !value.is_empty() => value.to_string(),
            _ => current.secret.clone(),
        }
    };
    let enabled = input.enabled.unwrap_or(current.enabled != 0);

    sqlx::query(
        "UPDATE webhooks SET name = ?, url = ?, secret = ?, headers = ?, event_types = ?, \
         enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(name)
    .bind(url)
    .bind(secret)
    .bind(headers)
    .bind(stored_events)
    .bind(enabled as i64)
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(map_sqlite_conflict)?;
    Ok(Json(webhook_view(&state, id).await?))
}

pub async fn delete_webhook(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    let result = sqlx::query("DELETE FROM webhooks WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("webhook not found".to_string()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_webhook_deliveries(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<WebhookDeliveryView>>> {
    let exists = sqlx::query_scalar::<_, i64>("SELECT EXISTS(SELECT 1 FROM webhooks WHERE id = ?)")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if exists == 0 {
        return Err(AppError::NotFound("webhook not found".to_string()));
    }
    Ok(Json(
        sqlx::query_as::<_, WebhookDeliveryView>(
            "SELECT * FROM webhook_deliveries WHERE webhook_id = ? ORDER BY id DESC LIMIT 50",
        )
        .bind(id)
        .fetch_all(&state.pool)
        .await?,
    ))
}

/// Sends a synthetic payload so an operator can verify the receiver end to end.
pub async fn test_webhook(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<WebhookDeliveryView>> {
    let webhook = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("webhook not found".to_string()))?;
    let event_type = webhook
        .event_type_list()
        .into_iter()
        .next()
        .unwrap_or_else(|| "request.completed".to_string());
    let data = json!({
        "test": true,
        "message": "OpenLLM test delivery",
        "request_id": null,
    });
    let outcome = crate::webhooks::deliver(&state, &webhook, &event_type, &data).await;
    crate::webhooks::record_delivery(&state, &webhook, &event_type, None, &outcome).await;
    let delivery = sqlx::query_as::<_, WebhookDeliveryView>(
        "SELECT * FROM webhook_deliveries WHERE webhook_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    if outcome.succeeded() {
        Ok(Json(delivery))
    } else {
        Err(AppError::BadRequest(format!(
            "webhook test delivery failed: {}",
            outcome
                .error
                .clone()
                .unwrap_or_else(|| "receiver rejected the payload".to_string())
        )))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/webhooks.rs"]
mod tests;
