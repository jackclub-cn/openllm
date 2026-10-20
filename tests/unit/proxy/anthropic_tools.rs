use super::*;

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
