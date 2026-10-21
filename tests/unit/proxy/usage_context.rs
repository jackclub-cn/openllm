use super::*;

fn category_tokens(context: &ContextBreakdown, key: &str) -> i64 {
    context
        .categories
        .iter()
        .find(|category| category.key == key)
        .map(|category| category.tokens)
        .unwrap_or_default()
}

#[test]
fn openai_chat_context_is_split_by_message_role_and_tools() {
    let body = json!({
        "messages": [
            {"role": "system", "content": "system instructions with useful detail"},
            {"role": "user", "content": "please inspect the repository"},
            {"role": "tool", "content": "tool output that is fairly long"},
            {"role": "assistant", "content": "I will inspect it"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "read a file from disk",
                "parameters": {"type": "object"}
            }
        }]
    });
    let context = request_context_breakdown(&body, OPENAI_CHAT_COMPLETIONS, 100);
    assert_eq!(
        context
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<i64>(),
        100
    );
    assert!(category_tokens(&context, "system") > 0);
    assert!(category_tokens(&context, "tools") > 0);
    assert!(category_tokens(&context, "tool_results") > 0);
    assert!(category_tokens(&context, "conversation") > 0);
    assert!(context.estimated);
}

#[test]
fn responses_and_anthropic_context_shapes_are_supported() {
    let responses = json!({
        "instructions": "you are a coding assistant",
        "tools": [{"type": "function", "name": "shell", "description": "run a shell command"}],
        "input": [
            {"type": "message", "role": "user", "content": "fix the tests"},
            {"type": "function_call_output", "call_id": "c1", "output": "failure output"}
        ]
    });
    let context = request_context_breakdown(&responses, OPENAI_RESPONSES, 80);
    assert!(category_tokens(&context, "system") > 0);
    assert!(category_tokens(&context, "tools") > 0);
    assert!(category_tokens(&context, "tool_results") > 0);

    let anthropic = json!({
        "system": "answer carefully",
        "tools": [{"name": "lookup", "description": "look something up", "input_schema": {}}],
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "find the issue"},
                {"type": "tool_result", "tool_use_id": "t1", "content": "large tool result"}
            ]}
        ]
    });
    let context = request_context_breakdown(&anthropic, ANTHROPIC_MESSAGES, 60);
    assert!(category_tokens(&context, "system") > 0);
    assert!(category_tokens(&context, "tools") > 0);
    assert!(category_tokens(&context, "tool_results") > 0);
    assert_eq!(
        context
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<i64>(),
        60
    );
}

#[test]
fn calibration_preserves_the_exact_upstream_total() {
    let mut context = ContextBreakdown {
        window_tokens: Some(1_000),
        input_tokens: 100,
        estimated: true,
        input_estimated: true,
        categories: vec![
            ContextCategory {
                key: "system".to_string(),
                tokens: 20,
                items: 1,
            },
            ContextCategory {
                key: "tools".to_string(),
                tokens: 30,
                items: 1,
            },
            ContextCategory {
                key: "tool_results".to_string(),
                tokens: 15,
                items: 1,
            },
            ContextCategory {
                key: "conversation".to_string(),
                tokens: 35,
                items: 1,
            },
        ],
        ..Default::default()
    };
    context.calibrate(137);
    assert!(context.estimated);
    assert!(!context.input_estimated);
    assert_eq!(context.input_tokens, 137);
    assert_eq!(
        context
            .categories
            .iter()
            .map(|category| category.tokens)
            .sum::<i64>(),
        137
    );
}

#[test]
fn media_is_not_counted_as_text_and_stays_unattributed_after_calibration() {
    let body = json!({"messages": [{"role": "user", "content": [
        {"type": "text", "text": "describe this"},
        {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", "A".repeat(10000))}}
    ]}]});
    let mut context = request_context_breakdown(&body, OPENAI_CHAT_COMPLETIONS, 10);
    assert_eq!(context.unmetered_items, 1);
    context.calibrate(200);
    assert_eq!(category_tokens(&context, "conversation"), 10);
    assert_eq!(category_tokens(&context, "unattributed"), 190);
    assert!(context.estimated);
    assert!(!context.input_estimated);
    context.calibrate(0);
    assert_eq!(context.input_tokens, 0);
    assert_eq!(
        context
            .categories
            .iter()
            .map(|item| item.tokens)
            .sum::<i64>(),
        0
    );
}
