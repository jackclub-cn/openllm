use super::*;

/// Hard per-request cost ceiling supplied by the caller.
pub(in crate::proxy) const BUDGET_USD_HEADER: &str = "x-openllm-budget-usd";

/// Per-request routing controls. Each is optional; when absent the route's own
/// configuration decides.
pub(in crate::proxy) const STRATEGY_HEADER: &str = "x-openllm-strategy";
pub(in crate::proxy) const PROVIDER_HEADER: &str = "x-openllm-provider";
pub(in crate::proxy) const EXCLUDE_PROVIDERS_HEADER: &str = "x-openllm-exclude-providers";

/// The routing controls a caller supplied for one request.
#[derive(Debug, Default, Clone)]
pub(in crate::proxy) struct RequestRoutingOverrides {
    /// Ordered strategy name that replaced the route's configured strategy.
    pub strategy: Option<String>,
    /// Provider selector the caller pinned the request to, as supplied.
    pub provider: Option<String>,
    /// Providers the caller excluded, as supplied.
    pub excluded_providers: Vec<String>,
}

impl RequestRoutingOverrides {
    pub fn is_empty(&self) -> bool {
        self.strategy.is_none() && self.provider.is_none() && self.excluded_providers.is_empty()
    }
}

/// Builds request routing overrides from an API key's stored policy.
///
/// A malformed or absent policy yields no overrides so a bad row can never
/// block traffic; write-time validation is what keeps stored values clean.
pub(in crate::proxy) fn routing_overrides_from_key(
    api_key: Option<&ApiKeyRecord>,
) -> RequestRoutingOverrides {
    let policy = crate::models::parse_routing_policy(
        api_key.and_then(|api_key| api_key.routing_policy.as_deref()),
    );
    RequestRoutingOverrides {
        strategy: policy.strategy,
        provider: policy.provider,
        excluded_providers: policy.exclude_providers,
    }
}

/// Layers per-request overrides on top of a key's policy. Each field is
/// independent: a request may override just the strategy and keep the key's
/// provider pin.
pub(in crate::proxy) fn merge_routing_overrides(
    policy: RequestRoutingOverrides,
    request: RequestRoutingOverrides,
) -> RequestRoutingOverrides {
    RequestRoutingOverrides {
        strategy: request.strategy.or(policy.strategy),
        provider: request.provider.or(policy.provider),
        excluded_providers: if request.excluded_providers.is_empty() {
            policy.excluded_providers
        } else {
            request.excluded_providers
        },
    }
}

pub(super) fn header_text(headers: &HeaderMap, name: &str) -> AppResult<Option<String>> {
    let Some(value) = headers.get(name) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::BadRequest(format!("{name} must be valid ASCII")))?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

/// Parses the optional per-request routing headers.
///
/// A malformed value is rejected rather than ignored: silently dropping a
/// caller's routing instruction could send traffic to a provider they meant to
/// avoid.
pub(in crate::proxy) fn request_routing_overrides(
    headers: &HeaderMap,
) -> AppResult<RequestRoutingOverrides> {
    let strategy = header_text(headers, STRATEGY_HEADER)?
        .map(|value| {
            RouteStrategy::from_str(&value)
                .map(|strategy| strategy.as_str().to_string())
                .map_err(AppError::BadRequest)
        })
        .transpose()?;
    let provider = header_text(headers, PROVIDER_HEADER)?;
    let excluded_providers = header_text(headers, EXCLUDE_PROVIDERS_HEADER)?
        .map(|value| {
            value
                .split(',')
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(RequestRoutingOverrides {
        strategy,
        provider,
        excluded_providers,
    })
}

/// Whether a selector names this target's provider, by numeric id or by name
/// (case-insensitive).
pub(super) fn selector_matches(selector: &str, target: &RouteTarget) -> bool {
    if let Ok(id) = selector.parse::<i64>() {
        return id == target.provider_id;
    }
    selector.eq_ignore_ascii_case(target.provider_name.trim())
}

/// Applies per-request routing controls to a resolved route.
///
/// Returns how many targets the filters removed. Pinning and excluding can
/// leave nothing behind; that is a client error, not a routing fallback.
pub(in crate::proxy) fn apply_routing_overrides(
    resolved: &mut ResolvedRoute,
    model: &str,
    overrides: &RequestRoutingOverrides,
) -> AppResult<usize> {
    if overrides.is_empty() {
        return Ok(0);
    }
    if let Some(strategy) = &overrides.strategy {
        resolved.strategy = strategy.clone();
    }
    if let Some(provider) = &overrides.provider {
        resolved
            .targets
            .retain(|target| selector_matches(provider, target));
        if resolved.targets.is_empty() {
            return Err(AppError::BadRequest(format!(
                "{PROVIDER_HEADER} '{provider}' does not match any target of model '{}'",
                model
            )));
        }
    }
    if !overrides.excluded_providers.is_empty() {
        let before = resolved.targets.len();
        resolved.targets.retain(|target| {
            !overrides
                .excluded_providers
                .iter()
                .any(|selector| selector_matches(selector, target))
        });
        if resolved.targets.is_empty() {
            return Err(AppError::BadRequest(format!(
                "{EXCLUDE_PROVIDERS_HEADER} removed every target of model '{}'",
                model
            )));
        }
        return Ok(before - resolved.targets.len());
    }
    Ok(0)
}

/// Echoes the routing controls that took effect so a caller can attribute the
/// response to the provider they asked for.
pub(in crate::proxy) fn apply_override_headers(
    response: &mut Response,
    overrides: &RequestRoutingOverrides,
) {
    if overrides.is_empty() {
        return;
    }
    let mut insert = |name: &'static str, value: String| {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    };
    if let Some(strategy) = &overrides.strategy {
        insert(STRATEGY_HEADER, strategy.clone());
    }
    if let Some(provider) = &overrides.provider {
        insert(PROVIDER_HEADER, provider.clone());
    }
    if !overrides.excluded_providers.is_empty() {
        insert(
            EXCLUDE_PROVIDERS_HEADER,
            overrides.excluded_providers.join(","),
        );
    }
}

/// Parses the optional per-request budget header.
///
/// The value is a USD ceiling for one request. A malformed or non-positive
/// value is rejected rather than ignored: silently dropping it would let a
/// caller believe a cost guarantee was in force when it was not.
pub(in crate::proxy) fn budget_usd_from_headers(headers: &HeaderMap) -> AppResult<Option<f64>> {
    let Some(raw) = headers.get(BUDGET_USD_HEADER) else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .map_err(|_| {
            AppError::BadRequest(format!("{BUDGET_USD_HEADER} must be a positive number"))
        })?
        .trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let budget = raw.parse::<f64>().map_err(|_| {
        AppError::BadRequest(format!(
            "{BUDGET_USD_HEADER} must be a positive number, got '{raw}'"
        ))
    })?;
    if !budget.is_finite() || budget <= 0.0 {
        return Err(AppError::BadRequest(format!(
            "{BUDGET_USD_HEADER} must be a positive number, got '{raw}'"
        )));
    }
    Ok(Some(budget))
}

/// Estimates what one request would cost on a target, in USD.
///
/// Returns `None` when the target has no usable price, because an unknown cost
/// cannot be proven to fit under a hard ceiling.
pub(in crate::proxy) fn estimate_target_cost_usd(
    target: &RouteTarget,
    prompt_tokens: i64,
    output_tokens: i64,
) -> Option<f64> {
    let (input_price, output_price) = target_cost_prices(target);
    if input_price.is_none() && output_price.is_none() {
        return None;
    }
    let prompt_tokens = prompt_tokens.max(0) as f64;
    let output_tokens = output_tokens.max(0) as f64;
    // Cache reads are billed at or below the fresh-input price, so charging
    // every prompt token at the input rate is the conservative estimate.
    let cost = prompt_tokens * input_price.unwrap_or(0.0) / 1_000_000.0
        + output_tokens * output_price.unwrap_or(0.0) / 1_000_000.0;
    Some(cost)
}

/// Drops every target whose estimated cost exceeds `budget_usd`.
///
/// Returns how many targets were removed. An error is returned when nothing
/// survives, including when the surviving set is empty only because no target
/// publishes pricing.
pub(in crate::proxy) fn apply_budget_filter(
    targets: &mut Vec<RouteTarget>,
    budget_usd: f64,
    prompt_tokens: i64,
    output_tokens: i64,
) -> AppResult<usize> {
    let before = targets.len();
    let mut cheapest: Option<f64> = None;
    let mut unpriced = 0usize;
    targets.retain(
        |target| match estimate_target_cost_usd(target, prompt_tokens, output_tokens) {
            Some(cost) => {
                cheapest = Some(cheapest.map_or(cost, |current| current.min(cost)));
                cost <= budget_usd
            }
            None => {
                unpriced += 1;
                false
            }
        },
    );
    let removed = before - targets.len();
    if !targets.is_empty() {
        return Ok(removed);
    }
    let detail = match cheapest {
        Some(cost) => format!("the cheapest known estimate is ${cost:.6}"),
        None if unpriced > 0 => {
            "no candidate publishes pricing, so the ceiling cannot be verified".to_string()
        }
        None => "no candidate is available".to_string(),
    };
    Err(AppError::BadRequest(format!(
        "no route target fits the ${budget_usd:.6} budget for this request: {detail}"
    )))
}
