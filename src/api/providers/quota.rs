use super::*;

use std::sync::LazyLock;

const COMMAND_CODE_PLAN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Clone)]
struct CommandCodePlanCacheEntry {
    fetched_at: std::time::Instant,
    models: Vec<ProviderQuotaModel>,
}

static COMMAND_CODE_PLAN_CACHE: LazyLock<
    tokio::sync::Mutex<HashMap<String, CommandCodePlanCacheEntry>>,
> = LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

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
    let (credits, subscription, usage) = tokio::join!(
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
        fetch_quota_json(
            state,
            "https://api.commandcode.ai/alpha/usage/summary",
            api_key,
            &headers,
        ),
    );
    let credits = credits?;
    let subscription = subscription.ok();
    let usage = usage.ok();
    let plan_id = subscription
        .as_ref()
        .and_then(|value| value.pointer("/data/planId"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let plan_name = plan_id.as_deref().map(command_code_plan_name);
    let monthly_remaining = credits
        .pointer("/credits/monthlyCredits")
        .and_then(Value::as_f64);
    let monthly_used = usage
        .as_ref()
        .and_then(|value| value.pointer("/totalMonthlyCredits"))
        .and_then(Value::as_f64);
    let monthly_total =
        command_code_monthly_total_value(monthly_used, monthly_remaining, plan_id.as_deref());
    let monthly_reset = subscription
        .as_ref()
        .and_then(|value| value.pointer("/data/currentPeriodEnd"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let model_quotas = match plan_id.as_deref() {
        Some(plan_id) => match command_code_plan_models(state, api_key, plan_id).await {
            Ok(models) => models,
            Err(error) => {
                tracing::warn!(plan_id, %error, "failed to load Command Code model credits");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
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
        let used = monthly_used.or_else(|| monthly_total.map(|total| (total - remaining).max(0.0)));
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
        source_url: plan_id
            .as_deref()
            .and_then(command_code_plan_url)
            .map(ToOwned::to_owned)
            .or_else(|| Some("https://commandcode.ai/".to_string())),
        items,
        model_quotas,
        details,
        prices: Vec::new(),
        fetched_at: Utc::now().to_rfc3339(),
    })
}

fn command_code_plan_url(plan_id: &str) -> Option<&'static str> {
    match plan_id {
        "individual-go" | "individual-go-v1" => Some("https://commandcode.ai/docs/plans/go"),
        "individual-goat" => Some("https://commandcode.ai/docs/plans/goat"),
        "individual-pro" | "individual-pro-v1" => Some("https://commandcode.ai/docs/plans/pro"),
        _ => None,
    }
}

pub(crate) fn command_code_monthly_total_value(
    used: Option<f64>,
    remaining: Option<f64>,
    plan_id: Option<&str>,
) -> Option<f64> {
    let derived = used
        .zip(remaining)
        .map(|(used, remaining)| ((used + remaining) * 100.0).round() / 100.0);
    match (derived, plan_id.and_then(command_code_monthly_total)) {
        // The two ledger reads are separate HTTP requests, so a request can
        // settle between them. Snap a near match to the published plan total
        // without hiding a real plan or promotion change larger than a quarter.
        (Some(derived), Some(expected)) if (derived - expected).abs() <= 0.25 => Some(expected),
        (Some(derived), _) => Some(derived),
        (None, expected) => expected,
    }
}

async fn command_code_plan_models(
    state: &AppState,
    api_key: &str,
    plan_id: &str,
) -> AppResult<Vec<ProviderQuotaModel>> {
    {
        let cache = COMMAND_CODE_PLAN_CACHE.lock().await;
        if let Some(entry) = cache.get(plan_id)
            && entry.fetched_at.elapsed() < COMMAND_CODE_PLAN_CACHE_TTL
        {
            return Ok(entry.models.clone());
        }
    }

    let Some(url) = command_code_plan_url(plan_id) else {
        return Ok(Vec::new());
    };
    let html = fetch_quota_text(state, url).await?;
    let mut models = parse_command_code_plan_credits(&html);
    if models.is_empty() {
        return Err(AppError::Upstream(
            "Command Code plan page did not contain a model credit table".to_string(),
        ));
    }
    if let Ok(catalog) = fetch_quota_json(
        state,
        "https://api.commandcode.ai/provider/v1/models",
        api_key,
        &[("User-Agent", "cli"), ("x-cli-environment", "cli")],
    )
    .await
    {
        apply_command_code_model_ids(&mut models, &catalog);
    }

    let mut cache = COMMAND_CODE_PLAN_CACHE.lock().await;
    cache.insert(
        plan_id.to_string(),
        CommandCodePlanCacheEntry {
            fetched_at: std::time::Instant::now(),
            models: models.clone(),
        },
    );
    Ok(models)
}

async fn fetch_quota_text(state: &AppState, url: &str) -> AppResult<String> {
    let response = state
        .client
        .get(url)
        .header(reqwest::header::ACCEPT, "text/html")
        .header(reqwest::header::USER_AGENT, "openllm-quota")
        .send()
        .await
        .map_err(|error| AppError::Upstream(format!("quota page request failed: {error}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| AppError::Upstream(format!("quota page response failed: {error}")))?;
    if !status.is_success() {
        let summary = body.chars().take(300).collect::<String>();
        return Err(AppError::Upstream(format!(
            "quota page returned {status}: {summary}"
        )));
    }
    Ok(body)
}

pub(crate) fn parse_command_code_plan_credits(html: &str) -> Vec<ProviderQuotaModel> {
    for table in html_elements(html, "table") {
        let header = table.split("</thead>").next().unwrap_or(table).to_string();
        let header_text = html_text(&header);
        if !header_text.contains("Model") || !header_text.contains("Monthly credits") {
            continue;
        }
        let body = table
            .split("<tbody")
            .nth(1)
            .unwrap_or(table)
            .split("</tbody>")
            .next()
            .unwrap_or(table);
        let mut rows = Vec::new();
        for row in html_elements(body, "tr") {
            let cells = html_elements(row, "td");
            if cells.len() < 2 {
                continue;
            }
            let model_name = html_text(cells[0]);
            if model_name.is_empty() {
                continue;
            }
            let (monthly_credits, promo_credits, note) =
                parse_command_code_credit_cell(&html_text(cells.last().unwrap_or(&cells[0])));
            rows.push(ProviderQuotaModel {
                model_name,
                model_id: None,
                monthly_credits,
                promo_credits,
                note,
            });
        }
        if !rows.is_empty() {
            return rows;
        }
    }
    Vec::new()
}

pub(crate) fn apply_command_code_model_ids(models: &mut [ProviderQuotaModel], catalog: &Value) {
    let mut names = HashMap::new();
    if let Some(items) = catalog.get("data").and_then(Value::as_array) {
        for item in items {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            names.insert(name.trim().to_ascii_lowercase(), id.to_string());
        }
    }
    for model in models {
        model.model_id = names
            .get(&model.model_name.trim().to_ascii_lowercase())
            .cloned();
    }
}

fn parse_command_code_credit_cell(text: &str) -> (Option<f64>, Option<f64>, Option<String>) {
    let chars = text.char_indices().collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut spans = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let (position, ch) = chars[index];
        if ch != '$' {
            index += 1;
            continue;
        }
        let number_start = position + 1;
        let mut next = index + 1;
        while next < chars.len() {
            let (_, candidate) = chars[next];
            if candidate.is_ascii_digit() || candidate == ',' || candidate == '.' {
                next += 1;
            } else {
                break;
            }
        }
        let number_end = if next < chars.len() {
            chars[next].0
        } else {
            text.len()
        };
        if let Ok(value) = text[number_start..number_end]
            .replace(',', "")
            .parse::<f64>()
        {
            values.push(value);
            spans.push((position, number_end));
        }
        index = next;
    }

    let note = spans
        .get(if values.len() > 1 { 1 } else { 0 })
        .map(|(_, end)| {
            text[*end..]
                .trim_matches(|ch: char| ch.is_whitespace() || ch == '-' || ch == ':')
                .to_string()
        })
        .filter(|value| !value.is_empty());
    (values.first().copied(), values.get(1).copied(), note)
}

fn html_elements<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = html;
    let mut elements = Vec::new();
    while let Some(start) = rest.find(&open) {
        let candidate = &rest[start..];
        let Some(end) = candidate.find(&close) else {
            break;
        };
        elements.push(&candidate[..end + close.len()]);
        rest = &candidate[end + close.len()..];
    }
    elements
}

fn html_text(fragment: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    let text = text
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
        model_quotas: Vec::new(),
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
        model_quotas: Vec::new(),
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
        "individual-go" | "individual-go-v1" => "Go".to_string(),
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
        "individual-go" | "individual-go-v1" => Some(10.0),
        "individual-goat" => Some(70.0),
        "individual-pro" => Some(30.0),
        "individual-pro-v1" => Some(80.0),
        "individual-provider" => Some(15.0),
        "individual-max" => Some(150.0),
        "individual-ultra" => Some(300.0),
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
