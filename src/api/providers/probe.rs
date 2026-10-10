use super::*;

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
pub(crate) struct ProviderProbeModel {
    name: String,
    supported_endpoints: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ProviderProbeKey {
    pub(crate) id: Option<i64>,
    pub(crate) name: String,
    pub(crate) api_key_suffix: String,
    pub(crate) secret: Option<String>,
}

pub(crate) async fn test_provider_inner(
    state: &AppState,
    id: i64,
) -> AppResult<ProviderTestResult> {
    {
        let mut running = state.provider_health_check.lock().await;
        if !running.insert(id) {
            return Err(AppError::Conflict(
                "provider health check is already in progress".to_string(),
            ));
        }
    }
    let result = test_provider_inner_unlocked(state, id).await;
    state.provider_health_check.lock().await.remove(&id);
    result
}

pub(crate) async fn test_provider_inner_unlocked(
    state: &AppState,
    id: i64,
) -> AppResult<ProviderTestResult> {
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

pub(crate) async fn test_provider_keys_inner(
    state: &AppState,
    id: i64,
) -> AppResult<ProviderKeyTestResult> {
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

pub(crate) async fn resolve_provider_probe_model(
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

pub(crate) fn build_provider_probe_request(
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

pub(crate) async fn probe_tool_search_support(
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

pub(crate) async fn persist_tool_search_support(
    state: &AppState,
    provider_id: i64,
    supported: bool,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE providers \
         SET tool_search_supported = ?, \
             tool_search_checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
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

pub(crate) async fn persist_provider_test(
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
    if result.ok {
        // An authoritative success closes the runtime circuit too: clear the
        // cooldown, reset the failure streak so escalation restarts cleanly, and
        // release any half-open probe slot. Without this the provider would stay
        // deprioritised (or short-circuited) until the cooldown expired even
        // though this probe just proved it answers again.
        state.provider_cooldown.lock().await.remove(&provider_id);
        state.provider_failure_streak.lock().await.remove(&provider_id);
        state.provider_probe.lock().await.remove(&provider_id);
    }
    Ok(())
}

pub(crate) async fn persist_provider_key_result(
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
pub(crate) async fn probe_provider(
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
