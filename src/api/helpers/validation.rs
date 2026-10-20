use super::*;

pub(in crate::api) fn validate_provider_input(input: &ProviderInput) -> AppResult<()> {
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

pub(in crate::api) fn validate_route_input(input: &RouteInput) -> AppResult<()> {
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
pub(in crate::api) fn validate_model_pattern(pattern: &str) -> AppResult<()> {
    let pattern = pattern.trim();
    globset::Glob::new(pattern).map(|_| ()).map_err(|error| {
        AppError::BadRequest(format!("invalid model pattern '{pattern}': {error}"))
    })
}

pub(in crate::api) fn validate_targets(targets: &[RouteTargetInput]) -> AppResult<()> {
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

pub(in crate::api) fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

pub(in crate::api) fn ollama_root(base_url: &str) -> &str {
    base_url
        .trim_end_matches('/')
        .strip_suffix("/v1")
        .unwrap_or_else(|| base_url.trim_end_matches('/'))
}

pub(in crate::api) fn normalize_model_prefix(value: &str) -> AppResult<String> {
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

pub(in crate::api) fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

pub(in crate::api) fn normalize_health_check_model(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(in crate::api) fn parse_provider_probe_endpoints(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default()
}

pub(in crate::api) fn provider_probe_supports(endpoints: &[String], expected: &str) -> bool {
    let expected = expected.trim_end_matches('/');
    endpoints.iter().any(|endpoint| {
        let endpoint = endpoint.trim_end_matches('/');
        !endpoint.is_empty() && (expected == endpoint || expected.ends_with(endpoint))
    })
}

pub(in crate::api) fn normalize_health_interval(value: Option<i64>) -> AppResult<Option<i64>> {
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "health check interval must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

/// Per-provider request timeout. `None`/`0` keeps the gateway-wide idle
/// timeout; anything else bounds one upstream call, capped at one hour.
///
/// For a buffered call the budget covers the whole request and body. For a
/// streamed call it bounds only the wait for response headers, because the
/// shared client's idle read timeout already guards the body and a total cap
/// would cut long generations.
pub(in crate::api) fn normalize_provider_timeout(value: Option<i64>) -> AppResult<Option<i64>> {
    const MAX_TIMEOUT_SECONDS: i64 = 3600;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "request timeout must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) if value > MAX_TIMEOUT_SECONDS => Err(AppError::BadRequest(format!(
            "request timeout must be at most {MAX_TIMEOUT_SECONDS} seconds"
        ))),
        Some(value) => Ok(Some(value)),
    }
}

/// Per-provider cooldown base after a retryable failure. `None`/`0` keeps the
/// derived schedule (30s for rate limits, 20s otherwise, then exponential).
pub(in crate::api) fn normalize_provider_cooldown(value: Option<i64>) -> AppResult<Option<i64>> {
    const MAX_COOLDOWN_SECONDS: i64 = 3600;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "cooldown must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) if value > MAX_COOLDOWN_SECONDS => Err(AppError::BadRequest(format!(
            "cooldown must be at most {MAX_COOLDOWN_SECONDS} seconds"
        ))),
        Some(value) => Ok(Some(value)),
    }
}

/// Per-provider upstream concurrency. `None`/`0` means unlimited.
pub(in crate::api) fn normalize_provider_concurrency(value: Option<i64>) -> AppResult<Option<i64>> {
    const MAX_CONCURRENCY: i64 = 1000;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "max concurrency must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) if value > MAX_CONCURRENCY => Err(AppError::BadRequest(format!(
            "max concurrency must be at most {MAX_CONCURRENCY}"
        ))),
        Some(value) => Ok(Some(value)),
    }
}

/// Maximum queue wait for a provider concurrency slot. `None` uses the
/// gateway default; `0` rejects immediately instead of queueing.
pub(in crate::api) fn normalize_provider_queue_timeout(
    value: Option<i64>,
) -> AppResult<Option<i64>> {
    const MAX_QUEUE_TIMEOUT_SECONDS: i64 = 300;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "queue timeout must be zero or a positive integer".to_string(),
        )),
        Some(value) if value > MAX_QUEUE_TIMEOUT_SECONDS => Err(AppError::BadRequest(format!(
            "queue timeout must be at most {MAX_QUEUE_TIMEOUT_SECONDS} seconds"
        ))),
        value => Ok(value),
    }
}

pub(in crate::api) fn normalize_provider_key_rpm(value: Option<i64>) -> AppResult<Option<i64>> {
    const MAX_REQUESTS_PER_MINUTE: i64 = 1_000_000;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "requests per minute must be zero or a positive integer".to_string(),
        )),
        Some(0) | None => Ok(None),
        Some(value) if value > MAX_REQUESTS_PER_MINUTE => Err(AppError::BadRequest(format!(
            "requests per minute must be at most {MAX_REQUESTS_PER_MINUTE}"
        ))),
        Some(value) => Ok(Some(value)),
    }
}

/// `None` leaves the provider-key queue unbounded; `0` disables queueing.
pub(in crate::api) fn normalize_provider_key_queue_limit(
    value: Option<i64>,
) -> AppResult<Option<i64>> {
    const MAX_QUEUE_LIMIT: i64 = 10_000;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "queue limit must be zero or a positive integer".to_string(),
        )),
        Some(value) if value > MAX_QUEUE_LIMIT => Err(AppError::BadRequest(format!(
            "queue limit must be at most {MAX_QUEUE_LIMIT}"
        ))),
        value => Ok(value),
    }
}

pub(in crate::api) fn normalize_provider_key_queue_wait(
    value: Option<i64>,
) -> AppResult<Option<i64>> {
    const MAX_QUEUE_WAIT_MS: i64 = 300_000;
    match value {
        Some(value) if value < 0 => Err(AppError::BadRequest(
            "queue wait must be zero or a positive integer".to_string(),
        )),
        Some(value) if value > MAX_QUEUE_WAIT_MS => Err(AppError::BadRequest(format!(
            "queue wait must be at most {MAX_QUEUE_WAIT_MS} milliseconds"
        ))),
        value => Ok(value),
    }
}

pub(in crate::api) fn map_sqlite_conflict(error: sqlx::Error) -> AppError {
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
