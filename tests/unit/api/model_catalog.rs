use super::*;

#[test]
fn parses_openai_style_model_lists() {
    let value = json!({ "object": "list", "data": [
        { "id": "gpt-4.1" },
        { "id": "gpt-4.1-mini" },
        { "name": "fallback-name" }
    ]});
    let names = parse_openai_model_entries(&value)
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["gpt-4.1", "gpt-4.1-mini", "fallback-name"]);
}

#[test]
fn captures_upstream_context_length_and_endpoints() {
    // CommandCode reports a flat `context_length` for every model, even ones
    // models.dev has never heard of.
    let value = json!({ "data": [
        { "id": "deepseek/deepseek-v4.1-flash", "context_length": 1000000,
          "supported_endpoints": ["/chat/completions", "/responses"] }
    ]});
    let entries = parse_openai_model_entries(&value);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.context_limit, Some(1000000));
    assert_eq!(
        entries[0].1.supported_endpoints,
        vec!["/chat/completions", "/responses"]
    );
}

#[test]
fn prefers_stricter_upstream_input_limit() {
    // The live CallAI catalog advertises 400K input, while models.dev
    // reports a 1.05M window. Keeping the smaller value prevents Codex from
    // compacting too late and hitting an upstream 400.
    let value = json!({ "data": [
        {
            "id": "gpt-6-astra",
            "context_length": 1050000,
            "max_input_tokens": 400000
        }
    ]});
    let entries = parse_openai_model_entries(&value);
    assert_eq!(entries[0].1.context_limit, Some(400_000));
}

#[test]
fn accepts_max_input_tokens_without_context_length() {
    let value = json!({ "data": [
        { "id": "codex-auto-review", "max_input_tokens": 400000 }
    ]});
    let entries = parse_openai_model_entries(&value);
    assert_eq!(entries[0].1.context_limit, Some(400_000));
}

#[test]
fn validates_manual_model_limits() {
    assert!(validate_limit("context", None).is_ok());
    assert!(validate_limit("context", Some(400_000)).is_ok());
    assert!(validate_limit("context", Some(0)).is_err());
    assert!(validate_limit("input", Some(-1)).is_err());

    assert_eq!(serialize_endpoint_override(None).unwrap(), None);
    assert_eq!(
        serialize_endpoint_override(Some(&[
            "/v1/responses/".to_string(),
            "/v1/responses".to_string()
        ]))
        .unwrap()
        .as_deref(),
        Some(r#"["/v1/responses"]"#)
    );
    assert!(serialize_endpoint_override(Some(&["responses".to_string()])).is_err());

    assert!(validate_cost_override("input cost", None).is_ok());
    assert!(validate_cost_override("input cost", Some(0.0)).is_ok());
    assert!(validate_cost_override("input cost", Some(1.25)).is_ok());
    assert!(validate_cost_override("input cost", Some(-0.1)).is_err());
}

#[test]
fn parses_ollama_style_model_lists() {
    let value = json!({ "models": [
        { "name": "llama3:8b", "model": "llama3:8b" },
        { "model": "qwen2:7b" }
    ]});
    assert_eq!(parse_ollama_models(&value), vec!["llama3:8b", "qwen2:7b"]);
}

#[test]
fn ignores_empty_model_entries() {
    let value = json!({ "data": [{ "id": "  " }, { "id": "real-model" }] });
    let names = parse_openai_model_entries(&value)
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["real-model"]);
}
