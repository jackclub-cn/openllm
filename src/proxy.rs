use std::collections::HashMap;
use std::io;
use std::str::FromStr;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use chrono::{Timelike, Utc};
use futures_util::StreamExt;
use globset::Glob;
use rand::distributions::{Distribution, WeightedIndex};
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{AppError, AppResult};
use crate::models::{
    ApiKeyRecord, ModelCapabilities, ModelList, ProviderType, PublicModel, Route,
    RouteDiagnoseRuntimeTarget, RouteDiagnoseTarget, RouteDiagnoseView, RouteStrategy, RouteTarget,
    Usage, effective_cost_value, estimate_cost_micros,
};
use crate::registry::BarrelEnvelope;
use crate::state::AppState;

const OPENAI_CHAT_COMPLETIONS: &str = "/v1/chat/completions";
const OPENAI_COMPLETIONS: &str = "/v1/completions";
const CONSOLE_API_KEY_ID_HEADER: &str = "x-openllm-api-key-id";
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";
const SESSION_ID_MAX_CHARS: usize = 256;
const OPENAI_RESPONSES: &str = "/v1/responses";

pub async fn public_models(
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

async fn public_models_inner(
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

async fn openai_public_models(
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
pub async fn public_model(
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

async fn public_model_inner(
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
fn wants_anthropic_models(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version")
}

/// Reads the caller's `anthropic-beta` feature flags, if any.
///
/// Anthropic gates capabilities such as prompt caching behind this header, so
/// it must reach a native upstream verbatim; the gateway never invents its own
/// value.
fn anthropic_beta_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Resolves a stable conversation identifier for OpenCode Go.
///
/// OpenCode clients send `x-opencode-session` directly. Codex sends its native
/// `session-id` and `thread-id` headers, while some proxies and Responses
/// clients only preserve the stable prompt cache key in the JSON body.
fn upstream_session_id(headers: &HeaderMap, body: &Value) -> Option<String> {
    for name in [
        OPENCODE_SESSION_HEADER,
        "session-id",
        "thread-id",
        "x-session-id",
        "x-claude-code-session-id",
    ] {
        if let Some(value) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.chars().take(SESSION_ID_MAX_CHARS).collect());
        }
    }

    for pointer in [
        "/prompt_cache_key",
        "/client_metadata/session_id",
        "/metadata/session_id",
    ] {
        if let Some(value) = body
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.chars().take(SESSION_ID_MAX_CHARS).collect());
        }
    }

    None
}

fn is_opencode_go_target(target: &RouteTarget) -> bool {
    let provider_name = target.provider_name.to_ascii_lowercase();
    let model_prefix = target.model_prefix.trim().trim_end_matches('/');
    let base_url = target.base_url.to_ascii_lowercase();
    provider_name.contains("opencode go")
        || model_prefix.eq_ignore_ascii_case("opencode-go")
        || base_url.contains("opencode.ai/zen/go")
}

fn custom_headers_contain(name: &str, headers: &str) -> bool {
    serde_json::from_str::<Value>(headers)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|headers| headers.keys().any(|key| key.eq_ignore_ascii_case(name)))
}

fn apply_opencode_session_header(
    request: RequestBuilder,
    target: &RouteTarget,
    session_id: Option<&str>,
) -> RequestBuilder {
    let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) else {
        return request;
    };
    if !is_opencode_go_target(target)
        || custom_headers_contain(OPENCODE_SESSION_HEADER, &target.provider_headers)
    {
        return request;
    }
    request.header(OPENCODE_SESSION_HEADER, session_id)
}

/// Renders the model registry in Anthropic's `/v1/models` shape.
///
/// Anthropic returns `data` entries of `{type, id, display_name, created_at}`
/// plus cursor fields, rather than OpenAI's `{id, object, created, owned_by}`.
async fn anthropic_models(
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

async fn anthropic_model_values(
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
fn paginate_models(models: Vec<Value>, page: &ModelPageQuery) -> (Vec<Value>, bool) {
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

const ANTHROPIC_MESSAGES: &str = "/v1/messages";

/// Extracts the required `model` field from a request body.
fn requested_model_of(body: &Value) -> AppResult<String> {
    body.get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::BadRequest("request body must include a model".to_string()))
}

/// Validates Anthropic's required `max_tokens` field.
///
/// Mirrors the upstream contract: it must be present and a positive integer.
/// A zero or negative value would be rejected later anyway, but catching it
/// here yields the precise `invalid_request_error` the client expects.
fn validate_anthropic_max_tokens(body: &Value) -> AppResult<()> {
    match body.get("max_tokens") {
        Some(value) => match value.as_i64() {
            Some(tokens) if tokens > 0 => Ok(()),
            Some(_) => Err(AppError::BadRequest(
                "max_tokens must be a positive integer".to_string(),
            )),
            None => Err(AppError::BadRequest(
                "max_tokens must be an integer".to_string(),
            )),
        },
        None => Err(AppError::BadRequest(
            "max_tokens is required for the Anthropic Messages API".to_string(),
        )),
    }
}

/// Formats one server-sent event the way Anthropic clients expect it.
fn sse_line(event: &str, data: Value) -> String {
    format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    )
}

/// Collects Anthropic text from either a bare string or an array of blocks.
fn anthropic_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// Converts one Anthropic image block into an OpenAI `image_url` part.
fn anthropic_image_to_openai(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media_type};base64,{data}")
        }
        Some("url") => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

/// Converts one non-`tool_result` Anthropic content block into an OpenAI part.
fn anthropic_block_to_openai_part(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("image") => anthropic_image_to_openai(block),
        _ => None,
    }
}

fn anthropic_tool_choice_to_openai(choice: &Value) -> Option<Value> {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Some(json!("auto")),
        Some("any") => Some(json!("required")),
        Some("none") => Some(json!("none")),
        Some("tool") => Some(json!({
            "type": "function",
            "function": {"name": choice.get("name").cloned().unwrap_or(Value::Null)}
        })),
        _ => None,
    }
}

/// Converts an inbound Anthropic Messages body into the OpenAI chat shape that
/// the rest of the gateway (routing, barrel clamping, forwarding) understands.
fn anthropic_request_to_openai(input: &Value, model: &str) -> Value {
    let mut messages = Vec::new();
    // Anthropic carries the system prompt outside `messages`.
    if let Some(system) = input.get("system")
        && let Some(text) = anthropic_text(system)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    if let Some(items) = input.get("messages").and_then(Value::as_array) {
        for message in items {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            anthropic_message_to_openai(role, &content, &mut messages);
        }
    }

    let mut output = json!({"model": model, "messages": messages});
    for (source, target) in [
        ("max_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(stop) = input.get("stop_sequences") {
        output["stop"] = stop.clone();
    }
    if input
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        output["stream"] = json!(true);
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("input_schema").cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                    }
                }))
            })
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            output["tools"] = json!(converted);
        }
    }
    if let Some(choice) = input.get("tool_choice")
        && let Some(mapped) = anthropic_tool_choice_to_openai(choice)
    {
        output["tool_choice"] = mapped;
    }
    output
}

/// Appends the OpenAI messages equivalent to one Anthropic message.
///
/// A single Anthropic user turn may mix text and `tool_result` blocks. Only the
/// latter become `role: tool` messages, and they are emitted in place so they
/// still follow the assistant turn whose `tool_calls` they answer.
fn anthropic_message_to_openai(role: &str, content: &Value, messages: &mut Vec<Value>) {
    if let Some(text) = content.as_str() {
        messages.push(json!({"role": role, "content": text}));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };

    if role == "assistant" {
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(part) = block.get("text").and_then(Value::as_str) {
                        text.push_str(part);
                    }
                }
                Some("tool_use") => tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": serde_json::to_string(
                            block.get("input").unwrap_or(&json!({}))
                        ).unwrap_or_else(|_| "{}".to_string())
                    }
                })),
                _ => {}
            }
        }
        let mut message = json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { json!(text) }
        });
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }
        messages.push(message);
        return;
    }

    // User (and any other) turn: split tool results out into their own messages.
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                "content": anthropic_text(block.get("content").unwrap_or(&Value::Null))
                    .unwrap_or_default()
            }));
        } else if let Some(part) = anthropic_block_to_openai_part(block) {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return;
    }
    // Collapse text-only content to a plain string: most providers (and some
    // strict validators) handle that more reliably than a one-element array.
    if parts
        .iter()
        .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    {
        let text = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<String>();
        messages.push(json!({"role": role, "content": text}));
    } else {
        messages.push(json!({"role": role, "content": parts}));
    }
}

/// Maps an OpenAI finish reason onto the Anthropic `stop_reason` vocabulary.
fn anthropic_stop_reason(finish: Option<&str>, has_tool_use: bool) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        _ if has_tool_use => "tool_use",
        _ => "end_turn",
    }
}

/// Converts a non-streamed OpenAI completion into an Anthropic message.
fn openai_response_to_anthropic(value: &Value, requested_model: &str) -> (Value, Usage) {
    let message = value.pointer("/choices/0/message");
    let mut content = Vec::new();
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let function = call.get("function").unwrap_or(call);
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").cloned().unwrap_or_else(
                    || json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))
                ),
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "input": input
            }));
        }
    }
    let has_tool_use = content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
    let finish = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("msg_{}", id.trim_start_matches("chatcmpl-")))
        .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple()));
    (
        json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": requested_model,
            "content": content,
            "stop_reason": anthropic_stop_reason(finish, has_tool_use),
            "stop_sequence": Value::Null,
            "usage": {
                "input_tokens": usage.prompt_tokens,
                "output_tokens": usage.completion_tokens
            }
        }),
        usage,
    )
}

/// Converts a Responses object into an Anthropic message.
///
/// Responses-first OpenAI upstreams serve Anthropic clients by normalising the
/// payload through the chat shape the Anthropic converter already understands.
fn responses_response_to_anthropic(value: &Value, requested_model: &str) -> (Value, Usage) {
    let (chat, usage) = responses_response_to_chat(value, requested_model);
    let (anthropic, _) = openai_response_to_anthropic(&chat, requested_model);
    (anthropic, usage)
}

/// Wraps a message in Anthropic's error envelope so Anthropic clients can parse
/// gateway-side failures the same way they parse upstream errors.
fn anthropic_error_body(error_type: &str, message: &str) -> Value {
    json!({"type": "error", "error": {"type": error_type, "message": message}})
}

/// Accumulates state while rewriting an OpenAI SSE stream into Anthropic events.
#[derive(Default)]
struct AnthropicStreamState {
    started: bool,
    text_index: Option<usize>,
    open_tools: Vec<usize>,
    tool_indices: std::collections::HashMap<i64, usize>,
    next_index: usize,
    has_tool_use: bool,
    finish_reason: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    text: String,
    first_token_ms: Option<i64>,
}

async fn send_anthropic_event(
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
    event: &str,
    data: Value,
) {
    let _ = tx.send(Ok(Bytes::from(sse_line(event, data)))).await;
}

impl AnthropicStreamState {
    fn mark_first_token(&mut self, started: Instant) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(started.elapsed().as_millis() as i64);
        }
    }

    /// Emits `message_start` once, before any content block.
    async fn ensure_started(
        &mut self,
        tx: &mpsc::Sender<Result<Bytes, io::Error>>,
        ctx: &StreamContext,
    ) {
        if self.started {
            return;
        }
        self.started = true;
        send_anthropic_event(
            tx,
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": ctx.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": ctx.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {"input_tokens": ctx.input_tokens, "output_tokens": 0}
                }
            }),
        )
        .await;
    }

    /// Closes the open text block so a tool block can start.
    async fn close_text_block(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        if let Some(index) = self.text_index.take() {
            send_anthropic_event(
                tx,
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            )
            .await;
        }
    }

    /// Closes every still-open block at end of stream.
    async fn close_all_blocks(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        self.close_text_block(tx).await;
        for index in self.open_tools.drain(..) {
            send_anthropic_event(
                tx,
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            )
            .await;
        }
    }
}

/// Immutable per-stream values shared with the state machine.
struct StreamContext {
    message_id: String,
    model: String,
    input_tokens: i64,
    started: Instant,
}

/// Handles one OpenAI SSE line, emitting the matching Anthropic events.
///
/// Returns `false` once `[DONE]` is seen so the caller can stop reading.
async fn process_openai_line_for_anthropic(
    line: &[u8],
    state: &mut AnthropicStreamState,
    ctx: &StreamContext,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) -> bool {
    let line = String::from_utf8_lossy(line);
    let Some(data) = line.trim().strip_prefix("data:") else {
        return true;
    };
    let data = data.trim();
    if data == "[DONE]" {
        return false;
    }
    if data.is_empty() {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return true;
    };

    if let Some(usage) = value.get("usage").filter(|value| !value.is_null()) {
        if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_i64) {
            state.input_tokens = prompt;
        }
        if let Some(completion) = usage.get("completion_tokens").and_then(Value::as_i64) {
            state.output_tokens = completion;
        }
        let cache_read = cache_read_of(usage);
        if cache_read > 0 {
            state.cache_read_tokens = cache_read;
        }
        let cache_write = cache_write_of(usage);
        if cache_write > 0 {
            state.cache_write_tokens = cache_write;
        }
    }
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        state.finish_reason = Some(reason.to_string());
    }

    let delta = value.pointer("/choices/0/delta");
    if let Some(text) = delta
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
        && !text.is_empty()
    {
        state.mark_first_token(ctx.started);
        state.ensure_started(tx, ctx).await;
        let index = match state.text_index {
            Some(index) => index,
            None => {
                let index = state.next_index;
                state.next_index += 1;
                state.text_index = Some(index);
                send_anthropic_event(
                    tx,
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "text", "text": ""}
                    }),
                )
                .await;
                index
            }
        };
        state.text.push_str(text);
        send_anthropic_event(
            tx,
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "text_delta", "text": text}
            }),
        )
        .await;
    }

    if let Some(calls) = delta
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let openai_index = call.get("index").and_then(Value::as_i64).unwrap_or(0);
            let function = call.get("function").unwrap_or(call);
            let index = if let Some(index) = state.tool_indices.get(&openai_index) {
                *index
            } else {
                // A new tool call: text must not stay open alongside it.
                state.mark_first_token(ctx.started);
                state.ensure_started(tx, ctx).await;
                state.close_text_block(tx).await;
                let index = state.next_index;
                state.next_index += 1;
                state.tool_indices.insert(openai_index, index);
                state.open_tools.push(index);
                state.has_tool_use = true;
                send_anthropic_event(
                    tx,
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": call.get("id").cloned().unwrap_or_else(
                                || json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))
                            ),
                            "name": function.get("name").cloned().unwrap_or(Value::Null),
                            "input": {}
                        }
                    }),
                )
                .await;
                index
            };
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str)
                && !arguments.is_empty()
            {
                state.mark_first_token(ctx.started);
                send_anthropic_event(
                    tx,
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": arguments}
                    }),
                )
                .await;
            }
        }
    }
    true
}

/// Finalises an Anthropic stream: close open blocks, then emit the terminal
/// `message_delta` and `message_stop` events.
///
/// Shared by the live handler and tests so the closing sequence is defined once.
async fn finish_anthropic_stream(
    state: &mut AnthropicStreamState,
    ctx: &StreamContext,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    // A stream that produced nothing still owes the client a well-formed
    // message, so open the message before closing out.
    if !state.started {
        state.ensure_started(tx, ctx).await;
    }
    state.close_all_blocks(tx).await;
    let stop_reason = anthropic_stop_reason(state.finish_reason.as_deref(), state.has_tool_use);
    send_anthropic_event(
        tx,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
            "usage": {"output_tokens": state.output_tokens}
        }),
    )
    .await;
    send_anthropic_event(tx, "message_stop", json!({"type": "message_stop"})).await;
}

const USAGE_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Refreshes a streaming request's activity timestamp at a bounded rate.
struct UsageHeartbeat {
    state: AppState,
    request_id: String,
    last_touch: Instant,
}

impl UsageHeartbeat {
    fn new(state: AppState, request_id: String) -> Self {
        Self {
            state,
            request_id,
            last_touch: Instant::now() - USAGE_HEARTBEAT_INTERVAL,
        }
    }

    async fn touch(&mut self) {
        if self.last_touch.elapsed() < USAGE_HEARTBEAT_INTERVAL {
            return;
        }
        self.last_touch = Instant::now();
        if let Err(error) = sqlx::query(
            "UPDATE usage_logs \
             SET last_activity_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE request_id = ? AND in_flight = 1",
        )
        .bind(&self.request_id)
        .execute(&self.state.pool)
        .await
        {
            tracing::warn!(
                %error,
                request_id = self.request_id,
                "failed to update streaming activity heartbeat"
            );
        }
    }
}

/// Rewrites an OpenAI SSE stream into the Anthropic event protocol.
#[allow(clippy::too_many_arguments)]
fn openai_stream_to_anthropic(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let context = StreamContext {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            model: requested_model.clone(),
            input_tokens: request_tokens,
            started,
        };
        let mut stream_state = AnthropicStreamState::default();
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut stream_error = None;
        let mut done = false;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while !done && let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                // `false` means `[DONE]`: the upstream body is complete.
                if !process_openai_line_for_anthropic(&line, &mut stream_state, &context, &tx).await
                {
                    // `[DONE]` received: stop reading the upstream body.
                    done = true;
                    break;
                }
            }
        }
        if !done && !buffer.is_empty() {
            process_openai_line_for_anthropic(&buffer, &mut stream_state, &context, &tx).await;
        }

        finish_anthropic_stream(&mut stream_state, &context, &tx).await;
        drop(tx);

        let usage = Usage {
            prompt_tokens: if stream_state.input_tokens > 0 {
                stream_state.input_tokens
            } else {
                request_tokens
            },
            completion_tokens: stream_state.output_tokens,
            total_tokens: stream_state.input_tokens + stream_state.output_tokens,
            cache_read_tokens: stream_state.cache_read_tokens,
            cache_write_tokens: stream_state.cache_write_tokens,
        }
        .normalized();
        let preview = response_preview(stream_state.text.as_bytes());
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream_state
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Rewrites a Responses SSE stream into the Anthropic event protocol so that
/// Anthropic clients can be served by a Responses-only OpenAI upstream.
#[allow(clippy::too_many_arguments)]
fn responses_stream_to_anthropic(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let context = StreamContext {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            model: requested_model.clone(),
            input_tokens: request_tokens,
            started,
        };
        let mut stream_state = AnthropicStreamState::default();
        let mut tool_indices = std::collections::HashMap::<String, usize>::new();
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_responses_line_for_anthropic(
                    &line,
                    &mut event_name,
                    &mut stream_state,
                    &mut tool_indices,
                    &context,
                    &tx,
                )
                .await;
            }
        }
        if !buffer.is_empty() {
            process_responses_line_for_anthropic(
                &buffer,
                &mut event_name,
                &mut stream_state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
        finish_anthropic_stream(&mut stream_state, &context, &tx).await;
        drop(tx);

        let prompt_tokens = if stream_state.input_tokens > 0 {
            stream_state.input_tokens
        } else {
            request_tokens
        };
        let completion_tokens = if stream_state.output_tokens > 0 {
            stream_state.output_tokens
        } else {
            (stream_state.text.chars().count() / 4) as i64
        };
        let usage = Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: stream_state.cache_read_tokens,
            cache_write_tokens: stream_state.cache_write_tokens,
        }
        .normalized();
        let preview = response_preview(stream_state.text.as_bytes());
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream_state
            .first_token_ms
            .or_else(|| (completion_tokens > 0).then_some(latency_ms));
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Handles one Responses SSE line, emitting the matching Anthropic events.
#[allow(clippy::too_many_arguments)]
async fn process_responses_line_for_anthropic(
    line: &[u8],
    event_name: &mut String,
    state: &mut AnthropicStreamState,
    tool_indices: &mut std::collections::HashMap<String, usize>,
    ctx: &StreamContext,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if let Some(event) = line.strip_prefix("event:") {
        *event_name = event.trim().to_string();
        return;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(event_name.as_str());

    match event {
        "response.output_item.added" => {
            let item = value.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                state.mark_first_token(ctx.started);
                state.ensure_started(tx, ctx).await;
                state.close_text_block(tx).await;
                let index = state.next_index;
                state.next_index += 1;
                state.open_tools.push(index);
                state.has_tool_use = true;
                if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                    tool_indices.insert(item_id.to_string(), index);
                }
                send_anthropic_event(
                    tx,
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": item.get("call_id").or_else(|| item.get("id")).cloned()
                                .unwrap_or_else(|| json!(format!("toolu_{}", uuid::Uuid::new_v4().simple()))),
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "input": {}
                        }
                    }),
                )
                .await;
            }
        }
        "response.output_text.delta" => {
            if let Some(text) = value.get("delta").and_then(Value::as_str)
                && !text.is_empty()
            {
                state.mark_first_token(ctx.started);
                state.ensure_started(tx, ctx).await;
                let index = match state.text_index {
                    Some(index) => index,
                    None => {
                        let index = state.next_index;
                        state.next_index += 1;
                        state.text_index = Some(index);
                        send_anthropic_event(
                            tx,
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": {"type": "text", "text": ""}
                            }),
                        )
                        .await;
                        index
                    }
                };
                state.text.push_str(text);
                send_anthropic_event(
                    tx,
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "text_delta", "text": text}
                    }),
                )
                .await;
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(partial) = value.get("delta").and_then(Value::as_str)
                && !partial.is_empty()
                && let Some(index) = value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .and_then(|item_id| tool_indices.get(item_id).copied())
            {
                state.mark_first_token(ctx.started);
                send_anthropic_event(
                    tx,
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": partial}
                    }),
                )
                .await;
            }
        }
        "response.completed" | "response.incomplete" | "response.failed" => {
            let response = value.get("response").unwrap_or(&Value::Null);
            if let Some(usage) = usage_from_value(response) {
                if usage.prompt_tokens > 0 {
                    state.input_tokens = usage.prompt_tokens;
                }
                if usage.completion_tokens > 0 {
                    state.output_tokens = usage.completion_tokens;
                }
                state.cache_read_tokens = usage.cache_read_tokens;
                state.cache_write_tokens = usage.cache_write_tokens;
            }
            if response.get("status").and_then(Value::as_str) == Some("incomplete") {
                state.finish_reason = Some("length".to_string());
            }
        }
        _ => {}
    }
}

/// Pagination parameters accepted by Anthropic's model list endpoint.
///
/// Anthropic uses opaque cursor pagination rather than offsets: `after_id`
/// walks forward and `before_id` walks backward. Both are optional, and
/// `limit` defaults to 20 with a maximum of 1000.
#[derive(Debug, Default)]
struct ModelPageQuery {
    limit: Option<usize>,
    after_id: Option<String>,
    before_id: Option<String>,
}

/// Anthropic's documented default and maximum page sizes.
const ANTHROPIC_DEFAULT_PAGE_SIZE: usize = 20;
const ANTHROPIC_MAX_PAGE_SIZE: usize = 1000;

impl ModelPageQuery {
    /// Parses the raw query string, ignoring malformed parameters rather than
    /// failing the request: an unrecognized `limit` should not take down the
    /// model list.
    fn parse(uri: &Uri) -> Self {
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
    fn page_size(&self) -> usize {
        self.limit
            .unwrap_or(ANTHROPIC_DEFAULT_PAGE_SIZE)
            .clamp(1, ANTHROPIC_MAX_PAGE_SIZE)
    }
}

/// Minimal percent-decoding for cursor values.
///
/// Model ids routinely contain `/` (for example `cmd/deepseek/v4`), which
/// clients percent-encode; without decoding, the cursor would never match.
fn percent_decode(value: &str) -> String {
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

/// Anthropic token-counting endpoint (`POST /v1/messages/count_tokens`).
///
/// Claude Code calls this before sending a request to decide how much context
/// remains, so its absence breaks the client outright. Most OpenAI-compatible
/// upstreams have no equivalent (verified: CommandCode returns 404), so this
/// answers locally with an estimate rather than proxying.
///
/// The estimate is deliberately deterministic: the same body always yields the
/// same count, which clients rely on to detect real context growth.
pub async fn count_tokens_anthropic(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match count_tokens_inner(&state, &headers, &body).await {
        Ok(response) => response,
        Err(error) => anthropic_error_response(error),
    }
}

async fn count_tokens_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
) -> AppResult<Response> {
    let inbound: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    // `model` is required by the real endpoint; reject early rather than
    // silently counting a body the client meant for a different model.
    let requested_model = requested_model_of(&inbound)?;
    let api_key = authenticate_gateway(state, headers).await?;
    enforce_api_key_model_access(api_key.as_ref(), &requested_model)?;
    resolve_route(state, &requested_model, ANTHROPIC_MESSAGES).await?;

    let input_tokens = estimate_request_tokens(&inbound);
    Ok(Json(json!({"input_tokens": input_tokens})).into_response())
}

/// Inbound entry point for the Anthropic Messages API (`POST /v1/messages`).
///
/// Anthropic-protocol clients (Claude Code, the Anthropic SDKs) can point their
/// base URL at this gateway. The request is converted into the gateway's
/// internal OpenAI shape so it reuses routing, barrel clamping and capability
/// headers, then the answer is converted back into Anthropic's message and
/// event protocol. Native Anthropic targets skip the conversion entirely.
pub async fn proxy_anthropic(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let response = match proxy_anthropic_inner(&state, &headers, &uri, &body, &request_id).await {
        Ok(response) => response,
        Err(error) => anthropic_error_response(error),
    };
    with_gateway_request_id(response, &request_id)
}

/// Renders a gateway error in Anthropic's envelope and status vocabulary so an
/// Anthropic client can parse it with its usual error handling.
fn anthropic_error_response(error: AppError) -> Response {
    let (status, error_type) = match &error {
        AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
        AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
        AppError::Forbidden(_) => (StatusCode::FORBIDDEN, "permission_error"),
        AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
        AppError::Conflict(_) => (StatusCode::CONFLICT, "invalid_request_error"),
        AppError::TooManyRequests(_) => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "api_error"),
        AppError::Database(_) | AppError::Http(_) | AppError::Internal(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "api_error")
        }
    };
    if status.is_server_error() {
        tracing::error!(error = %error, "anthropic request failed");
    }
    (
        status,
        Json(anthropic_error_body(error_type, &error.to_string())),
    )
        .into_response()
}

/// Exposes the gateway request ID so clients can correlate a response with the
/// request-log row even when the upstream response is streamed or failed.
fn with_gateway_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response
            .headers_mut()
            .insert("x-openllm-request-id", value.clone());
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

async fn proxy_anthropic_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
) -> AppResult<Response> {
    let started = Instant::now();
    let endpoint = uri.path().to_string();
    let inbound: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    let requested_model = requested_model_of(&inbound)?;
    // Anthropic requires `max_tokens`; the real API answers 400 with an
    // `invalid_request_error` when it is missing (verified against the live
    // provider). Defaulting silently would hide a client bug and let the
    // request fail later with a less obvious error.
    validate_anthropic_max_tokens(&inbound)?;
    let streamed = inbound
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Convert once up front: routing, barrel clamping and token estimation all
    // operate on the OpenAI shape.
    let mut request_json = anthropic_request_to_openai(&inbound, &requested_model);
    let session_id = upstream_session_id(headers, &inbound);
    let request_tokens = estimate_request_tokens(&request_json);

    let api_key = authenticate_gateway(state, headers).await?;
    enforce_policy_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
    )
    .await?;
    let resolved = resolve_route_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
    )
    .await?;
    if let Err(error) = enforce_context_capacity(request_tokens, resolved.barrel.as_ref()) {
        let message = error.to_string();
        log_request_rejection(
            state,
            api_key.as_ref(),
            request_id,
            session_id.as_deref(),
            &requested_model,
            &endpoint,
            streamed,
            started,
            400,
            &message,
        )
        .await;
        return Err(error);
    }
    let route_id = resolved.route_id;
    let requested_output_tokens = requested_output_tokens_of(&request_json);
    let clamped_output_tokens = clamp_output_request(&mut request_json, resolved.barrel.as_ref());
    let receipt = capability_receipt(
        resolved.barrel.as_ref(),
        requested_output_tokens,
        clamped_output_tokens,
    );
    let ordering_key = route_id.unwrap_or(-1);
    let ordered_targets = order_targets(
        state,
        ordering_key,
        &resolved.strategy,
        resolved.targets,
        session_id.as_deref(),
    )
    .await?;
    enforce_api_key_rate_limit_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        request_tokens,
        streamed,
        started,
    )
    .await?;
    log_usage_started(
        state,
        request_id,
        session_id.as_deref(),
        api_key.as_ref().map(|key| key.id),
        route_id,
        &requested_model,
        &endpoint,
        request_tokens,
        streamed,
    )
    .await;

    let mut last_error = None;
    let mut last_target = None;
    for target in ordered_targets {
        let target_provider_id = target.provider_id;
        let target_provider_key_id = target.provider_api_key_id;
        let target_upstream_model = target.upstream_model.clone();
        log_usage_target(
            state,
            request_id,
            target_provider_id,
            &target_upstream_model,
            target_provider_key_id,
        )
        .await;
        mark_provider_api_key_used(state, target_provider_key_id).await;
        last_target = Some((target_provider_id, target_upstream_model));
        let result = if target.provider_type == "anthropic" {
            // Native target: forward the caller's Anthropic payload unchanged,
            // only swapping in the resolved upstream model.
            let mut native = inbound.clone();
            native["model"] = json!(target.upstream_model);
            forward_anthropic_native(
                state,
                request_id,
                &requested_model,
                native,
                target,
                streamed,
                request_tokens,
                api_key.as_ref(),
                started,
                receipt.clone(),
                anthropic_beta_of(headers),
                session_id.as_deref(),
            )
            .await
        } else {
            forward_openai_as_anthropic(
                state,
                request_id,
                &requested_model,
                &request_json,
                target,
                streamed,
                request_tokens,
                api_key.as_ref(),
                started,
                receipt.clone(),
                session_id.as_deref(),
            )
            .await
        };
        match result {
            Ok(response) => {
                if response.status().is_success() {
                    mark_provider_success(state, target_provider_id).await;
                    mark_provider_api_key_success(state, target_provider_key_id).await;
                }
                return Ok(response);
            }
            Err(error) => last_error = Some(error),
        }
    }

    let error = last_error
        .unwrap_or_else(|| AppError::Upstream("all configured route targets failed".to_string()));
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.as_ref().map(|key| key.id),
            route_id,
            provider_id: last_target.as_ref().map(|(provider_id, _)| *provider_id),
            requested_model: &requested_model,
            upstream_model: last_target
                .as_ref()
                .map(|(_, upstream_model)| upstream_model.as_str()),
            endpoint: &endpoint,
            usage: Usage::new(request_tokens, 0),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: 502,
            success: false,
            streamed,
            error_message: Some(&error.to_string()),
            response_preview: None,
        },
    )
    .await;
    Err(error)
}

/// Forwards an Anthropic-shaped request to a native Anthropic provider.
///
/// The upstream already speaks the caller's protocol, so the response can be
/// streamed straight back with no translation.
#[allow(clippy::too_many_arguments)]
async fn forward_anthropic_native(
    state: &AppState,
    request_id: &str,
    requested_model: &str,
    body: Value,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
    anthropic_beta: Option<String>,
    session_id: Option<&str>,
) -> AppResult<Response> {
    let url = join_upstream_url(&target.base_url, ANTHROPIC_MESSAGES);
    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        // Required by the Anthropic protocol regardless of authentication:
        // a keyless self-hosted Anthropic-compatible endpoint still rejects a
        // request that omits it.
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    // Feature flags such as prompt caching are opt-in per request via
    // `anthropic-beta`; dropping it silently disables them upstream.
    if let Some(beta) = &anthropic_beta {
        request = request.header("anthropic-beta", beta);
    }
    if let Some(key) = &target.api_key {
        request = request.header("x-api-key", key);
    }
    request = apply_opencode_session_header(request, &target, session_id);
    request = apply_custom_headers(request, &target.provider_headers)?;

    let response = send_provider_request(state, &target, request).await?;
    let status = response.status();
    if !status.is_success() {
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let message = String::from_utf8_lossy(&response_body)
            .chars()
            .take(600)
            .collect::<String>();
        if retryable_status(status) {
            mark_provider_error(state, target.provider_id, Some(status)).await;
        }
        if provider_key_failure(status) {
            mark_provider_api_key_error(
                state,
                target.provider_api_key_id,
                status,
                &format!("{} returned {}: {}", target.provider_name, status, message),
            )
            .await;
        }
        if should_try_next_target(&target, status) {
            return Err(AppError::Upstream(format!(
                "{} returned {}: {}",
                target.provider_name, status, message
            )));
        }
        let preview = response_preview(&response_body);
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
                usage: Usage::new(request_tokens, 0),
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
                status_code: status.as_u16() as i64,
                success: false,
                streamed,
                error_message: Some(&message),
                response_preview: preview.as_deref(),
            },
        )
        .await;
        // Preserve the upstream status and Anthropic-shaped error body.
        return Ok(Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_body))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
    }

    if streamed {
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        return Ok(passthrough_stream_response(
            state.clone(),
            response,
            content_type,
            ANTHROPIC_MESSAGES.to_string(),
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
            receipt,
        ));
    }

    let response_bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let usage = extract_usage_from_json(&response_bytes)
        .unwrap_or_else(|| estimated_completion_usage(request_tokens, &response_bytes));
    let preview = response_preview(&response_bytes);
    let latency_ms = started.elapsed().as_millis() as i64;
    log_usage_detached(
        state.clone(),
        OwnedUsageLogEntry {
            request_id: request_id.to_string(),
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model: requested_model.to_string(),
            upstream_model: Some(target.upstream_model.clone()),
            endpoint: ANTHROPIC_MESSAGES.to_string(),
            usage,
            latency_ms,
            first_token_ms: Some(latency_ms),
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview,
        },
    );
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Body::from(response_bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}

/// Forwards an OpenAI-shaped request to an OpenAI-compatible provider and
/// converts the completion back into an Anthropic message.
#[allow(clippy::too_many_arguments)]
async fn forward_openai_as_anthropic(
    state: &AppState,
    request_id: &str,
    requested_model: &str,
    request_json: &Value,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
    session_id: Option<&str>,
) -> AppResult<Response> {
    let upstream_endpoint =
        target_upstream_endpoint(&target, ANTHROPIC_MESSAGES).unwrap_or(OPENAI_CHAT_COMPLETIONS);
    let use_responses = upstream_endpoint == OPENAI_RESPONSES;
    let body = if use_responses {
        chat_request_to_responses(request_json, &target.upstream_model, streamed)
    } else {
        let mut body = request_json.clone();
        body["model"] = json!(target.upstream_model);
        body
    };
    let url = join_upstream_url(&target.base_url, upstream_endpoint);
    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body);
    if let Some(key) = &target.api_key {
        request = request.bearer_auth(key);
    }
    request = apply_opencode_session_header(request, &target, session_id);
    request = apply_custom_headers(request, &target.provider_headers)?;

    let response = send_provider_request(state, &target, request).await?;
    let status = response.status();
    if !status.is_success() {
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let message = String::from_utf8_lossy(&response_body)
            .chars()
            .take(600)
            .collect::<String>();
        if retryable_status(status) {
            mark_provider_error(state, target.provider_id, Some(status)).await;
        }
        if provider_key_failure(status) {
            mark_provider_api_key_error(
                state,
                target.provider_api_key_id,
                status,
                &format!("{} returned {}: {}", target.provider_name, status, message),
            )
            .await;
        }
        if should_try_next_target(&target, status) {
            return Err(AppError::Upstream(format!(
                "{} returned {}: {}",
                target.provider_name, status, message
            )));
        }
        // Non-retryable: surface the upstream failure verbatim so the operator
        // sees the provider's own message rather than a generic gateway error.
        let preview = response_preview(&response_body);
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: ANTHROPIC_MESSAGES,
                usage: Usage::new(request_tokens, 0),
                latency_ms: started.elapsed().as_millis() as i64,
                first_token_ms: None,
                status_code: status.as_u16() as i64,
                success: false,
                streamed,
                error_message: Some(&message),
                response_preview: preview.as_deref(),
            },
        )
        .await;
        return Ok(Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&anthropic_error_body("api_error", &message))
                    .unwrap_or_default(),
            ))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()));
    }

    if streamed {
        if use_responses {
            return Ok(responses_stream_to_anthropic(
                state.clone(),
                response,
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
            ));
        }
        return Ok(openai_stream_to_anthropic(
            state.clone(),
            response,
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let upstream: Value = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::Upstream(format!("invalid JSON from upstream: {error}")))?;
    let (converted, usage) = if use_responses {
        responses_response_to_anthropic(&upstream, requested_model)
    } else {
        openai_response_to_anthropic(&upstream, requested_model)
    };
    let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
    let preview = response_preview(converted_bytes.as_slice());
    let latency_ms = started.elapsed().as_millis() as i64;
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model,
            upstream_model: Some(&target.upstream_model),
            endpoint: ANTHROPIC_MESSAGES,
            usage: fill_usage(usage, request_tokens, &converted),
            latency_ms,
            first_token_ms: Some(latency_ms),
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview.as_deref(),
        },
    )
    .await;
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Body::from(converted_bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}

pub async fn proxy_openai(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let result = proxy_openai_inner(&state, &headers, &uri, &body, &request_id, None).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    with_gateway_request_id(response, &request_id)
}

pub async fn proxy_openai_console(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let result = proxy_openai_console_inner(&state, &headers, &body, &request_id).await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    with_gateway_request_id(response, &request_id)
}

async fn proxy_openai_console_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: &str,
) -> AppResult<Response> {
    let api_key = selected_console_api_key(state, headers).await?;
    let uri = Uri::from_static(OPENAI_CHAT_COMPLETIONS);
    proxy_openai_inner(state, headers, &uri, body, request_id, api_key).await
}

async fn proxy_openai_inner(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    request_id: &str,
    api_key_override: Option<ApiKeyRecord>,
) -> AppResult<Response> {
    let started = Instant::now();
    let endpoint = uri.path().to_string();
    let mut request_json: Value = serde_json::from_slice(body).map_err(|error| {
        AppError::BadRequest(format!("request body must be valid JSON: {error}"))
    })?;
    let requested_model = requested_model_of(&request_json)?;
    let session_id = upstream_session_id(headers, &request_json);
    let streamed = request_json
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_tokens = estimate_request_tokens(&request_json);

    let api_key = match api_key_override {
        Some(api_key) => Some(api_key),
        None => authenticate_gateway(state, headers).await?,
    };
    enforce_policy_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
    )
    .await?;
    let resolved = resolve_route_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        streamed,
        started,
    )
    .await?;
    if let Err(error) = enforce_context_capacity(request_tokens, resolved.barrel.as_ref()) {
        let message = error.to_string();
        log_request_rejection(
            state,
            api_key.as_ref(),
            request_id,
            session_id.as_deref(),
            &requested_model,
            &endpoint,
            streamed,
            started,
            400,
            &message,
        )
        .await;
        return Err(error);
    }
    let route_id = resolved.route_id;
    // Barrel mode: clamp the requested output length to the strictest common
    // ceiling across every target, so no target is picked that would reject the
    // request as too large.
    let requested_output_tokens = requested_output_tokens_of(&request_json);
    let clamped_output_tokens = clamp_output_request(&mut request_json, resolved.barrel.as_ref());
    let receipt = capability_receipt(
        resolved.barrel.as_ref(),
        requested_output_tokens,
        clamped_output_tokens,
    );
    let ordering_key = route_id.unwrap_or(-1);
    let ordered_targets = order_targets(
        state,
        ordering_key,
        &resolved.strategy,
        resolved.targets,
        session_id.as_deref(),
    )
    .await?;
    enforce_api_key_rate_limit_or_log(
        state,
        api_key.as_ref(),
        request_id,
        session_id.as_deref(),
        &requested_model,
        &endpoint,
        request_tokens,
        streamed,
        started,
    )
    .await?;
    log_usage_started(
        state,
        request_id,
        session_id.as_deref(),
        api_key.as_ref().map(|key| key.id),
        route_id,
        &requested_model,
        &endpoint,
        request_tokens,
        streamed,
    )
    .await;

    let mut last_error = None;
    let mut last_target = None;
    for target in ordered_targets {
        let target_provider_id = target.provider_id;
        let target_provider_key_id = target.provider_api_key_id;
        let target_upstream_model = target.upstream_model.clone();
        last_target = Some((target_provider_id, target_upstream_model.clone()));
        if target.provider_type == "anthropic"
            && endpoint != OPENAI_CHAT_COMPLETIONS
            && endpoint != OPENAI_COMPLETIONS
            && endpoint != OPENAI_RESPONSES
        {
            last_error = Some(format!(
                "{} does not support the {} endpoint",
                target.provider_name, endpoint
            ));
            continue;
        }

        log_usage_target(
            state,
            request_id,
            target_provider_id,
            &target_upstream_model,
            target_provider_key_id,
        )
        .await;
        mark_provider_api_key_used(state, target_provider_key_id).await;
        match forward_to_target(
            state,
            request_id,
            &endpoint,
            &requested_model,
            &request_json,
            body,
            session_id.as_deref(),
            target,
            streamed,
            request_tokens,
            api_key.as_ref(),
            started,
            receipt.clone(),
        )
        .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    mark_provider_success(state, target_provider_id).await;
                    mark_provider_api_key_success(state, target_provider_key_id).await;
                }
                return Ok(response);
            }
            Err(AppError::BadRequest(message)) => {
                last_error = Some(message);
            }
            Err(AppError::Upstream(message)) => {
                last_error = Some(message);
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
    }

    let message = last_error.unwrap_or_else(|| "all configured route targets failed".to_string());
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.as_ref().map(|key| key.id),
            route_id,
            provider_id: last_target.as_ref().map(|(provider_id, _)| *provider_id),
            requested_model: &requested_model,
            upstream_model: last_target
                .as_ref()
                .map(|(_, upstream_model)| upstream_model.as_str()),
            endpoint: &endpoint,
            usage: Usage::new(request_tokens, 0),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: 502,
            success: false,
            streamed,
            error_message: Some(&message),
            response_preview: None,
        },
    )
    .await;
    Err(AppError::Upstream(message))
}

fn build_upstream_request(
    state: &AppState,
    url: &str,
    provider_type: ProviderType,
    target: &RouteTarget,
    request_body: &Value,
    session_id: Option<&str>,
) -> AppResult<RequestBuilder> {
    let mut request = state
        .client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(request_body);

    request = match provider_type {
        ProviderType::Anthropic => {
            if let Some(key) = &target.api_key {
                request = request
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01");
            }
            request
        }
        _ => {
            if let Some(key) = &target.api_key {
                request = request.bearer_auth(key);
            }
            request
        }
    };
    request = apply_opencode_session_header(request, target, session_id);
    apply_custom_headers(request, &target.provider_headers)
}

fn strip_tool_search_tools(body: &Value) -> Option<Value> {
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

pub(crate) fn upstream_rejects_tool_search(body: &[u8]) -> bool {
    let message = String::from_utf8_lossy(body).to_ascii_lowercase();
    if !message.contains("tool_search") {
        return false;
    }
    [
        "unknown tool type",
        "unsupported tool type",
        "invalid tool type",
        "tool.type",
        "tool_search is not supported",
        "tool_search not supported",
        "tool_search is unsupported",
        "does not support tool_search",
        "doesn't support tool_search",
        "unsupported tool_search",
        "unknown tool: tool_search",
        "unsupported tool: tool_search",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

async fn mark_provider_tool_search_unsupported(state: &AppState, provider_id: i64) {
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

#[allow(clippy::too_many_arguments)]
async fn upstream_error_response(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    target: &RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    status: StatusCode,
    response_headers: HeaderMap,
    response_body: Bytes,
) -> AppResult<Response> {
    let message = String::from_utf8_lossy(&response_body)
        .chars()
        .take(600)
        .collect::<String>();

    if retryable_status(status) {
        mark_provider_error(state, target.provider_id, Some(status)).await;
    }
    if provider_key_failure(status) {
        mark_provider_api_key_error(
            state,
            target.provider_api_key_id,
            status,
            &format!("{} returned {}: {}", target.provider_name, status, message),
        )
        .await;
    }

    if should_try_next_target(target, status) {
        tracing::warn!(
            provider = %target.provider_name,
            model = %target.upstream_model,
            %status,
            "upstream failed, trying next route target"
        );
        return Err(AppError::Upstream(format!(
            "{} returned {}: {}",
            target.provider_name, status, message
        )));
    }

    let preview = response_preview(&response_body);
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model,
            upstream_model: Some(&target.upstream_model),
            endpoint,
            usage: Usage::new(request_tokens, 0),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code: status.as_u16() as i64,
            success: false,
            streamed,
            error_message: Some(&message),
            response_preview: preview.as_deref(),
        },
    )
    .await;

    let mut builder = Response::builder().status(status);
    if let Some(value) = response_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        builder = builder.header(reqwest::header::CONTENT_TYPE, value);
    }
    Ok(builder
        .body(Body::from(response_body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()))
}

#[allow(clippy::too_many_arguments)]
async fn forward_to_target(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    requested_model: &str,
    request_json: &Value,
    _raw_body: &Bytes,
    session_id: Option<&str>,
    target: RouteTarget,
    streamed: bool,
    request_tokens: i64,
    api_key: Option<&ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> AppResult<Response> {
    let provider_type =
        ProviderType::from_str(&target.provider_type).map_err(AppError::BadRequest)?;
    let upstream_endpoint = target_upstream_endpoint(&target, endpoint).unwrap_or(endpoint);
    // Chat completions upstreams can serve a Responses client through
    // translation; everything else is a straight passthrough.
    let translate_responses_to_chat =
        endpoint == OPENAI_RESPONSES && upstream_endpoint == OPENAI_CHAT_COMPLETIONS;
    let translate_chat_to_responses =
        endpoint == OPENAI_CHAT_COMPLETIONS && upstream_endpoint == OPENAI_RESPONSES;

    let (url, mut request_body) = match provider_type {
        ProviderType::Anthropic => {
            let body = match endpoint {
                OPENAI_RESPONSES => {
                    responses_request_to_anthropic(request_json, &target.upstream_model, streamed)
                }
                OPENAI_COMPLETIONS => {
                    completions_request_to_anthropic(request_json, &target.upstream_model, streamed)
                }
                _ => convert_request_to_anthropic(request_json, &target.upstream_model, streamed),
            };
            (join_upstream_url(&target.base_url, "/v1/messages"), body)
        }
        ProviderType::Openai | ProviderType::Ollama | ProviderType::Custom => {
            let body = if translate_responses_to_chat {
                responses_request_to_chat(request_json, &target.upstream_model, streamed)
            } else if translate_chat_to_responses {
                chat_request_to_responses(request_json, &target.upstream_model, streamed)
            } else {
                let mut body = request_json.clone();
                body["model"] = json!(target.upstream_model);
                body
            };
            (join_upstream_url(&target.base_url, upstream_endpoint), body)
        }
    };

    if target.tool_search_supported == 0
        && let Some(compat_body) = strip_tool_search_tools(&request_body)
    {
        request_body = compat_body;
    }

    let mut response = send_provider_request(
        state,
        &target,
        build_upstream_request(
            state,
            &url,
            provider_type,
            &target,
            &request_body,
            session_id,
        )?,
    )
    .await?;
    let mut status = response.status();
    let mut transient_retry_used = false;

    while !status.is_success() {
        let response_headers = response.headers().clone();
        let response_body = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;

        // Aggregator gateways sometimes answer with an opaque
        // "invalid request error" carrying only a trace id when one of their
        // internal channels fails. That is a transient routing failure rather
        // than a client error, so resend the same request once before giving
        // up on the target.
        if !transient_retry_used && transient_upstream_4xx(status, &response_body) {
            transient_retry_used = true;
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                %status,
                "upstream returned an opaque 4xx; retrying once"
            );
            response = send_provider_request(
                state,
                &target,
                build_upstream_request(
                    state,
                    &url,
                    provider_type,
                    &target,
                    &request_body,
                    session_id,
                )?,
            )
            .await?;
            status = response.status();
            continue;
        }

        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ) && upstream_rejects_tool_search(&response_body)
            && let Some(compat_body) = strip_tool_search_tools(&request_body)
        {
            tracing::warn!(
                provider = %target.provider_name,
                model = %target.upstream_model,
                "upstream rejected tool_search; retrying without it"
            );
            mark_provider_tool_search_unsupported(state, target.provider_id).await;
            request_body = compat_body;
            response = send_provider_request(
                state,
                &target,
                build_upstream_request(
                    state,
                    &url,
                    provider_type,
                    &target,
                    &request_body,
                    session_id,
                )?,
            )
            .await?;
            status = response.status();
            continue;
        }

        return upstream_error_response(
            state,
            request_id,
            endpoint,
            requested_model,
            &target,
            streamed,
            request_tokens,
            api_key,
            started,
            status,
            response_headers,
            response_body,
        )
        .await;
    }

    let response_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if provider_type == ProviderType::Anthropic {
        if streamed {
            if endpoint == OPENAI_RESPONSES {
                return Ok(anthropic_stream_to_responses(
                    state.clone(),
                    response,
                    request_id.to_string(),
                    requested_model.to_string(),
                    target,
                    request_tokens,
                    api_key.cloned(),
                    started,
                    receipt,
                ));
            }
            if endpoint == OPENAI_COMPLETIONS {
                return Ok(anthropic_stream_to_completions(
                    state.clone(),
                    response,
                    request_id.to_string(),
                    requested_model.to_string(),
                    target,
                    request_tokens,
                    api_key.cloned(),
                    started,
                    receipt,
                ));
            }
            return Ok(anthropic_stream_response(
                state.clone(),
                response,
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt,
            ));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::Upstream(format!("invalid JSON from Anthropic: {error}")))?;
        let (converted, usage) = match endpoint {
            OPENAI_RESPONSES => anthropic_response_to_responses(&upstream_json, requested_model),
            OPENAI_COMPLETIONS => {
                anthropic_response_to_completions(&upstream_json, requested_model)
            }
            _ => convert_anthropic_response(&upstream_json),
        };
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if translate_responses_to_chat {
        if streamed {
            return Ok(openai_chat_stream_to_responses(
                state.clone(),
                response,
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
                receipt,
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Upstream(format!("invalid JSON from chat completions: {error}"))
        })?;
        let (converted, usage) = chat_response_to_responses(&upstream_json, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if translate_chat_to_responses {
        if streamed {
            return Ok(responses_stream_to_chat(
                state.clone(),
                response,
                request_id.to_string(),
                requested_model.to_string(),
                target,
                request_tokens,
                api_key.cloned(),
                started,
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let upstream_json: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Upstream(format!("invalid JSON from responses upstream: {error}"))
        })?;
        let (converted, usage) = responses_response_to_chat(&upstream_json, requested_model);
        let converted_bytes = serde_json::to_vec(&converted).unwrap_or_default();
        let converted_bytes =
            inject_capability_receipt(&converted_bytes, &receipt).unwrap_or(converted_bytes);
        let preview = response_preview(&converted_bytes);
        let latency_ms = started.elapsed().as_millis() as i64;
        log_usage(
            state,
            UsageLogEntry {
                request_id,
                api_key_id: api_key.map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint,
                usage: fill_usage(usage, request_tokens, &converted),
                latency_ms,
                first_token_ms: Some(latency_ms),
                status_code: status.as_u16() as i64,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: preview.as_deref(),
            },
        )
        .await;
        let mut response = Response::builder()
            .status(status)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(Body::from(converted_bytes))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        apply_capability_headers(&mut response, &receipt);
        return Ok(response);
    }

    if streamed {
        return Ok(passthrough_stream_response(
            state.clone(),
            response,
            response_content_type,
            endpoint.to_string(),
            request_id.to_string(),
            requested_model.to_string(),
            target,
            request_tokens,
            api_key.cloned(),
            started,
            receipt,
        ));
    }

    let response_bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let usage = extract_usage_from_json(&response_bytes)
        .unwrap_or_else(|| estimated_completion_usage(request_tokens, &response_bytes));
    let response_text = String::from_utf8_lossy(&response_bytes)
        .chars()
        .take(600)
        .collect::<String>();
    let preview = response_preview(&response_bytes);
    let latency_ms = started.elapsed().as_millis() as i64;
    // Detach: the caller should not wait on a SQLite write to receive the
    // upstream response they already paid for.
    log_usage_detached(
        state.clone(),
        OwnedUsageLogEntry {
            request_id: request_id.to_string(),
            api_key_id: api_key.map(|key| key.id),
            route_id: target.route_id,
            provider_id: Some(target.provider_id),
            requested_model: requested_model.to_string(),
            upstream_model: Some(target.upstream_model.clone()),
            endpoint: endpoint.to_string(),
            usage,
            latency_ms,
            first_token_ms: Some(latency_ms),
            status_code: status.as_u16() as i64,
            success: true,
            streamed: false,
            error_message: None,
            response_preview: preview,
        },
    );

    // Inject the receipt only when the body is a JSON object, and only after
    // usage logging has read the original bytes.
    let body_bytes = inject_capability_receipt(&response_bytes, &receipt)
        .map(Bytes::from)
        .unwrap_or(response_bytes);
    let mut response = Response::builder()
        .status(status)
        .header(reqwest::header::CONTENT_TYPE, response_content_type)
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| {
            AppError::Upstream(format!("could not forward response: {response_text}"))
                .into_response()
        });
    apply_capability_headers(&mut response, &receipt);
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
fn passthrough_stream_response(
    state: AppState,
    response: reqwest::Response,
    content_type: String,
    endpoint: String,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let mut upstream = response.bytes_stream();
        let mut parser = UsageParser::new(started);
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    heartbeat.touch().await;
                    parser.push(&bytes);
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    let _ = tx.send(Err(io::Error::other(error))).await;
                    break;
                }
            }
        }
        let usage = parser
            .finish()
            .map(|mut usage| {
                if usage.prompt_tokens == 0 {
                    usage.prompt_tokens = request_tokens;
                }
                usage.normalized()
            })
            .unwrap_or_else(|| Usage {
                prompt_tokens: request_tokens,
                completion_tokens: 1,
                total_tokens: request_tokens + 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = parser
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = parser.preview();
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: &endpoint,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

#[allow(clippy::too_many_arguments)]
fn anthropic_stream_response(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let message_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut sent_role = false;
        let mut saw_tool_use = false;
        // Anthropic numbers every content block (text and tools alike), while
        // OpenAI numbers only tool calls. Track the mapping so a text block
        // before a tool call does not shift the tool index.
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut stream_error = None;
        let mut text = String::new();
        let mut first_token_ms = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_anthropic_line(
                    &line,
                    &mut event_name,
                    &mut usage,
                    &mut output_chars,
                    &mut text,
                    &mut sent_role,
                    &mut saw_tool_use,
                    &mut next_tool_index,
                    &mut tool_indices,
                    &mut first_token_ms,
                    started,
                    &message_id,
                    &requested_model,
                    &tx,
                )
                .await;
            }
        }

        if !buffer.is_empty() {
            process_anthropic_line(
                &buffer,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                started,
                &message_id,
                &requested_model,
                &tx,
            )
            .await;
        }

        // Prefer the real token counts Anthropic reported (message_start /
        // message_delta) and only fall back to our estimate when absent.
        let prompt_tokens = if usage.prompt_tokens > 0 {
            usage.prompt_tokens
        } else {
            request_tokens
        };
        let completion_tokens = if usage.completion_tokens > 0 {
            usage.completion_tokens
        } else {
            (output_chars / 4) as i64
        };
        usage.prompt_tokens = prompt_tokens;
        usage.completion_tokens = completion_tokens;
        usage.total_tokens = prompt_tokens + completion_tokens;

        // Emit the terminal choice first so clients observe `finish_reason`
        // (`tool_calls` triggers tool execution in the OpenAI SDKs), then send
        // usage in a separate chunk — a usage-bearing chunk carries no choices
        // and would otherwise swallow the finish reason entirely.
        let final_chunk = openai_stream_chunk(
            &message_id,
            &requested_model,
            json!({}),
            Some(if saw_tool_use { "tool_calls" } else { "stop" }),
            None,
        );
        let usage_chunk = openai_stream_chunk(
            &message_id,
            &requested_model,
            json!({}),
            None,
            Some(json!({
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": usage.total_tokens
            })),
        );
        let _ = tx
            .send(Ok(Bytes::from(format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                serde_json::to_string(&final_chunk).unwrap_or_default(),
                serde_json::to_string(&usage_chunk).unwrap_or_default()
            ))))
            .await;
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms =
            first_token_ms.or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(text.as_bytes());
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_CHAT_COMPLETIONS,
                usage: usage.normalized(),
                latency_ms,
                first_token_ms,
                status_code: 200,
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

#[derive(Debug)]
enum ResponsesStreamBlock {
    Text {
        item_id: String,
        output_index: usize,
        text: String,
    },
    Tool {
        item_id: String,
        output_index: usize,
        call_id: Value,
        name: Value,
        arguments: String,
    },
}

struct ResponsesStreamState {
    response_id: String,
    model: String,
    created_at: i64,
    sequence_number: i64,
    created: bool,
    output: Vec<Value>,
    current: Option<ResponsesStreamBlock>,
    usage: Usage,
    stop_reason: Option<String>,
    first_token_ms: Option<i64>,
    output_chars: usize,
    started: Instant,
}

impl ResponsesStreamState {
    fn new(model: String, started: Instant) -> Self {
        Self {
            response_id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            model,
            created_at: chrono::Utc::now().timestamp(),
            sequence_number: 0,
            created: false,
            output: Vec::new(),
            current: None,
            usage: Usage::default(),
            stop_reason: None,
            first_token_ms: None,
            output_chars: 0,
            started,
        }
    }

    fn response_object(&self, status: &str) -> Value {
        let incomplete = status == "incomplete";
        let usage = if status == "in_progress" {
            Value::Null
        } else {
            responses_usage_json(&self.usage)
        };
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "error": Value::Null,
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
            "instructions": Value::Null,
            "max_output_tokens": Value::Null,
            "model": self.model,
            "output": self.output,
            "parallel_tool_calls": true,
            "previous_response_id": Value::Null,
            "reasoning": Value::Null,
            "store": false,
            "temperature": Value::Null,
            "text": {"format": {"type": "text"}},
            "tool_choice": "auto",
            "tools": [],
            "top_p": Value::Null,
            "truncation": "disabled",
            "usage": usage
        })
    }

    async fn send_event(
        &mut self,
        tx: &mpsc::Sender<Result<Bytes, io::Error>>,
        event: &str,
        mut data: Value,
    ) {
        data["sequence_number"] = json!(self.sequence_number);
        self.sequence_number += 1;
        let _ = tx.send(Ok(Bytes::from(sse_line(event, data)))).await;
    }

    async fn ensure_created(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        if self.created {
            return;
        }
        self.created = true;
        let response = self.response_object("in_progress");
        self.send_event(
            tx,
            "response.created",
            json!({"type": "response.created", "response": response.clone()}),
        )
        .await;
        self.send_event(
            tx,
            "response.in_progress",
            json!({"type": "response.in_progress", "response": response}),
        )
        .await;
    }

    fn mark_first_token(&mut self) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(self.started.elapsed().as_millis() as i64);
        }
    }

    async fn finish_current(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        let Some(current) = self.current.take() else {
            return;
        };
        match current {
            ResponsesStreamBlock::Text {
                item_id,
                output_index,
                text,
            } => {
                self.send_event(
                    tx,
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text
                    }),
                )
                .await;
                let part = json!({
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                    "logprobs": []
                });
                self.send_event(
                    tx,
                    "response.content_part.done",
                    json!({
                        "type": "response.content_part.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": part
                    }),
                )
                .await;
                let item = json!({
                    "id": item_id,
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [part]
                });
                self.send_event(
                    tx,
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item
                    }),
                )
                .await;
                self.output.push(item);
            }
            ResponsesStreamBlock::Tool {
                item_id,
                output_index,
                call_id,
                name,
                arguments,
            } => {
                let arguments = if arguments.is_empty() {
                    "{}".to_string()
                } else {
                    arguments
                };
                self.send_event(
                    tx,
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "arguments": arguments
                    }),
                )
                .await;
                let item = json!({
                    "id": item_id,
                    "type": "function_call",
                    "status": "completed",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments
                });
                self.send_event(
                    tx,
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item
                    }),
                )
                .await;
                self.output.push(item);
            }
        }
    }

    async fn handle_line(
        &mut self,
        line: &[u8],
        event_name: &mut String,
        tx: &mpsc::Sender<Result<Bytes, io::Error>>,
    ) {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if let Some(event) = line.strip_prefix("event:") {
            *event_name = event.trim().to_string();
            return;
        }
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        self.ensure_created(tx).await;

        match event_name.as_str() {
            "message_start" => {
                if let Some(message_usage) = value.pointer("/message/usage") {
                    if let Some(input_tokens) =
                        message_usage.get("input_tokens").and_then(Value::as_i64)
                    {
                        self.usage.prompt_tokens = input_tokens;
                    }
                    self.usage.cache_read_tokens = cache_read_of(message_usage);
                    self.usage.cache_write_tokens = cache_write_of(message_usage);
                }
            }
            "content_block_start" => {
                if self.current.is_some() {
                    self.finish_current(tx).await;
                }
                let content_block = value.get("content_block").unwrap_or(&Value::Null);
                match content_block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let item_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
                        let output_index = self.output.len();
                        let initial = content_block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.current = Some(ResponsesStreamBlock::Text {
                            item_id: item_id.clone(),
                            output_index,
                            text: initial.clone(),
                        });
                        self.send_event(
                            tx,
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": output_index,
                                "item": {
                                    "id": item_id,
                                    "type": "message",
                                    "status": "in_progress",
                                    "role": "assistant",
                                    "content": []
                                }
                            }),
                        )
                        .await;
                        self.send_event(
                            tx,
                            "response.content_part.added",
                            json!({
                                "type": "response.content_part.added",
                                "item_id": item_id,
                                "output_index": output_index,
                                "content_index": 0,
                                "part": {
                                    "type": "output_text",
                                    "text": "",
                                    "annotations": [],
                                    "logprobs": []
                                }
                            }),
                        )
                        .await;
                        if !initial.is_empty() {
                            self.mark_first_token();
                            self.output_chars += initial.chars().count();
                            self.send_event(
                                tx,
                                "response.output_text.delta",
                                json!({
                                    "type": "response.output_text.delta",
                                    "item_id": item_id,
                                    "output_index": output_index,
                                    "content_index": 0,
                                    "delta": initial
                                }),
                            )
                            .await;
                        }
                    }
                    Some("tool_use") => {
                        let item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());
                        let output_index = self.output.len();
                        let call_id = content_block.get("id").cloned().unwrap_or_else(|| {
                            json!(format!("call_{}", uuid::Uuid::new_v4().simple()))
                        });
                        let name = content_block.get("name").cloned().unwrap_or(Value::Null);
                        self.current = Some(ResponsesStreamBlock::Tool {
                            item_id: item_id.clone(),
                            output_index,
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                        });
                        self.mark_first_token();
                        self.send_event(
                            tx,
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": output_index,
                                "item": {
                                    "id": item_id,
                                    "type": "function_call",
                                    "status": "in_progress",
                                    "call_id": call_id,
                                    "name": name,
                                    "arguments": ""
                                }
                            }),
                        )
                        .await;
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let Some(text) = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                        else {
                            return;
                        };
                        let (item_id, output_index) = match self.current.as_ref() {
                            Some(ResponsesStreamBlock::Text {
                                item_id,
                                output_index,
                                ..
                            }) => (item_id.clone(), *output_index),
                            _ => return,
                        };
                        if let Some(ResponsesStreamBlock::Text {
                            text: accumulated, ..
                        }) = self.current.as_mut()
                        {
                            accumulated.push_str(&text);
                        }
                        self.output_chars += text.chars().count();
                        self.mark_first_token();
                        self.send_event(
                            tx,
                            "response.output_text.delta",
                            json!({
                                "type": "response.output_text.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "content_index": 0,
                                "delta": text
                            }),
                        )
                        .await;
                    }
                    Some("input_json_delta") => {
                        let Some(partial) = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                        else {
                            return;
                        };
                        let (item_id, output_index) = match self.current.as_ref() {
                            Some(ResponsesStreamBlock::Tool {
                                item_id,
                                output_index,
                                ..
                            }) => (item_id.clone(), *output_index),
                            _ => return,
                        };
                        if let Some(ResponsesStreamBlock::Tool { arguments, .. }) =
                            self.current.as_mut()
                        {
                            arguments.push_str(&partial);
                        }
                        self.output_chars += partial.chars().count();
                        self.mark_first_token();
                        self.send_event(
                            tx,
                            "response.function_call_arguments.delta",
                            json!({
                                "type": "response.function_call_arguments.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "delta": partial
                            }),
                        )
                        .await;
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str) {
                            self.output_chars += thinking.chars().count();
                            self.mark_first_token();
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                self.finish_current(tx).await;
            }
            "message_delta" => {
                if let Some(output_tokens) = value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_i64)
                {
                    self.usage.completion_tokens = output_tokens;
                }
                if let Some(stop_reason) =
                    value.pointer("/delta/stop_reason").and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop_reason.to_string());
                }
            }
            _ => {}
        }
    }

    async fn complete(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>) {
        self.finish_current(tx).await;
        let status = if self.stop_reason.as_deref() == Some("max_tokens") {
            "incomplete"
        } else {
            "completed"
        };
        let response = self.response_object(status);
        self.send_event(
            tx,
            "response.completed",
            json!({"type": "response.completed", "response": response}),
        )
        .await;
    }

    async fn fail(&mut self, tx: &mpsc::Sender<Result<Bytes, io::Error>>, message: &str) {
        self.finish_current(tx).await;
        let mut response = self.response_object("failed");
        response["error"] = json!({"code": "upstream_error", "message": message});
        self.send_event(
            tx,
            "response.failed",
            json!({"type": "response.failed", "response": response}),
        )
        .await;
    }
}

/// Streams a Responses API event stream from an OpenAI chat-completions chunk
/// stream, so Responses clients can use chat-only upstreams.
#[allow(clippy::too_many_arguments)]
fn openai_chat_stream_to_responses(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let mut stream = ResponsesStreamState::new(requested_model.clone(), started);
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut tool_index: Option<i64> = None;
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_chat_chunk_line(&mut stream, &line, &mut tool_index, &tx).await;
            }
        }
        if !buffer.is_empty() {
            process_chat_chunk_line(&mut stream, &buffer, &mut tool_index, &tx).await;
        }

        if let Some(error) = stream_error.as_deref() {
            stream.fail(&tx, error).await;
        } else {
            stream.ensure_created(&tx).await;
            stream.complete(&tx).await;
        }
        drop(tx);

        if stream.usage.prompt_tokens == 0 {
            stream.usage.prompt_tokens = request_tokens;
        }
        if stream.usage.completion_tokens == 0 {
            stream.usage.completion_tokens = (stream.output_chars / 4) as i64;
        }
        let usage = stream.usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(
            stream
                .output
                .iter()
                .filter_map(|item| {
                    item.pointer("/content/0/text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            item.get("arguments")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        })
                })
                .collect::<Vec<_>>()
                .join("")
                .as_bytes(),
        );
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_RESPONSES,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

async fn process_chat_chunk_line(
    stream: &mut ResponsesStreamState,
    line: &[u8],
    tool_index: &mut Option<i64>,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    stream.ensure_created(tx).await;

    if value.get("usage").is_some_and(|usage| !usage.is_null())
        && let Some(usage) = usage_from_value(&value)
    {
        stream.usage = usage;
    }

    let Some(choice) = value.pointer("/choices/0") else {
        return;
    };
    let delta = choice.get("delta").unwrap_or(&Value::Null);

    if let Some(content) = delta.get("content").and_then(Value::as_str)
        && !content.is_empty()
    {
        if !matches!(stream.current, Some(ResponsesStreamBlock::Text { .. })) {
            stream.finish_current(tx).await;
            let item_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
            let output_index = stream.output.len();
            stream.current = Some(ResponsesStreamBlock::Text {
                item_id: item_id.clone(),
                output_index,
                text: String::new(),
            });
            stream
                .send_event(
                    tx,
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": []
                        }
                    }),
                )
                .await;
            stream
                .send_event(
                    tx,
                    "response.content_part.added",
                    json!({
                        "type": "response.content_part.added",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": {
                            "type": "output_text",
                            "text": "",
                            "annotations": [],
                            "logprobs": []
                        }
                    }),
                )
                .await;
        }
        let (item_id, output_index) = match stream.current.as_ref() {
            Some(ResponsesStreamBlock::Text {
                item_id,
                output_index,
                ..
            }) => (item_id.clone(), *output_index),
            _ => return,
        };
        if let Some(ResponsesStreamBlock::Text { text, .. }) = stream.current.as_mut() {
            text.push_str(content);
        }
        stream.output_chars += content.chars().count();
        stream.mark_first_token();
        stream
            .send_event(
                tx,
                "response.output_text.delta",
                json!({
                    "type": "response.output_text.delta",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "delta": content
                }),
            )
            .await;
    }

    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let index = call.get("index").and_then(Value::as_i64).unwrap_or(0);
            let start_new = !matches!(
                (&stream.current, *tool_index),
                (Some(ResponsesStreamBlock::Tool { .. }), Some(current)) if current == index
            );
            if start_new {
                stream.finish_current(tx).await;
                let item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());
                let output_index = stream.output.len();
                let call_id = call
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple())));
                let name = call
                    .pointer("/function/name")
                    .cloned()
                    .unwrap_or(Value::Null);
                stream.current = Some(ResponsesStreamBlock::Tool {
                    item_id: item_id.clone(),
                    output_index,
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                *tool_index = Some(index);
                stream
                    .send_event(
                        tx,
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {
                                "id": item_id,
                                "type": "function_call",
                                "status": "in_progress",
                                "call_id": call_id,
                                "name": name,
                                "arguments": ""
                            }
                        }),
                    )
                    .await;
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                && !arguments.is_empty()
            {
                let (item_id, output_index) = match stream.current.as_ref() {
                    Some(ResponsesStreamBlock::Tool {
                        item_id,
                        output_index,
                        ..
                    }) => (item_id.clone(), *output_index),
                    _ => continue,
                };
                if let Some(ResponsesStreamBlock::Tool { arguments: acc, .. }) =
                    stream.current.as_mut()
                {
                    acc.push_str(arguments);
                }
                stream.output_chars += arguments.chars().count();
                stream.mark_first_token();
                stream
                    .send_event(
                        tx,
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": arguments
                        }),
                    )
                    .await;
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        stream.stop_reason = Some(if reason == "length" {
            "max_tokens".to_string()
        } else {
            reason.to_string()
        });
    }
}

/// Streams a chat-completions response from a Responses-only upstream by
/// translating the Responses SSE events back into chat chunks.
#[allow(clippy::too_many_arguments)]
fn responses_stream_to_chat(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let message_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut chat_state = ChatStreamState::default();
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_responses_line_for_chat(
                    &line,
                    &mut event_name,
                    &mut chat_state,
                    &message_id,
                    &requested_model,
                    started,
                    &tx,
                )
                .await;
            }
        }
        if !buffer.is_empty() {
            process_responses_line_for_chat(
                &buffer,
                &mut event_name,
                &mut chat_state,
                &message_id,
                &requested_model,
                started,
                &tx,
            )
            .await;
        }

        if let Some(reason) = chat_state.finish_reason() {
            let final_chunk =
                openai_stream_chunk(&message_id, &requested_model, json!({}), Some(reason), None);
            let usage_chunk = openai_stream_chunk(
                &message_id,
                &requested_model,
                json!({}),
                None,
                Some(json!({
                    "prompt_tokens": chat_state.prompt_tokens,
                    "completion_tokens": chat_state.completion_tokens,
                    "total_tokens": chat_state.prompt_tokens + chat_state.completion_tokens
                })),
            );
            let _ = tx
                .send(Ok(Bytes::from(format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&final_chunk).unwrap_or_default(),
                    serde_json::to_string(&usage_chunk).unwrap_or_default()
                ))))
                .await;
        }
        drop(tx);

        let prompt_tokens = if chat_state.prompt_tokens > 0 {
            chat_state.prompt_tokens
        } else {
            request_tokens
        };
        let completion_tokens = if chat_state.completion_tokens > 0 {
            chat_state.completion_tokens
        } else {
            (chat_state.text.chars().count() / 4) as i64
        };
        let usage = Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: chat_state.cache_read_tokens,
            cache_write_tokens: chat_state.cache_write_tokens,
        }
        .normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let preview = response_preview(chat_state.text.as_bytes());
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_CHAT_COMPLETIONS,
                usage,
                latency_ms,
                first_token_ms: chat_state.first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

#[derive(Default)]
struct ChatStreamState {
    text: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    first_token_ms: Option<i64>,
    sent_role: bool,
    saw_tool_call: bool,
    stop_reason: Option<String>,
    /// Maps a Responses function-call item id to its chat tool-call index.
    tool_indices: std::collections::HashMap<String, usize>,
}

impl ChatStreamState {
    fn finish_reason(&self) -> Option<&'static str> {
        Some(if self.saw_tool_call {
            "tool_calls"
        } else if self.stop_reason.as_deref() == Some("max_tokens")
            || self.stop_reason.as_deref() == Some("length")
        {
            "length"
        } else {
            "stop"
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_responses_line_for_chat(
    line: &[u8],
    event_name: &mut String,
    state: &mut ChatStreamState,
    message_id: &str,
    model: &str,
    started: Instant,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if let Some(event) = line.strip_prefix("event:") {
        *event_name = event.trim().to_string();
        return;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };

    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(event_name.as_str());
    match event {
        "response.output_item.added" => {
            let item = value.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = item.get("name").cloned().unwrap_or(Value::Null);
                let index = state.tool_indices.len();
                if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                    state.tool_indices.insert(item_id.to_string(), index);
                }
                state.saw_tool_call = true;
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"tool_calls": [{
                        "index": index,
                        "id": call_id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    }]}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                state.text.push_str(delta);
                if state.first_token_ms.is_none() {
                    state.first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let delta_json = if state.sent_role {
                    json!({"content": delta})
                } else {
                    state.sent_role = true;
                    json!({"role": "assistant", "content": delta})
                };
                let chunk = openai_stream_chunk(message_id, model, delta_json, None, None);
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                let index = value
                    .get("item_id")
                    .and_then(Value::as_str)
                    .and_then(|item_id| state.tool_indices.get(item_id).copied())
                    .unwrap_or(0);
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"tool_calls": [{
                        "index": index,
                        "function": {"arguments": delta}
                    }]}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "response.completed" | "response.incomplete" | "response.failed" => {
            let response = value.get("response").unwrap_or(&Value::Null);
            if let Some(usage) = usage_from_value(response) {
                state.prompt_tokens = usage.prompt_tokens;
                state.completion_tokens = usage.completion_tokens;
                state.cache_read_tokens = usage.cache_read_tokens;
                state.cache_write_tokens = usage.cache_write_tokens;
            }
            if let Some(details) = response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
            {
                state.stop_reason = Some(details.to_string());
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn anthropic_stream_to_responses(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let mut stream = ResponsesStreamState::new(requested_model.clone(), started);
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                stream.handle_line(&line, &mut event_name, &tx).await;
            }
        }
        if !buffer.is_empty() {
            stream.handle_line(&buffer, &mut event_name, &tx).await;
        }

        if let Some(error) = stream_error.as_deref() {
            stream.fail(&tx, error).await;
        } else {
            stream.complete(&tx).await;
        }
        drop(tx);

        if stream.usage.prompt_tokens == 0 {
            stream.usage.prompt_tokens = request_tokens;
        }
        if stream.usage.completion_tokens == 0 {
            stream.usage.completion_tokens = (stream.output_chars / 4) as i64;
        }
        let usage = stream.usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms = stream
            .first_token_ms
            .or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(
            stream
                .output
                .iter()
                .filter_map(|item| {
                    item.pointer("/content/0/text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect::<Vec<_>>()
                .join("")
                .as_bytes(),
        );
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_RESPONSES,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

fn completions_stream_chunk(
    id: &str,
    model: &str,
    text: &str,
    finish_reason: Option<&str>,
) -> Value {
    json!({
        "id": id,
        "object": "text_completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "text": text,
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": finish_reason
        }]
    })
}

/// Streams a legacy `/v1/completions` response from an Anthropic Messages SSE
/// stream. Only text deltas map across; tool use has no legacy equivalent and
/// is dropped.
#[allow(clippy::too_many_arguments)]
fn anthropic_stream_to_completions(
    state: AppState,
    response: reqwest::Response,
    request_id: String,
    requested_model: String,
    target: RouteTarget,
    request_tokens: i64,
    api_key: Option<ApiKeyRecord>,
    started: Instant,
    receipt: Option<Value>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::spawn(async move {
        let completion_id = format!("cmpl-{}", uuid::Uuid::new_v4().simple());
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut text = String::new();
        let mut stop_reason: Option<String> = None;
        let mut first_token_ms = None;
        let mut stream_error = None;
        let mut heartbeat = UsageHeartbeat::new(state.clone(), request_id.clone());

        while let Some(chunk) = upstream.next().await {
            let chunk = match chunk {
                Ok(chunk) => {
                    heartbeat.touch().await;
                    chunk
                }
                Err(error) => {
                    stream_error = Some(error.to_string());
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=position).collect::<Vec<_>>();
                process_completions_anthropic_line(
                    &line,
                    &mut event_name,
                    &mut usage,
                    &mut text,
                    &mut stop_reason,
                    &mut first_token_ms,
                    started,
                    &completion_id,
                    &requested_model,
                    &tx,
                )
                .await;
            }
        }
        if !buffer.is_empty() {
            process_completions_anthropic_line(
                &buffer,
                &mut event_name,
                &mut usage,
                &mut text,
                &mut stop_reason,
                &mut first_token_ms,
                started,
                &completion_id,
                &requested_model,
                &tx,
            )
            .await;
        }

        let finish_reason = if stop_reason.as_deref() == Some("max_tokens") {
            "length"
        } else {
            "stop"
        };
        if stream_error.is_none() {
            let final_chunk =
                completions_stream_chunk(&completion_id, &requested_model, "", Some(finish_reason));
            let _ = tx
                .send(Ok(Bytes::from(format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&final_chunk).unwrap_or_default()
                ))))
                .await;
        }
        drop(tx);

        if usage.prompt_tokens == 0 {
            usage.prompt_tokens = request_tokens;
        }
        if usage.completion_tokens == 0 {
            usage.completion_tokens = (text.chars().count() / 4) as i64;
        }
        let usage = usage.normalized();
        let latency_ms = started.elapsed().as_millis() as i64;
        let first_token_ms =
            first_token_ms.or_else(|| (usage.completion_tokens > 0).then_some(latency_ms));
        let preview = response_preview(text.as_bytes());
        log_usage(
            &state,
            UsageLogEntry {
                request_id: &request_id,
                api_key_id: api_key.as_ref().map(|key| key.id),
                route_id: target.route_id,
                provider_id: Some(target.provider_id),
                requested_model: &requested_model,
                upstream_model: Some(&target.upstream_model),
                endpoint: OPENAI_COMPLETIONS,
                usage,
                latency_ms,
                first_token_ms,
                status_code: if stream_error.is_some() { 502 } else { 200 },
                success: stream_error.is_none(),
                streamed: true,
                error_message: stream_error.as_deref(),
                response_preview: preview.as_deref(),
            },
        )
        .await;
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    apply_capability_headers(&mut response, &receipt);
    response
}

#[allow(clippy::too_many_arguments)]
async fn process_completions_anthropic_line(
    line: &[u8],
    event_name: &mut String,
    usage: &mut Usage,
    text: &mut String,
    stop_reason: &mut Option<String>,
    first_token_ms: &mut Option<i64>,
    started: Instant,
    completion_id: &str,
    model: &str,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if let Some(event) = line.strip_prefix("event:") {
        *event_name = event.trim().to_string();
        return;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };

    match event_name.as_str() {
        "message_start" => {
            if let Some(message_usage) = value.pointer("/message/usage") {
                if let Some(input_tokens) =
                    message_usage.get("input_tokens").and_then(Value::as_i64)
                {
                    usage.prompt_tokens = input_tokens;
                }
                usage.cache_read_tokens = cache_read_of(message_usage);
                usage.cache_write_tokens = cache_write_of(message_usage);
            }
        }
        "content_block_delta" => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            if delta.get("type").and_then(Value::as_str) == Some("text_delta")
                && let Some(part) = delta.get("text").and_then(Value::as_str)
            {
                text.push_str(part);
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let chunk = completions_stream_chunk(completion_id, model, part, None);
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "message_delta" => {
            if let Some(output_tokens) = value
                .pointer("/usage/output_tokens")
                .and_then(Value::as_i64)
            {
                usage.completion_tokens = output_tokens;
            }
            if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                *stop_reason = Some(reason.to_string());
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_anthropic_line(
    line: &[u8],
    event_name: &mut String,
    usage: &mut Usage,
    output_chars: &mut usize,
    text_acc: &mut String,
    sent_role: &mut bool,
    saw_tool_use: &mut bool,
    next_tool_index: &mut usize,
    tool_indices: &mut std::collections::HashMap<i64, usize>,
    first_token_ms: &mut Option<i64>,
    started: Instant,
    message_id: &str,
    model: &str,
    tx: &mpsc::Sender<Result<Bytes, io::Error>>,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if let Some(event) = line.strip_prefix("event:") {
        *event_name = event.trim().to_string();
        return;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };

    match event_name.as_str() {
        "message_start" => {
            if let Some(message_usage) = value.pointer("/message/usage") {
                if let Some(input_tokens) =
                    message_usage.get("input_tokens").and_then(Value::as_i64)
                {
                    usage.prompt_tokens = input_tokens;
                }
                // Anthropic reports cache traffic on the initial message too.
                usage.cache_read_tokens = cache_read_of(message_usage);
                usage.cache_write_tokens = cache_write_of(message_usage);
            }
            if !*sent_role {
                *sent_role = true;
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"role": "assistant", "content": ""}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "content_block_delta" => {
            if let Some(delta_text) = value.pointer("/delta/text").and_then(Value::as_str) {
                if first_token_ms.is_none() && !delta_text.is_empty() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                *output_chars += delta_text.chars().count();
                push_preview_text(text_acc, delta_text);
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({"content": delta_text}),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
            if let Some(thinking) = value.pointer("/delta/thinking").and_then(Value::as_str)
                && !thinking.is_empty()
            {
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                *output_chars += thinking.chars().count();
            }
            // Tool call arguments stream as JSON fragments; OpenAI expects
            // them accumulated under `tool_calls[].function.arguments`.
            if value.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta")
                && let Some(partial) = value.pointer("/delta/partial_json").and_then(Value::as_str)
            {
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let block_index = value.get("index").and_then(Value::as_i64).unwrap_or(0);
                let index = *tool_indices.get(&block_index).unwrap_or(&0);
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "function": { "arguments": partial }
                        }]
                    }),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "content_block_start" => {
            if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                if first_token_ms.is_none() {
                    *first_token_ms = Some(started.elapsed().as_millis() as i64);
                }
                let id = value
                    .pointer("/content_block/id")
                    .cloned()
                    .unwrap_or(Value::Null);
                let name = value
                    .pointer("/content_block/name")
                    .cloned()
                    .unwrap_or(Value::Null);
                // Parallel tool calls arrive as separate content blocks; the
                // OpenAI `index` must distinguish them or clients merge them.
                let block_index = value.get("index").and_then(Value::as_i64).unwrap_or(0);
                let index = *next_tool_index;
                *next_tool_index += 1;
                tool_indices.insert(block_index, index);
                *saw_tool_use = true;
                let chunk = openai_stream_chunk(
                    message_id,
                    model,
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": "" }
                        }]
                    }),
                    None,
                    None,
                );
                let _ = tx
                    .send(Ok(Bytes::from(format!(
                        "data: {}\n\n",
                        serde_json::to_string(&chunk).unwrap_or_default()
                    ))))
                    .await;
            }
        }
        "message_delta" => {
            if let Some(output_tokens) = value
                .pointer("/usage/output_tokens")
                .and_then(Value::as_i64)
            {
                usage.completion_tokens = output_tokens;
            }
            // `message_delta` may also repeat cache counters; keep them in sync.
            if let Some(delta_usage) = value.get("usage") {
                let cache_read = cache_read_of(delta_usage);
                if cache_read > 0 {
                    usage.cache_read_tokens = cache_read;
                }
                let cache_write = cache_write_of(delta_usage);
                if cache_write > 0 {
                    usage.cache_write_tokens = cache_write;
                }
            }
        }
        _ => {}
    }
}

fn convert_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let mut output = json!({
        "model": model,
        "max_tokens": requested_output_tokens_of(input).unwrap_or(4096),
        "stream": streamed
    });

    let mut system = Vec::new();
    let mut messages = Vec::new();
    if let Some(items) = input.get("messages").and_then(Value::as_array) {
        for message in items {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if role == "system" || role == "developer" {
                if let Some(text) = content_text(&content) {
                    system.push(text);
                }
                continue;
            }

            // A tool result carries its association through `tool_call_id`.
            // Anthropic expects a user message holding a `tool_result` block.
            if role == "tool" {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content_text(&content).unwrap_or_default()
                    }]
                }));
                continue;
            }

            let anthropic_role = if role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            let mut blocks = convert_anthropic_content(&content);
            // An assistant turn may request tools; Anthropic represents those
            // as `tool_use` blocks alongside any text.
            if role == "assistant"
                && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
                && let Some(array) = blocks.as_array_mut()
            {
                // An assistant turn that only calls tools has no text content;
                // drop the placeholder empty block so Anthropic does not reject it.
                array.retain(|block| {
                    block.get("type").and_then(Value::as_str) != Some("text")
                        || block
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty())
                });
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    let Ok(arguments) = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .parse::<Value>()
                    else {
                        continue;
                    };
                    array.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "input": arguments
                    }));
                }
            }
            messages.push(json!({
                "role": anthropic_role,
                "content": blocks
            }));
        }
    }

    if !system.is_empty() {
        output["system"] = json!(system.join("\n\n"));
    }
    output["messages"] = json!(messages);

    if let Some(temperature) = input.get("temperature") {
        output["temperature"] = temperature.clone();
    }
    if let Some(top_p) = input.get("top_p") {
        output["top_p"] = top_p.clone();
    }
    if let Some(stop) = input.get("stop") {
        output["stop_sequences"] = match stop {
            Value::String(value) => json!([value]),
            Value::Array(_) => stop.clone(),
            _ => Value::Null,
        };
    }

    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let anthropic_tools = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function").unwrap_or(tool);
                let name = function.get("name")?.as_str()?;
                Some(json!({
                    "name": name,
                    "description": function.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": function.get("parameters").cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                }))
            })
            .collect::<Vec<_>>();
        if !anthropic_tools.is_empty() {
            output["tools"] = json!(anthropic_tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::String(value) if value == "required" => json!({"type": "any"}),
            Value::String(value) if value == "auto" || value == "none" => {
                json!({"type": value})
            }
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "tool", "name": name})
            }
            _ => choice.clone(),
        };
    }
    output
}

/// Converts an OpenAI Responses request into the Anthropic Messages shape.
///
/// Responses is item-oriented while Messages is message-oriented, so the
/// conversion first normalizes the input into the chat shape that
/// `convert_request_to_anthropic` already knows how to handle.
fn responses_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let chat = responses_request_to_chat(input, model, streamed);
    convert_request_to_anthropic(&chat, model, streamed)
}

fn responses_request_to_chat(input: &Value, model: &str, streamed: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(instructions) = input.get("instructions")
        && let Some(text) = content_text(instructions)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    append_responses_input(&mut messages, input.get("input"));

    let mut output = json!({"model": model, "messages": messages});
    if streamed {
        output["stream"] = json!(true);
    }
    for (source, target) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(responses_tool_to_chat)
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            output["tools"] = json!(tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "function", "function": {"name": name}})
            }
            _ => choice.clone(),
        };
    }
    output
}

fn append_responses_input(messages: &mut Vec<Value>, input: Option<&Value>) {
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let call = json!({
                            "id": item.get("call_id").or_else(|| item.get("id")).cloned()
                                .unwrap_or(Value::Null),
                            "type": "function",
                            "function": {
                                "name": item.get("name").cloned().unwrap_or(Value::Null),
                                "arguments": item.get("arguments").cloned()
                                    .unwrap_or_else(|| json!("{}"))
                            }
                        });
                        if let Some(last) = messages.last_mut()
                            && last.get("role").and_then(Value::as_str) == Some("assistant")
                        {
                            let calls = last
                                .as_object_mut()
                                .and_then(|object| object.get_mut("tool_calls"))
                                .and_then(Value::as_array_mut);
                            if let Some(calls) = calls {
                                calls.push(call);
                                continue;
                            }
                        }
                        messages.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [call]
                        }));
                    }
                    Some("function_call_output") => {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                            "content": item.get("output").cloned().unwrap_or(Value::Null)
                        }));
                    }
                    _ => {
                        let Some(role) = item.get("role").and_then(Value::as_str) else {
                            continue;
                        };
                        let content =
                            responses_content_to_chat(item.get("content").unwrap_or(&Value::Null));
                        messages.push(json!({"role": role, "content": content}));
                    }
                }
            }
        }
        _ => {}
    }
}

fn responses_content_to_chat(content: &Value) -> Value {
    match content {
        Value::String(_) | Value::Null => content.clone(),
        Value::Array(items) => json!(
            items
                .iter()
                .filter_map(responses_content_part_to_chat)
                .collect::<Vec<_>>()
        ),
        _ => content.clone(),
    }
}

fn responses_content_part_to_chat(item: &Value) -> Option<Value> {
    match item.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text" | "text") => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("input_image") => {
            let url = item
                .get("image_url")
                .or_else(|| item.get("url"))
                .and_then(Value::as_str)?;
            Some(json!({"type": "image_url", "image_url": {"url": url}}))
        }
        Some("image_url") => Some(item.clone()),
        _ => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
    }
}

fn responses_tool_to_chat(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    Some(json!({
        "type": "function",
        "function": {
            "name": tool.get("name")?.clone(),
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": tool.get("parameters").cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
        }
    }))
}

/// Converts a legacy OpenAI `/v1/completions` request into the Anthropic
/// Messages shape by way of the chat shape that `convert_request_to_anthropic`
/// already understands. The legacy `prompt` field replaces the message list.
/// Converts an OpenAI chat-completions request into the Responses shape.
///
/// This is the inverse of [`responses_request_to_chat`] and lets a
/// Responses-only upstream serve callers that speak `/v1/chat/completions`.
fn chat_request_to_responses(input: &Value, model: &str, streamed: bool) -> Value {
    let mut instructions = Vec::new();
    let mut items = Vec::new();
    if let Some(messages) = input.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if role == "system" || role == "developer" {
                if let Some(text) = content_text(&content)
                    && !text.is_empty()
                {
                    instructions.push(text);
                }
                continue;
            }
            if role == "tool" {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": message.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "output": content_text(&content).unwrap_or_default()
                }));
                continue;
            }
            let text_type = if role == "assistant" {
                "output_text"
            } else {
                "input_text"
            };
            let parts = chat_content_to_responses_parts(&content, text_type);
            if !parts.is_empty() {
                items.push(json!({"type": "message", "role": role, "content": parts}));
            }
            if role == "assistant"
                && let Some(calls) = message.get("tool_calls").and_then(Value::as_array)
            {
                for call in calls {
                    let function = call.get("function").unwrap_or(call);
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call
                            .get("id")
                            .or_else(|| call.get("call_id"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        "name": function.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": match function.get("arguments") {
                            Some(Value::String(text)) => json!(text),
                            Some(other) => json!(other.to_string()),
                            None => json!("{}"),
                        }
                    }));
                }
            }
        }
    }

    let mut output = json!({"model": model, "input": items});
    if !instructions.is_empty() {
        output["instructions"] = json!(instructions.join("\n\n"));
    }
    if streamed {
        output["stream"] = json!(true);
    }
    for (source, target) in [
        ("max_tokens", "max_output_tokens"),
        ("max_completion_tokens", "max_output_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(value) = input.get(source) {
            output[target] = value.clone();
        }
    }
    if let Some(tools) = input.get("tools").and_then(Value::as_array) {
        let tools = tools
            .iter()
            .filter_map(chat_tool_to_responses)
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            output["tools"] = json!(tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        output["tool_choice"] = match choice {
            Value::Object(_) => {
                let name = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({"type": "function", "name": name})
            }
            _ => choice.clone(),
        };
    }
    output
}

fn chat_content_to_responses_parts(content: &Value, text_type: &str) -> Vec<Value> {
    match content {
        Value::String(text) => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({"type": text_type, "text": text})]
            }
        }
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                Some("image_url") => item
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .map(|url| json!({"type": "input_image", "image_url": url})),
                _ => item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| json!({"type": text_type, "text": text})),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn chat_tool_to_responses(tool: &Value) -> Option<Value> {
    let function = tool.get("function").unwrap_or(tool);
    function.get("name")?;
    Some(json!({
        "type": "function",
        "name": function.get("name")?.clone(),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "parameters": function.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    }))
}

/// Converts a Responses object into a chat-completions response.
fn responses_response_to_chat(value: &Value, requested_model: &str) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(part) = part.get("text").and_then(Value::as_str) {
                                text.push_str(part);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    tool_calls.push(json!({
                        "id": item.get("call_id").or_else(|| item.get("id"))
                            .cloned().unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple()))),
                        "type": "function",
                        "function": {
                            "name": item.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": item.get("arguments").cloned()
                                .unwrap_or_else(|| json!("{}"))
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let incomplete = value.get("status").and_then(Value::as_str) == Some("incomplete");
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if incomplete {
        "length"
    } else {
        "stop"
    };
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { json!(text) }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("chatcmpl-{}", id.trim_start_matches("resp_")))
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let created = value
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    (
        json!({
            "id": id,
            "object": "chat.completion",
            "created": created,
            "model": requested_model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        }),
        usage,
    )
}

fn completions_request_to_anthropic(input: &Value, model: &str, streamed: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(prompt) = completions_prompt_text(input.get("prompt")) {
        messages.push(json!({"role": "user", "content": prompt}));
    }

    let mut chat = json!({"model": model, "messages": messages});
    if streamed {
        chat["stream"] = json!(true);
    }
    for key in ["max_tokens", "temperature", "top_p", "stop"] {
        if let Some(value) = input.get(key) {
            chat[key] = value.clone();
        }
    }
    convert_request_to_anthropic(&chat, model, streamed)
}

/// Flattens the legacy `prompt` field into a single string. OpenAI allows a
/// plain string, an array of strings, or token arrays; token arrays cannot be
/// decoded without the provider's tokenizer, so they are skipped.
fn completions_prompt_text(prompt: Option<&Value>) -> Option<String> {
    match prompt {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let mut text = String::new();
            for item in items {
                match item {
                    Value::String(part) => text.push_str(part),
                    Value::Array(parts) => {
                        for part in parts {
                            if let Some(part) = part.as_str() {
                                text.push_str(part);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(text)
        }
        _ => None,
    }
}

fn convert_anthropic_content(content: &Value) -> Value {
    match content {
        Value::String(text) => json!([{"type": "text", "text": text}]),
        Value::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = item.pointer("/image_url/url").and_then(Value::as_str) {
                            if let Some((meta, data)) = url.split_once(',') {
                                let media_type = meta
                                    .strip_prefix("data:")
                                    .and_then(|value| value.split(';').next())
                                    .unwrap_or("image/png");
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data
                                    }
                                }));
                            } else {
                                blocks.push(json!({
                                    "type": "image",
                                    "source": {"type": "url", "url": url}
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if blocks.is_empty() {
                json!([{"type": "text", "text": ""}])
            } else {
                json!(blocks)
            }
        }
        _ => json!([{"type": "text", "text": ""}]),
    }
}

fn convert_anthropic_response(value: &Value) -> (Value, Usage) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(part) = block.get("text").and_then(Value::as_str) {
                        text.push_str(part);
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(json!({
                        "id": block.get("id").cloned().unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple()))),
                        "type": "function",
                        "function": {
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": serde_json::to_string(
                                block.get("input").unwrap_or(&Value::Null)
                            ).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let finish_reason = match value.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    };
    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { json!(text) }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    // Reuse the shared reader so cache counters (`cache_read_input_tokens`,
    // `cache_creation_input_tokens`) are captured for native Anthropic too.
    let usage = value
        .get("usage")
        .map(|usage| {
            Usage {
                prompt_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                completion_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                total_tokens: 0,
                cache_read_tokens: cache_read_of(usage),
                cache_write_tokens: cache_write_of(usage),
            }
            .normalized()
        })
        .unwrap_or_default();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let model = value.get("model").cloned().unwrap_or(Value::Null);

    (
        json!({
            "id": id,
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        }),
        usage,
    )
}

fn anthropic_response_to_responses(value: &Value, requested_model: &str) -> (Value, Usage) {
    let (chat, usage) = convert_anthropic_response(value);
    let (response, _) = chat_response_to_responses(&chat, requested_model);
    (response, usage)
}

fn anthropic_response_to_completions(value: &Value, requested_model: &str) -> (Value, Usage) {
    let (chat, usage) = convert_anthropic_response(value);
    let text = chat
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let finish_reason = match value.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => "length",
        _ => "stop",
    };
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("cmpl-{}", id.trim_start_matches("msg_")))
        .unwrap_or_else(|| format!("cmpl-{}", uuid::Uuid::new_v4().simple()));
    (
        json!({
            "id": id,
            "object": "text_completion",
            "created": chrono::Utc::now().timestamp(),
            "model": requested_model,
            "choices": [{
                "text": text,
                "index": 0,
                "logprobs": Value::Null,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        }),
        usage,
    )
}

fn chat_response_to_responses(value: &Value, requested_model: &str) -> (Value, Usage) {
    let message = value.pointer("/choices/0/message");
    let mut output = Vec::new();
    if let Some(text) = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        output.push(json!({
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [],
                "logprobs": []
            }]
        }));
    }
    if let Some(calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for call in calls {
            let function = call.get("function").unwrap_or(call);
            let call_id = call
                .get("id")
                .cloned()
                .unwrap_or_else(|| json!(format!("call_{}", uuid::Uuid::new_v4().simple())));
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!("{}"));
            output.push(json!({
                "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "arguments": match arguments {
                    Value::String(_) => arguments,
                    other => json!(serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()))
                }
            }));
        }
    }

    let finish_reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let incomplete = finish_reason == Some("length");
    let usage = usage_from_value(value).unwrap_or_default().normalized();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!("resp_{}", id.trim_start_matches("chatcmpl-")))
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let created_at = value
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    (
        json!({
            "id": id,
            "object": "response",
            "created_at": created_at,
            "status": if incomplete { "incomplete" } else { "completed" },
            "error": Value::Null,
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
            "instructions": Value::Null,
            "max_output_tokens": Value::Null,
            "model": requested_model,
            "output": output,
            "parallel_tool_calls": true,
            "previous_response_id": Value::Null,
            "reasoning": Value::Null,
            "store": false,
            "temperature": Value::Null,
            "text": {"format": {"type": "text"}},
            "tool_choice": "auto",
            "tools": [],
            "top_p": Value::Null,
            "truncation": "disabled",
            "usage": responses_usage_json(&usage)
        }),
        usage,
    )
}

fn responses_usage_json(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.prompt_tokens,
        "input_tokens_details": {
            "cached_tokens": usage.cache_read_tokens
        },
        "output_tokens": usage.completion_tokens,
        "output_tokens_details": {
            "reasoning_tokens": 0
        },
        "total_tokens": usage.total_tokens
    })
}

fn openai_stream_chunk(
    id: &str,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
    usage: Option<Value>,
) -> Value {
    let mut value = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason
        }]
    });
    if let Some(usage) = usage {
        value["usage"] = usage;
        value["choices"] = json!([]);
    }
    value
}

async fn selected_console_api_key(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<ApiKeyRecord>> {
    let key_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
        .fetch_one(&state.pool)
        .await?;
    let selected_id = headers
        .get(CONSOLE_API_KEY_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| AppError::BadRequest("invalid gateway API key id".to_string()))
        })
        .transpose()?;

    let Some(key_id) = selected_id else {
        if key_count > 0 {
            return Err(AppError::BadRequest(
                "select a gateway API key for the playground".to_string(),
            ));
        }
        return Ok(None);
    };

    sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT * FROM api_keys \
         WHERE id = ? AND enabled = 1 \
           AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
    )
    .bind(key_id)
    .fetch_optional(&state.pool)
    .await?
    .map(Some)
    .ok_or_else(|| AppError::BadRequest("selected gateway API key is unavailable".to_string()))
}

async fn authenticate_gateway(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<ApiKeyRecord>> {
    // Presence of any configured key enables gateway auth. Counting only
    // enabled keys would silently fall back to anonymous access when every
    // key is disabled, which is the opposite of the operator's intent.
    // Copy the cached value out in its own statement: keeping the read guard
    // alive across the match would deadlock when a write lock is requested
    // below (tokio's RwLock is write-preferring).
    let cached = *state.auth_required.read().await;
    let auth_required = match cached {
        Some(cached) => cached,
        None => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
                .fetch_one(&state.pool)
                .await?;
            let required = count > 0;
            *state.auth_required.write().await = Some(required);
            required
        }
    };
    if !auth_required {
        return Ok(None);
    }

    let supplied = bearer_token(headers)
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .ok_or_else(|| {
            AppError::Unauthorized("missing gateway API key in Authorization header".to_string())
        })?;
    let hash = hash_secret(supplied);
    let record = sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT * FROM api_keys \
         WHERE key_hash = ? AND enabled = 1 \
           AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
    )
    .bind(hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("invalid gateway API key".to_string()))?;

    // Writing on every request would take a SQLite write lock each time.
    // Once a minute per key is plenty for a "last used" display.
    let should_touch = {
        let mut touched = state.key_touched.lock().await;
        match touched.get(&record.id) {
            Some(last) if last.elapsed() < std::time::Duration::from_secs(60) => false,
            _ => {
                touched.insert(record.id, std::time::Instant::now());
                true
            }
        }
    };
    if should_touch
        && let Err(error) = sqlx::query(
            "UPDATE api_keys SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
        )
        .bind(record.id)
        .execute(&state.pool)
        .await
    {
        tracing::warn!(%error, key_id = record.id, "failed to update API key last_used_at");
    }
    Ok(Some(record))
}

/// Checks a key's soft daily quotas before any upstream work begins.
///
/// Usage is committed after the response completes, so concurrent requests can
/// overshoot by at most the work already in flight. The guard still prevents a
/// key from continuing to spend after its previous usage has crossed a limit.
async fn enforce_api_key_daily_quota(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
) -> AppResult<()> {
    let Some(api_key) = api_key else {
        return Ok(());
    };
    if api_key.daily_token_limit.is_none() && api_key.daily_cost_limit_micros.is_none() {
        return Ok(());
    }

    let day_start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
        .to_rfc3339();
    let (tokens, cost_micros) = sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT COALESCE(SUM(total_tokens), 0), SUM(estimated_cost_micros) \
         FROM usage_logs WHERE api_key_id = ? AND created_at >= ? AND in_flight = 0",
    )
    .bind(api_key.id)
    .bind(day_start)
    .fetch_one(&state.pool)
    .await?;

    if let Some(limit) = api_key.daily_token_limit
        && tokens >= limit
    {
        return Err(AppError::TooManyRequests(format!(
            "daily token limit reached for this API key ({tokens}/{limit})"
        )));
    }
    if let Some(limit) = api_key.daily_cost_limit_micros
        && cost_micros.unwrap_or(0) >= limit
    {
        return Err(AppError::TooManyRequests(format!(
            "daily cost limit reached for this API key (${:.4}/${:.4})",
            cost_micros.unwrap_or(0) as f64 / 1_000_000.0,
            limit as f64 / 1_000_000.0
        )));
    }
    Ok(())
}

/// Atomically checks and reserves a rate-limit slot.
///
/// Counting and inserting in one SQLite statement closes the race where a
/// burst of requests could all pass a read-only limit check. The inserted row
/// is the same in-flight row later completed by usage logging.
#[allow(clippy::too_many_arguments)]
async fn reserve_api_key_rate_limit(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
) -> AppResult<()> {
    let Some(api_key) = api_key else {
        return Ok(());
    };
    if api_key.requests_per_minute.is_none() && api_key.max_concurrency.is_none() {
        return Ok(());
    }

    let minute_start = Utc::now()
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("current minute start is valid")
        .to_rfc3339();
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, session_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, response_preview, last_activity_at
        )
        SELECT ?, ?, ?, NULL, NULL, ?, NULL, ?, ?, 0, ?, 0, 0, 0,
               NULL, NULL, 0, 1, 0, ?, NULL, NULL,
               strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        WHERE (
            ? IS NULL OR (
                SELECT COUNT(*) FROM usage_logs
                WHERE api_key_id = ? AND created_at >= ?
            ) < ?
        )
        AND (
            ? IS NULL OR (
                SELECT COUNT(*) FROM usage_logs
                WHERE api_key_id = ? AND in_flight = 1
            ) < ?
        )
        ON CONFLICT(request_id) DO NOTHING
        "#,
    )
    .bind(request_id)
    .bind(session_id)
    .bind(api_key.id)
    .bind(requested_model)
    .bind(endpoint)
    .bind(request_tokens)
    .bind(request_tokens)
    .bind(streamed as i64)
    .bind(api_key.requests_per_minute)
    .bind(api_key.id)
    .bind(&minute_start)
    .bind(api_key.requests_per_minute)
    .bind(api_key.max_concurrency)
    .bind(api_key.id)
    .bind(api_key.max_concurrency)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() > 0 {
        return Ok(());
    }

    if let Some(limit) = api_key.requests_per_minute {
        let requests = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_logs \
             WHERE api_key_id = ? AND created_at >= ?",
        )
        .bind(api_key.id)
        .bind(&minute_start)
        .fetch_one(&state.pool)
        .await?;
        if requests >= limit {
            return Err(AppError::TooManyRequests(format!(
                "requests per minute limit reached for this API key ({requests}/{limit})"
            )));
        }
    }

    if let Some(limit) = api_key.max_concurrency {
        let in_flight = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_logs WHERE api_key_id = ? AND in_flight = 1",
        )
        .bind(api_key.id)
        .fetch_one(&state.pool)
        .await?;
        if in_flight >= limit {
            return Err(AppError::TooManyRequests(format!(
                "concurrency limit reached for this API key ({in_flight}/{limit})"
            )));
        }
    }

    Err(AppError::TooManyRequests(
        "API key rate limit reached".to_string(),
    ))
}

fn api_key_model_patterns(api_key: Option<&ApiKeyRecord>) -> AppResult<Option<Vec<String>>> {
    let Some(raw) = api_key.and_then(|api_key| api_key.allowed_models.as_deref()) else {
        return Ok(None);
    };
    let patterns = serde_json::from_str::<Vec<String>>(raw)
        .map_err(|_| AppError::Forbidden("API key model permissions are invalid".to_string()))?;
    Ok((!patterns.is_empty()).then_some(patterns))
}

fn model_matches_patterns(patterns: Option<&[String]>, model: &str) -> bool {
    patterns.is_none_or(|patterns| {
        patterns.iter().any(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher().is_match(model))
                .unwrap_or(false)
        })
    })
}

fn filter_allowed_models<T>(
    patterns: Option<&[String]>,
    models: Vec<T>,
    model_id: impl Fn(&T) -> &str,
) -> Vec<T> {
    models
        .into_iter()
        .filter(|model| model_matches_patterns(patterns, model_id(model)))
        .collect()
}

fn enforce_api_key_model_access(
    api_key: Option<&ApiKeyRecord>,
    requested_model: &str,
) -> AppResult<()> {
    let patterns = api_key_model_patterns(api_key)?;
    if !model_matches_patterns(patterns.as_deref(), requested_model) {
        return Err(AppError::Forbidden(format!(
            "API key is not allowed to call model '{requested_model}'"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn enforce_policy_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
) -> AppResult<()> {
    let rejection = match enforce_api_key_model_access(api_key, requested_model) {
        Ok(()) => enforce_api_key_daily_quota(state, api_key).await.err(),
        Err(error) => Some(error),
    };
    match rejection {
        None => Ok(()),
        Some(error) => {
            let message = error.to_string();
            let status_code = match error {
                AppError::Forbidden(_) => 403,
                AppError::TooManyRequests(_) => 429,
                _ => 500,
            };
            log_request_rejection(
                state,
                api_key,
                request_id,
                session_id,
                requested_model,
                endpoint,
                streamed,
                started,
                status_code,
                &message,
            )
            .await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn enforce_api_key_rate_limit_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
    started: Instant,
) -> AppResult<()> {
    match reserve_api_key_rate_limit(
        state,
        api_key,
        request_id,
        session_id,
        requested_model,
        endpoint,
        request_tokens,
        streamed,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = error.to_string();
            let status_code = match error {
                AppError::TooManyRequests(_) => 429,
                _ => 500,
            };
            log_request_rejection(
                state,
                api_key,
                request_id,
                session_id,
                requested_model,
                endpoint,
                streamed,
                started,
                status_code,
                &message,
            )
            .await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn log_request_rejection(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    requested_model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
    status_code: i64,
    message: &str,
) {
    log_usage(
        state,
        UsageLogEntry {
            request_id,
            api_key_id: api_key.map(|key| key.id),
            route_id: None,
            provider_id: None,
            requested_model,
            upstream_model: None,
            endpoint,
            usage: Usage::default(),
            latency_ms: started.elapsed().as_millis() as i64,
            first_token_ms: None,
            status_code,
            success: false,
            streamed,
            error_message: Some(message),
            response_preview: None,
        },
    )
    .await;
    if let Some(session_id) = session_id
        && let Err(error) = sqlx::query("UPDATE usage_logs SET session_id = ? WHERE request_id = ?")
            .bind(session_id)
            .bind(request_id)
            .execute(&state.pool)
            .await
    {
        tracing::warn!(%error, request_id, "failed to attach session to rejected usage log");
    }
}

fn enforce_context_capacity(request_tokens: i64, barrel: Option<&BarrelEnvelope>) -> AppResult<()> {
    let limit = barrel
        .and_then(|barrel| barrel.capabilities.as_ref())
        .and_then(effective_input_limit);
    if let Some(limit) = limit
        && request_tokens > limit
    {
        return Err(AppError::BadRequest(format!(
            "estimated input tokens ({request_tokens}) exceed the route input limit ({limit})"
        )));
    }
    Ok(())
}

/// Returns the strictest input capacity when a provider publishes both a
/// context window and a separate input ceiling.
fn effective_input_limit(capabilities: &ModelCapabilities) -> Option<i64> {
    match (capabilities.input_limit, capabilities.context_limit) {
        (Some(input), Some(context)) => Some(input.min(context)),
        (Some(input), None) => Some(input),
        (None, Some(context)) => Some(context),
        (None, None) => None,
    }
}

struct ResolvedRoute {
    route_id: Option<i64>,
    strategy: String,
    targets: Vec<RouteTarget>,
    /// Strictest common capability envelope across the route's targets. Only
    /// meaningful for explicit routes; a directly matched model reports its own
    /// capabilities.
    barrel: Option<BarrelEnvelope>,
}

fn normalized_endpoint(value: &str) -> &str {
    let value = value
        .split('?')
        .next()
        .unwrap_or(value)
        .trim_end_matches('/');
    let value = value.strip_prefix("/v1").unwrap_or(value);
    value.strip_prefix('/').unwrap_or(value)
}

fn target_upstream_endpoint<'a>(
    target: &RouteTarget,
    request_endpoint: &'a str,
) -> Option<&'a str> {
    let provider_type = ProviderType::from_str(&target.provider_type).ok()?;
    // An OpenAI-compatible upstream that only advertises chat completions can
    // still serve a Responses client: the gateway translates the request and
    // the streamed/non-streamed response. Only take that path when the metadata
    // explicitly rules out `/responses`, so providers that do support it keep
    // getting a straight passthrough.
    if matches!(provider_type, ProviderType::Openai | ProviderType::Custom)
        && request_endpoint == OPENAI_RESPONSES
        && !endpoint_metadata_supports(target.supported_endpoints.as_deref(), OPENAI_RESPONSES)
        && endpoint_metadata_supports(
            target.supported_endpoints.as_deref(),
            OPENAI_CHAT_COMPLETIONS,
        )
    {
        return Some(OPENAI_CHAT_COMPLETIONS);
    }
    // The reverse: a Responses-only upstream can serve chat-completions
    // callers through the same translation, in the other direction.
    if matches!(provider_type, ProviderType::Openai | ProviderType::Custom)
        && request_endpoint == OPENAI_CHAT_COMPLETIONS
        && !endpoint_metadata_supports(
            target.supported_endpoints.as_deref(),
            OPENAI_CHAT_COMPLETIONS,
        )
        && endpoint_metadata_supports(target.supported_endpoints.as_deref(), OPENAI_RESPONSES)
    {
        return Some(OPENAI_RESPONSES);
    }
    // Anthropic callers normally reach an OpenAI provider through its chat
    // endpoint, but a Responses-only upstream can serve them too.
    if matches!(provider_type, ProviderType::Openai | ProviderType::Custom)
        && request_endpoint == ANTHROPIC_MESSAGES
        && !endpoint_metadata_supports(
            target.supported_endpoints.as_deref(),
            OPENAI_CHAT_COMPLETIONS,
        )
        && endpoint_metadata_supports(target.supported_endpoints.as_deref(), OPENAI_RESPONSES)
    {
        return Some(OPENAI_RESPONSES);
    }
    provider_upstream_endpoint(provider_type, request_endpoint)
}

fn provider_upstream_endpoint(provider_type: ProviderType, request_endpoint: &str) -> Option<&str> {
    match provider_type {
        ProviderType::Anthropic => {
            if request_endpoint == OPENAI_CHAT_COMPLETIONS
                || request_endpoint == OPENAI_COMPLETIONS
                || request_endpoint == OPENAI_RESPONSES
                || request_endpoint == ANTHROPIC_MESSAGES
            {
                Some(ANTHROPIC_MESSAGES)
            } else {
                None
            }
        }
        _ if request_endpoint == ANTHROPIC_MESSAGES => Some(OPENAI_CHAT_COMPLETIONS),
        _ => Some(request_endpoint),
    }
}

fn target_supports_endpoint(target: &RouteTarget, request_endpoint: &str) -> bool {
    let Some(upstream_endpoint) = target_upstream_endpoint(target, request_endpoint) else {
        return false;
    };
    endpoint_metadata_supports(target.supported_endpoints.as_deref(), upstream_endpoint)
}

fn endpoint_metadata_supports(raw: Option<&str>, upstream_endpoint: &str) -> bool {
    let endpoints = supported_endpoint_list(raw);
    if endpoints.is_empty() {
        return true;
    }
    let expected = normalized_endpoint(upstream_endpoint);
    endpoints
        .iter()
        .any(|endpoint| normalized_endpoint(endpoint) == expected)
}

fn supported_endpoint_list(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
}

fn filter_targets_for_endpoint(
    targets: Vec<RouteTarget>,
    request_endpoint: &str,
) -> Vec<RouteTarget> {
    targets
        .into_iter()
        .filter(|target| target_supports_endpoint(target, request_endpoint))
        .collect()
}

async fn resolve_route(state: &AppState, model: &str, endpoint: &str) -> AppResult<ResolvedRoute> {
    if let Some(route) = find_explicit_route(state, model).await? {
        let targets = filter_targets_for_endpoint(load_targets(state, route.id).await?, endpoint);
        if targets.is_empty() {
            return Err(AppError::BadRequest(format!(
                "model '{model}' has no enabled route target that supports endpoint '{endpoint}'"
            )));
        }
        let pairs = targets
            .iter()
            .map(|target| (target.provider_id, target.upstream_model.clone()))
            .collect::<Vec<_>>();
        let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
        return Ok(ResolvedRoute {
            route_id: Some(route.id),
            strategy: route.strategy,
            targets,
            barrel: Some(barrel),
        });
    }

    let targets = find_prefixed_targets(state, model, endpoint).await?;
    if targets.is_empty() {
        return Err(AppError::BadRequest(format!(
            "model '{model}' is not available for endpoint '{endpoint}'"
        )));
    }
    let pairs = targets
        .iter()
        .map(|target| (target.provider_id, target.upstream_model.clone()))
        .collect::<Vec<_>>();
    let barrel = crate::registry::barrel_for_targets(&state.pool, &pairs).await?;
    Ok(ResolvedRoute {
        route_id: None,
        strategy: "priority".to_string(),
        targets,
        barrel: Some(barrel),
    })
}

pub async fn diagnose_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let session_id = session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(SESSION_ID_MAX_CHARS).collect::<String>());
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT * FROM routes
        ORDER BY
            CASE WHEN instr(model_pattern, '*') = 0 AND instr(model_pattern, '?') = 0 THEN 0 ELSE 1 END,
            length(model_pattern) DESC,
            id
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut disabled_match = None;
    for route in routes {
        let Ok(glob) = Glob::new(&route.model_pattern) else {
            continue;
        };
        if !glob.compile_matcher().is_match(model) {
            continue;
        }
        if route.enabled != 0 {
            return diagnose_explicit_route(
                state,
                model,
                endpoint,
                route,
                true,
                session_id.as_deref(),
            )
            .await;
        }
        disabled_match.get_or_insert(route);
    }
    let direct = diagnose_direct_route(state, model, endpoint, session_id.as_deref()).await?;
    if direct.matched {
        return Ok(direct);
    }
    if let Some(route) = disabled_match {
        return diagnose_explicit_route(
            state,
            model,
            endpoint,
            route,
            false,
            session_id.as_deref(),
        )
        .await;
    }
    Ok(direct)
}

async fn diagnose_explicit_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    route: Route,
    route_enabled: bool,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let rows = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, rt.id AS target_id, rt.upstream_model,
               rt.weight AS target_weight, rt.priority AS target_priority,
               rt.enabled AS target_enabled,
               p.enabled AS provider_enabled,
               CASE WHEN pm.model_name IS NULL THEN 0 ELSE 1 END AS model_exists,
               COALESCE(pm.enabled, 1) AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.last_test_ok AS provider_health
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        LEFT JOIN provider_models pm
          ON pm.provider_id = rt.provider_id
         AND pm.model_name = rt.upstream_model
        WHERE rt.route_id = ?
        ORDER BY rt.priority ASC, rt.id
        "#,
    )
    .bind(route.id)
    .fetch_all(&state.pool)
    .await?;

    let diagnosed = rows
        .iter()
        .map(|row| row.diagnose(endpoint, route_enabled))
        .collect::<Vec<_>>();
    let eligible_pairs = rows
        .iter()
        .zip(&diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| (row.provider_id, row.upstream_model.clone()))
        .collect::<Vec<_>>();
    let barrel = crate::registry::barrel_for_targets(&state.pool, &eligible_pairs).await?;
    let resolved = route_enabled && !eligible_pairs.is_empty();
    let runtime_targets = if resolved {
        diagnostic_runtime_targets(
            state,
            Some(route.id),
            &route.strategy,
            &rows,
            &diagnosed,
            session_id,
        )
        .await?
    } else {
        None
    };
    let message = if !route_enabled {
        format!("route '{}' is disabled", route.name)
    } else if resolved {
        format!("{} target(s) can serve {}", eligible_pairs.len(), endpoint)
    } else {
        format!(
            "route '{}' has no eligible target for {endpoint}",
            route.name
        )
    };

    Ok(RouteDiagnoseView {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        matched: true,
        resolved,
        match_type: "explicit_route".to_string(),
        route_id: Some(route.id),
        route_name: Some(route.name),
        strategy: Some(route.strategy),
        message,
        barrel: barrel.capabilities,
        barrel_incomplete: barrel.incomplete,
        session_id: session_id.map(ToOwned::to_owned),
        runtime_targets,
        targets: diagnosed.into_iter().map(|(target, _)| target).collect(),
    })
}

async fn diagnose_direct_route(
    state: &AppState,
    model: &str,
    endpoint: &str,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let prefixed = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, p.id AS target_id, pm.model_name AS upstream_model,
               100 AS target_weight, 0 AS target_priority,
               1 AS target_enabled,
               p.enabled AS provider_enabled,
               1 AS model_exists,
               pm.enabled AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.last_test_ok AS provider_health
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id
        WHERE p.model_prefix <> ''
          AND substr(?, 1, length(p.model_prefix)) = p.model_prefix
          AND substr(?, length(p.model_prefix) + 1) = pm.model_name
        ORDER BY p.id, pm.model_name
        "#,
    )
    .bind(model)
    .bind(model)
    .fetch_all(&state.pool)
    .await?;
    let prefixed_diagnosed = prefixed
        .iter()
        .map(|row| row.diagnose(endpoint, true))
        .collect::<Vec<_>>();
    if prefixed_diagnosed.iter().any(|(_, eligible)| *eligible) {
        return build_direct_diagnosis(
            state,
            model,
            endpoint,
            "prefix",
            prefixed,
            prefixed_diagnosed,
            session_id,
        )
        .await;
    }

    let unprefixed = sqlx::query_as::<_, DiagnosticTargetRow>(
        r#"
        SELECT p.id AS provider_id, p.name AS provider_name,
               p.provider_type, p.id AS target_id, pm.model_name AS upstream_model,
               100 AS target_weight, 0 AS target_priority,
               1 AS target_enabled,
               p.enabled AS provider_enabled,
               1 AS model_exists,
               pm.enabled AS model_enabled,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.last_test_ok AS provider_health
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id
        WHERE p.model_prefix = '' AND pm.model_name = ?
        ORDER BY p.id
        "#,
    )
    .bind(model)
    .fetch_all(&state.pool)
    .await?;
    let unprefixed_diagnosed = unprefixed
        .iter()
        .map(|row| row.diagnose(endpoint, true))
        .collect::<Vec<_>>();
    if !unprefixed.is_empty() {
        return build_direct_diagnosis(
            state,
            model,
            endpoint,
            "direct",
            unprefixed,
            unprefixed_diagnosed,
            session_id,
        )
        .await;
    }

    let match_type = if prefixed.is_empty() {
        "none"
    } else {
        "prefix"
    };
    build_direct_diagnosis(
        state,
        model,
        endpoint,
        match_type,
        prefixed,
        prefixed_diagnosed,
        session_id,
    )
    .await
}

async fn build_direct_diagnosis(
    state: &AppState,
    model: &str,
    endpoint: &str,
    match_type: &str,
    rows: Vec<DiagnosticTargetRow>,
    diagnosed: Vec<(RouteDiagnoseTarget, bool)>,
    session_id: Option<&str>,
) -> AppResult<RouteDiagnoseView> {
    let eligible_pairs = rows
        .iter()
        .zip(&diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| (row.provider_id, row.upstream_model.clone()))
        .collect::<Vec<_>>();
    let matched = !rows.is_empty();
    let conflict = match_type == "direct" && eligible_pairs.len() > 1;
    let resolved = if match_type == "prefix" {
        !eligible_pairs.is_empty()
    } else {
        eligible_pairs.len() == 1
    };
    let effective_match_type = if conflict { "conflict" } else { match_type };
    let message = if !matched {
        format!("no enabled or disabled provider model matches '{model}'")
    } else if conflict {
        format!("model '{model}' exists on multiple providers; add a prefix or explicit route")
    } else if resolved {
        format!("direct model match can serve {endpoint}")
    } else {
        format!("model '{model}' exists but no target can serve {endpoint}")
    };
    let barrel = crate::registry::barrel_for_targets(&state.pool, &eligible_pairs).await?;
    let runtime_targets = if resolved {
        diagnostic_runtime_targets(
            state,
            None,
            RouteStrategy::Priority.as_str(),
            &rows,
            &diagnosed,
            session_id,
        )
        .await?
    } else {
        None
    };

    Ok(RouteDiagnoseView {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        matched,
        resolved,
        match_type: effective_match_type.to_string(),
        route_id: None,
        route_name: None,
        strategy: None,
        message,
        barrel: barrel.capabilities,
        barrel_incomplete: barrel.incomplete,
        session_id: session_id.map(ToOwned::to_owned),
        runtime_targets,
        targets: diagnosed.into_iter().map(|(target, _)| target).collect(),
    })
}

#[derive(Debug, sqlx::FromRow)]
struct DiagnosticTargetRow {
    provider_id: i64,
    provider_name: String,
    provider_type: String,
    target_id: i64,
    upstream_model: String,
    target_weight: i64,
    target_priority: i64,
    target_enabled: i64,
    provider_enabled: i64,
    model_exists: i64,
    model_enabled: i64,
    supported_endpoints: Option<String>,
    provider_health: Option<i64>,
}

impl DiagnosticTargetRow {
    fn diagnose(&self, endpoint: &str, route_enabled: bool) -> (RouteDiagnoseTarget, bool) {
        let reason = if !route_enabled {
            Some("route is disabled".to_string())
        } else if self.target_enabled == 0 {
            Some("route target is disabled".to_string())
        } else if self.provider_enabled == 0 {
            Some("provider is disabled".to_string())
        } else if self.model_exists != 0 && self.model_enabled == 0 {
            Some("model is disabled".to_string())
        } else {
            match ProviderType::from_str(&self.provider_type)
                .ok()
                .and_then(|provider_type| provider_upstream_endpoint(provider_type, endpoint))
            {
                None => Some("provider does not support this endpoint".to_string()),
                Some(upstream_endpoint)
                    if !endpoint_metadata_supports(
                        self.supported_endpoints.as_deref(),
                        upstream_endpoint,
                    ) =>
                {
                    Some("model does not declare support for this endpoint".to_string())
                }
                Some(_) => None,
            }
        };
        let eligible = reason.is_none();
        (
            RouteDiagnoseTarget {
                provider_id: self.provider_id,
                provider_name: self.provider_name.clone(),
                provider_type: self.provider_type.clone(),
                upstream_model: self.upstream_model.clone(),
                eligible,
                reason: reason.unwrap_or_else(|| "eligible".to_string()),
                supported_endpoints: supported_endpoint_list(self.supported_endpoints.as_deref()),
                provider_health: self.provider_health.map(|value| value != 0),
            },
            eligible,
        )
    }
}

async fn diagnostic_runtime_targets(
    state: &AppState,
    route_id: Option<i64>,
    strategy: &str,
    rows: &[DiagnosticTargetRow],
    diagnosed: &[(RouteDiagnoseTarget, bool)],
    session_id: Option<&str>,
) -> AppResult<Option<Vec<RouteDiagnoseRuntimeTarget>>> {
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let targets = rows
        .iter()
        .zip(diagnosed)
        .filter(|(_, (_, eligible))| *eligible)
        .map(|(row, _)| RouteTarget {
            id: row.target_id,
            route_id,
            provider_id: row.provider_id,
            provider_name: row.provider_name.clone(),
            provider_type: row.provider_type.clone(),
            base_url: String::new(),
            model_prefix: String::new(),
            api_key: None,
            provider_headers: "{}".to_string(),
            supported_endpoints: row.supported_endpoints.clone(),
            context_limit: None,
            input_limit: None,
            output_limit: None,
            provider_enabled: None,
            model_enabled: None,
            tool_search_supported: 1,
            provider_health: row.provider_health,
            upstream_model: row.upstream_model.clone(),
            weight: row.target_weight,
            priority: row.target_priority,
            enabled: 1,
            provider_api_key_id: None,
            provider_api_key_name: None,
            auth_retryable: false,
        })
        .collect::<Vec<_>>();
    let ordered = order_targets(
        state,
        route_id.unwrap_or(-1),
        strategy,
        targets,
        Some(session_id),
    )
    .await?;
    Ok(Some(
        ordered
            .into_iter()
            .enumerate()
            .map(|(index, target)| RouteDiagnoseRuntimeTarget {
                order: index + 1,
                provider_id: target.provider_id,
                provider_name: target.provider_name,
                upstream_model: target.upstream_model,
                provider_api_key_id: target.provider_api_key_id,
                provider_api_key_name: target.provider_api_key_name,
                provider_health: target.provider_health.map(|value| value != 0),
            })
            .collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
async fn resolve_route_or_log(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    request_id: &str,
    session_id: Option<&str>,
    model: &str,
    endpoint: &str,
    streamed: bool,
    started: Instant,
) -> AppResult<ResolvedRoute> {
    match resolve_route(state, model, endpoint).await {
        Ok(route) => Ok(route),
        Err(error) => {
            let status_code = match &error {
                AppError::BadRequest(_) => 400,
                AppError::Unauthorized(_) => 401,
                AppError::Forbidden(_) => 403,
                AppError::NotFound(_) => 404,
                AppError::Conflict(_) => 409,
                AppError::TooManyRequests(_) => 429,
                AppError::Upstream(_) => 502,
                AppError::Database(_) | AppError::Http(_) | AppError::Internal(_) => 500,
            };
            let message = error.to_string();
            log_request_rejection(
                state,
                api_key,
                request_id,
                session_id,
                model,
                endpoint,
                streamed,
                started,
                status_code,
                &message,
            )
            .await;
            Err(error)
        }
    }
}

async fn find_explicit_route(state: &AppState, model: &str) -> AppResult<Option<Route>> {
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT * FROM routes
        WHERE enabled = 1
        ORDER BY
            CASE WHEN instr(model_pattern, '*') = 0 AND instr(model_pattern, '?') = 0 THEN 0 ELSE 1 END,
            length(model_pattern) DESC,
            id
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    for route in routes {
        let Ok(glob) = Glob::new(&route.model_pattern) else {
            tracing::warn!(pattern = %route.model_pattern, "ignoring invalid route pattern");
            continue;
        };
        if glob.compile_matcher().is_match(model) {
            return Ok(Some(route));
        }
    }

    Ok(None)
}

async fn find_prefixed_targets(
    state: &AppState,
    model: &str,
    endpoint: &str,
) -> AppResult<Vec<RouteTarget>> {
    let prefixed = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT NULL AS id, NULL AS route_id, p.id AS provider_id,
               p.name AS provider_name, p.provider_type, p.base_url,
               p.model_prefix, p.api_key, p.headers AS provider_headers,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.tool_search_supported,
               p.last_test_ok AS provider_health,
               pm.model_name AS upstream_model,
               100 AS weight, 0 AS priority, 1 AS enabled
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
          AND p.model_prefix <> ''
          AND substr(?, 1, length(p.model_prefix)) = p.model_prefix
          AND substr(?, length(p.model_prefix) + 1) = pm.model_name
        ORDER BY p.id, pm.model_name
        "#,
    )
    .bind(model)
    .bind(model)
    .fetch_all(&state.pool)
    .await?;

    let prefixed = filter_targets_for_endpoint(prefixed, endpoint);
    if !prefixed.is_empty() {
        return Ok(prefixed);
    }

    let unprefixed = sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT NULL AS id, NULL AS route_id, p.id AS provider_id,
               p.name AS provider_name, p.provider_type, p.base_url,
               p.model_prefix, p.api_key, p.headers AS provider_headers,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.tool_search_supported,
               p.last_test_ok AS provider_health,
               pm.model_name AS upstream_model,
               100 AS weight, 0 AS priority, 1 AS enabled
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
          AND p.model_prefix = ''
          AND pm.model_name = ?
        ORDER BY p.id
        LIMIT 2
        "#,
    )
    .bind(model)
    .fetch_all(&state.pool)
    .await?;

    let unprefixed = filter_targets_for_endpoint(unprefixed, endpoint);
    if unprefixed.len() > 1 {
        return Err(AppError::Conflict(format!(
            "model '{model}' exists on multiple providers; configure a model prefix or an explicit route"
        )));
    }
    if unprefixed.is_empty() {
        return Err(AppError::NotFound(format!(
            "no enabled provider model matches '{model}' for endpoint '{endpoint}'"
        )));
    }
    Ok(unprefixed)
}

async fn load_targets(state: &AppState, route_id: i64) -> AppResult<Vec<RouteTarget>> {
    Ok(sqlx::query_as::<_, RouteTarget>(
        r#"
        SELECT rt.*, p.name AS provider_name, p.provider_type,
               p.base_url, p.model_prefix, p.api_key, p.headers AS provider_headers,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               p.tool_search_supported,
               p.last_test_ok AS provider_health,
               p.enabled AS provider_enabled
        FROM route_targets rt
        JOIN providers p ON p.id = rt.provider_id
        LEFT JOIN provider_models pm
          ON pm.provider_id = rt.provider_id
         AND pm.model_name = rt.upstream_model
        WHERE rt.route_id = ? AND rt.enabled = 1 AND p.enabled = 1
          AND NOT EXISTS (
              SELECT 1 FROM provider_models pm
              WHERE pm.provider_id = rt.provider_id
                AND pm.model_name = rt.upstream_model
                AND pm.enabled = 0
          )
        ORDER BY rt.priority ASC, rt.id
        "#,
    )
    .bind(route_id)
    .fetch_all(&state.pool)
    .await?)
}

fn provider_health_rank(health: Option<i64>) -> u8 {
    match health {
        Some(0) => 2,
        Some(_) => 0,
        None => 1,
    }
}

const PROVIDER_KEY_TOUCH_INTERVAL: Duration = Duration::from_secs(60);

fn provider_cooldown(status: Option<StatusCode>) -> Duration {
    match status {
        Some(StatusCode::TOO_MANY_REQUESTS) => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    }
}

fn provider_key_cooldown(status: StatusCode) -> Duration {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Duration::from_secs(300),
        StatusCode::TOO_MANY_REQUESTS => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    }
}

async fn mark_provider_api_key_used(state: &AppState, provider_api_key_id: Option<i64>) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    {
        let mut touched = state.provider_key_touched.lock().await;
        if touched
            .get(&provider_api_key_id)
            .is_some_and(|last| last.elapsed() < PROVIDER_KEY_TOUCH_INTERVAL)
        {
            return;
        }
        touched.insert(provider_api_key_id, Instant::now());
    }

    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?",
        )
        .bind(provider_api_key_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%error, provider_api_key_id, "failed to update provider key usage");
        }
    });
}

async fn mark_provider_error(state: &AppState, provider_id: i64, status: Option<StatusCode>) {
    state
        .provider_cooldown
        .lock()
        .await
        .insert(provider_id, Instant::now() + provider_cooldown(status));
}

async fn mark_provider_success(state: &AppState, provider_id: i64) {
    state.provider_cooldown.lock().await.remove(&provider_id);
}

async fn send_provider_request(
    state: &AppState,
    target: &RouteTarget,
    request: RequestBuilder,
) -> AppResult<reqwest::Response> {
    match request.send().await {
        Ok(response) => Ok(response),
        Err(error) => {
            mark_provider_error(state, target.provider_id, None).await;
            Err(AppError::Upstream(format!(
                "{} request failed: {error}",
                target.provider_name
            )))
        }
    }
}

async fn mark_provider_api_key_error(
    state: &AppState,
    provider_api_key_id: Option<i64>,
    status: StatusCode,
    message: &str,
) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    state.provider_key_cooldown.lock().await.insert(
        provider_api_key_id,
        Instant::now() + provider_key_cooldown(status),
    );
    state
        .provider_key_error_state
        .lock()
        .await
        .insert(provider_api_key_id);
    let message = message.chars().take(1000).collect::<String>();
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE provider_api_keys \
             SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 last_error_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 last_error = ? \
             WHERE id = ?",
        )
        .bind(message)
        .bind(provider_api_key_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%error, provider_api_key_id, "failed to record provider key error");
        }
    });
}

async fn mark_provider_api_key_success(state: &AppState, provider_api_key_id: Option<i64>) {
    let Some(provider_api_key_id) = provider_api_key_id else {
        return;
    };
    state
        .provider_key_cooldown
        .lock()
        .await
        .remove(&provider_api_key_id);
    let had_error = state
        .provider_key_error_state
        .lock()
        .await
        .remove(&provider_api_key_id);
    if !had_error {
        return;
    }
    if let Err(error) = sqlx::query(
        "UPDATE provider_api_keys \
         SET last_error_at = NULL, last_error = NULL \
         WHERE id = ?",
    )
    .bind(provider_api_key_id)
    .execute(&state.pool)
    .await
    {
        state
            .provider_key_error_state
            .lock()
            .await
            .insert(provider_api_key_id);
        tracing::warn!(
            %error,
            provider_api_key_id,
            "failed to clear recovered provider key error"
        );
    }
}

fn stable_hash64(parts: &[&[u8]]) -> u64 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 digests contain at least eight bytes"),
    )
}

fn session_target_hash(session_id: &str, ordering_key: i64, target: &RouteTarget) -> u64 {
    stable_hash64(&[
        b"openllm-session-target-v1",
        &ordering_key.to_be_bytes(),
        &target.id.to_be_bytes(),
        &target.provider_id.to_be_bytes(),
        target.upstream_model.as_bytes(),
        session_id.as_bytes(),
    ])
}

fn session_provider_key_hash(session_id: &str, provider_id: i64, key_id: i64) -> u64 {
    stable_hash64(&[
        b"openllm-session-provider-key-v1",
        &provider_id.to_be_bytes(),
        &key_id.to_be_bytes(),
        session_id.as_bytes(),
    ])
}

/// Orders weighted and round-robin targets without storing per-session state.
///
/// Weighted routes use weighted rendezvous hashing, so sessions still spread
/// according to target weights while a given session keeps the same preference
/// order. Priority routes already have a deterministic order and are left
/// untouched.
fn order_targets_for_session(
    session_id: &str,
    ordering_key: i64,
    strategy: RouteStrategy,
    targets: &mut Vec<RouteTarget>,
) {
    match strategy {
        RouteStrategy::Priority => {}
        RouteStrategy::Weighted => {
            let mut ranked = targets
                .drain(..)
                .map(|target| {
                    let hash = session_target_hash(session_id, ordering_key, &target);
                    let uniform = (hash as f64 + 1.0) / (u64::MAX as f64 + 1.0);
                    let score = -uniform.ln() / target.weight.max(1) as f64;
                    (score, target)
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                right
                    .0
                    .total_cmp(&left.0)
                    .then_with(|| left.1.id.cmp(&right.1.id))
            });
            targets.extend(ranked.into_iter().map(|(_, target)| target));
        }
        RouteStrategy::RoundRobin => {
            let mut ranked = targets
                .drain(..)
                .map(|target| {
                    (
                        session_target_hash(session_id, ordering_key, &target),
                        target,
                    )
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.id.cmp(&right.1.id))
            });
            targets.extend(ranked.into_iter().map(|(_, target)| target));
        }
    }
}

async fn expand_target_provider_keys(
    state: &AppState,
    targets: Vec<RouteTarget>,
    session_id: Option<&str>,
) -> AppResult<Vec<RouteTarget>> {
    let mut keys_by_provider = HashMap::new();
    for target in &targets {
        if keys_by_provider.contains_key(&target.provider_id) {
            continue;
        }
        let keys = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, name, secret FROM provider_api_keys \
             WHERE provider_id = ? AND enabled = 1 \
             ORDER BY id",
        )
        .bind(target.provider_id)
        .fetch_all(&state.pool)
        .await?;
        keys_by_provider.insert(target.provider_id, keys);
    }

    let mut cooldowns = state.provider_key_cooldown.lock().await;
    let now = Instant::now();
    cooldowns.retain(|_, until| *until > now);
    let mut cursors = state.provider_key_cursor.lock().await;
    let mut expanded = Vec::new();
    for target in targets {
        let keys = keys_by_provider
            .get(&target.provider_id)
            .cloned()
            .unwrap_or_default();
        if keys.is_empty() {
            expanded.push(target);
            continue;
        }

        let available = keys
            .iter()
            .filter(|(id, _, _)| !cooldowns.contains_key(id))
            .cloned()
            .collect::<Vec<_>>();
        let mut keys = if available.is_empty() {
            keys
        } else {
            available
        };
        if let Some(session_id) = session_id {
            let mut ranked = keys
                .drain(..)
                .map(|key| {
                    (
                        session_provider_key_hash(session_id, target.provider_id, key.0),
                        key,
                    )
                })
                .collect::<Vec<_>>();
            ranked
                .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.0.cmp(&right.1.0)));
            keys.extend(ranked.into_iter().map(|(_, key)| key));
        } else {
            let cursor = cursors.entry(target.provider_id).or_default();
            let offset = *cursor % keys.len();
            keys.rotate_left(offset);
            *cursor = cursor.wrapping_add(1);
        }

        for (key_id, key_name, secret) in keys {
            let mut candidate = target.clone();
            candidate.api_key = Some(secret);
            candidate.provider_api_key_id = Some(key_id);
            candidate.provider_api_key_name = Some(key_name);
            expanded.push(candidate);
        }
    }

    let last_index = expanded.len().saturating_sub(1);
    for (index, target) in expanded.iter_mut().enumerate() {
        target.auth_retryable = index < last_index;
    }
    Ok(expanded)
}

async fn order_targets(
    state: &AppState,
    route_id: i64,
    strategy: &str,
    mut targets: Vec<RouteTarget>,
    session_id: Option<&str>,
) -> AppResult<Vec<RouteTarget>> {
    let strategy = RouteStrategy::from_str(strategy).map_err(AppError::BadRequest)?;
    let session_id = session_id.map(str::trim).filter(|value| !value.is_empty());
    if let Some(session_id) = session_id {
        if strategy == RouteStrategy::Priority {
            targets.sort_by_key(|target| (target.priority, target.id));
        }
        order_targets_for_session(session_id, route_id, strategy, &mut targets);
    } else {
        match strategy {
            RouteStrategy::Priority => {
                targets.sort_by_key(|target| (target.priority, target.id));
            }
            RouteStrategy::Weighted => {
                let mut rng = rand::thread_rng();
                let mut selected = Vec::with_capacity(targets.len());
                while !targets.is_empty() {
                    let weights = targets
                        .iter()
                        .map(|target| target.weight.max(1) as u32)
                        .collect::<Vec<_>>();
                    let index = WeightedIndex::new(&weights)
                        .map(|distribution| distribution.sample(&mut rng))
                        .unwrap_or(0);
                    selected.push(targets.remove(index));
                }
                targets = selected;
            }
            RouteStrategy::RoundRobin => {
                let mut cursors = state.round_robin.lock().await;
                let cursor = cursors.entry(route_id).or_default();
                let offset = *cursor % targets.len();
                targets.rotate_left(offset);
                *cursor = cursor.wrapping_add(1);
            }
        }
    }
    // Prefer providers outside their runtime cooldown window. If every
    // candidate is cooling, keep them all so an all-cooling route still has a
    // chance to recover instead of failing before it reaches the upstream.
    let mut provider_cooldowns = state.provider_cooldown.lock().await;
    let now = Instant::now();
    provider_cooldowns.retain(|_, until| *until > now);
    if targets
        .iter()
        .any(|target| !provider_cooldowns.contains_key(&target.provider_id))
    {
        targets.retain(|target| !provider_cooldowns.contains_key(&target.provider_id));
    }
    drop(provider_cooldowns);

    // Keep strategy order within each health group, but try explicitly failed
    // providers last. Unknown health stays ahead of failed providers so a
    // previously-tested outage does not permanently suppress a recovery.
    targets.sort_by_key(|target| provider_health_rank(target.provider_health));
    expand_target_provider_keys(state, targets, session_id).await
}

struct UsageParser {
    buffer: Vec<u8>,
    usage: Option<Usage>,
    output_chars: usize,
    text: String,
    started_at: Instant,
    first_token_ms: Option<i64>,
}

impl UsageParser {
    fn new(started_at: Instant) -> Self {
        Self {
            buffer: Vec::new(),
            usage: None,
            output_chars: 0,
            text: String::new(),
            started_at,
            first_token_ms: None,
        }
    }

    /// Records time-to-first-token the first time any real content arrives.
    fn mark_first_token(&mut self) {
        if self.first_token_ms.is_none() {
            self.first_token_ms = Some(self.started_at.elapsed().as_millis() as i64);
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=position).collect::<Vec<_>>();
            self.parse_line(&line);
        }
    }

    fn finish(&mut self) -> Option<Usage> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.parse_line(&line);
        }
        let estimated =
            (self.output_chars / 4).max(if self.output_chars > 0 { 1 } else { 0 }) as i64;
        match self.usage.take() {
            Some(mut usage) => {
                if usage.completion_tokens == 0 {
                    usage.completion_tokens = estimated.max(1);
                }
                Some(usage.normalized())
            }
            // No usage frame was sent: fall back to the characters we actually
            // saw streamed, not a fixed constant.
            None if self.output_chars > 0 => Some(Usage {
                prompt_tokens: 0,
                completion_tokens: estimated,
                total_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            None => None,
        }
    }

    fn preview(&self) -> Option<String> {
        response_preview(self.text.as_bytes())
    }

    fn parse_line(&mut self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        let Some(data) = line.trim().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(usage) = usage_from_value(&value) {
            // Merge instead of replace: Anthropic reports input tokens in
            // `message_start` and output tokens later in `message_delta`, so
            // overwriting would drop whichever side arrived first.
            self.usage = Some(match self.usage.take() {
                Some(previous) => Usage {
                    prompt_tokens: if usage.prompt_tokens > 0 {
                        usage.prompt_tokens
                    } else {
                        previous.prompt_tokens
                    },
                    completion_tokens: if usage.completion_tokens > 0 {
                        usage.completion_tokens
                    } else {
                        previous.completion_tokens
                    },
                    total_tokens: 0,
                    // Cache counts arrive alongside input tokens in the same
                    // event, so take the newer value but keep the older one
                    // when this event simply did not mention caching.
                    cache_read_tokens: if usage.cache_read_tokens > 0 {
                        usage.cache_read_tokens
                    } else {
                        previous.cache_read_tokens
                    },
                    cache_write_tokens: if usage.cache_write_tokens > 0 {
                        usage.cache_write_tokens
                    } else {
                        previous.cache_write_tokens
                    },
                }
                .normalized(),
                None => usage,
            });
        }
        // Native Anthropic streams emit text, reasoning, and tool arguments as
        // content block deltas instead of OpenAI choices.
        if let Some(delta) = value.get("delta") {
            let delta_type = delta.get("type").and_then(Value::as_str);
            let content = match delta_type {
                Some("text_delta") => delta.get("text").and_then(Value::as_str),
                Some("thinking_delta") => delta.get("thinking").and_then(Value::as_str),
                Some("input_json_delta") => delta.get("partial_json").and_then(Value::as_str),
                _ => None,
            };
            if let Some(content) = content.filter(|content| !content.is_empty()) {
                self.mark_first_token();
                self.output_chars += content.chars().count();
                if delta_type == Some("text_delta") {
                    push_preview_text(&mut self.text, content);
                }
            }
        }
        // Tool names arrive before their argument deltas. Count the block start
        // as the first output token for tool-only responses.
        if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
            self.mark_first_token();
        }
        if let Some(content) = value
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += content.chars().count();
            push_preview_text(&mut self.text, content);
        }
        // Legacy `/v1/completions` streams put the text in `choices[0].text`
        // instead of a delta object.
        if let Some(text) = value.pointer("/choices/0/text").and_then(Value::as_str) {
            if !text.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += text.chars().count();
            push_preview_text(&mut self.text, text);
        }
        if let Some(text) = value.get("output_text").and_then(Value::as_str) {
            if !text.is_empty() {
                self.mark_first_token();
            }
            self.output_chars += text.chars().count();
            push_preview_text(&mut self.text, text);
        }
        for path in [
            "/choices/0/delta/reasoning_content",
            "/choices/0/delta/reasoning",
        ] {
            if let Some(delta) = value.pointer(path).and_then(Value::as_str)
                && !delta.is_empty()
            {
                self.mark_first_token();
                self.output_chars += delta.chars().count();
            }
        }
        if let Some(tool_calls) = value
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in tool_calls {
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                    && !arguments.is_empty()
                {
                    self.mark_first_token();
                    self.output_chars += arguments.chars().count();
                }
            }
        }
        if let Some(arguments) = value
            .pointer("/choices/0/delta/function_call/arguments")
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            self.mark_first_token();
            self.output_chars += arguments.chars().count();
        }
        // Responses API emits several output delta families: visible text,
        // reasoning summaries, and function-call arguments. Tool-only turns
        // still produce tokens, so any non-empty `response.*.delta` starts the
        // first-token clock.
        if let Some(kind) = value
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| kind.starts_with("response.") && kind.ends_with(".delta"))
            && let Some(delta) = value.get("delta").and_then(Value::as_str)
            && !delta.is_empty()
        {
            self.mark_first_token();
            self.output_chars += delta.chars().count();
            if kind == "response.output_text.delta" {
                push_preview_text(&mut self.text, delta);
            }
        }
    }
}

fn extract_usage_from_json(bytes: &[u8]) -> Option<Usage> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    usage_from_value(&value)
}

fn usage_from_value(value: &Value) -> Option<Usage> {
    let usage = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        // Native Anthropic streams report usage in two different places:
        // `message_start` nests it under `message.usage`, while `message_delta`
        // carries a top-level `usage` (handled by the first branch above).
        .or_else(|| value.pointer("/message/usage"))?;
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let cache_read_tokens = cache_read_of(usage);
    let cache_write_tokens = cache_write_of(usage);
    // Normalise input accounting across providers.
    //
    // OpenAI-style responses already fold cache traffic into `prompt_tokens`
    // (verified: prompt_tokens stays constant while `cached_tokens` rises), so
    // the value is used as-is. Anthropic reports the cache numbers *besides*
    // `input_tokens`, so the totals only add up once they are included.
    // The discriminator is placement: nested under `prompt_tokens_details` means
    // already counted; top-level means additional.
    let cache_included_in_prompt = usage.pointer("/prompt_tokens_details").is_some()
        || usage.pointer("/input_tokens_details").is_some();
    let prompt_tokens = if cache_included_in_prompt {
        prompt_tokens
    } else {
        prompt_tokens + cache_read_tokens + cache_write_tokens
    };
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(prompt_tokens + completion_tokens)
        .max(prompt_tokens + completion_tokens);
    Some(
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
            cache_write_tokens,
        }
        .normalized(),
    )
}

/// Reads cache-read tokens across provider shapes.
///
/// Anthropic reports `cache_read_input_tokens` at the top level; OpenAI-style
/// providers nest `cached_tokens` (and, on some gateways,
/// `cache_read_input_tokens`) under `prompt_tokens_details`.
fn cache_read_of(usage: &Value) -> i64 {
    [
        usage.get("cache_read_input_tokens"),
        usage.pointer("/prompt_tokens_details/cached_tokens"),
        usage.pointer("/prompt_tokens_details/cache_read_input_tokens"),
        usage.pointer("/input_tokens_details/cached_tokens"),
        usage.get("cache_read_tokens"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_i64)
    .unwrap_or_default()
}

/// Reads cache-write tokens across provider shapes. Anthropic calls these
/// `cache_creation_input_tokens`; some OpenAI-compatible gateways use
/// `cache_write_tokens`.
fn cache_write_of(usage: &Value) -> i64 {
    [
        usage.get("cache_creation_input_tokens"),
        usage.get("cache_write_tokens"),
        usage.pointer("/prompt_tokens_details/cache_write_tokens"),
        usage.pointer("/prompt_tokens_details/cache_creation_input_tokens"),
        usage.pointer("/input_tokens_details/cache_write_tokens"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_i64)
    .unwrap_or_default()
}

fn estimated_completion_usage(request_tokens: i64, bytes: &[u8]) -> Usage {
    let completion_tokens = (bytes.len() / 4).max(1) as i64;
    Usage::new(request_tokens, completion_tokens)
}

fn estimate_request_tokens(value: &Value) -> i64 {
    // Walk the structured content and sum only text-bearing fields. The
    // previous implementation stringified the whole payload, so JSON keys,
    // braces and quotes inflated the estimate on every request.
    let chars = count_text_chars(value, None);
    // Rough per-item framing overhead: providers wrap each message and tool in
    // a handful of structural tokens beyond its visible text.
    let items = value
        .get("messages")
        .or_else(|| value.get("input"))
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.len())
        .unwrap_or(0);
    let framing = (items as i64) * 4 + (tools as i64) * 8;
    (chars as i64 / 4 + framing).max(1)
}

/// Field names whose string values represent prompt content worth counting.
/// Anything else (ids, roles, media URLs, base64 blobs) is ignored so the
/// estimate tracks text, not wire format.
const TEXT_BEARING_KEYS: [&str; 9] = [
    "text",
    "content",
    "input",
    "prompt",
    "system",
    "description",
    "arguments",
    "partial_json",
    "name",
];

/// Sums the characters of text-bearing strings nested anywhere in `value`.
///
/// A string is counted only when its key is text-bearing, or when it is the
/// root value (as in a bare `input: "..."` payload). This deliberately skips
/// `image_url` / base64 data, which providers meter separately from text.
fn count_text_chars(value: &Value, key: Option<&str>) -> usize {
    match value {
        Value::String(text) => {
            if key.is_none_or(|key| TEXT_BEARING_KEYS.contains(&key)) {
                text.chars().count()
            } else {
                0
            }
        }
        Value::Array(items) => items.iter().map(|item| count_text_chars(item, key)).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| count_text_chars(value, Some(key)))
            .sum(),
        _ => 0,
    }
}

fn fill_usage(usage: Usage, request_tokens: i64, response: &Value) -> Usage {
    let mut usage = usage;
    if usage.prompt_tokens == 0 {
        usage.prompt_tokens = request_tokens;
    }
    if usage.completion_tokens == 0 {
        usage.completion_tokens = (response.to_string().chars().count() / 4).max(1) as i64;
    }
    usage.normalized()
}

fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

pub(crate) fn join_upstream_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = format!("/{}", path.trim_start_matches('/'));
    if base.ends_with("/v1") && path.starts_with("/v1/") {
        format!("{base}{}", &path[3..])
    } else {
        format!("{base}{path}")
    }
}

pub(crate) fn apply_custom_headers(
    mut request: RequestBuilder,
    headers: &str,
) -> AppResult<RequestBuilder> {
    let Ok(headers) = serde_json::from_str::<Value>(headers) else {
        return Ok(request);
    };
    let Some(headers) = headers.as_object() else {
        return Ok(request);
    };
    for (name, value) in headers {
        let Some(value) = value.as_str() else {
            continue;
        };
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            AppError::BadRequest(format!("invalid header name '{name}': {error}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            AppError::BadRequest(format!("invalid value for header '{name}': {error}"))
        })?;
        request = request.header(name, value);
    }
    Ok(request)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn hash_secret(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// Detects the opaque 4xx that aggregator upstreams return when one of their
/// internal channels fails: an "invalid request error" that carries only a
/// trace id and no actionable `param`. These are transient routing failures, so
/// retrying is worthwhile instead of surfacing them as client errors.
fn transient_upstream_4xx(status: StatusCode, body: &[u8]) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    text.contains("trace_id") && text.contains("invalid request") && !text.contains("\"param\"")
}

fn should_try_next_target(target: &RouteTarget, status: StatusCode) -> bool {
    retryable_status(status)
        || (target.auth_retryable
            && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN))
}

fn provider_key_failure(status: StatusCode) -> bool {
    retryable_status(status) || matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

/// Downward compatibility for multi-target routes.
///
/// A route may fan out to models with different ceilings. Clamping the
/// requested output length to the strictest common `output_limit` keeps the
/// request valid for *every* target, so whichever one the strategy picks can
/// serve it instead of failing with a "max_tokens too large" error.
///
/// Returns the clamped value when a reduction happened, so the caller can
/// surface it in the response receipt.
fn clamp_output_request(body: &mut Value, barrel: Option<&BarrelEnvelope>) -> Option<i64> {
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

fn requested_output_tokens_of(body: &Value) -> Option<i64> {
    ["max_tokens", "max_completion_tokens", "max_output_tokens"]
        .into_iter()
        .find_map(|key| body.get(key).and_then(Value::as_i64))
}

/// Builds the capability receipt returned alongside a completion.
///
/// `requested_output_tokens` and `clamped_output_tokens` make the barrel
/// behaviour visible: a client that asked for more than the route supports can
/// see that the gateway reduced the request rather than silently ignoring it.
fn capability_receipt(
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
fn inject_capability_receipt(bytes: &[u8], receipt: &Option<Value>) -> Option<Vec<u8>> {
    let receipt = receipt.as_ref()?;
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    value
        .as_object_mut()?
        .insert("capabilities".to_string(), receipt.clone());
    serde_json::to_vec(&value).ok()
}

/// Limits mirrored onto response headers so streaming clients, which never
/// receive a single JSON body, can still read the effective ceiling.
fn apply_capability_headers(response: &mut Response, receipt: &Option<Value>) {
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

/// Keep a short, human-readable slice of the response for later debugging
/// without storing unbounded payloads.
fn response_preview(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = trimmed.chars().take(2000).collect::<String>();
    Some(preview)
}

/// Upper bound on how much streamed text is retained for the preview. Anything
/// beyond this is discarded as it arrives, so a very long generation does not
/// buffer its entire output in memory just to store 2000 characters.
const PREVIEW_CHAR_LIMIT: usize = 2000;

/// Appends `chunk` to `target` only while the preview budget allows it.
fn push_preview_text(target: &mut String, chunk: &str) {
    let remaining = PREVIEW_CHAR_LIMIT.saturating_sub(target.chars().count());
    if remaining == 0 {
        return;
    }
    target.extend(chunk.chars().take(remaining));
}

struct UsageLogEntry<'a> {
    request_id: &'a str,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    provider_id: Option<i64>,
    requested_model: &'a str,
    upstream_model: Option<&'a str>,
    endpoint: &'a str,
    usage: Usage,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    status_code: i64,
    success: bool,
    streamed: bool,
    error_message: Option<&'a str>,
    response_preview: Option<&'a str>,
}

/// Owned mirror of [`UsageLogEntry`]; needed when the log is written from a
/// detached task that outlives the request's borrowed values.
struct OwnedUsageLogEntry {
    request_id: String,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    provider_id: Option<i64>,
    requested_model: String,
    upstream_model: Option<String>,
    endpoint: String,
    usage: Usage,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    status_code: i64,
    success: bool,
    streamed: bool,
    error_message: Option<String>,
    response_preview: Option<String>,
}

impl OwnedUsageLogEntry {
    fn as_borrowed(&self) -> UsageLogEntry<'_> {
        UsageLogEntry {
            request_id: &self.request_id,
            api_key_id: self.api_key_id,
            route_id: self.route_id,
            provider_id: self.provider_id,
            requested_model: &self.requested_model,
            upstream_model: self.upstream_model.as_deref(),
            endpoint: &self.endpoint,
            usage: self.usage,
            latency_ms: self.latency_ms,
            first_token_ms: self.first_token_ms,
            status_code: self.status_code,
            success: self.success,
            streamed: self.streamed,
            error_message: self.error_message.as_deref(),
            response_preview: self.response_preview.as_deref(),
        }
    }
}

/// Writes the usage row off the response path so a slow SQLite write lock does
/// not inflate the latency the caller observes.
fn log_usage_detached(state: AppState, entry: OwnedUsageLogEntry) {
    tokio::spawn(async move {
        log_usage(&state, entry.as_borrowed()).await;
    });
}

#[allow(clippy::too_many_arguments)]
async fn log_usage_started(
    state: &AppState,
    request_id: &str,
    session_id: Option<&str>,
    api_key_id: Option<i64>,
    route_id: Option<i64>,
    requested_model: &str,
    endpoint: &str,
    request_tokens: i64,
    streamed: bool,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, session_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, response_preview, last_activity_at
        ) VALUES (
            ?, ?, ?, ?, NULL, ?, NULL, ?, ?, 0, ?, 0, 0, 0, NULL, NULL, 0, 1, 0, ?,
            NULL, NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        )
        ON CONFLICT(request_id) DO UPDATE SET
            session_id = COALESCE(excluded.session_id, usage_logs.session_id),
            route_id = excluded.route_id,
            requested_model = excluded.requested_model,
            endpoint = excluded.endpoint,
            prompt_tokens = excluded.prompt_tokens,
            total_tokens = excluded.total_tokens,
            streamed = excluded.streamed,
            last_activity_at = excluded.last_activity_at,
            in_flight = 1
        WHERE usage_logs.in_flight = 1
        "#,
    )
    .bind(request_id)
    .bind(session_id)
    .bind(api_key_id)
    .bind(route_id)
    .bind(requested_model)
    .bind(endpoint)
    .bind(request_tokens)
    .bind(request_tokens)
    .bind(streamed as i64)
    .execute(&state.pool)
    .await;

    match result {
        Ok(result) => {
            if result.rows_affected() > 0 {
                match sqlx::query_scalar::<_, i64>("SELECT id FROM usage_logs WHERE request_id = ?")
                    .bind(request_id)
                    .fetch_one(&state.pool)
                    .await
                {
                    Ok(id) => {
                        let _ = state.events.send(crate::state::UsageEvent {
                            id,
                            request_id: request_id.to_string(),
                            success: false,
                            streamed,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, request_id, "failed to find in-flight usage row");
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, request_id, "failed to write in-flight usage log");
        }
    }
}

async fn log_usage_target(
    state: &AppState,
    request_id: &str,
    provider_id: i64,
    upstream_model: &str,
    provider_api_key_id: Option<i64>,
) {
    if let Err(error) = sqlx::query(
        "UPDATE usage_logs SET provider_id = ?, provider_api_key_id = ?, \
             provider_api_key_name = (SELECT name FROM provider_api_keys WHERE id = ?), \
             upstream_model = ?, \
             last_activity_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE request_id = ? AND in_flight = 1",
    )
    .bind(provider_id)
    .bind(provider_api_key_id)
    .bind(provider_api_key_id)
    .bind(upstream_model)
    .bind(request_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(%error, request_id, "failed to update in-flight usage target");
    }
}

async fn log_usage(state: &AppState, entry: UsageLogEntry<'_>) {
    let usage = entry.usage.normalized();
    let estimated_cost_micros =
        if matches!(entry.status_code, 400 | 403 | 429) && entry.provider_id.is_none() {
            Some(0)
        } else {
            match (entry.provider_id, entry.upstream_model) {
                (Some(provider_id), Some(upstream_model)) if usage.has_tokens() => {
                    estimate_usage_cost(state, provider_id, upstream_model, usage).await
                }
                _ => None,
            }
        };
    let result = sqlx::query(
        r#"
        INSERT INTO usage_logs (
            request_id, api_key_id, route_id, provider_id, requested_model,
            upstream_model, endpoint, prompt_tokens, completion_tokens,
            total_tokens, cache_read_tokens, cache_write_tokens, latency_ms,
            estimated_cost_micros, first_token_ms, status_code, in_flight,
            success, streamed, error_message, response_preview, last_activity_at
        ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        )
        ON CONFLICT(request_id) DO UPDATE SET
            api_key_id = excluded.api_key_id,
            route_id = excluded.route_id,
            provider_id = excluded.provider_id,
            requested_model = excluded.requested_model,
            upstream_model = excluded.upstream_model,
            endpoint = excluded.endpoint,
            prompt_tokens = excluded.prompt_tokens,
            completion_tokens = excluded.completion_tokens,
            total_tokens = excluded.total_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cache_write_tokens = excluded.cache_write_tokens,
            latency_ms = excluded.latency_ms,
            estimated_cost_micros = excluded.estimated_cost_micros,
            first_token_ms = excluded.first_token_ms,
            status_code = excluded.status_code,
            in_flight = 0,
            success = excluded.success,
            streamed = excluded.streamed,
            error_message = excluded.error_message,
            response_preview = excluded.response_preview,
            last_activity_at = excluded.last_activity_at
        "#,
    )
    .bind(entry.request_id)
    .bind(entry.api_key_id)
    .bind(entry.route_id)
    .bind(entry.provider_id)
    .bind(entry.requested_model)
    .bind(entry.upstream_model)
    .bind(entry.endpoint)
    .bind(usage.prompt_tokens)
    .bind(usage.completion_tokens)
    .bind(usage.total_tokens)
    .bind(usage.cache_read_tokens)
    .bind(usage.cache_write_tokens)
    .bind(entry.latency_ms)
    .bind(estimated_cost_micros)
    .bind(entry.first_token_ms)
    .bind(entry.status_code)
    .bind(entry.success as i64)
    .bind(entry.streamed as i64)
    .bind(entry.error_message)
    .bind(entry.response_preview)
    .execute(&state.pool)
    .await;

    match result {
        Ok(_) => {
            let id = sqlx::query_scalar::<_, i64>("SELECT id FROM usage_logs WHERE request_id = ?")
                .bind(entry.request_id)
                .fetch_one(&state.pool)
                .await;
            match id {
                Ok(id) => {
                    let _ = state.events.send(crate::state::UsageEvent {
                        id,
                        request_id: entry.request_id.to_string(),
                        success: entry.success,
                        streamed: entry.streamed,
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_id = entry.request_id,
                        "failed to read usage log id after update"
                    );
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, request_id = %entry.request_id, "failed to write usage log");
        }
    }
}

async fn estimate_usage_cost(
    state: &AppState,
    provider_id: i64,
    upstream_model: &str,
    usage: Usage,
) -> Option<i64> {
    let row = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
        ),
    >(
        "SELECT cost, cost_input_override, cost_output_override, \
                cost_cache_read_override, cost_cache_write_override \
         FROM provider_models \
         WHERE provider_id = ? AND model_name = ? AND enabled = 1",
    )
    .bind(provider_id)
    .bind(upstream_model)
    .fetch_optional(&state.pool)
    .await
    .ok()??;
    let synced = row
        .0
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let cost = effective_cost_value(synced.as_ref(), row.1, row.2, row.3, row.4)?;
    estimate_cost_micros(Some(&cost), usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_estimate_ignores_json_scaffolding() {
        // A short message must not be inflated by keys, braces and quotes.
        let body = json!({
            "model": "claude-x",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let estimate = estimate_request_tokens(&body);
        // 5 chars of content -> 1 token, plus one message of framing (4).
        assert_eq!(estimate, 5);
        // Sanity: far below the old "stringify the whole payload" behaviour.
        let stringified = body.to_string().chars().count() / 4;
        assert!(estimate < stringified as i64, "{estimate} vs {stringified}");
    }

    #[test]
    fn text_estimate_counts_nested_content_blocks() {
        let body = json!({
            "model": "claude-x",
            "system": "be brief",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "abcdefgh"},
                    {"type": "text", "text": "ijkl"}
                ]}
            ]
        });
        // 8 ("be brief") + 12 (content) = 20 chars -> 5 tokens + 4 framing.
        assert_eq!(estimate_request_tokens(&body), 9);
    }

    #[test]
    fn text_estimate_skips_image_payloads() {
        // A base64 image must not be metered as prompt text.
        let body = json!({
            "model": "claude-x",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "describe"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAAAAAABBBBBBBBCCCCCCCC"}}
            ]}]
        });
        let estimate = estimate_request_tokens(&body);
        // Only "describe" (8 chars -> 2) plus one message of framing (4).
        assert_eq!(estimate, 6);
    }

    #[test]
    fn text_estimate_is_deterministic_and_grows_with_content() {
        let small = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let large =
            json!({"model": "m", "messages": [{"role": "user", "content": "hi".repeat(200)}]});
        assert_eq!(
            estimate_request_tokens(&small),
            estimate_request_tokens(&small)
        );
        assert!(estimate_request_tokens(&large) > estimate_request_tokens(&small));
    }

    #[test]
    fn detects_anthropic_clients_from_the_version_header() {
        let mut anthropic = HeaderMap::new();
        anthropic.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        assert!(wants_anthropic_models(&anthropic));

        // OpenAI clients authenticate with a bearer token and send no
        // anthropic-version header, so they keep the OpenAI shape.
        let mut openai = HeaderMap::new();
        openai.insert("authorization", HeaderValue::from_static("Bearer sk-test"));
        assert!(!wants_anthropic_models(&openai));
        assert!(!wants_anthropic_models(&HeaderMap::new()));
    }

    #[tokio::test]
    async fn gateway_errors_expose_the_request_id_header() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let state = AppState::new(pool, None);

        let response = proxy_openai(
            State(state.clone()),
            HeaderMap::new(),
            "/v1/chat/completions".parse().unwrap(),
            Bytes::from_static(b"{"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let request_id = response
            .headers()
            .get("x-openllm-request-id")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(response.headers().get("x-request-id").unwrap(), request_id);

        let response = proxy_anthropic(
            State(state),
            HeaderMap::new(),
            "/v1/messages".parse().unwrap(),
            Bytes::from_static(b"{"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let request_id = response
            .headers()
            .get("x-openllm-request-id")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(response.headers().get("x-request-id").unwrap(), request_id);
    }

    #[tokio::test]
    async fn console_api_key_selection_uses_id_without_exposing_secret() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE api_keys (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                key_hash TEXT NOT NULL,
                key_prefix TEXT NOT NULL,
                key_suffix TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                last_used_at TEXT,
                created_at TEXT NOT NULL,
                daily_token_limit INTEGER,
                daily_cost_limit_micros INTEGER,
                requests_per_minute INTEGER,
                max_concurrency INTEGER,
                allowed_models TEXT,
                expires_at TEXT
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix, enabled, created_at
             ) VALUES (7, 'Console', 'hash', 'sk-con', 'sole', 1, '2026-10-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);

        let error = selected_console_api_key(&state, &HeaderMap::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppError::BadRequest(message) if message.contains("select a gateway API key")
        ));

        let mut headers = HeaderMap::new();
        headers.insert(CONSOLE_API_KEY_ID_HEADER, HeaderValue::from_static("7"));
        let key = selected_console_api_key(&state, &headers)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.id, 7);
        assert_eq!(key.name, "Console");
        assert_eq!(key.key_suffix, "sole");
    }

    #[test]
    fn requires_max_tokens_like_the_anthropic_api() {
        // Missing entirely: the live API answers 400, so the gateway must too
        // rather than silently defaulting and hiding a client bug.
        assert!(validate_anthropic_max_tokens(&json!({"messages": []})).is_err());
        // Present and positive is the only accepted form.
        assert!(validate_anthropic_max_tokens(&json!({"max_tokens": 1})).is_ok());
        assert!(validate_anthropic_max_tokens(&json!({"max_tokens": 4096})).is_ok());
        // Zero, negative and non-numeric values are rejected.
        assert!(validate_anthropic_max_tokens(&json!({"max_tokens": 0})).is_err());
        assert!(validate_anthropic_max_tokens(&json!({"max_tokens": -5})).is_err());
        assert!(validate_anthropic_max_tokens(&json!({"max_tokens": "1024"})).is_err());
        assert!(validate_anthropic_max_tokens(&json!({"max_tokens": null})).is_err());
    }

    #[test]
    fn forwards_anthropic_beta_flags_verbatim() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("prompt-caching-2024-07-31"),
        );
        assert_eq!(
            anthropic_beta_of(&headers).as_deref(),
            Some("prompt-caching-2024-07-31")
        );

        // Absent or blank values must not produce a header, so the gateway
        // never invents feature flags the caller did not ask for.
        assert_eq!(anthropic_beta_of(&HeaderMap::new()), None);
        let mut blank = HeaderMap::new();
        blank.insert("anthropic-beta", HeaderValue::from_static("   "));
        assert_eq!(anthropic_beta_of(&blank), None);
    }

    #[test]
    fn resolves_stable_session_ids_from_headers_and_responses_body() {
        let mut headers = HeaderMap::new();
        headers.insert(
            OPENCODE_SESSION_HEADER,
            HeaderValue::from_static("opencode-session"),
        );
        headers.insert("session-id", HeaderValue::from_static("codex-session"));
        assert_eq!(
            upstream_session_id(&headers, &json!({})).as_deref(),
            Some("opencode-session")
        );

        let mut codex = HeaderMap::new();
        codex.insert("session-id", HeaderValue::from_static("codex-session"));
        assert_eq!(
            upstream_session_id(&codex, &json!({})).as_deref(),
            Some("codex-session")
        );
        assert_eq!(
            upstream_session_id(
                &HeaderMap::new(),
                &json!({"prompt_cache_key": "cache-session"})
            )
            .as_deref(),
            Some("cache-session")
        );
    }

    #[test]
    fn adds_opencode_session_only_for_opencode_go_targets() {
        let mut opencode = endpoint_test_target("openai", None);
        opencode.provider_name = "OpenCode Go".to_string();
        let request = apply_opencode_session_header(
            reqwest::Client::new().post("http://upstream"),
            &opencode,
            Some("session-123"),
        )
        .build()
        .unwrap();
        assert_eq!(
            request.headers().get(OPENCODE_SESSION_HEADER).unwrap(),
            "session-123"
        );

        let other = endpoint_test_target("openai", None);
        let request = apply_opencode_session_header(
            reqwest::Client::new().post("http://upstream"),
            &other,
            Some("session-123"),
        )
        .build()
        .unwrap();
        assert!(request.headers().get(OPENCODE_SESSION_HEADER).is_none());
    }

    #[tokio::test]
    async fn upstream_request_includes_the_resolved_session() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let state = AppState::new(pool, None);
        let mut opencode = endpoint_test_target("openai", None);
        opencode.provider_name = "OpenCode Go".to_string();

        let request = build_upstream_request(
            &state,
            "http://upstream/v1/chat/completions",
            ProviderType::Openai,
            &opencode,
            &json!({"model": "test"}),
            Some("session-123"),
        )
        .unwrap()
        .build()
        .unwrap();

        assert_eq!(
            request.headers().get(OPENCODE_SESSION_HEADER).unwrap(),
            "session-123"
        );
    }

    #[test]
    fn configured_opencode_session_header_takes_precedence() {
        let mut opencode = endpoint_test_target("openai", None);
        opencode.base_url = "https://opencode.ai/zen/go/v1".to_string();
        opencode.provider_headers = r#"{"x-opencode-session":"configured-session"}"#.to_string();

        let request = apply_opencode_session_header(
            reqwest::Client::new().post("http://upstream"),
            &opencode,
            Some("request-session"),
        )
        .build()
        .unwrap();
        assert!(request.headers().get(OPENCODE_SESSION_HEADER).is_none());
    }

    /// Builds a page of model entries with the given ids, matching the shape
    /// `anthropic_models` produces.
    fn model_page(ids: &[&str]) -> Vec<Value> {
        ids.iter()
            .map(|id| json!({"type": "model", "id": id, "display_name": id, "created_at": Value::Null}))
            .collect()
    }

    fn ids_of(models: &[Value]) -> Vec<String> {
        models
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect()
    }

    #[test]
    fn paginates_forward_with_after_id() {
        let models = model_page(&["a", "b", "c", "d", "e"]);
        let query = ModelPageQuery {
            limit: Some(2),
            after_id: Some("b".to_string()),
            before_id: None,
        };
        let (window, has_more) = paginate_models(models, &query);
        assert_eq!(ids_of(&window), vec!["c", "d"]);
        assert!(has_more, "e remains beyond the window");
    }

    #[test]
    fn paginates_backward_with_before_id() {
        let models = model_page(&["a", "b", "c", "d", "e"]);
        let query = ModelPageQuery {
            limit: Some(2),
            after_id: None,
            before_id: Some("d".to_string()),
        };
        let (window, has_more) = paginate_models(models, &query);
        // The page ends just before the cursor, keeping the last two entries.
        assert_eq!(ids_of(&window), vec!["b", "c"]);
        // `d`/`e` still sit after `last_id`, so the client can page forward.
        assert!(has_more);
    }

    #[test]
    fn reports_has_more_on_a_partial_first_page() {
        let models = model_page(&["a", "b", "c"]);
        let query = ModelPageQuery {
            limit: Some(2),
            after_id: None,
            before_id: None,
        };
        let (window, has_more) = paginate_models(models, &query);
        assert_eq!(ids_of(&window), vec!["a", "b"]);
        assert!(has_more);
    }

    #[test]
    fn returns_last_page_without_claiming_more() {
        let models = model_page(&["a", "b", "c"]);
        let query = ModelPageQuery {
            limit: Some(10),
            after_id: None,
            before_id: None,
        };
        let (window, has_more) = paginate_models(models, &query);
        assert_eq!(ids_of(&window), vec!["a", "b", "c"]);
        assert!(!has_more);
    }

    #[test]
    fn unknown_cursor_yields_an_empty_page() {
        let models = model_page(&["a", "b"]);
        let query = ModelPageQuery {
            limit: Some(2),
            after_id: Some("missing".to_string()),
            before_id: None,
        };
        let (window, has_more) = paginate_models(models, &query);
        assert!(window.is_empty());
        assert!(!has_more);
    }

    #[test]
    fn page_size_defaults_and_clamps_to_anthropic_bounds() {
        assert_eq!(
            ModelPageQuery::default().page_size(),
            ANTHROPIC_DEFAULT_PAGE_SIZE
        );
        let tiny = ModelPageQuery {
            limit: Some(0),
            ..Default::default()
        };
        assert_eq!(tiny.page_size(), 1, "zero clamps up");
        let huge = ModelPageQuery {
            limit: Some(99999),
            ..Default::default()
        };
        assert_eq!(huge.page_size(), ANTHROPIC_MAX_PAGE_SIZE, "clamps down");
    }

    #[test]
    fn parses_pagination_params_including_slashes() {
        // Model ids contain '/', which clients percent-encode.
        let uri: Uri = "/v1/models?limit=5&after_id=cmd%2Fdeepseek%2Fv4&ignored=x"
            .parse()
            .unwrap();
        let query = ModelPageQuery::parse(&uri);
        assert_eq!(query.limit, Some(5));
        assert_eq!(query.after_id.as_deref(), Some("cmd/deepseek/v4"));
        assert_eq!(query.before_id, None);

        // A malformed limit is ignored rather than rejecting the request.
        let bad: Uri = "/v1/models?limit=abc".parse().unwrap();
        assert_eq!(ModelPageQuery::parse(&bad).limit, None);
    }

    #[test]
    fn joins_base_and_path_without_double_v1() {
        assert_eq!(
            join_upstream_url("https://api.openai.com/v1", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream_url("https://api.openai.com", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_upstream_url("http://localhost:11434/v1", "/models"),
            "http://localhost:11434/v1/models"
        );
    }

    fn endpoint_test_target(provider_type: &str, supported_endpoints: Option<&str>) -> RouteTarget {
        RouteTarget {
            id: 1,
            route_id: None,
            provider_id: 1,
            provider_name: "test".to_string(),
            provider_type: provider_type.to_string(),
            base_url: "http://upstream".to_string(),
            model_prefix: String::new(),
            api_key: None,
            provider_headers: "{}".to_string(),
            supported_endpoints: supported_endpoints.map(ToOwned::to_owned),
            context_limit: None,
            input_limit: None,
            output_limit: None,
            provider_enabled: None,
            model_enabled: None,
            tool_search_supported: 1,
            provider_health: None,
            upstream_model: "model".to_string(),
            weight: 100,
            priority: 0,
            enabled: 1,
            provider_api_key_id: None,
            provider_api_key_name: None,
            auth_retryable: false,
        }
    }

    #[test]
    fn filters_targets_by_upstream_supported_endpoints() {
        let openai = endpoint_test_target("openai", Some(r#"["/chat/completions", "/responses"]"#));
        assert!(target_supports_endpoint(&openai, OPENAI_CHAT_COMPLETIONS));
        assert!(target_supports_endpoint(&openai, OPENAI_RESPONSES));
        assert!(!target_supports_endpoint(&openai, "/v1/embeddings"));

        let no_leading_slash = endpoint_test_target("openai", Some(r#"["chat/completions"]"#));
        assert!(target_supports_endpoint(
            &no_leading_slash,
            OPENAI_CHAT_COMPLETIONS
        ));

        let anthropic = endpoint_test_target("anthropic", Some(r#"["/messages"]"#));
        assert!(target_supports_endpoint(
            &anthropic,
            OPENAI_CHAT_COMPLETIONS
        ));
        assert!(target_supports_endpoint(&anthropic, ANTHROPIC_MESSAGES));
        // Anthropic targets serve Responses requests by translating them to
        // the Messages API, so they advertise Responses support too.
        assert!(target_supports_endpoint(&anthropic, OPENAI_RESPONSES));

        let unknown = endpoint_test_target("custom", None);
        assert!(target_supports_endpoint(&unknown, "/v1/embeddings"));

        let filtered =
            filter_targets_for_endpoint(vec![openai.clone(), anthropic.clone()], OPENAI_RESPONSES);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].provider_type, "openai");
        assert_eq!(filtered[1].provider_type, "anthropic");

        // Anthropic targets also serve the legacy completions endpoint.
        assert!(target_supports_endpoint(&anthropic, OPENAI_COMPLETIONS));
        let filtered = filter_targets_for_endpoint(
            vec![openai.clone(), anthropic.clone()],
            OPENAI_COMPLETIONS,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider_type, "anthropic");

        // A chat-only OpenAI-compatible target still advertises Responses
        // support because the gateway translates the request on the way out.
        let chat_only = endpoint_test_target("openai", Some(r#"["/chat/completions"]"#));
        assert!(target_supports_endpoint(&chat_only, OPENAI_RESPONSES));
        assert_eq!(
            target_upstream_endpoint(&chat_only, OPENAI_RESPONSES),
            Some(OPENAI_CHAT_COMPLETIONS)
        );
        // A target that genuinely supports Responses keeps the passthrough.
        assert_eq!(
            target_upstream_endpoint(&openai, OPENAI_RESPONSES),
            Some(OPENAI_RESPONSES)
        );

        // Conversely, a Responses-only target can serve chat callers.
        let responses_only = endpoint_test_target("openai", Some(r#"["/responses"]"#));
        assert!(target_supports_endpoint(
            &responses_only,
            OPENAI_CHAT_COMPLETIONS
        ));
        assert_eq!(
            target_upstream_endpoint(&responses_only, OPENAI_CHAT_COMPLETIONS),
            Some(OPENAI_RESPONSES)
        );
        // Anthropic callers can also reach a Responses-only target.
        assert!(target_supports_endpoint(
            &responses_only,
            ANTHROPIC_MESSAGES
        ));
        assert_eq!(
            target_upstream_endpoint(&responses_only, ANTHROPIC_MESSAGES),
            Some(OPENAI_RESPONSES)
        );
    }

    #[test]
    fn transient_upstream_4xx_only_matches_opaque_aggregator_errors() {
        // The trace-id-only aggregator error is transient and worth a retry.
        assert!(transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"{\"type\":\"invalid_request_error\",\"code\":\"\",\"message\":\"invalid request error trace_id: 63eeeadddaa2d5224a4acf42a78dd1de\"}\n","type":"invalid_request_error"}}"#
        ));
        // Actionable client errors carry a `param` and must not be retried.
        assert!(!transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"Too small: expected array to have >=1 items","type":"invalid_request_error","param":"input"}}"#
        ));
        // Success and other status classes are never treated as transient.
        assert!(!transient_upstream_4xx(
            StatusCode::OK,
            br#"{"error":{"message":"invalid request error trace_id: abc"}}"#
        ));
        assert!(!transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"context length exceeded"}}"#
        ));
    }

    #[tokio::test]
    async fn auto_route_ignores_models_that_do_not_support_the_requested_endpoint() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url) VALUES
                (1, 'embeddings-only', 'openai', 'http://embeddings-only'),
                (2, 'responses', 'openai', 'http://responses')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, supported_endpoints) VALUES
                (1, 'shared', '[\"/embeddings\"]'),
                (2, 'shared', '[\"/chat/completions\",\"/responses\"]')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let resolved = resolve_route(&state, "shared", OPENAI_RESPONSES)
            .await
            .unwrap();
        assert_eq!(resolved.targets.len(), 1);
        assert_eq!(resolved.targets[0].provider_id, 2);

        // The embeddings-only provider is excluded from chat completions even
        // though it advertises a different OpenAI-compatible endpoint.
        let resolved = resolve_route(&state, "shared", OPENAI_CHAT_COMPLETIONS)
            .await
            .unwrap();
        assert_eq!(resolved.targets.len(), 1);
        assert_eq!(resolved.targets[0].provider_id, 2);
    }

    #[tokio::test]
    async fn endpoint_override_controls_routing_and_public_metadata() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix
             ) VALUES (1, 'override', 'openai', 'http://override', 'vendor/')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (
                provider_id, model_name, supported_endpoints,
                supported_endpoints_override
             ) VALUES (
                1, 'model', '[\"/chat/completions\"]', '[\"/responses\"]'
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        // The override drops native chat support, but chat callers are still
        // served by translating to Responses.
        assert_eq!(
            resolve_route(&state, "vendor/model", OPENAI_CHAT_COMPLETIONS)
                .await
                .unwrap()
                .targets
                .len(),
            1
        );
        // Endpoints the override does not imply stay unroutable.
        assert!(
            resolve_route(&state, "vendor/model", "/v1/embeddings")
                .await
                .is_err()
        );
        let resolved = resolve_route(&state, "vendor/model", OPENAI_RESPONSES)
            .await
            .unwrap();
        assert_eq!(resolved.targets.len(), 1);

        let models = crate::registry::synced_models(&state.pool).await.unwrap();
        assert_eq!(
            models[0].supported_endpoints,
            Some(vec!["/responses".to_string()])
        );

        let uri: Uri = "/v1/models".parse().unwrap();
        let response = public_models_inner(&state, &HeaderMap::new(), &uri)
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            payload["data"][0]["supported_endpoints"],
            json!(["/responses"])
        );
    }

    #[tokio::test]
    async fn usage_cost_uses_manual_price_overrides() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'priced', 'openai', 'http://priced')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (
                provider_id, model_name, enabled,
                cost_input_override, cost_output_override
             ) VALUES (1, 'model', 1, 2.0, 4.0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let cost = estimate_usage_cost(&state, 1, "model", Usage::new(1_000_000, 1_000_000))
            .await
            .unwrap();
        assert_eq!(cost, 6_000_000);
    }

    #[tokio::test]
    async fn route_diagnosis_explains_each_target() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'chat-only', 'openai', 'http://chat-only', 1, 1),
                (2, 'responses', 'openai', 'http://responses', 1, 1),
                (3, 'disabled', 'openai', 'http://disabled', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (
                provider_id, model_name, supported_endpoints
             ) VALUES
                (1, 'model', '[\"/chat/completions\"]'),
                (2, 'model', '[\"/chat/completions\",\"/responses\"]'),
                (3, 'model', '[\"/responses\"]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'shared route', 'shared', 'priority', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 0, 1),
                (2, 1, 2, 'model', 1, 1),
                (3, 1, 3, 'model', 2, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let diagnosis = diagnose_route(&state, "shared", OPENAI_RESPONSES, None)
            .await
            .unwrap();
        assert!(diagnosis.matched);
        assert!(diagnosis.resolved);
        assert_eq!(diagnosis.match_type, "explicit_route");
        assert_eq!(diagnosis.targets.len(), 3);
        assert!(!diagnosis.targets[0].eligible);
        assert!(diagnosis.targets[0].reason.contains("does not declare"));
        assert!(diagnosis.targets[1].eligible);
        assert!(!diagnosis.targets[2].eligible);
        assert!(diagnosis.targets[2].reason.contains("provider is disabled"));
        assert!(diagnosis.runtime_targets.is_none());
    }

    #[tokio::test]
    async fn route_diagnosis_returns_sticky_runtime_order_and_key_names() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, enabled, last_test_ok
             ) VALUES
                (1, 'primary', 'openai', 'http://primary', 1, 1),
                (2, 'secondary', 'openai', 'http://secondary', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model'), (2, 'model')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'primary-a', 'sk-a', 1),
                    (12, 1, 'primary-b', 'sk-b', 1),
                    (21, 2, 'secondary-a', 'sk-c', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'sticky route', 'shared', 'weighted', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO route_targets (
                id, route_id, provider_id, upstream_model, weight, priority, enabled
             ) VALUES
                (1, 1, 1, 'model', 1, 0, 1),
                (2, 1, 2, 'model', 100, 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let first = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
            .await
            .unwrap();
        let repeated = diagnose_route(&state, "shared", OPENAI_CHAT_COMPLETIONS, Some("session-a"))
            .await
            .unwrap();

        let first_runtime = first.runtime_targets.as_ref().unwrap();
        let repeated_runtime = repeated.runtime_targets.as_ref().unwrap();
        assert_eq!(first.session_id.as_deref(), Some("session-a"));
        assert_eq!(first_runtime.len(), 3);
        assert_eq!(
            first_runtime
                .iter()
                .map(|target| (target.order, target.provider_id, target.provider_api_key_id,))
                .collect::<Vec<_>>(),
            repeated_runtime
                .iter()
                .map(|target| (target.order, target.provider_id, target.provider_api_key_id,))
                .collect::<Vec<_>>()
        );
        assert!(
            first_runtime
                .iter()
                .any(|target| target.provider_api_key_name.as_deref() == Some("primary-a"))
        );
        assert!(
            first_runtime
                .iter()
                .all(|target| target.order > 0 && target.order <= first_runtime.len())
        );
    }

    #[tokio::test]
    async fn disabled_route_does_not_hide_direct_prefix_diagnosis() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix
             ) VALUES (1, 'prefixed', 'openai', 'http://prefixed', 'vendor/')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name)
             VALUES (1, 'model')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
             VALUES (1, 'disabled exact', 'vendor/model', 'priority', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool, None);
        let diagnosis = diagnose_route(&state, "vendor/model", OPENAI_CHAT_COMPLETIONS, None)
            .await
            .unwrap();
        assert!(diagnosis.matched);
        assert!(diagnosis.resolved);
        assert_eq!(diagnosis.match_type, "prefix");
        assert_eq!(diagnosis.route_id, None);
    }

    #[tokio::test]
    async fn route_resolution_failures_are_logged() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let state = AppState::new(pool.clone(), None);

        let error = resolve_route_or_log(
            &state,
            None,
            "route-rejection",
            None,
            "missing-model",
            OPENAI_RESPONSES,
            false,
            Instant::now(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(error, AppError::NotFound(_)));

        let row: (String, i64, i64, String) = sqlx::query_as(
            "SELECT request_id, status_code, success, error_message
             FROM usage_logs WHERE request_id = 'route-rejection'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "route-rejection");
        assert_eq!(row.1, 404);
        assert_eq!(row.2, 0);
        assert!(row.3.contains("missing-model"));
    }

    #[tokio::test]
    async fn prefix_matching_is_literal_and_case_sensitive() {
        // A pooled `sqlite::memory:` database gives each connection its own
        // empty database, so pin the pool to a single connection.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                tool_search_supported INTEGER NOT NULL DEFAULT 1,
                last_test_ok INTEGER,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_limit INTEGER,
                input_limit INTEGER,
                output_limit INTEGER,
                context_override INTEGER,
                input_override INTEGER,
                output_override INTEGER,
                attachment INTEGER,
                reasoning INTEGER,
                tool_call INTEGER,
                structured_output INTEGER,
                temperature INTEGER,
                open_weights INTEGER,
                modalities TEXT,
                cost TEXT,
                family TEXT,
                knowledge TEXT,
                release_date TEXT,
                last_updated TEXT,
                canonical_model_id TEXT,
                supported_endpoints TEXT,
                supported_endpoints_override TEXT,
                cost_input_override REAL,
                cost_output_override REAL,
                cost_cache_read_override REAL,
                cost_cache_write_override REAL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url, model_prefix)
             VALUES (1, 'underscore', 'openai', 'http://upstream', 'a_b/')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO provider_models (provider_id, model_name) VALUES (1, 'x')")
            .execute(&pool)
            .await
            .unwrap();

        let state = AppState::new(pool, None);

        // `_` must be treated literally, not as a single-character wildcard.
        let literal = find_prefixed_targets(&state, "a_b/x", OPENAI_CHAT_COMPLETIONS)
            .await
            .unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].upstream_model, "x");
        assert!(
            find_prefixed_targets(&state, "aXb/x", OPENAI_CHAT_COMPLETIONS)
                .await
                .is_err()
        );

        // Prefix comparison must stay case-sensitive so `A_B/x` cannot reach a
        // provider registered as `a_b`.
        assert!(
            find_prefixed_targets(&state, "A_B/x", OPENAI_CHAT_COMPLETIONS)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn disabled_provider_model_is_skipped_by_explicit_routes() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE providers (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                provider_type TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_prefix TEXT NOT NULL DEFAULT '',
                api_key TEXT,
                headers TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                models_synced_at TEXT,
                models_sync_error TEXT,
                tool_search_supported INTEGER NOT NULL DEFAULT 1,
                last_test_ok INTEGER,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE provider_models (
                provider_id INTEGER NOT NULL,
                model_name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                context_limit INTEGER,
                input_limit INTEGER,
                output_limit INTEGER,
                context_override INTEGER,
                input_override INTEGER,
                output_override INTEGER,
                attachment INTEGER,
                reasoning INTEGER,
                tool_call INTEGER,
                structured_output INTEGER,
                temperature INTEGER,
                open_weights INTEGER,
                modalities TEXT,
                cost TEXT,
                family TEXT,
                knowledge TEXT,
                release_date TEXT,
                last_updated TEXT,
                canonical_model_id TEXT,
                supported_endpoints TEXT,
                supported_endpoints_override TEXT,
                cost_input_override REAL,
                cost_output_override REAL,
                cost_cache_read_override REAL,
                cost_cache_write_override REAL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE route_targets (
                id INTEGER PRIMARY KEY,
                route_id INTEGER,
                provider_id INTEGER NOT NULL,
                upstream_model TEXT NOT NULL,
                weight INTEGER NOT NULL DEFAULT 100,
                priority INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE routes (
                id INTEGER PRIMARY KEY,
                model_pattern TEXT NOT NULL,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (1, 'upstream', 'openai', 'http://upstream')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'disabled-model', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO route_targets (id, route_id, provider_id, upstream_model)
             VALUES (1, 1, 1, 'disabled-model')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO routes (id, model_pattern, name)
             VALUES (1, 'route-model', 'route-model')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = AppState::new(pool.clone(), None);
        assert!(load_targets(&state, 1).await.unwrap().is_empty());
        assert!(
            crate::registry::route_models(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );

        sqlx::query("UPDATE provider_models SET enabled = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(load_targets(&state, 1).await.unwrap().len(), 1);
        let routes = crate::registry::route_models(&state.pool).await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].target_count, 1);
    }

    #[tokio::test]
    async fn api_key_daily_token_quota_blocks_after_limit() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (api_key_id, total_tokens, created_at)
             VALUES (1, 10, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: Some(10),
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool, None);
        assert!(matches!(
            enforce_api_key_daily_quota(&state, Some(&key)).await,
            Err(AppError::TooManyRequests(_))
        ));
    }

    #[tokio::test]
    async fn in_flight_usage_log_is_replaced_by_final_status() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let state = AppState::new(pool.clone(), None);

        log_usage_started(
            &state,
            "request-in-flight",
            Some("session-123"),
            None,
            None,
            "test-model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await;
        let pending: (i64, i64, i64, i64, Option<String>) = sqlx::query_as(
            "SELECT in_flight, status_code, prompt_tokens, total_tokens, session_id
             FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, (1, 0, 10, 10, Some("session-123".to_string())));

        sqlx::query(
            "INSERT INTO providers (id, name, provider_type, base_url)
             VALUES (7, 'test-provider', 'openai', 'https://example.com/v1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 7, 'Primary', 'sk-primary', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        log_usage_target(
            &state,
            "request-in-flight",
            7,
            "upstream-test-model",
            Some(11),
        )
        .await;
        let target: (i64, String, Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT provider_id, upstream_model, provider_api_key_id, provider_api_key_name
             FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            target,
            (
                7,
                "upstream-test-model".to_string(),
                Some(11),
                Some("Primary".to_string())
            )
        );

        log_usage(
            &state,
            UsageLogEntry {
                request_id: "request-in-flight",
                api_key_id: None,
                route_id: None,
                provider_id: Some(7),
                requested_model: "test-model",
                upstream_model: Some("upstream-test-model"),
                endpoint: "/v1/chat/completions",
                usage: Usage::new(10, 5),
                latency_ms: 120,
                first_token_ms: None,
                status_code: 200,
                success: true,
                streamed: false,
                error_message: None,
                response_preview: None,
            },
        )
        .await;

        #[allow(clippy::type_complexity)]
        let completed: (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT in_flight, status_code, prompt_tokens, completion_tokens,
                    total_tokens, provider_id, upstream_model, provider_api_key_id,
                    provider_api_key_name, session_id
             FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            completed,
            (
                0,
                200,
                10,
                5,
                15,
                7,
                "upstream-test-model".to_string(),
                Some(11),
                Some("Primary".to_string()),
                Some("session-123".to_string())
            )
        );

        log_usage_target(&state, "request-in-flight", 8, "late-update", None).await;
        let provider_id: i64 = sqlx::query_scalar(
            "SELECT provider_id FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(provider_id, 7);
        let provider_api_key_id: Option<i64> = sqlx::query_scalar(
            "SELECT provider_api_key_id FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(provider_api_key_id, Some(11));
        let provider_api_key_name: Option<String> = sqlx::query_scalar(
            "SELECT provider_api_key_name FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(provider_api_key_name.as_deref(), Some("Primary"));

        sqlx::query("DELETE FROM provider_api_keys WHERE id = 11")
            .execute(&pool)
            .await
            .unwrap();
        let snapshot: (Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT provider_api_key_id, provider_api_key_name
             FROM usage_logs WHERE request_id = 'request-in-flight'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(snapshot, (None, Some("Primary".to_string())));
    }

    #[tokio::test]
    async fn api_key_daily_cost_quota_uses_estimated_cost() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (api_key_id, total_tokens, estimated_cost_micros, created_at)
             VALUES (1, 1, 2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: None,
            daily_cost_limit_micros: Some(2),
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool, None);
        assert!(matches!(
            enforce_api_key_daily_quota(&state, Some(&key)).await,
            Err(AppError::TooManyRequests(_))
        ));
    }

    #[tokio::test]
    async fn api_key_requests_per_minute_quota_blocks_after_limit() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'rate-limited', 'rate-hash', 'sk-openllm', 'test', 1, 2, NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "rate-limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: Some(2),
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool.clone(), None);
        for request_id in ["first", "second"] {
            reserve_api_key_rate_limit(
                &state,
                Some(&key),
                request_id,
                None,
                "model",
                "/v1/chat/completions",
                10,
                false,
            )
            .await
            .unwrap();
        }
        assert!(matches!(
            reserve_api_key_rate_limit(
                &state,
                Some(&key),
                "third",
                None,
                "model",
                "/v1/chat/completions",
                10,
                false,
            )
            .await,
            Err(AppError::TooManyRequests(_))
        ));
        let in_flight: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(in_flight, 2);
    }

    #[tokio::test]
    async fn api_key_concurrency_quota_blocks_at_limit() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'concurrency-limited', 'concurrency-hash', 'sk-openllm', 'test', 1, NULL, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "concurrency-limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: Some(1),
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool.clone(), None);
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "active",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .unwrap();
        assert!(matches!(
            reserve_api_key_rate_limit(
                &state,
                Some(&key),
                "blocked",
                None,
                "model",
                "/v1/chat/completions",
                10,
                false,
            )
            .await,
            Err(AppError::TooManyRequests(_))
        ));

        sqlx::query("UPDATE usage_logs SET in_flight = 0 WHERE request_id = 'active'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            reserve_api_key_rate_limit(
                &state,
                Some(&key),
                "next",
                None,
                "model",
                "/v1/chat/completions",
                10,
                false,
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn api_key_without_limits_skips_quota_queries() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "unlimited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool, None);
        assert!(
            reserve_api_key_rate_limit(
                &state,
                Some(&key),
                "unlimited",
                None,
                "model",
                "/v1/chat/completions",
                10,
                false,
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn api_key_rate_limit_rejection_is_logged_without_extra_in_flight() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO api_keys (
                id, name, key_hash, key_prefix, key_suffix,
                enabled, requests_per_minute, max_concurrency
             ) VALUES (1, 'concurrency-limited', 'reject-hash', 'sk-openllm', 'test', 1, NULL, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "concurrency-limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: Some(1),
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool.clone(), None);
        reserve_api_key_rate_limit(
            &state,
            Some(&key),
            "active",
            None,
            "model",
            "/v1/chat/completions",
            10,
            false,
        )
        .await
        .unwrap();

        assert!(matches!(
            enforce_api_key_rate_limit_or_log(
                &state,
                Some(&key),
                "blocked",
                None,
                "model",
                "/v1/chat/completions",
                10,
                false,
                Instant::now(),
            )
            .await,
            Err(AppError::TooManyRequests(_))
        ));
        let blocked: (i64, i64) = sqlx::query_as(
            "SELECT status_code, in_flight FROM usage_logs WHERE request_id = 'blocked'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(blocked, (429, 0));
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(active, 1);
    }

    #[tokio::test]
    async fn api_key_daily_quota_ignores_in_flight_requests() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE usage_logs (
                api_key_id INTEGER,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                in_flight INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                api_key_id, total_tokens, estimated_cost_micros, in_flight, created_at
             ) VALUES (1, 1000, 1000000, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: Some(10),
            daily_cost_limit_micros: Some(1),
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool.clone(), None);

        assert!(
            enforce_api_key_daily_quota(&state, Some(&key))
                .await
                .is_ok()
        );

        sqlx::query("UPDATE usage_logs SET in_flight = 0")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            enforce_api_key_daily_quota(&state, Some(&key)).await,
            Err(AppError::TooManyRequests(_))
        ));
    }

    #[tokio::test]
    async fn quota_rejection_is_logged_as_zero_usage() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE usage_logs (
                request_id TEXT NOT NULL UNIQUE,
                api_key_id INTEGER,
                route_id INTEGER,
                provider_id INTEGER,
                requested_model TEXT NOT NULL,
                upstream_model TEXT,
                endpoint TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                latency_ms INTEGER NOT NULL DEFAULT 0,
                estimated_cost_micros INTEGER,
                first_token_ms INTEGER,
                status_code INTEGER NOT NULL,
                in_flight INTEGER NOT NULL DEFAULT 0,
                success INTEGER NOT NULL,
                streamed INTEGER NOT NULL DEFAULT 0,
                error_message TEXT,
                response_preview TEXT,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT ''
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (
                request_id, api_key_id, requested_model, endpoint,
                total_tokens, status_code, success, created_at
             ) VALUES (
                'seed', 1, 'seed', '/v1/chat/completions', 1, 200, 1,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let key = ApiKeyRecord {
            id: 1,
            name: "limited".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: Some(1),
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: None,
            expires_at: None,
        };
        let state = AppState::new(pool.clone(), None);
        assert!(
            enforce_policy_or_log(
                &state,
                Some(&key),
                "rejected",
                None,
                "model",
                "/v1/chat/completions",
                false,
                Instant::now(),
            )
            .await
            .is_err()
        );
        let row: (i64, i64, Option<i64>, i64) = sqlx::query_as(
            "SELECT total_tokens, status_code, estimated_cost_micros, success \
             FROM usage_logs WHERE request_id = 'rejected'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (0, 429, Some(0), 0));
    }

    #[test]
    fn api_key_model_permissions_support_globs() {
        let key = ApiKeyRecord {
            id: 1,
            name: "scoped".to_string(),
            key_prefix: "sk-openllm".to_string(),
            key_suffix: "test".to_string(),
            enabled: 1,
            last_used_at: None,
            created_at: String::new(),
            daily_token_limit: None,
            daily_cost_limit_micros: None,
            requests_per_minute: None,
            max_concurrency: None,
            allowed_models: Some(r#"["gpt-*","claude-sonnet-*"]"#.to_string()),
            expires_at: None,
        };
        assert!(enforce_api_key_model_access(Some(&key), "gpt-5.4").is_ok());
        assert!(enforce_api_key_model_access(Some(&key), "claude-sonnet-5").is_ok());
        assert!(matches!(
            enforce_api_key_model_access(Some(&key), "claude-opus-5"),
            Err(AppError::Forbidden(_))
        ));
        let malformed = ApiKeyRecord {
            allowed_models: Some("{".to_string()),
            ..key
        };
        assert!(matches!(
            enforce_api_key_model_access(Some(&malformed), "gpt-5.4"),
            Err(AppError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn public_model_list_respects_api_key_model_permissions() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Scoped', 'openai', 'https://example.com/v1', 'vendor/', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1), (1, 'claude-4', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let raw_key = "sk-openllm-scoped-model-list";
        sqlx::query(
            "INSERT INTO api_keys (
                name, key_hash, key_prefix, key_suffix, enabled, allowed_models
             ) VALUES ('scoped', ?, 'sk-openllm-s', 'list', 1, '[\"vendor/gpt-*\"]')",
        )
        .bind(hash_secret(raw_key))
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);

        let mut openai_headers = HeaderMap::new();
        openai_headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(&format!("Bearer {raw_key}")).unwrap(),
        );
        let uri: Uri = "/v1/models".parse().unwrap();
        let response = public_models_inner(&state, &openai_headers, &uri)
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let ids = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["vendor/gpt-5"]);

        let response = public_model_inner(&state, &openai_headers, "vendor/gpt-5")
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], "vendor/gpt-5");
        assert_eq!(value["object"], "model");
        assert!(matches!(
            public_model_inner(&state, &openai_headers, "vendor/claude-4").await,
            Err(AppError::NotFound(_))
        ));

        let mut anthropic_headers = openai_headers.clone();
        anthropic_headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        let response = public_models_inner(&state, &anthropic_headers, &uri)
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let ids = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["vendor/gpt-5"]);

        let response = public_model_inner(&state, &anthropic_headers, "vendor/gpt-5")
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], "vendor/gpt-5");
        assert_eq!(value["type"], "model");
    }

    #[tokio::test]
    async fn model_retrieve_route_accepts_slashed_model_ids() {
        use axum::body::Body;
        use tower::ServiceExt;

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Vendor', 'openai', 'https://example.com/v1', 'vendor/', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);
        let response = crate::build_router(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/models/vendor/gpt-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], "vendor/gpt-5");
    }

    #[test]
    fn retries_only_transient_statuses() {
        for status in [408, 409, 425, 429, 500, 502, 503] {
            assert!(
                retryable_status(StatusCode::from_u16(status).unwrap()),
                "{status} should be retryable"
            );
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !retryable_status(StatusCode::from_u16(status).unwrap()),
                "{status} should not be retryable"
            );
        }
    }

    #[test]
    fn health_ranking_prefers_known_good_then_unknown_then_failed() {
        assert_eq!(provider_health_rank(Some(1)), 0);
        assert_eq!(provider_health_rank(None), 1);
        assert_eq!(provider_health_rank(Some(0)), 2);
        assert_eq!(provider_health_rank(Some(7)), 0);
    }

    #[test]
    fn session_target_order_is_stable_and_spreads_weighted_sessions() {
        let base = (1..=4)
            .map(|id| {
                let mut target = endpoint_test_target("openai", None);
                target.id = id;
                target.provider_id = id;
                target.upstream_model = format!("model-{id}");
                target
            })
            .collect::<Vec<_>>();

        for strategy in [RouteStrategy::Weighted, RouteStrategy::RoundRobin] {
            let ordered_ids = |session_id: &str| {
                let mut targets = base.clone();
                order_targets_for_session(session_id, 7, strategy, &mut targets);
                targets
                    .into_iter()
                    .map(|target| target.id)
                    .collect::<Vec<_>>()
            };

            let first = ordered_ids("session-a");
            assert_eq!(first, ordered_ids("session-a"));

            let mut primaries = Vec::new();
            for index in 0..64 {
                let primary = ordered_ids(&format!("session-{index}"))[0];
                if !primaries.contains(&primary) {
                    primaries.push(primary);
                }
            }
            assert_eq!(
                primaries.len(),
                base.len(),
                "sessions should spread across every target for {strategy:?}"
            );
        }
    }

    #[tokio::test]
    async fn provider_keys_are_sticky_per_session_without_advancing_rotation() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Sticky', 'openai', 'https://example.com/v1', '', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'First', 'sk-first', 1),
                    (12, 1, 'Second', 'sk-second', 1),
                    (13, 1, 'Third', 'sk-third', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);
        let mut target = endpoint_test_target("openai", None);
        target.id = 1;
        let keys = |targets: Vec<RouteTarget>| {
            targets
                .into_iter()
                .map(|target| target.api_key.unwrap())
                .collect::<Vec<_>>()
        };

        let first = keys(
            order_targets(
                &state,
                1,
                "priority",
                vec![target.clone()],
                Some("session-a"),
            )
            .await
            .unwrap(),
        );
        let repeated = keys(
            order_targets(
                &state,
                1,
                "priority",
                vec![target.clone()],
                Some("session-a"),
            )
            .await
            .unwrap(),
        );
        assert_eq!(first, repeated);

        let no_session_first = keys(
            order_targets(&state, 1, "priority", vec![target.clone()], None)
                .await
                .unwrap(),
        );
        let no_session_second = keys(
            order_targets(&state, 1, "priority", vec![target], None)
                .await
                .unwrap(),
        );
        assert_eq!(no_session_first[0], "sk-first");
        assert_eq!(no_session_second[0], "sk-second");
    }

    #[tokio::test]
    async fn provider_keys_rotate_and_expand_a_route_target() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Rotating', 'openai', 'https://example.com/v1', '', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
             VALUES (1, 'First', 'sk-first', 1), (1, 'Second', 'sk-second', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);
        let mut target = endpoint_test_target("openai", None);
        target.id = 1;

        let first = order_targets(&state, 1, "priority", vec![target.clone()], None)
            .await
            .unwrap();
        assert_eq!(
            first
                .iter()
                .map(|target| target.api_key.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("sk-first"), Some("sk-second")]
        );
        assert!(first[0].auth_retryable);
        assert!(!first[1].auth_retryable);
        assert!(
            first
                .iter()
                .all(|target| target.provider_api_key_id.is_some())
        );

        let second = order_targets(&state, 1, "priority", vec![target], None)
            .await
            .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|target| target.api_key.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("sk-second"), Some("sk-first")]
        );
    }

    #[tokio::test]
    async fn provider_keys_skip_cooling_candidates_when_alternatives_exist() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Cooling', 'openai', 'https://example.com/v1', '', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled)
             VALUES (11, 1, 'First', 'sk-first', 1),
                    (12, 1, 'Second', 'sk-second', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);
        let mut target = endpoint_test_target("openai", None);
        target.id = 1;

        state
            .provider_key_cooldown
            .lock()
            .await
            .insert(11, Instant::now() + Duration::from_secs(60));
        let available = order_targets(&state, 1, "priority", vec![target.clone()], None)
            .await
            .unwrap();
        assert_eq!(
            available
                .iter()
                .map(|target| target.api_key.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("sk-second")]
        );

        state
            .provider_key_cooldown
            .lock()
            .await
            .insert(12, Instant::now() + Duration::from_secs(60));
        let fallback = order_targets(&state, 1, "priority", vec![target], None)
            .await
            .unwrap();
        assert_eq!(fallback.len(), 2);
        assert!(
            fallback
                .iter()
                .all(|target| target.provider_api_key_id.is_some())
        );
    }

    #[tokio::test]
    async fn provider_cooldowns_skip_cooling_providers_when_alternatives_exist() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                id, name, provider_type, base_url, model_prefix, enabled
             ) VALUES
                (1, 'Cooling', 'openai', 'https://cooling.example/v1', '', 1),
                (2, 'Healthy', 'openai', 'https://healthy.example/v1', '', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);
        let mut cooling = endpoint_test_target("openai", None);
        cooling.id = 1;
        cooling.provider_id = 1;
        let mut healthy = endpoint_test_target("openai", None);
        healthy.id = 2;
        healthy.provider_id = 2;

        state
            .provider_cooldown
            .lock()
            .await
            .insert(1, Instant::now() + Duration::from_secs(60));
        let available = order_targets(
            &state,
            1,
            "priority",
            vec![cooling.clone(), healthy.clone()],
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            available
                .iter()
                .map(|target| target.provider_id)
                .collect::<Vec<_>>(),
            vec![2]
        );

        state
            .provider_cooldown
            .lock()
            .await
            .insert(2, Instant::now() + Duration::from_secs(60));
        let fallback = order_targets(&state, 1, "priority", vec![cooling, healthy], None)
            .await
            .unwrap();
        assert_eq!(
            fallback
                .iter()
                .map(|target| target.provider_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn provider_cooldown_is_set_for_retryable_failures_and_cleared_on_success() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let state = AppState::new(pool, None);

        mark_provider_error(&state, 7, Some(StatusCode::TOO_MANY_REQUESTS)).await;
        let until = state
            .provider_cooldown
            .lock()
            .await
            .get(&7)
            .copied()
            .unwrap();
        assert!(until > Instant::now() + Duration::from_secs(25));

        mark_provider_success(&state, 7).await;
        assert!(!state.provider_cooldown.lock().await.contains_key(&7));
    }

    #[tokio::test]
    async fn provider_key_success_clears_recovered_error_state() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (
                name, provider_type, base_url, model_prefix, enabled
             ) VALUES ('Recovered', 'openai', 'https://example.com/v1', '', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_api_keys (
                id, provider_id, name, secret, enabled, last_error_at, last_error
             ) VALUES (
                11, 1, 'Primary', 'sk-primary', 1,
                '2026-01-01T00:00:00Z', 'stale unauthorized'
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool.clone(), None);
        state.load_provider_key_error_state().await.unwrap();
        state
            .provider_key_cooldown
            .lock()
            .await
            .insert(11, Instant::now() + Duration::from_secs(60));

        mark_provider_api_key_success(&state, Some(11)).await;

        assert!(!state.provider_key_cooldown.lock().await.contains_key(&11));
        assert!(!state.provider_key_error_state.lock().await.contains(&11));
        let error: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT last_error_at, last_error FROM provider_api_keys WHERE id = 11")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(error, (None, None));
    }

    #[test]
    fn auth_failures_only_fall_through_when_another_candidate_exists() {
        let mut target = endpoint_test_target("openai", None);
        assert!(!should_try_next_target(&target, StatusCode::UNAUTHORIZED));
        target.auth_retryable = true;
        assert!(should_try_next_target(&target, StatusCode::UNAUTHORIZED));
        assert!(should_try_next_target(&target, StatusCode::FORBIDDEN));
        assert!(!should_try_next_target(&target, StatusCode::BAD_REQUEST));
    }

    #[test]
    fn rejects_estimated_input_above_route_context_limit() {
        let barrel = BarrelEnvelope {
            capabilities: Some(crate::models::ModelCapabilities {
                context_limit: Some(100),
                ..Default::default()
            }),
            incomplete: false,
            target_count: 1,
        };
        assert!(enforce_context_capacity(100, Some(&barrel)).is_ok());
        assert!(matches!(
            enforce_context_capacity(101, Some(&barrel)),
            Err(AppError::BadRequest(_))
        ));
        assert!(enforce_context_capacity(101, None).is_ok());
    }

    #[test]
    fn rejects_estimated_input_above_model_input_limit_without_context_limit() {
        let barrel = BarrelEnvelope {
            capabilities: Some(crate::models::ModelCapabilities {
                input_limit: Some(100),
                ..Default::default()
            }),
            incomplete: false,
            target_count: 1,
        };
        assert!(enforce_context_capacity(100, Some(&barrel)).is_ok());
        assert!(matches!(
            enforce_context_capacity(101, Some(&barrel)),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn effective_input_limit_prefers_the_stricter_value() {
        let capabilities = ModelCapabilities {
            context_limit: Some(100),
            input_limit: Some(200),
            ..Default::default()
        };
        assert_eq!(effective_input_limit(&capabilities), Some(100));
    }

    #[test]
    fn capability_headers_include_input_and_context_without_output_limit() {
        let mut response = Response::new(Body::empty());
        let receipt = Some(json!({
            "context_limit": 100,
            "input_limit": 80
        }));
        apply_capability_headers(&mut response, &receipt);
        assert_eq!(
            response
                .headers()
                .get("x-openllm-max-context-tokens")
                .and_then(|value| value.to_str().ok()),
            Some("100")
        );
        assert_eq!(
            response
                .headers()
                .get("x-openllm-max-input-tokens")
                .and_then(|value| value.to_str().ok()),
            Some("80")
        );
        assert!(
            response
                .headers()
                .get("x-openllm-max-output-tokens")
                .is_none()
        );
    }

    fn barrel_with_output_limit(output_limit: Option<i64>) -> BarrelEnvelope {
        BarrelEnvelope {
            capabilities: Some(crate::models::ModelCapabilities {
                output_limit,
                ..Default::default()
            }),
            incomplete: false,
            target_count: 2,
        }
    }

    #[test]
    fn clamps_requested_output_to_barrel_limit() {
        let barrel = barrel_with_output_limit(Some(8000));
        let mut body = json!({"model": "m", "max_tokens": 32000});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(8000));
        assert_eq!(body["max_tokens"], 8000);
    }

    #[test]
    fn leaves_requests_within_the_barrel_untouched() {
        let barrel = barrel_with_output_limit(Some(8000));
        let mut body = json!({"model": "m", "max_tokens": 4096});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), None);
        assert_eq!(body["max_tokens"], 4096);

        // No known limit means there is nothing to clamp against.
        let unknown = barrel_with_output_limit(None);
        let mut body = json!({"model": "m", "max_tokens": 999999});
        assert_eq!(clamp_output_request(&mut body, Some(&unknown)), None);
        assert_eq!(body["max_tokens"], 999999);
    }

    #[test]
    fn clamps_max_completion_tokens_too() {
        let barrel = barrel_with_output_limit(Some(1000));
        let mut body = json!({"model": "m", "max_completion_tokens": 5000});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
        assert_eq!(body["max_completion_tokens"], 1000);
    }

    #[test]
    fn clamps_responses_max_output_tokens_too() {
        let barrel = barrel_with_output_limit(Some(1000));
        let mut body = json!({"model": "m", "max_output_tokens": 5000});
        assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
        assert_eq!(body["max_output_tokens"], 1000);
        assert_eq!(
            requested_output_tokens_of(&json!({"max_output_tokens": 5000})),
            Some(5000)
        );
    }

    #[test]
    fn receipt_reports_clamping_and_target_count() {
        let barrel = barrel_with_output_limit(Some(8000));
        let receipt = capability_receipt(Some(&barrel), Some(32000), Some(8000)).unwrap();
        assert_eq!(receipt["output_limit"], 8000);
        assert_eq!(receipt["requested_output_tokens"], 32000);
        assert_eq!(receipt["clamped_output_tokens"], 8000);
        assert_eq!(receipt["target_count"], 2);
        assert_eq!(receipt["limits_verified"], true);
    }

    #[test]
    fn receipt_injected_only_into_json_objects() {
        let receipt = Some(json!({"output_limit": 8000}));
        let injected = inject_capability_receipt(br#"{"id":"x"}"#, &receipt).unwrap();
        let value: Value = serde_json::from_slice(&injected).unwrap();
        assert_eq!(value["capabilities"]["output_limit"], 8000);
        // Non-JSON bodies must be left alone rather than replaced.
        assert!(inject_capability_receipt(b"not json", &receipt).is_none());
        assert!(inject_capability_receipt(b"{}", &None).is_none());
    }

    #[test]
    fn strips_tool_search_for_compat_retry() {
        let body = json!({
            "model": "m",
            "tools": [
                {"type": "function", "name": "lookup"},
                {"type": "tool_search", "execution": "client"}
            ]
        });
        let stripped = strip_tool_search_tools(&body).unwrap();
        assert_eq!(stripped["tools"].as_array().unwrap().len(), 1);
        assert_eq!(stripped["tools"][0]["name"], "lookup");

        let only_tool_search = json!({
            "tools": [{"type": "tool_search", "execution": "client"}]
        });
        let stripped = strip_tool_search_tools(&only_tool_search).unwrap();
        assert!(stripped.get("tools").is_none());

        let standard_tools = json!({
            "tools": [{"type": "function", "name": "lookup"}]
        });
        assert!(strip_tool_search_tools(&standard_tools).is_none());
    }

    #[test]
    fn recognizes_unsupported_tool_search_error() {
        for error in [
            r#"{
                "type": "BadRequest",
                "code": "InvalidParameter",
                "message": "The parameter `tool.type` specified in the request are not valid: The parameter `type` specified in the request are not valid: unknown tool type: tool_search."
            }"#,
            r#"{"error":{"message":"unsupported tool type: tool_search"}}"#,
            r#"{"error":{"message":"tool_search is not supported by this model"}}"#,
            r#"{"error":{"message":"unknown tool: tool_search"}}"#,
        ] {
            assert!(upstream_rejects_tool_search(error.as_bytes()));
        }
        for error in [
            r#"{"error":{"message":"rate limit exceeded"}}"#,
            r#"{"error":{"message":"tool_search returned an invalid result"}}"#,
        ] {
            assert!(!upstream_rejects_tool_search(error.as_bytes()));
        }
    }

    #[tokio::test]
    async fn retries_openai_requests_without_tool_search_when_upstream_rejects_it() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        for (endpoint, request_json, success_body) in [
            (
                OPENAI_RESPONSES,
                json!({
                    "model": "requested-model",
                    "input": "hello",
                    "tools": [{"type": "tool_search", "execution": "client"}]
                }),
                json!({
                    "id": "ok",
                    "object": "response",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 2,
                        "total_tokens": 12
                    }
                }),
            ),
            (
                OPENAI_CHAT_COMPLETIONS,
                json!({
                    "model": "requested-model",
                    "messages": [{"role": "user", "content": "hello"}],
                    "tools": [{"type": "tool_search", "execution": "client"}]
                }),
                json!({
                    "id": "ok",
                    "object": "chat.completion",
                    "choices": [],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 2,
                        "total_tokens": 12
                    }
                }),
            ),
        ] {
            let attempts = Arc::new(AtomicUsize::new(0));
            let success_body = success_body.clone();
            let app = axum::Router::new().route(
                endpoint,
                axum::routing::post({
                    let attempts = attempts.clone();
                    move |Json(body): Json<Value>| {
                        let attempts = attempts.clone();
                        let success_body = success_body.clone();
                        async move {
                            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                                assert!(
                                    body["tools"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .any(|tool| tool["type"] == "tool_search")
                                );
                                (
                                    StatusCode::BAD_REQUEST,
                                    Json(json!({
                                        "type": "BadRequest",
                                        "code": "InvalidParameter",
                                        "message": "The parameter `tool.type` specified in the request are not valid: The parameter `type` specified in the request are not valid: unknown tool type: tool_search."
                                    })),
                                )
                            } else {
                                assert!(body.get("tools").is_none());
                                (StatusCode::OK, Json(success_body))
                            }
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            sqlx::migrate!("./migrations").run(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO providers (id, name, provider_type, base_url)
                 VALUES (1, 'mock', 'openai', ?)",
            )
            .bind(format!("http://{address}"))
            .execute(&pool)
            .await
            .unwrap();
            let state = AppState::new(pool.clone(), None);
            let mut target = RouteTarget {
                id: 1,
                route_id: None,
                provider_id: 1,
                provider_name: "mock".to_string(),
                provider_type: "openai".to_string(),
                base_url: format!("http://{address}"),
                model_prefix: String::new(),
                api_key: None,
                provider_headers: "{}".to_string(),
                supported_endpoints: None,
                context_limit: None,
                input_limit: None,
                output_limit: None,
                provider_enabled: None,
                model_enabled: None,
                tool_search_supported: 1,
                provider_health: None,
                upstream_model: "upstream".to_string(),
                weight: 100,
                priority: 0,
                enabled: 1,
                provider_api_key_id: None,
                provider_api_key_name: None,
                auth_retryable: false,
            };

            let response = forward_to_target(
                &state,
                "tool-search-retry",
                endpoint,
                "requested-model",
                &request_json,
                &Bytes::new(),
                None,
                target.clone(),
                false,
                10,
                None,
                Instant::now(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            let tool_search_supported: i64 =
                sqlx::query_scalar("SELECT tool_search_supported FROM providers WHERE id = 1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(tool_search_supported, 0);
            let tool_search_checked_at: Option<String> =
                sqlx::query_scalar("SELECT tool_search_checked_at FROM providers WHERE id = 1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(tool_search_checked_at.is_some());

            target.tool_search_supported = 0;
            let response = forward_to_target(
                &state,
                "tool-search-cached",
                endpoint,
                "requested-model",
                &request_json,
                &Bytes::new(),
                None,
                target,
                false,
                10,
                None,
                Instant::now(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(attempts.load(Ordering::SeqCst), 3);

            server.abort();
        }
    }

    #[test]
    fn converts_anthropic_request_into_openai_shape() {
        let inbound = json!({
            "model": "claude-x",
            "system": "be brief",
            "max_tokens": 256,
            "temperature": 0.3,
            "stop_sequences": ["STOP"],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]}
            ],
            "tools": [{
                "name": "get_weather",
                "description": "weather",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
            }],
            "tool_choice": {"type": "tool", "name": "get_weather"}
        });
        let openai = anthropic_request_to_openai(&inbound, "claude-x");

        assert_eq!(openai["max_tokens"], 256);
        assert_eq!(openai["temperature"], 0.3);
        assert_eq!(openai["stop"], json!(["STOP"]));
        // The system prompt becomes a leading system message.
        assert_eq!(openai["messages"][0]["role"], "system");
        assert_eq!(openai["messages"][0]["content"], "be brief");
        // A text-only user turn collapses to a plain string.
        assert_eq!(openai["messages"][1]["role"], "user");
        assert_eq!(openai["messages"][1]["content"], "hi");
        // Anthropic tool schema maps onto the OpenAI function shape.
        assert_eq!(openai["tools"][0]["type"], "function");
        assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(
            openai["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
        assert_eq!(openai["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn converts_anthropic_tool_use_and_results_round_trip() {
        // Assistant asks for a tool, then the user returns its result.
        let inbound = json!({
            "model": "claude-x",
            "max_tokens": 128,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18C"}
                ]}
            ]
        });
        let openai = anthropic_request_to_openai(&inbound, "claude-x");
        let assistant = &openai["messages"][0];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "checking");
        assert_eq!(assistant["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        // The tool result becomes a dedicated role: tool message.
        let tool = &openai["messages"][1];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "toolu_1");
        assert_eq!(tool["content"], "18C");
    }

    #[test]
    fn converts_openai_completion_into_anthropic_message() {
        let upstream = json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
        });
        let (message, usage) = openai_response_to_anthropic(&upstream, "claude-x");
        assert_eq!(message["type"], "message");
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["model"], "claude-x");
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][0]["text"], "hello");
        assert_eq!(message["stop_reason"], "end_turn");
        assert_eq!(message["usage"]["input_tokens"], 4);
        assert_eq!(message["usage"]["output_tokens"], 2);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
    }

    #[test]
    fn converts_openai_tool_call_into_anthropic_tool_use() {
        let upstream = json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8}
        });
        let (message, _) = openai_response_to_anthropic(&upstream, "claude-x");
        // No text block: a tool-only answer must not emit an empty text block.
        assert_eq!(message["content"].as_array().unwrap().len(), 1);
        assert_eq!(message["content"][0]["type"], "tool_use");
        assert_eq!(message["content"][0]["id"], "call_1");
        assert_eq!(message["content"][0]["name"], "get_weather");
        assert_eq!(message["content"][0]["input"]["city"], "Paris");
        assert_eq!(message["stop_reason"], "tool_use");
    }

    #[test]
    fn maps_openai_finish_reasons_to_anthropic_stop_reasons() {
        assert_eq!(anthropic_stop_reason(Some("length"), false), "max_tokens");
        assert_eq!(anthropic_stop_reason(Some("tool_calls"), false), "tool_use");
        assert_eq!(anthropic_stop_reason(Some("stop"), false), "end_turn");
        // A tool_use block is authoritative even without a matching reason.
        assert_eq!(anthropic_stop_reason(None, true), "tool_use");
    }

    #[tokio::test]
    async fn rewrites_openai_stream_into_anthropic_events() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let context = StreamContext {
            message_id: "msg_test".to_string(),
            model: "claude-x".to_string(),
            input_tokens: 7,
            started: Instant::now(),
        };
        let mut state = AnthropicStreamState::default();

        let lines: [&[u8]; 5] = [
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n",
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n",
            b"data: [DONE]\n",
        ];
        let mut saw_done = false;
        for line in lines {
            if !process_openai_line_for_anthropic(line, &mut state, &context, &tx).await {
                saw_done = true;
            }
        }
        assert!(saw_done, "[DONE] must terminate the stream");
        // Exercise the same closing sequence the live handler uses.
        finish_anthropic_stream(&mut state, &context, &tx).await;
        drop(tx);

        let mut frames = Vec::new();
        while let Some(item) = rx.recv().await {
            frames.push(String::from_utf8(item.unwrap().to_vec()).unwrap());
        }
        let all = frames.join("");

        assert!(all.contains("event: message_start"), "{all}");
        assert!(all.contains("event: content_block_start"), "{all}");
        assert!(all.contains("\"type\":\"text_delta\""), "{all}");
        // Text deltas must not be re-joined; each chunk passes through.
        assert!(all.contains("\"text\":\"Hel\""), "{all}");
        assert!(all.contains("\"text\":\"lo\""), "{all}");
        assert!(all.contains("event: content_block_stop"), "{all}");
        assert!(all.contains("event: message_delta"), "{all}");
        assert!(all.contains("\"stop_reason\":\"end_turn\""), "{all}");
        assert!(all.contains("event: message_stop"), "{all}");
        assert_eq!(state.output_tokens, 2);
        assert_eq!(state.text, "Hello");
        assert!(state.first_token_ms.is_some());
    }

    #[tokio::test]
    async fn anthropic_stream_maps_tool_calls_to_tool_use_blocks() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let context = StreamContext {
            message_id: "msg_test".to_string(),
            model: "claude-x".to_string(),
            input_tokens: 5,
            started: Instant::now(),
        };
        let mut state = AnthropicStreamState::default();
        // Build the frames with serde so nested JSON escaping stays correct.
        let frames = [
            json!({"choices":[{"index":0,"delta":{"content":"thinking"}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}),
        ];
        for frame in frames {
            let line = format!("data: {frame}\n");
            process_openai_line_for_anthropic(line.as_bytes(), &mut state, &context, &tx).await;
        }
        process_openai_line_for_anthropic(b"data: [DONE]\n", &mut state, &context, &tx).await;
        drop(tx);

        let mut all = String::new();
        while let Some(item) = rx.recv().await {
            all.push_str(std::str::from_utf8(&item.unwrap()).unwrap());
        }
        assert!(all.contains("\"type\":\"tool_use\""), "{all}");
        assert!(all.contains("\"name\":\"get_weather\""), "{all}");
        assert!(all.contains("\"type\":\"input_json_delta\""), "{all}");
        assert!(all.contains("partial_json"), "{all}");
        assert!(state.has_tool_use);
        assert!(state.first_token_ms.is_some());
        assert_eq!(state.finish_reason, None);
    }

    #[test]
    fn anthropic_request_requires_a_model() {
        let missing = json!({"max_tokens": 10, "messages": []});
        assert!(requested_model_of(&missing).is_err());
        let present = json!({"model": "claude-x"});
        assert_eq!(requested_model_of(&present).unwrap(), "claude-x");
    }

    #[tokio::test]
    async fn count_tokens_enforces_model_access_and_route_availability() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO providers (name, provider_type, base_url, enabled)
             VALUES ('Scoped', 'anthropic', 'https://example.com', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO provider_models (provider_id, model_name, enabled)
             VALUES (1, 'gpt-5', 1), (1, 'claude-4', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let raw_key = "sk-openllm-count-tokens";
        sqlx::query(
            "INSERT INTO api_keys (
                name, key_hash, key_prefix, key_suffix, enabled, allowed_models
             ) VALUES ('scoped', ?, 'sk-openllm-c', 'kens', 1, '[\"gpt-*\"]')",
        )
        .bind(hash_secret(raw_key))
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::new(pool, None);
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(&format!("Bearer {raw_key}")).unwrap(),
        );

        let allowed = Bytes::from_static(
            br#"{"model":"gpt-5","messages":[{"role":"user","content":"hello"}]}"#,
        );
        assert!(count_tokens_inner(&state, &headers, &allowed).await.is_ok());

        let forbidden = Bytes::from_static(br#"{"model":"claude-4","messages":[]}"#);
        assert!(matches!(
            count_tokens_inner(&state, &headers, &forbidden).await,
            Err(AppError::Forbidden(_))
        ));

        let missing = Bytes::from_static(br#"{"model":"gpt-missing","messages":[]}"#);
        assert!(matches!(
            count_tokens_inner(&state, &headers, &missing).await,
            Err(AppError::NotFound(_))
        ));
    }

    #[test]
    fn stream_usage_merges_anthropic_start_and_delta() {
        // Anthropic sends input tokens in `message_start`...
        let mut parser = UsageParser::new(Instant::now());
        parser.push(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":0}}}\n\n",
        );
        // ...and output tokens later in `message_delta`.
        parser.push(
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        );
        let usage = parser.finish().expect("usage should be parsed");
        // Both sides must survive; replacing would leave one of them at zero.
        assert_eq!(usage.prompt_tokens, 11);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.total_tokens, 14);
    }

    #[test]
    fn extracts_openai_usage() {
        let usage = usage_from_value(&json!({
            "usage": { "prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19 }
        }))
        .expect("usage should parse");
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (12, 7, 19)
        );
    }

    #[test]
    fn reads_openai_cache_tokens_without_double_counting() {
        // Shape verified against a live CommandCode response: `cached_tokens`
        // lives under prompt_tokens_details and is *already included* in
        // prompt_tokens (638 stays constant while cached_tokens rises 0 -> 512).
        let usage = usage_from_value(&json!({
            "usage": {
                "prompt_tokens": 638,
                "completion_tokens": 32,
                "total_tokens": 670,
                "prompt_tokens_details": { "cached_tokens": 512 }
            }
        }))
        .expect("usage should parse");
        assert_eq!(usage.prompt_tokens, 638, "must not add cached on top");
        assert_eq!(usage.cache_read_tokens, 512);
        assert_eq!(usage.total_tokens, 670);
    }

    #[test]
    fn reads_anthropic_cache_tokens_as_additional_input() {
        // Anthropic reports input_tokens *excluding* cache traffic, so the
        // cache counts must be folded in for the total input to be correct.
        let usage = usage_from_value(&json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_read_input_tokens": 800,
                "cache_creation_input_tokens": 50
            }
        }))
        .expect("usage should parse");
        assert_eq!(usage.cache_read_tokens, 800);
        assert_eq!(usage.cache_write_tokens, 50);
        assert_eq!(usage.prompt_tokens, 950, "100 fresh + 800 read + 50 write");
    }

    #[test]
    fn cache_fields_default_to_zero_when_absent() {
        let usage = usage_from_value(&json!({
            "usage": { "prompt_tokens": 10, "completion_tokens": 2 }
        }))
        .unwrap();
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
        assert_eq!(usage.prompt_tokens, 10);
    }

    #[test]
    fn negative_cache_counts_are_clamped() {
        let usage = Usage {
            prompt_tokens: 5,
            completion_tokens: 1,
            total_tokens: 6,
            cache_read_tokens: -10,
            cache_write_tokens: -3,
        }
        .normalized();
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn extracts_nested_responses_usage() {
        let usage = usage_from_value(&json!({
            "type": "response.completed",
            "response": { "usage": { "input_tokens": 88, "output_tokens": 42, "total_tokens": 130 } }
        }))
        .expect("nested responses usage should parse");
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (88, 42, 130)
        );
    }

    #[test]
    fn derives_total_when_upstream_omits_it() {
        let usage = usage_from_value(&json!({
            "usage": { "prompt_tokens": 5, "completion_tokens": 3 }
        }))
        .expect("usage should parse");
        assert_eq!(usage.total_tokens, 8);
    }

    #[test]
    fn stream_parser_counts_chat_completion_text() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
        parser.push(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2,\"total_tokens\":4}}\n\n",
        );
        let usage = parser.finish().expect("usage should be parsed");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (2, 2));
    }

    #[test]
    fn stream_parser_records_first_token_once_on_real_content() {
        let mut parser = UsageParser::new(Instant::now());
        // A role-only opening frame and an empty delta must not start the
        // clock; otherwise TTFT would be reported as roughly zero even though
        // the model has not produced anything yet.
        parser.push(b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n");
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n");
        assert_eq!(parser.first_token_ms, None);
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
        let first = parser.first_token_ms.expect("first token should be timed");
        parser.push(b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n");
        assert_eq!(
            parser.first_token_ms,
            Some(first),
            "the timestamp must be captured once and not overwritten"
        );
        parser.finish();
    }

    #[test]
    fn stream_parser_counts_responses_api_deltas() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n");
        parser.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n");
        assert!(parser.first_token_ms.is_some());
        let usage = parser
            .finish()
            .expect("estimated usage should be produced from streamed text");
        assert!(
            usage.completion_tokens > 0,
            "estimated completion tokens should be non-zero"
        );
    }

    #[test]
    fn stream_parser_times_tool_call_and_reasoning_output() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n\n");
        let first = parser
            .first_token_ms
            .expect("reasoning output should start the first-token clock");
        parser.push(b"data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"path\\\":\"}\n\n");
        assert_eq!(parser.first_token_ms, Some(first));

        let mut chat = UsageParser::new(Instant::now());
        chat.push(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
        );
        assert!(
            chat.first_token_ms.is_some(),
            "tool-call argument deltas should count as output"
        );
        let usage = chat
            .finish()
            .expect("tool-call arguments should produce an estimate");
        assert!(usage.completion_tokens > 0);
    }

    #[test]
    fn stream_parser_times_native_anthropic_content() {
        let mut text = UsageParser::new(Instant::now());
        text.push(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        );
        assert!(text.first_token_ms.is_some());

        let mut tool = UsageParser::new(Instant::now());
        tool.push(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"name\":\"get_weather\"}}\n\n",
        );
        assert!(
            tool.first_token_ms.is_some(),
            "tool block starts should be treated as the beginning of output"
        );

        let mut thinking = UsageParser::new(Instant::now());
        thinking.push(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
        );
        assert!(thinking.first_token_ms.is_some());
    }

    #[test]
    fn stream_parser_counts_legacy_completions_text() {
        let mut parser = UsageParser::new(Instant::now());
        parser.push(b"data: {\"choices\":[{\"text\":\"hello \"}]}\n\n");
        parser.push(b"data: {\"choices\":[{\"text\":\"world\"}]}\n\n");
        let usage = parser
            .finish()
            .expect("legacy completions text should produce an estimate");
        assert!(
            usage.completion_tokens >= 2,
            "legacy completions text should be counted, got {}",
            usage.completion_tokens
        );
    }

    #[test]
    fn stream_preview_is_bounded() {
        let mut parser = UsageParser::new(Instant::now());
        // Feed far more text than the preview budget across many frames.
        for _ in 0..200 {
            let frame = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
                "x".repeat(100)
            );
            parser.push(frame.as_bytes());
        }
        let preview = parser.preview().expect("preview should exist");
        assert!(
            preview.chars().count() <= PREVIEW_CHAR_LIMIT,
            "preview should be capped at {PREVIEW_CHAR_LIMIT}, got {}",
            preview.chars().count()
        );
    }

    #[test]
    fn anthropic_response_converts_to_openai_shape() {
        let (converted, usage) = convert_anthropic_response(&json!({
            "id": "msg_1",
            "model": "claude-x",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "hi there" }],
            "usage": { "input_tokens": 30, "output_tokens": 12 }
        }));
        assert_eq!(converted["object"], "chat.completion");
        assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
    }

    #[test]
    fn converts_openai_tool_calls_and_results_to_anthropic() {
        let converted = convert_request_to_anthropic(
            &json!({
                "messages": [
                    { "role": "user", "content": "weather in Paris?" },
                    {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_1", "content": "18C" }
                ]
            }),
            "claude-x",
            false,
        );

        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 3);

        let assistant = &messages[1]["content"];
        let blocks = assistant.as_array().expect("assistant content blocks");
        assert_eq!(blocks.len(), 1, "empty text block should be dropped");
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "call_1");
        assert_eq!(blocks[0]["name"], "get_weather");
        assert_eq!(blocks[0]["input"]["city"], "Paris");

        let tool_result = &messages[2]["content"][0];
        assert_eq!(tool_result["type"], "tool_result");
        assert_eq!(tool_result["tool_use_id"], "call_1");
        assert_eq!(tool_result["content"], "18C");
    }

    #[test]
    fn converts_max_output_tokens_to_anthropic_max_tokens() {
        let converted = convert_request_to_anthropic(
            &json!({
                "max_output_tokens": 1200,
                "messages": [{ "role": "user", "content": "hello" }]
            }),
            "claude-x",
            false,
        );
        assert_eq!(converted["max_tokens"], 1200);
    }

    #[test]
    fn responses_request_maps_instructions_and_input_to_anthropic() {
        let converted = responses_request_to_anthropic(
            &json!({
                "instructions": "be terse",
                "max_output_tokens": 512,
                "input": [
                    {
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "hello" }]
                    }
                ],
                "tools": [{
                    "type": "function",
                    "name": "get_weather",
                    "parameters": { "type": "object", "properties": {} }
                }]
            }),
            "claude-x",
            false,
        );
        assert_eq!(converted["system"], "be terse");
        assert_eq!(converted["max_tokens"], 512);
        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "hello");
        assert_eq!(converted["tools"][0]["name"], "get_weather");
    }

    #[test]
    fn responses_accepts_plain_string_input() {
        let converted =
            responses_request_to_anthropic(&json!({ "input": "hello world" }), "claude-x", false);
        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "hello world");
    }

    #[test]
    fn responses_function_call_items_round_trip_to_anthropic() {
        let converted = responses_request_to_anthropic(
            &json!({
                "input": [
                    { "role": "user", "content": "weather in Paris?" },
                    {
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_1",
                        "output": "18C"
                    }
                ]
            }),
            "claude-x",
            false,
        );
        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 3);
        let assistant = messages[1]["content"].as_array().expect("assistant blocks");
        assert_eq!(assistant.len(), 1);
        assert_eq!(assistant[0]["type"], "tool_use");
        assert_eq!(assistant[0]["id"], "call_1");
        assert_eq!(assistant[0]["name"], "get_weather");
        assert_eq!(assistant[0]["input"]["city"], "Paris");
        let tool_result = &messages[2]["content"][0];
        assert_eq!(tool_result["type"], "tool_result");
        assert_eq!(tool_result["tool_use_id"], "call_1");
        assert_eq!(tool_result["content"], "18C");
    }

    #[test]
    fn anthropic_response_converts_to_responses_output() {
        let (converted, usage) = anthropic_response_to_responses(
            &json!({
                "id": "msg_1",
                "model": "claude-x",
                "stop_reason": "tool_use",
                "content": [
                    { "type": "text", "text": "let me check" },
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "get_weather",
                        "input": { "city": "Paris" }
                    }
                ],
                "usage": { "input_tokens": 30, "output_tokens": 12 }
            }),
            "coding",
        );
        assert_eq!(converted["object"], "response");
        assert_eq!(converted["model"], "coding");
        assert_eq!(converted["status"], "completed");
        assert_eq!(converted["output"][0]["type"], "message");
        assert_eq!(converted["output"][0]["content"][0]["text"], "let me check");
        assert_eq!(converted["output"][1]["type"], "function_call");
        assert_eq!(converted["output"][1]["call_id"], "toolu_1");
        assert_eq!(converted["output"][1]["name"], "get_weather");
        assert_eq!(converted["output"][1]["arguments"], "{\"city\":\"Paris\"}");
        assert_eq!(converted["usage"]["input_tokens"], 30);
        assert_eq!(converted["usage"]["output_tokens"], 12);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (30, 12));
    }

    #[tokio::test]
    async fn anthropic_stream_converts_to_responses_events() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
        let mut event_name = String::new();
        for line in [
            b"event: message_start\n".as_slice(),
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n".as_slice(),
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n".as_slice(),
            b"event: content_block_stop\n".as_slice(),
            b"data: {\"type\":\"content_block_stop\",\"index\":0}\n".as_slice(),
            b"event: message_delta\n".as_slice(),
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n".as_slice(),
            b"event: message_stop\n".as_slice(),
            b"data: {\"type\":\"message_stop\"}\n".as_slice(),
        ] {
            stream.handle_line(line, &mut event_name, &tx).await;
        }
        stream.complete(&tx).await;
        drop(tx);

        let mut events = Vec::new();
        while let Some(chunk) = rx.recv().await {
            let chunk = chunk.expect("stream chunk");
            events.push(String::from_utf8_lossy(&chunk).to_string());
        }
        let joined = events.join("");
        assert!(joined.contains("event: response.created"));
        assert!(joined.contains("event: response.output_text.delta"));
        assert!(joined.contains("\"delta\":\"hello\""));
        assert!(joined.contains("event: response.output_text.done"));
        assert!(joined.contains("event: response.completed"));
        assert_eq!(stream.usage.prompt_tokens, 7);
        assert_eq!(stream.usage.completion_tokens, 3);
    }

    #[tokio::test]
    async fn chat_stream_converts_to_responses_events() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut stream = ResponsesStreamState::new("coding".to_string(), Instant::now());
        let mut tool_index = None;
        for line in [
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hel\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n".as_slice(),
            b"data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n".as_slice(),
            b"data: [DONE]\n".as_slice(),
        ] {
            process_chat_chunk_line(&mut stream, line, &mut tool_index, &tx).await;
        }
        stream.ensure_created(&tx).await;
        stream.complete(&tx).await;
        drop(tx);

        let mut joined = String::new();
        while let Some(chunk) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
        }
        assert!(joined.contains("event: response.created"));
        assert!(joined.contains("event: response.output_text.delta"));
        assert!(joined.contains("\"delta\":\"hel\""));
        assert!(joined.contains("\"delta\":\"lo\""));
        assert!(joined.contains("event: response.output_text.done"));
        assert!(joined.contains("event: response.completed"));
        assert_eq!(stream.usage.prompt_tokens, 5);
        assert_eq!(stream.usage.completion_tokens, 2);
    }

    #[test]
    fn completions_request_maps_prompt_to_anthropic() {
        let converted = completions_request_to_anthropic(
            &json!({
                "prompt": "write a haiku",
                "max_tokens": 64,
                "temperature": 0.5,
                "stop": ["\n\n"]
            }),
            "claude-x",
            false,
        );
        assert_eq!(converted["max_tokens"], 64);
        assert_eq!(converted["temperature"], 0.5);
        assert_eq!(converted["stop_sequences"][0], "\n\n");
        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "write a haiku");
    }

    #[test]
    fn completions_request_joins_prompt_array() {
        let converted = completions_request_to_anthropic(
            &json!({ "prompt": ["hello ", "world"] }),
            "claude-x",
            false,
        );
        let messages = converted["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["content"][0]["text"], "hello world");
    }

    #[test]
    fn anthropic_response_converts_to_text_completion() {
        let (converted, usage) = anthropic_response_to_completions(
            &json!({
                "id": "msg_1",
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": "a haiku" }],
                "usage": { "input_tokens": 9, "output_tokens": 4 }
            }),
            "coding",
        );
        assert_eq!(converted["object"], "text_completion");
        assert_eq!(converted["model"], "coding");
        assert_eq!(converted["choices"][0]["text"], "a haiku");
        assert_eq!(converted["choices"][0]["finish_reason"], "stop");
        assert_eq!(converted["usage"]["completion_tokens"], 4);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (9, 4));
    }

    #[tokio::test]
    async fn anthropic_stream_converts_to_completions_events() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut text = String::new();
        let mut stop_reason = None;
        let mut first_token_ms = None;
        for line in [
            b"event: message_start\n".as_slice(),
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n".as_slice(),
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n".as_slice(),
            b"event: message_delta\n".as_slice(),
            b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n".as_slice(),
        ] {
            process_completions_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut text,
                &mut stop_reason,
                &mut first_token_ms,
                Instant::now(),
                "cmpl_test",
                "coding",
                &tx,
            )
            .await;
        }
        drop(tx);
        let mut events = Vec::new();
        while let Some(chunk) = rx.recv().await {
            events.push(String::from_utf8_lossy(&chunk.expect("chunk")).to_string());
        }
        let joined = events.join("");
        assert!(joined.contains("\"object\":\"text_completion\""));
        assert!(joined.contains("\"text\":\"hi\""));
        assert_eq!(text, "hi");
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    }

    #[tokio::test]
    async fn anthropic_stream_tool_use_becomes_openai_tool_calls() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut text = String::new();
        let mut sent_role = false;
        let mut saw_tool_use = false;
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut first_token_ms: Option<i64> = None;

        // Anthropic emits a tool_use block start followed by JSON argument deltas.
        for line in [
            b"event: content_block_start\n".as_slice(),
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        for line in [
            b"event: content_block_delta\n".as_slice(),
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}\n".as_slice(),
        ] {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        drop(tx);

        let mut frames = Vec::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            frames.push(String::from_utf8_lossy(&bytes).to_string());
        }
        let joined = frames.join("");

        assert!(saw_tool_use, "tool_use block start should be detected");
        assert!(
            joined.contains("\"tool_calls\""),
            "should emit tool_calls delta: {joined}"
        );
        assert!(
            joined.contains("toolu_1"),
            "should carry the tool call id: {joined}"
        );
        assert!(
            joined.contains("get_weather"),
            "should carry the function name: {joined}"
        );
        assert!(
            joined.contains("Paris"),
            "should carry streamed arguments: {joined}"
        );
    }

    #[tokio::test]
    async fn parallel_anthropic_tool_calls_keep_distinct_indices() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut text = String::new();
        let mut sent_role = false;
        let mut saw_tool_use = false;
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut first_token_ms: Option<i64> = None;

        let lines: [&[u8]; 8] = [
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"get_weather\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"get_time\"}}\n",
            b"event: content_block_delta\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            b"event: message_delta\n",
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n",
        ];
        for line in lines {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        drop(tx);

        let mut joined = String::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&bytes));
        }

        assert!(joined.contains("toolu_a"), "first tool call should appear");
        assert!(joined.contains("toolu_b"), "second tool call should appear");
        assert!(
            joined.contains("\"index\":1"),
            "second tool call should keep index 1 rather than collapsing to 0: {joined}"
        );
        assert!(
            joined.contains("get_time"),
            "second tool name should appear"
        );
    }

    #[tokio::test]
    async fn text_block_before_tools_does_not_shift_tool_indices() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut event_name = String::new();
        let mut usage = Usage::default();
        let mut output_chars = 0usize;
        let mut text = String::new();
        let mut sent_role = false;
        let mut saw_tool_use = false;
        let mut next_tool_index = 0usize;
        let mut tool_indices = std::collections::HashMap::<i64, usize>::new();
        let mut first_token_ms: Option<i64> = None;

        // Anthropic block 0 is text; the tool calls are blocks 1 and 2.
        // OpenAI must renumber the tool calls to 0 and 1.
        let lines: [&[u8]; 10] = [
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"get_weather\"}}\n",
            b"event: content_block_start\n",
            b"data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"get_time\"}}\n",
            b"event: content_block_delta\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            b"event: message_delta\n",
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n",
        ];
        for line in lines {
            process_anthropic_line(
                line,
                &mut event_name,
                &mut usage,
                &mut output_chars,
                &mut text,
                &mut sent_role,
                &mut saw_tool_use,
                &mut next_tool_index,
                &mut tool_indices,
                &mut first_token_ms,
                Instant::now(),
                "chatcmpl_test",
                "claude-x",
                &tx,
            )
            .await;
        }
        drop(tx);

        let mut joined = String::new();
        while let Some(Ok(bytes)) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&bytes));
        }

        assert!(
            joined.contains("toolu_a") && joined.contains("get_weather"),
            "first tool call should appear: {joined}"
        );
        assert!(
            joined.contains("\"index\":0") && joined.contains("\"index\":1"),
            "tool calls should be renumbered to 0 and 1: {joined}"
        );
        assert!(
            !joined.contains("\"index\":2"),
            "OpenAI tool indices must not include Anthropic text-block offsets: {joined}"
        );
    }

    #[test]
    fn chat_request_converts_to_responses_shape() {
        let converted = chat_request_to_responses(
            &json!({
                "model": "coding",
                "max_tokens": 256,
                "temperature": 0.2,
                "messages": [
                    { "role": "system", "content": "be terse" },
                    { "role": "user", "content": [
                        { "type": "text", "text": "guess 2+2" }
                    ] },
                    {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "calc", "arguments": "{\"expr\":\"2+2\"}" }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_1", "content": "4" }
                ],
                "tools": [{
                    "type": "function",
                    "function": { "name": "calc", "parameters": { "type": "object" } }
                }]
            }),
            "deepseek/x",
            true,
        );
        assert_eq!(converted["model"], "deepseek/x");
        assert_eq!(converted["stream"], true);
        assert_eq!(converted["instructions"], "be terse");
        assert_eq!(converted["max_output_tokens"], 256);
        let input = converted["input"].as_array().expect("input array");
        // user message, function_call, function_call_output (assistant text was empty)
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "guess 2+2");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[1]["name"], "calc");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "4");
        assert_eq!(converted["tools"][0]["name"], "calc");
    }

    #[test]
    fn responses_result_converts_to_chat_completion() {
        let (converted, usage) = responses_response_to_chat(
            &json!({
                "id": "resp_abc",
                "status": "completed",
                "output": [
                    { "type": "reasoning", "summary": [] },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "hi there" }]
                    },
                    {
                        "type": "function_call",
                        "call_id": "call_9",
                        "name": "calc",
                        "arguments": "{\"expr\":\"2+2\"}"
                    }
                ],
                "usage": { "input_tokens": 11, "output_tokens": 5, "total_tokens": 16 }
            }),
            "coding",
        );
        assert_eq!(converted["object"], "chat.completion");
        assert_eq!(converted["model"], "coding");
        assert_eq!(converted["choices"][0]["message"]["content"], "hi there");
        assert_eq!(converted["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            converted["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_9"
        );
        assert_eq!(converted["usage"]["prompt_tokens"], 11);
        assert_eq!(converted["usage"]["completion_tokens"], 5);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (11, 5));
    }

    #[tokio::test]
    async fn responses_stream_converts_to_chat_chunks() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let mut event_name = String::new();
        let mut state = ChatStreamState::default();
        for line in [
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\" there\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3},\"status\":\"completed\"}}\n".as_slice(),
        ] {
            process_responses_line_for_chat(
                line,
                &mut event_name,
                &mut state,
                "chatcmpl_test",
                "coding",
                Instant::now(),
                &tx,
            )
            .await;
        }
        drop(tx);
        let mut joined = String::new();
        while let Some(chunk) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
        }
        assert!(joined.contains("\"content\":\"hi\""));
        assert!(joined.contains("\"content\":\" there\""));
        assert_eq!(state.text, "hi there");
        assert_eq!(state.prompt_tokens, 7);
        assert_eq!(state.completion_tokens, 3);
    }

    #[test]
    fn responses_result_converts_to_anthropic_message() {
        let (converted, usage) = responses_response_to_anthropic(
            &json!({
                "id": "resp_1",
                "status": "completed",
                "output": [
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "hi" }]
                    },
                    {
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "calc",
                        "arguments": "{\"x\":1}"
                    }
                ],
                "usage": { "input_tokens": 4, "output_tokens": 2 }
            }),
            "coding",
        );
        assert_eq!(converted["type"], "message");
        assert_eq!(converted["model"], "coding");
        assert_eq!(converted["content"][0]["type"], "text");
        assert_eq!(converted["content"][0]["text"], "hi");
        assert_eq!(converted["content"][1]["type"], "tool_use");
        assert_eq!(converted["content"][1]["name"], "calc");
        assert_eq!(converted["content"][1]["input"]["x"], 1);
        assert_eq!(converted["stop_reason"], "tool_use");
        assert_eq!(converted["usage"]["output_tokens"], 2);
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (4, 2));
    }

    #[tokio::test]
    async fn responses_stream_converts_to_anthropic_events() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
        let context = StreamContext {
            message_id: "msg_test".to_string(),
            model: "coding".to_string(),
            input_tokens: 5,
            started: Instant::now(),
        };
        let mut state = AnthropicStreamState::default();
        let mut tool_indices = std::collections::HashMap::new();
        let mut event_name = String::new();
        for line in [
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n".as_slice(),
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n".as_slice(),
            b"event: response.completed\n".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":6,\"output_tokens\":2}}}\n".as_slice(),
        ] {
            process_responses_line_for_anthropic(
                line,
                &mut event_name,
                &mut state,
                &mut tool_indices,
                &context,
                &tx,
            )
            .await;
        }
        finish_anthropic_stream(&mut state, &context, &tx).await;
        drop(tx);
        let mut joined = String::new();
        while let Some(chunk) = rx.recv().await {
            joined.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
        }
        assert!(joined.contains("event: message_start"));
        assert!(joined.contains("event: content_block_start"));
        assert!(joined.contains("\"text_delta\""));
        assert!(joined.contains("\"text\":\"hi\""));
        assert!(joined.contains("event: message_stop"));
        assert_eq!(state.text, "hi!");
        assert_eq!(state.output_tokens, 2);
    }
}
