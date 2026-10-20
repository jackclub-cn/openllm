use super::*;

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
    parser.push(
        b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n\n",
    );
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
fn request_preview_masks_common_secrets_and_respects_the_limit() {
    let body = json!({
        "model": "model",
        "max_tokens": 1024,
        "api_key": "sk-secret",
        "metadata": {
            "access_token": "token-secret",
            "password": "pw-secret"
        },
        "messages": [{"role": "user", "content": "hello"}]
    });
    let preview = request_preview(&body, 4000).unwrap();
    assert!(preview.contains("<redacted>"));
    assert!(!preview.contains("sk-secret"));
    assert!(!preview.contains("token-secret"));
    assert!(preview.contains("\"max_tokens\": 1024"));

    let bounded = request_preview(&body, 32).unwrap();
    assert_eq!(bounded.chars().count(), 32);
}

#[tokio::test]
async fn request_preview_is_persisted_when_supplied() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    log_usage_started(
        &state,
        "request-preview",
        None,
        None,
        None,
        "model",
        "/v1/chat/completions",
        3,
        false,
        Some("{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}"),
    )
    .await;

    let preview: Option<String> =
        sqlx::query_scalar("SELECT request_preview FROM usage_logs WHERE request_id = ?")
            .bind("request-preview")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        preview.as_deref(),
        Some("{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}")
    );
}
