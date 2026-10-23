use super::*;

mod media;

pub(crate) use media::{normalize_image_detail_original, rejected_image_detail_options};

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

pub(crate) fn strip_strict_tools(body: &Value) -> Option<Value> {
    let mut compat = body.clone();
    let tools = compat.get_mut("tools")?.as_array_mut()?;
    let mut changed = false;
    for tool in tools {
        let Some(tool) = tool.as_object_mut() else {
            continue;
        };
        changed |= tool.remove("strict").is_some();
        if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
            changed |= function.remove("strict").is_some();
        }
    }
    changed.then_some(compat)
}

pub(crate) fn upstream_rejects_strict_tools(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    let strict_field = [
        "unknown strict",
        "unsupported strict",
        "strict is not supported",
        "strict not supported",
        "strict is unsupported",
        "does not support strict",
        "doesn't support strict",
        "strict field",
        "strict parameter",
        "strict property",
        "property strict",
        "parameter strict",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    strict_field && (text.contains("tool") || text.contains("function") || text.contains("schema"))
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponsesStepKind {
    Other,
    Reasoning,
    Said,
    Call,
    Output,
    Turn,
}

fn responses_step_kind(item: &Value) -> ResponsesStepKind {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let role = item.get("role").and_then(Value::as_str).unwrap_or_default();
    if item_type == "reasoning" {
        ResponsesStepKind::Reasoning
    } else if item_type.ends_with("_output") {
        ResponsesStepKind::Output
    } else if item_type.ends_with("_call") {
        ResponsesStepKind::Call
    } else if (item_type == "message" || item_type.is_empty()) && role == "assistant" {
        ResponsesStepKind::Said
    } else if item_type == "message" || (item_type.is_empty() && !role.is_empty()) {
        ResponsesStepKind::Turn
    } else {
        ResponsesStepKind::Other
    }
}

fn is_responses_step(kind: ResponsesStepKind) -> bool {
    matches!(
        kind,
        ResponsesStepKind::Reasoning | ResponsesStepKind::Said | ResponsesStepKind::Call
    )
}

fn has_nonempty_reasoning_text(item: &Value) -> bool {
    item.get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|part| {
                part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            })
        })
}

fn placeholder_reasoning_item() -> Value {
    json!({
        "type": "reasoning",
        "summary": [],
        "content": [{"type": "reasoning_text", "text": " "}]
    })
}

/// Repairs the reasoning shape DeepSeek requires in Responses histories.
///
/// DeepSeek treats a run of reasoning, assistant messages, and calls after a
/// user message or tool output as one step. In the continued turn, a step
/// that makes a call or follows a tool output must start with a reasoning item
/// whose `content` includes non-empty `reasoning_text`. Codex can replay a
/// step as `[message, reasoning, function_call]`, or with no reasoning at all,
/// after another provider produced that turn.
pub(crate) fn normalize_responses_reasoning_history(body: &mut Value) -> usize {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return 0;
    };

    let mut changed = 0;
    for item in items.iter_mut() {
        if responses_step_kind(item) != ResponsesStepKind::Reasoning
            || has_nonempty_reasoning_text(item)
        {
            continue;
        }
        let text = responses_reasoning_text(item).unwrap_or_else(|| " ".to_string());
        item["content"] = json!([{"type": "reasoning_text", "text": text}]);
        changed += 1;
    }

    let original = std::mem::take(items);
    let current_turn_start = original
        .iter()
        .rposition(|item| {
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            (item_type == "message" || item_type.is_empty())
                && item.get("role").and_then(Value::as_str) == Some("user")
        })
        .map_or(0, |index| index + 1);
    let mut ordered = Vec::with_capacity(original.len() + 1);
    let mut after_output = false;
    let mut index = 0;
    while index < original.len() {
        let kind = responses_step_kind(&original[index]);
        if !is_responses_step(kind) {
            match kind {
                ResponsesStepKind::Output => after_output = true,
                ResponsesStepKind::Turn => after_output = false,
                _ => {}
            }
            ordered.push(original[index].clone());
            index += 1;
            continue;
        }

        let start = index;
        let mut end = index;
        let mut has_call = false;
        let mut first_reasoning = None;
        while end < original.len() && is_responses_step(responses_step_kind(&original[end])) {
            let kind = responses_step_kind(&original[end]);
            has_call |= kind == ResponsesStepKind::Call;
            if kind == ResponsesStepKind::Reasoning && first_reasoning.is_none() {
                first_reasoning = Some(end);
            }
            end += 1;
        }

        if start >= current_turn_start
            && (has_call || after_output)
            && first_reasoning != Some(start)
        {
            if let Some(first) = first_reasoning {
                ordered.push(original[first].clone());
            } else {
                ordered.push(placeholder_reasoning_item());
            }
            for (current, item) in original.iter().enumerate().take(end).skip(start) {
                if Some(current) != first_reasoning {
                    ordered.push(item.clone());
                }
            }
            changed += 1;
        } else {
            ordered.extend_from_slice(&original[start..end]);
        }
        index = end;
    }
    *items = ordered;
    changed
}

/// Whether an upstream is known to require replayed reasoning in Responses
/// histories. OpenAI-family endpoints accept reasoning items but should not
/// receive synthetic ones.
pub(crate) fn responses_replays_reasoning(target: &RouteTarget) -> bool {
    target_replays_reasoning(target)
}

fn target_replays_reasoning(target: &RouteTarget) -> bool {
    let host = target.base_url.to_ascii_lowercase();
    let model = target.upstream_model.to_ascii_lowercase();
    let provider = target.provider_name.to_ascii_lowercase();
    let prefix = target.model_prefix.to_ascii_lowercase();
    (model.contains("deepseek")
        || provider.contains("deepseek")
        || prefix.contains("deepseek")
        || host.contains("deepseek"))
        && !host.contains("api.openai.com")
        && !host.contains("openai.azure.com")
}

fn chat_replays_reasoning(target: &RouteTarget) -> bool {
    target_replays_reasoning(target)
}

pub(crate) fn normalize_chat_reasoning_history(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let current_turn_start = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map_or(0, |index| index + 1);
    let mut after_tool_result = false;
    let mut changed = 0;
    for (index, message) in messages.iter_mut().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role == "tool" {
            after_tool_result = true;
            continue;
        }
        if role != "assistant" {
            after_tool_result = false;
            continue;
        }
        let has_tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty());
        let needs_reasoning = index >= current_turn_start && (has_tool_calls || after_tool_result);
        if needs_reasoning {
            let existing_content = message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty());
            if existing_content.is_none() {
                if let Some(reasoning) = chat_reasoning_text(message) {
                    message["reasoning_content"] = json!(reasoning);
                } else {
                    message["reasoning_content"] = json!(" ");
                }
                changed += 1;
            }
        }
        after_tool_result = false;
    }
    changed
}

pub(crate) fn normalize_anthropic_thinking_history(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut after_tool_result = false;
    let mut changed = 0;
    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            after_tool_result = role_has_tool_result(message);
            continue;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            after_tool_result = false;
            continue;
        };
        let has_tool_use = content
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
        let needs_thinking = has_tool_use || after_tool_result;
        if !needs_thinking {
            after_tool_result = false;
            continue;
        }

        let mut has_thinking = false;
        let before = content.len();
        content.retain(|block| {
            let block_type = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if block_type == "thinking"
                && (block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
                    || block
                        .get("signature")
                        .and_then(Value::as_str)
                        .is_some_and(|signature| !signature.is_empty()))
            {
                has_thinking = true;
                true
            } else {
                block_type != "redacted_thinking"
            }
        });
        if before.saturating_sub(content.len()) > 0 {
            changed += 1;
        }
        if !has_thinking {
            content.insert(0, json!({"type": "thinking", "thinking": " "}));
            changed += 1;
        }
        after_tool_result = false;
    }
    changed
}

fn role_has_tool_result(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
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
    let has_reasoning = chat_reasoning_text(message).is_some();
    !has_content && !has_tool_calls && !has_reasoning
}

/// Chat completions upstreams require every assistant `tool_calls` entry to
/// have a matching `tool` result. The result must also follow the call in
/// history, so repair the pairing in order instead of comparing two global id
/// sets. This catches results replayed before their call and results separated
/// from the call by another assistant or user turn.
pub(crate) fn sanitize_chat_tool_history(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };

    let mut removed = 0;
    let mut pending: Option<(usize, HashSet<String>)> = None;
    let mut sanitized = Vec::with_capacity(messages.len());

    for mut message in messages.iter().cloned() {
        let role = message.get("role").and_then(Value::as_str);
        if role == Some("assistant") {
            if let Some((index, call_ids)) = pending.take() {
                removed += remove_chat_tool_calls(&mut sanitized[index], &call_ids);
            }

            let mut call_ids = HashSet::new();
            if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                let before = calls.len();
                calls.retain(|call| {
                    let Some(id) = chat_tool_call_id(call) else {
                        return false;
                    };
                    call_ids.insert(id.to_string());
                    true
                });
                removed += before.saturating_sub(calls.len());
                if calls.is_empty() {
                    message
                        .as_object_mut()
                        .map(|object| object.remove("tool_calls"));
                }
            }
            if !call_ids.is_empty() {
                pending = Some((sanitized.len(), call_ids));
            }
            sanitized.push(message);
            continue;
        }

        if role == Some("tool") {
            let call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if let Some(call_id) = call_id
                && let Some((_, call_ids)) = pending.as_mut()
                && call_ids.remove(&call_id)
            {
                sanitized.push(message);
                continue;
            }
            removed += 1;
            continue;
        }

        if let Some((index, call_ids)) = pending.take() {
            removed += remove_chat_tool_calls(&mut sanitized[index], &call_ids);
        }
        sanitized.push(message);
    }

    if let Some((index, call_ids)) = pending.take() {
        removed += remove_chat_tool_calls(&mut sanitized[index], &call_ids);
    }

    let before = sanitized.len();
    sanitized.retain(|message| {
        message.get("role").and_then(Value::as_str) != Some("assistant")
            || !chat_message_is_empty(message)
    });
    removed += before.saturating_sub(sanitized.len());
    *messages = sanitized;
    removed
}

fn remove_chat_tool_calls(message: &mut Value, call_ids: &HashSet<String>) -> usize {
    let remove = {
        let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            return 0;
        };
        let before = calls.len();
        calls.retain(|call| chat_tool_call_id(call).is_some_and(|id| !call_ids.contains(id)));
        let removed = before.saturating_sub(calls.len());
        (removed, calls.is_empty())
    };
    if remove.1 {
        message
            .as_object_mut()
            .map(|object| object.remove("tool_calls"));
    }
    remove.0
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

/// Maps an effort value to the nearest value accepted by the upstream model.
///
/// The closest numeric level wins; ties choose the higher level.
pub(crate) fn normalize_reasoning_effort(body: &mut Value, supported: &[String]) -> usize {
    if supported.is_empty() {
        return 0;
    }
    let mut changes = 0;
    let normalize = |value: &mut Value, changes: &mut usize| {
        let Some(current) = value.as_str() else {
            return;
        };
        let Some(mapped) = nearest_reasoning_effort(current, supported) else {
            return;
        };
        if mapped != current {
            *value = json!(mapped);
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

fn nearest_reasoning_effort(current: &str, supported: &[String]) -> Option<String> {
    let current = current.trim().to_ascii_lowercase();
    if let Some(exact) = supported
        .iter()
        .find(|value| value.eq_ignore_ascii_case(&current))
    {
        return Some(exact.clone());
    }
    let target_rank = reasoning_effort_rank(&current).unwrap_or(3);
    supported
        .iter()
        .filter_map(|value| {
            reasoning_effort_rank(value).map(|rank| (value, rank, target_rank.abs_diff(rank)))
        })
        .min_by_key(|(_, rank, distance)| (*distance, std::cmp::Reverse(*rank)))
        .map(|(value, _, _)| value.clone())
        .or_else(|| supported.first().cloned())
}

fn reasoning_effort_rank(value: &str) -> Option<u8> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(0),
        "minimal" => Some(1),
        "low" => Some(2),
        "medium" => Some(3),
        "high" => Some(4),
        "xhigh" => Some(5),
        "max" => Some(6),
        _ => None,
    }
}

/// Extracts accepted reasoning effort values from a precise upstream enum
/// rejection such as:
///
/// Only 400/422 responses are considered. Image-detail rejections use the
/// same enum wording and are deliberately excluded here.
pub(crate) fn rejected_reasoning_effort_options(
    status: StatusCode,
    body: &[u8],
    request: &Value,
) -> Option<Vec<String>> {
    let (text, values) = rejected_enum_options(status, body)?;
    if is_image_detail_rejection(&text, &values) {
        return None;
    }
    let rejected = rejected_enum_variant(&text)?;
    let requested = request
        .get("reasoning_effort")
        .or_else(|| request.pointer("/reasoning/effort"))
        .and_then(Value::as_str)?;
    rejected
        .eq_ignore_ascii_case(requested.trim())
        .then_some(values)
}

fn rejected_enum_variant(text: &str) -> Option<&str> {
    let rest = text.split_once("unknown variant")?.1.trim_start();
    let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|ch| matches!(ch, '\'' | '"' | '`'))?;
    let rest = rest.get(quote.len_utf8()..)?;
    let value = rest[..rest.find(quote)?].trim();
    (!value.is_empty()).then_some(value)
}

pub(super) fn rejected_enum_options(
    status: StatusCode,
    body: &[u8],
) -> Option<(String, Vec<String>)> {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return None;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    if !text.contains("unknown variant") || !text.contains("expected one of") {
        return None;
    }
    let start = text.find("expected one of")? + "expected one of".len();
    let expected = &text[start..start.saturating_add(256).min(text.len())];
    let mut values = Vec::new();
    let mut rest = expected;
    while let Some(quote_index) = rest.find(['\'', '"', '`']) {
        let quote = rest.as_bytes()[quote_index] as char;
        rest = &rest[quote_index + 1..];
        let Some(end) = rest.find(quote) else {
            break;
        };
        let value = rest[..end].trim();
        if !value.is_empty()
            && value.len() <= 32
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            && !values.iter().any(|existing| existing == value)
        {
            values.push(value.to_string());
        }
        rest = &rest[end + 1..];
    }
    (!values.is_empty()).then_some((text, values))
}

pub(super) fn is_image_detail_rejection(text: &str, values: &[String]) -> bool {
    if text.contains("reasoning") || text.contains("effort") {
        return false;
    }
    let image_context =
        text.contains("content part") || text.contains("image") || text.contains("detail");
    let classic_detail_enum = values.iter().any(|value| value == "auto")
        && values.iter().any(|value| value == "high")
        && values
            .iter()
            .all(|value| matches!(value.as_str(), "auto" | "low" | "high"));
    text.contains("original") && (image_context || classic_detail_enum)
}

fn target_reasoning_efforts(target: &RouteTarget) -> Option<Vec<String>> {
    let values = serde_json::from_str::<Vec<String>>(target.reasoning_efforts.as_deref()?).ok()?;
    (!values.is_empty()).then_some(values)
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
/// - normalize image detail levels unsupported by strict upstreams,
/// - clamp/normalize CommandCode output limits and reasoning effort.
///
/// `shape` describes the wire format actually being sent, because OpenAI
/// `messages` and Anthropic `messages` are different enough that the pairing
/// rules must not be applied to the wrong one.
pub(crate) async fn apply_upstream_compat(
    state: &AppState,
    request_id: &str,
    target: &RouteTarget,
    endpoint: &str,
    shape: UpstreamShape,
    body: &mut Value,
) {
    let reasoning_profile = crate::capabilities::capability_support(
        state,
        target.provider_id,
        &target.upstream_model,
        endpoint,
        crate::capabilities::CAPABILITY_REASONING_HISTORY,
    )
    .await;
    let tool_search_profile = crate::capabilities::capability_support(
        state,
        target.provider_id,
        &target.upstream_model,
        endpoint,
        crate::capabilities::CAPABILITY_TOOL_SEARCH,
    )
    .await;
    let strict_tools_profile = crate::capabilities::capability_support(
        state,
        target.provider_id,
        &target.upstream_model,
        endpoint,
        crate::capabilities::CAPABILITY_STRICT_TOOLS,
    )
    .await;
    let image_detail_profile = crate::capabilities::capability_support(
        state,
        target.provider_id,
        &target.upstream_model,
        endpoint,
        crate::capabilities::CAPABILITY_IMAGE_DETAIL_ORIGINAL,
    )
    .await;
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

    if shape == UpstreamShape::Responses
        && (reasoning_profile == Some(true) || responses_replays_reasoning(target))
    {
        let reasoning_items = normalize_responses_reasoning_history(body);
        if reasoning_items > 0 {
            tracing::debug!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                reasoning_items,
                "normalized replayed reasoning before Responses request"
            );
        }
    }

    if shape == UpstreamShape::Chat
        && (reasoning_profile == Some(true) || chat_replays_reasoning(target))
    {
        let reasoning_messages = normalize_chat_reasoning_history(body);
        if reasoning_messages > 0 {
            tracing::debug!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                reasoning_messages,
                "normalized replayed reasoning before chat request"
            );
        }
    }

    if shape == UpstreamShape::Anthropic
        && (reasoning_profile == Some(true) || target_replays_reasoning(target))
    {
        let thinking_messages = normalize_anthropic_thinking_history(body);
        if thinking_messages > 0 {
            tracing::debug!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                thinking_messages,
                "normalized replayed thinking before Anthropic request"
            );
        }
    }

    if (target.tool_search_supported == 0 || tool_search_profile == Some(false))
        && let Some(compat_body) = strip_tool_search_tools(body)
    {
        *body = compat_body;
    }

    if strict_tools_profile == Some(false)
        && let Some(compat_body) = strip_strict_tools(body)
    {
        *body = compat_body;
    }

    if image_detail_profile == Some(false) || is_command_code_target(target) {
        let changes = normalize_image_detail_original(body, &[]);
        if changes > 0 {
            tracing::debug!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                image_parts = changes,
                "downleveled original image detail before upstream request"
            );
        }
    }

    if let Some(supported) = target_reasoning_efforts(target) {
        let changes = normalize_reasoning_effort(body, &supported);
        if changes > 0 {
            tracing::debug!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                request_id,
                supported = ?supported,
                "mapped reasoning effort to a value supported by the upstream model"
            );
        }
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
    crate::capabilities::record_capability(
        state,
        crate::capabilities::CapabilityKey {
            provider_id,
            model: String::new(),
            endpoint: String::new(),
            capability: crate::capabilities::CAPABILITY_TOOL_SEARCH.to_string(),
        },
        false,
        crate::capabilities::SOURCE_RUNTIME,
        "exact",
    )
    .await;
}
