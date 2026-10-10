use super::*;

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

pub(crate) struct ProviderQuotaCredential {
    pub(crate) key_id: Option<i64>,
    pub(crate) key_name: String,
    pub(crate) key_suffix: String,
    pub(crate) secret: String,
}

pub(crate) async fn provider_quota_credential(
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

pub(crate) fn quota_credential_from_record(
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

pub(crate) fn quota_title(kind: &str) -> &'static str {
    match kind {
        "command_code" => "Command Code 额度",
        "opencode_go" => "OpenCode Go 额度",
        "deepseek" => "DeepSeek 余额与价格",
        _ => "提供商额度",
    }
}

pub(crate) async fn fetch_quota_json(
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

pub(crate) async fn fetch_command_code_quota(
    state: &AppState,
    api_key: &str,
) -> AppResult<ProviderQuotaView> {
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

pub(crate) async fn fetch_opencode_go_quota(
    state: &AppState,
    api_key: &str,
) -> AppResult<ProviderQuotaView> {
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

pub(crate) async fn fetch_deepseek_quota(
    state: &AppState,
    api_key: &str,
) -> AppResult<ProviderQuotaView> {
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

pub(crate) fn epoch_millis_to_rfc3339(value: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(value).map(|value| value.to_rfc3339())
}

pub(crate) fn quota_reset_at(value: Option<&Value>) -> Option<String> {
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

pub(crate) fn command_code_plan_name(plan_id: &str) -> String {
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

pub(crate) fn command_code_monthly_total(plan_id: &str) -> Option<f64> {
    match plan_id {
        "individual-go" => Some(10.0),
        "individual-goat" => Some(70.0),
        "individual-pro" => Some(80.0),
        "teams-pro" => Some(40.0),
        _ => None,
    }
}

pub(crate) async fn provider_prices(
    state: &AppState,
    provider_id: i64,
) -> AppResult<Vec<ProviderPriceView>> {
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
