use super::*;

/// Downward compatibility for multi-target routes.
///
/// A route may fan out to models with different ceilings. Clamping the
/// requested output length to the strictest common `output_limit` keeps the
/// request valid for *every* target, so whichever one the strategy picks can
/// serve it instead of failing with a "max_tokens too large" error.
///
/// Returns the clamped value when a reduction happened, so the caller can
/// surface it in the response receipt.
pub(crate) fn clamp_output_request(
    body: &mut Value,
    barrel: Option<&BarrelEnvelope>,
) -> Option<i64> {
    let limit = barrel
        .and_then(|barrel| barrel.capabilities.as_ref())
        .and_then(|capabilities| capabilities.output_limit)?;
    let mut clamped = None;
    for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        if let Some(requested) = body.get(key).and_then(Value::as_i64)
            && requested > limit
        {
            body[key] = json!(limit);
            clamped = Some(limit);
        }
    }
    clamped
}

pub(crate) fn requested_output_tokens_of(body: &Value) -> Option<i64> {
    ["max_tokens", "max_completion_tokens", "max_output_tokens"]
        .into_iter()
        .find_map(|key| body.get(key).and_then(Value::as_i64))
}

/// Builds the capability receipt returned alongside a completion.
///
/// `requested_output_tokens` and `clamped_output_tokens` make the barrel
/// behaviour visible: a client that asked for more than the route supports can
/// see that the gateway reduced the request rather than silently ignoring it.
pub(crate) fn capability_receipt(
    barrel: Option<&BarrelEnvelope>,
    requested_output_tokens: Option<i64>,
    clamped_output_tokens: Option<i64>,
) -> Option<Value> {
    let capabilities = barrel.and_then(|barrel| barrel.capabilities.as_ref())?;
    let mut receipt = serde_json::to_value(capabilities).ok()?;
    let object = receipt.as_object_mut()?;
    let incomplete = barrel.is_some_and(|barrel| barrel.incomplete);
    object.insert("limits_verified".to_string(), json!(!incomplete));
    if let Some(barrel) = barrel {
        object.insert("target_count".to_string(), json!(barrel.target_count));
    }
    if let Some(requested) = requested_output_tokens {
        object.insert("requested_output_tokens".to_string(), json!(requested));
    }
    if let Some(clamped) = clamped_output_tokens {
        object.insert("clamped_output_tokens".to_string(), json!(clamped));
    }
    Some(receipt)
}

/// Injects the receipt into a JSON response body. Bodies that are not JSON
/// objects are returned unchanged so upstream payloads are never corrupted.
pub(crate) fn inject_capability_receipt(bytes: &[u8], receipt: &Option<Value>) -> Option<Vec<u8>> {
    let receipt = receipt.as_ref()?;
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    value
        .as_object_mut()?
        .insert("capabilities".to_string(), receipt.clone());
    serde_json::to_vec(&value).ok()
}

/// Limits mirrored onto response headers so streaming clients, which never
/// receive a single JSON body, can still read the effective ceiling.
pub(crate) fn apply_capability_headers(response: &mut Response, receipt: &Option<Value>) {
    let Some(receipt) = receipt.as_ref() else {
        return;
    };
    if let Some(output) = receipt.get("output_limit").and_then(Value::as_i64)
        && let Ok(value) = HeaderValue::from_str(&output.to_string())
    {
        response
            .headers_mut()
            .insert("x-openllm-max-output-tokens", value);
    }
    if let Some(context) = receipt.get("context_limit").and_then(Value::as_i64)
        && let Ok(value) = HeaderValue::from_str(&context.to_string())
    {
        response
            .headers_mut()
            .insert("x-openllm-max-context-tokens", value);
    }
    let input = match (
        receipt.get("input_limit").and_then(Value::as_i64),
        receipt.get("context_limit").and_then(Value::as_i64),
    ) {
        (Some(input), Some(context)) => Some(input.min(context)),
        (Some(input), None) => Some(input),
        (None, Some(context)) => Some(context),
        (None, None) => None,
    };
    if let Some(input) = input
        && let Ok(value) = HeaderValue::from_str(&input.to_string())
    {
        response
            .headers_mut()
            .insert("x-openllm-max-input-tokens", value);
    }
}
