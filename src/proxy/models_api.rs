use super::*;

pub(crate) async fn public_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    match public_models_inner(&state, &headers, &uri).await {
        Ok(response) => response,
        // Match the error envelope to the protocol the caller speaks, so both
        // client families can parse failures with their own error handling.
        Err(error) if wants_anthropic_models(&headers) => anthropic_error_response(error),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn public_models_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
) -> AppResult<Response> {
    let api_key = authenticate_gateway(state, headers).await?;
    let model_patterns = api_key_model_patterns(api_key.as_ref())?;
    if wants_anthropic_models(headers) {
        return anthropic_models(state, uri, model_patterns.as_deref()).await;
    }
    let data = openai_public_models(state, model_patterns.as_deref()).await?;
    Ok(Json(ModelList {
        object: "list",
        data,
    })
    .into_response())
}

pub(crate) async fn openai_public_models(
    state: &AppState,
    model_patterns: Option<&[String]>,
) -> AppResult<Vec<PublicModel>> {
    let routes = filter_allowed_models(
        model_patterns,
        crate::registry::route_models(&state.pool).await?,
        |model| model.id.as_str(),
    );
    let synced = filter_allowed_models(
        model_patterns,
        crate::registry::synced_models(&state.pool).await?,
        |model| model.id.as_str(),
    );
    let auto_summary = crate::registry::auto_model_summary(&state.pool).await?;
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    // A route pattern can coincide with a synced model id (for example an
    // exact-match route for a prefixed model). Explicit routes win because they
    // carry the barrel intersection across their targets, while deduping keeps
    // clients from seeing the same id twice.
    let mut by_id = std::collections::BTreeMap::new();
    for model in routes {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created,
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
                // The route's own name is the friendly label, matching the
                // Anthropic shape so both agree.
                display_name: model.display_name,
                supported_endpoints: model.supported_endpoints,
            }
            .with_flat_limits(),
        );
    }
    if let Some(summary) = auto_summary {
        for (id, display_name, _) in crate::registry::AUTO_MODELS {
            if !model_matches_patterns(model_patterns, id) {
                continue;
            }
            by_id.entry(id.to_string()).or_insert(
                PublicModel {
                    id: id.to_string(),
                    object: "model",
                    created,
                    owned_by: "openllm",
                    provider: None,
                    upstream_model: None,
                    capabilities: None,
                    target_count: Some(summary.target_count),
                    limits_verified: Some(false),
                    context_length: None,
                    max_input_tokens: None,
                    max_output_tokens: None,
                    max_completion_tokens: None,
                    display_name: Some(display_name.to_string()),
                    supported_endpoints: summary.supported_endpoints.clone(),
                }
                .with_flat_limits(),
            );
        }
    }
    for model in synced {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created,
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
    Ok(by_id.into_values().collect())
}

/// Retrieves one public model using the caller's protocol.
pub(crate) async fn public_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> Response {
    match public_model_inner(&state, &headers, &model_id).await {
        Ok(response) => response,
        Err(error) if wants_anthropic_models(&headers) => anthropic_error_response(error),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn public_model_inner(
    state: &AppState,
    headers: &HeaderMap,
    model_id: &str,
) -> AppResult<Response> {
    let api_key = authenticate_gateway(state, headers).await?;
    let model_patterns = api_key_model_patterns(api_key.as_ref())?;
    if wants_anthropic_models(headers) {
        let model = anthropic_model_values(state, model_patterns.as_deref())
            .await?
            .into_iter()
            .find(|model| model.get("id").and_then(Value::as_str) == Some(model_id))
            .ok_or_else(|| AppError::NotFound(format!("model '{model_id}' not found")))?;
        return Ok(Json(model).into_response());
    }

    let model = openai_public_models(state, model_patterns.as_deref())
        .await?
        .into_iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| AppError::NotFound(format!("model '{model_id}' not found")))?;
    Ok(Json(model).into_response())
}

/// True when the caller speaks the Anthropic protocol.
///
/// `/v1/models` is shared by both protocols but the payload shapes differ, and
/// the `anthropic-version` header is the reliable discriminator: every
/// Anthropic client sends it, no OpenAI client does. Everything else keeps the
/// OpenAI shape so existing clients are unaffected.
pub(crate) fn wants_anthropic_models(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version")
}

/// Renders the model registry in Anthropic's `/v1/models` shape.
///
/// Anthropic returns `data` entries of `{type, id, display_name, created_at}`
/// plus cursor fields, rather than OpenAI's `{id, object, created, owned_by}`.
pub(crate) async fn anthropic_models(
    state: &AppState,
    uri: &Uri,
    model_patterns: Option<&[String]>,
) -> AppResult<Response> {
    let models = anthropic_model_values(state, model_patterns).await?;
    let page = ModelPageQuery::parse(uri);
    let (models, has_more) = paginate_models(models, &page);
    let first = models.first().and_then(|m| m.get("id")).cloned();
    let last = models.last().and_then(|m| m.get("id")).cloned();
    Ok(Json(json!({
        "data": models,
        "has_more": has_more,
        "first_id": first,
        "last_id": last,
    }))
    .into_response())
}

pub(crate) async fn anthropic_model_values(
    state: &AppState,
    model_patterns: Option<&[String]>,
) -> AppResult<Vec<Value>> {
    let routes = filter_allowed_models(
        model_patterns,
        crate::registry::route_models(&state.pool).await?,
        |model| model.id.as_str(),
    );
    let synced = filter_allowed_models(
        model_patterns,
        crate::registry::synced_models(&state.pool).await?,
        |model| model.id.as_str(),
    );
    let auto_summary = crate::registry::auto_model_summary(&state.pool).await?;
    let mut models = Vec::new();
    // Routes first, matching the precedence used by the OpenAI-shaped list.
    for model in routes {
        models.push(json!({
            "type": "model",
            "id": model.id,
            // A route's own name is the sensible label; it was previously left
            // null, so alias entries showed no name in clients.
            "display_name": model.display_name,
            "created_at": model.created_at,
        }));
    }
    if auto_summary.is_some() {
        for (id, display_name, _) in crate::registry::AUTO_MODELS {
            if !model_matches_patterns(model_patterns, id) {
                continue;
            }
            if models
                .iter()
                .any(|model| model.get("id").and_then(Value::as_str) == Some(id))
            {
                continue;
            }
            models.push(json!({
                "type": "model",
                "id": id,
                "display_name": display_name,
                "created_at": serde_json::Value::Null,
            }));
        }
    }
    for model in synced {
        // Prefer the provider's own label, then the upstream id, and only then
        // the namespaced id — clients show this verbatim.
        let display_name = model
            .display_name
            .clone()
            .unwrap_or_else(|| model.upstream_model.clone());
        models.push(json!({
            "type": "model",
            "id": model.id,
            "display_name": display_name,
            "created_at": model.created_at,
        }));
    }
    Ok(models)
}

/// Applies Anthropic's cursor pagination to an ordered model list.
///
/// Returns the requested window plus whether more entries remain beyond it. An
/// unknown cursor yields an empty page rather than an error, matching how
/// Anthropic treats a cursor that is no longer valid.
pub(crate) fn paginate_models(models: Vec<Value>, page: &ModelPageQuery) -> (Vec<Value>, bool) {
    let id_of = |model: &Value| {
        model
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    let mut start = 0usize;
    if let Some(after) = &page.after_id {
        // Resume strictly after the cursor.
        start = match models.iter().position(|model| id_of(model) == *after) {
            Some(index) => index + 1,
            None => return (Vec::new(), false),
        };
    }

    let mut end = models.len();
    if let Some(before) = &page.before_id {
        // Walk backward from the cursor: the page ends just before it.
        end = match models.iter().position(|model| id_of(model) == *before) {
            Some(index) => index,
            None => return (Vec::new(), false),
        };
        // Keep the last `limit` entries before the cursor.
        start = end.saturating_sub(page.page_size());
    }

    if start >= end {
        return (Vec::new(), false);
    }
    let size = page.page_size();
    let window_end = (start + size).min(end);
    // `has_more` consistently means "entries exist after `last_id`", so a
    // client can always continue forward with `after_id=last_id` and terminate.
    let has_more = window_end < models.len();
    let window = models
        .into_iter()
        .skip(start)
        .take(window_end - start)
        .collect();
    (window, has_more)
}

// ===== Inbound Anthropic Messages API compatibility =====
//
// Anthropic-protocol clients (Claude Code, the Anthropic SDKs) POST to
// `/v1/messages`. We accept that shape, route it through the same
// resolve/barrel machinery as OpenAI traffic, and translate at the boundary:
// a OpenAI-compatible target gets a converted request and its answer is
// converted back, while a native Anthropic target is used verbatim.

pub(crate) const ANTHROPIC_MESSAGES: &str = "/v1/messages";

/// Pagination parameters accepted by Anthropic's model list endpoint.
///
/// Anthropic uses opaque cursor pagination rather than offsets: `after_id`
/// walks forward and `before_id` walks backward. Both are optional, and
/// `limit` defaults to 20 with a maximum of 1000.
#[derive(Debug, Default)]
pub(crate) struct ModelPageQuery {
    pub(in crate::proxy) limit: Option<usize>,
    pub(in crate::proxy) after_id: Option<String>,
    pub(in crate::proxy) before_id: Option<String>,
}

/// Anthropic's documented default and maximum page sizes.
pub(crate) const ANTHROPIC_DEFAULT_PAGE_SIZE: usize = 20;

pub(crate) const ANTHROPIC_MAX_PAGE_SIZE: usize = 1000;

impl ModelPageQuery {
    /// Parses the raw query string, ignoring malformed parameters rather than
    /// failing the request: an unrecognized `limit` should not take down the
    /// model list.
    pub(in crate::proxy) fn parse(uri: &Uri) -> Self {
        let mut query = Self::default();
        for (key, value) in uri
            .query()
            .unwrap_or_default()
            .split('&')
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                Some((key, value))
            })
        {
            let decoded = percent_decode(value);
            match key {
                "limit" => query.limit = decoded.trim().parse::<usize>().ok(),
                "after_id" if !decoded.trim().is_empty() => query.after_id = Some(decoded),
                "before_id" if !decoded.trim().is_empty() => query.before_id = Some(decoded),
                _ => {}
            }
        }
        query
    }

    /// Effective page size, clamped to Anthropic's bounds.
    pub(in crate::proxy) fn page_size(&self) -> usize {
        self.limit
            .unwrap_or(ANTHROPIC_DEFAULT_PAGE_SIZE)
            .clamp(1, ANTHROPIC_MAX_PAGE_SIZE)
    }
}

/// Minimal percent-decoding for cursor values.
///
/// Model ids routinely contain `/` (for example `cmd/deepseek/v4`), which
/// clients percent-encode; without decoding, the cursor would never match.
pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
