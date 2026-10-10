use super::*;

pub(crate) fn strip_tool_search_tools(body: &Value) -> Option<Value> {
    let mut compat = body.clone();
    let tools = compat.get_mut("tools")?.as_array_mut()?;
    let original_len = tools.len();
    tools.retain(|tool| tool.get("type").and_then(Value::as_str) != Some("tool_search"));
    if tools.len() == original_len {
        return None;
    }
    if tools.is_empty() {
        compat.as_object_mut()?.remove("tools");
    }
    Some(compat)
}

pub(crate) fn responses_tool_call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Aggregators reject replayed Responses histories that contain a
/// `function_call` without its matching `function_call_output`, or an output
/// whose call was pruned. Repair both sides before the request leaves the
/// gateway.
pub(crate) fn sanitize_responses_tool_history(body: &mut Value) -> usize {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return 0;
    };

    let mut call_ids = HashSet::new();
    let mut result_ids = HashSet::new();
    for item in items.iter() {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                if let Some(id) = responses_tool_call_id(item) {
                    call_ids.insert(id.to_string());
                }
            }
            Some("function_call_output") => {
                if let Some(id) = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    result_ids.insert(id.to_string());
                }
            }
            _ => {}
        }
    }

    let paired = call_ids
        .intersection(&result_ids)
        .cloned()
        .collect::<HashSet<_>>();
    let before = items.len();
    items.retain(|item| match item.get("type").and_then(Value::as_str) {
        Some("function_call") => responses_tool_call_id(item).is_some_and(|id| paired.contains(id)),
        Some("function_call_output") => item
            .get("call_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|id| paired.contains(id)),
        _ => true,
    });
    before.saturating_sub(items.len())
}

pub(crate) fn chat_tool_call_id(call: &Value) -> Option<&str> {
    call.get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn chat_message_is_empty(message: &Value) -> bool {
    let has_content = match message.get("content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(parts)) => !parts.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    let has_tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    !has_content && !has_tool_calls
}

/// Chat completions upstreams require every assistant `tool_calls` entry to
/// have a matching `tool` result. Drop only the unpaired side and remove
/// assistant shells that become empty.
pub(crate) fn sanitize_chat_tool_history(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };

    let mut call_ids = HashSet::new();
    let mut result_ids = HashSet::new();
    for message in messages.iter() {
        if message.get("role").and_then(Value::as_str) == Some("assistant")
            && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
        {
            for call in calls {
                if let Some(id) = chat_tool_call_id(call) {
                    call_ids.insert(id.to_string());
                }
            }
        }
        if message.get("role").and_then(Value::as_str) == Some("tool")
            && let Some(id) = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        {
            result_ids.insert(id.to_string());
        }
    }

    let paired = call_ids
        .intersection(&result_ids)
        .cloned()
        .collect::<HashSet<_>>();
    let mut removed = 0;

    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) == Some("assistant")
            && let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut)
        {
            let before = calls.len();
            calls.retain(|call| chat_tool_call_id(call).is_some_and(|id| paired.contains(id)));
            removed += before.saturating_sub(calls.len());
            if calls.is_empty() {
                message
                    .as_object_mut()
                    .map(|object| object.remove("tool_calls"));
            }
        }
    }

    let before = messages.len();
    messages.retain(|message| {
        let role = message.get("role").and_then(Value::as_str);
        if role == Some("tool") {
            return message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some_and(|id| paired.contains(id));
        }
        !(role == Some("assistant") && chat_message_is_empty(message))
    });
    removed + before.saturating_sub(messages.len())
}

pub(crate) fn command_code_max_output_tokens(body: &mut Value) -> usize {
    const MAX_COMMAND_CODE_TOKENS: i64 = 200_000;
    let mut changes = 0;
    for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        let Some(value) = body.get(key).and_then(Value::as_i64) else {
            continue;
        };
        if value <= 0 {
            if let Some(object) = body.as_object_mut() {
                object.remove(key);
                changes += 1;
            }
        } else if value > MAX_COMMAND_CODE_TOKENS {
            body[key] = json!(MAX_COMMAND_CODE_TOKENS);
            changes += 1;
        }
    }
    changes
}

pub(crate) fn normalize_command_code_reasoning_effort(body: &mut Value) -> usize {
    let responses_shape = body.get("input").is_some() && body.get("messages").is_none();
    let mut changes = 0;

    let normalize = |value: &mut Value, changes: &mut usize| {
        let Some(effort) = value.as_str() else {
            return;
        };
        let normalized = match effort.to_ascii_lowercase().as_str() {
            "minimal" => Some("low"),
            "none" if !responses_shape => Some("low"),
            _ => None,
        };
        if let Some(normalized) = normalized {
            *value = json!(normalized);
            *changes += 1;
        }
    };

    if let Some(value) = body.get_mut("reasoning_effort") {
        normalize(value, &mut changes);
    }
    if let Some(value) = body
        .get_mut("reasoning")
        .and_then(Value::as_object_mut)
        .and_then(|reasoning| reasoning.get_mut("effort"))
    {
        normalize(value, &mut changes);
    }
    changes
}

pub(crate) fn is_command_code_target(target: &RouteTarget) -> bool {
    let base_url = target.base_url.to_ascii_lowercase();
    let prefix = target
        .model_prefix
        .trim()
        .trim_end_matches('/')
        .to_ascii_lowercase();
    base_url.contains("api.commandcode.ai")
        || base_url.contains("commandcode.ai/provider/v1")
        || prefix == "commandcode"
        || prefix == "command-code"
}

pub(crate) fn normalize_command_code_request(body: &mut Value) -> usize {
    command_code_max_output_tokens(body) + normalize_command_code_reasoning_effort(body)
}

/// Applies the compatibility repairs that must run on the exact body sent
/// upstream, regardless of which inbound protocol produced it:
///
/// - drop unpaired tool calls and orphan tool results (aggregators and strict
///   providers reject these with opaque 400s),
/// - strip `tool_search` for providers known not to support it,
/// - clamp/normalize CommandCode output limits and reasoning effort.
///
/// `shape` describes the wire format actually being sent, because OpenAI
/// `messages` and Anthropic `messages` are different enough that the pairing
/// rules must not be applied to the wrong one.
pub(crate) async fn apply_upstream_compat(
    state: &AppState,
    request_id: &str,
    target: &RouteTarget,
    shape: UpstreamShape,
    body: &mut Value,
) {
    let repaired_tool_items = match shape {
        UpstreamShape::Responses => sanitize_responses_tool_history(body),
        UpstreamShape::Chat => sanitize_chat_tool_history(body),
        UpstreamShape::Anthropic => 0,
    };
    if repaired_tool_items > 0 {
        tracing::warn!(
            provider = %target.provider_name,
            model = %target.upstream_model,
            request_id,
            repaired_tool_items,
            "repaired incomplete tool history before upstream request"
        );
        log_usage_warning(
            state,
            request_id,
            &format!(
                "Removed {repaired_tool_items} incomplete tool-history item(s) before forwarding."
            ),
        )
        .await;
    }

    if target.tool_search_supported == 0
        && let Some(compat_body) = strip_tool_search_tools(body)
    {
        *body = compat_body;
    }

    if is_command_code_target(target) {
        let changes = normalize_command_code_request(body);
        if changes > 0 {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                changes,
                "normalized CommandCode request before upstream send"
            );
            log_usage_warning(
                state,
                request_id,
                "Applied CommandCode compatibility normalization to output limits or reasoning effort.",
            )
            .await;
        }
    }
}

/// Wire shape of the body that will be sent to the upstream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UpstreamShape {
    Responses,
    Chat,
    Anthropic,
}

/// Classifies the body about to be sent to `provider_type` so the correct
/// tool-history repair runs. A body with neither `input` nor the expected
/// `messages` shape classifies as `Chat`; the chat repair then no-ops because
/// there is no `messages` array to walk.
pub(crate) fn classify_upstream_shape(provider_type: ProviderType, body: &Value) -> UpstreamShape {
    if provider_type == ProviderType::Anthropic {
        return UpstreamShape::Anthropic;
    }
    if body.get("input").is_some() && body.get("messages").is_none() {
        UpstreamShape::Responses
    } else {
        UpstreamShape::Chat
    }
}

pub(crate) async fn mark_provider_tool_search_unsupported(state: &AppState, provider_id: i64) {
    if let Err(error) = sqlx::query(
        "UPDATE providers \
         SET tool_search_supported = 0, \
             tool_search_checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
    .bind(provider_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(%error, provider_id, "failed to persist tool_search compatibility");
    }
}
