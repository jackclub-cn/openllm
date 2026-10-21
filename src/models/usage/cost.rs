#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Prompt tokens served from the provider's cache, billed at a discount.
    pub cache_read_tokens: i64,
    /// Prompt tokens written into the cache for later reuse.
    pub cache_write_tokens: i64,
}

impl Usage {
    /// Convenience constructor for the common case with no cache traffic.
    pub fn new(prompt_tokens: i64, completion_tokens: i64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }

    pub fn normalized(mut self) -> Self {
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens + self.completion_tokens;
        }
        // Defensive: a provider reporting a negative or absurd cache count must
        // not corrupt aggregates.
        self.cache_read_tokens = self.cache_read_tokens.max(0);
        self.cache_write_tokens = self.cache_write_tokens.max(0);
        self
    }

    pub fn has_tokens(&self) -> bool {
        self.prompt_tokens > 0 || self.completion_tokens > 0
    }
}

/// Merges manual price overrides into a models.dev cost object.
///
/// Overrides are applied to both the base object and every tier, so a manual
/// correction wins regardless of which context tier the request selects.
pub fn effective_cost_value(
    cost: Option<&serde_json::Value>,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
) -> Option<serde_json::Value> {
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return cost.cloned().filter(|value| value.is_object());
    }
    let mut object = cost
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in [
        ("input", input),
        ("output", output),
        ("cache_read", cache_read),
        ("cache_write", cache_write),
    ] {
        let Some(value) = value else {
            continue;
        };
        let value = serde_json::json!(value);
        object.insert(key.to_string(), value.clone());
        if let Some(tiers) = object
            .get_mut("tiers")
            .and_then(serde_json::Value::as_array_mut)
        {
            for tier in tiers {
                if let Some(tier) = tier.as_object_mut() {
                    tier.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    Some(serde_json::Value::Object(object))
}

/// Reads a base, non-tiered price for display in the management console.
pub fn cost_base_price(cost: &serde_json::Value, key: &str) -> Option<f64> {
    cost_price(cost, cost, key)
}

/// Estimates a request's cost in micro-US dollars from models.dev pricing.
///
/// Prices are expressed per million tokens, so multiplying a token count by
/// the price directly yields micro-dollars. Cache traffic is separated from
/// fresh input because providers bill it at different rates. Tiered context
/// pricing uses the largest published tier that the prompt has crossed.
pub fn estimate_cost_micros(cost: Option<&serde_json::Value>, usage: Usage) -> Option<i64> {
    let cost = cost?;
    let usage = usage.normalized();
    let selected = select_cost_tier(cost, usage.prompt_tokens);
    let input = cost_price(selected, cost, "input");
    let output = cost_price(selected, cost, "output");
    let cache_read = cost_price(selected, cost, "cache_read").or(input);
    let cache_write = cost_price(selected, cost, "cache_write").or(input);
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return None;
    }

    let prompt = usage.prompt_tokens.max(0);
    let read = usage.cache_read_tokens.max(0).min(prompt);
    let write = usage.cache_write_tokens.max(0).min(prompt - read);
    let fresh = prompt - read - write;
    let completion = usage.completion_tokens.max(0);

    let amount = (fresh as f64) * input.unwrap_or(0.0)
        + (read as f64) * cache_read.unwrap_or(0.0)
        + (write as f64) * cache_write.unwrap_or(0.0)
        + (completion as f64) * output.unwrap_or(0.0);
    Some(amount.round().max(0.0).min(i64::MAX as f64) as i64)
}

pub(crate) fn select_cost_tier(cost: &serde_json::Value, prompt_tokens: i64) -> &serde_json::Value {
    let Some(tiers) = cost.get("tiers").and_then(serde_json::Value::as_array) else {
        return cost;
    };
    let mut selected = cost;
    let mut selected_size = -1;
    for tier in tiers {
        let Some(size) = tier
            .pointer("/tier/size")
            .and_then(serde_json::Value::as_i64)
        else {
            continue;
        };
        if tier
            .pointer("/tier/type")
            .and_then(serde_json::Value::as_str)
            != Some("context")
            || size > prompt_tokens
            || size < selected_size
        {
            continue;
        }
        selected = tier;
        selected_size = size;
    }
    selected
}

pub(crate) fn cost_price(
    selected: &serde_json::Value,
    base: &serde_json::Value,
    key: &str,
) -> Option<f64> {
    selected
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .or_else(|| base.get(key).and_then(serde_json::Value::as_f64))
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Derives output throughput from a completed request.
///
/// Streamed requests measure the generation phase alone (total latency minus
/// the first-token wait), since including queue time would understate a fast
/// model behind a slow first token. Requests without a first-token timestamp
/// fall back to the full latency. Returns `None` rather than infinity or zero
/// when there is nothing meaningful to report, so the UI can show a dash.
pub(crate) fn output_tps_of(
    completion_tokens: i64,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    streamed: bool,
) -> Option<f64> {
    if completion_tokens <= 0 {
        return None;
    }
    let elapsed_ms = if streamed {
        match first_token_ms {
            // Guard against a first-token stamp that exceeds the total
            // (possible with clock granularity): fall back to full latency.
            Some(first) if latency_ms > first => latency_ms - first,
            _ => latency_ms,
        }
    } else {
        latency_ms
    };
    if elapsed_ms <= 0 {
        return None;
    }
    let tps = completion_tokens as f64 / (elapsed_ms as f64 / 1000.0);
    tps.is_finite().then_some(tps)
}
