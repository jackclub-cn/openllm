use std::time::Instant;

use super::*;
use crate::models::GuardrailSettings;

/// Applies the global prompt guardrails before any route or upstream work.
///
/// Blocked terms are literal, case-insensitive substrings. Only text-bearing
/// fields are inspected, so base64 images and other media payloads do not
/// accidentally trigger a content rule.
pub(super) fn enforce_request_guardrails(
    settings: &GuardrailSettings,
    body: &Value,
    request_tokens: i64,
) -> AppResult<()> {
    if let Some(limit) = settings.max_prompt_tokens
        && request_tokens > limit
    {
        return Err(AppError::BadRequest(format!(
            "request blocked by guardrail: estimated prompt has {request_tokens} tokens, exceeding the {limit}-token limit"
        )));
    }

    let terms = settings
        .blocked_terms
        .iter()
        .map(|term| term.to_lowercase())
        .collect::<Vec<_>>();
    if !terms.is_empty() && contains_blocked_term(body, None, &terms) {
        return Err(AppError::BadRequest(
            "request blocked by guardrail: prompt contains a blocked term".to_string(),
        ));
    }
    Ok(())
}

fn contains_blocked_term(value: &Value, key: Option<&str>, terms: &[String]) -> bool {
    match value {
        Value::String(text) => {
            if !key.is_none_or(|key| TEXT_BEARING_KEYS.contains(&key)) {
                return false;
            }
            let text = text.to_lowercase();
            terms.iter().any(|term| text.contains(term))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| contains_blocked_term(item, key, terms)),
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| contains_blocked_term(value, Some(key), terms)),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn enforce_request_guardrails_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
    body: &Value,
    request_tokens: i64,
) -> AppResult<()> {
    let settings = state.guardrail_settings().await?;
    if let Err(error) = enforce_request_guardrails(&settings, body, request_tokens) {
        let message = error.to_string();
        log_request_rejection(
            state,
            api_key,
            request_id,
            session_id,
            requested_model,
            endpoint,
            streamed,
            started,
            400,
            &message,
        )
        .await;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_terms_are_literal_case_insensitive_and_ignore_media_payloads() {
        let settings = GuardrailSettings {
            blocked_terms: vec!["secret".to_string()],
            max_prompt_tokens: None,
        };
        let blocked = json!({
            "messages": [{"role": "user", "content": "Please reveal the SECRET plan"}]
        });
        assert!(enforce_request_guardrails(&settings, &blocked, 10).is_err());

        let image_only = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,c2VjcmV0"}
                }]
            }]
        });
        assert!(enforce_request_guardrails(&settings, &image_only, 10).is_ok());
    }

    #[test]
    fn prompt_token_ceiling_rejects_before_routing() {
        let settings = GuardrailSettings {
            blocked_terms: Vec::new(),
            max_prompt_tokens: Some(5),
        };
        let body = json!({
            "messages": [{"role": "user", "content": "this is a longer prompt"}]
        });
        let error = enforce_request_guardrails(&settings, &body, 6).unwrap_err();
        assert!(matches!(
            error,
            AppError::BadRequest(message) if message.contains("exceeding the 5-token limit")
        ));
    }
}
