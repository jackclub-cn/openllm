use super::*;

#[test]
fn normalizes_model_prefix_with_trailing_slash() {
    assert_eq!(normalize_model_prefix("openai").unwrap(), "openai/");
    assert_eq!(normalize_model_prefix("openai/").unwrap(), "openai/");
    assert_eq!(normalize_model_prefix("  /local/  ").unwrap(), "local/");
    assert_eq!(normalize_model_prefix("").unwrap(), "");
}

#[test]
fn normalizes_and_validates_model_auto_include_rules() {
    let rules = normalize_model_auto_include_rules(&[
        " gpt-* ".to_string(),
        "gpt-*".to_string(),
        "re:^deepseek-".to_string(),
        String::new(),
    ])
    .unwrap();
    assert_eq!(rules, vec!["gpt-*", "re:^deepseek-"]);
    assert!(normalize_model_auto_include_rules(&["[".to_string()]).is_err());
    assert!(normalize_model_auto_include_rules(&["re:(".to_string()]).is_err());
}

#[test]
fn provider_test_urls_reuse_the_shared_joiner() {
    // The probe must hit the inference endpoint, which is what enforces
    // credentials, and must not double up `/v1`.
    assert_eq!(
        join_upstream_url(
            "https://api.commandcode.ai/provider/v1",
            "/v1/chat/completions"
        ),
        "https://api.commandcode.ai/provider/v1/chat/completions"
    );
    assert_eq!(
        join_upstream_url("https://api.anthropic.com", "/v1/messages"),
        "https://api.anthropic.com/v1/messages"
    );
    // A base without `/v1` keeps the full path.
    assert_eq!(
        join_upstream_url("http://localhost:8000", "/v1/chat/completions"),
        "http://localhost:8000/v1/chat/completions"
    );
}

#[test]
fn rejects_model_prefix_with_invalid_characters() {
    assert!(normalize_model_prefix("bad prefix").is_err());
    assert!(normalize_model_prefix("spaces/and").is_err());
}

#[test]
fn derives_ollama_root_from_root_and_v1_base_urls() {
    assert_eq!(
        ollama_root("http://localhost:11434"),
        "http://localhost:11434"
    );
    assert_eq!(
        ollama_root("http://localhost:11434/"),
        "http://localhost:11434"
    );
    assert_eq!(
        ollama_root("http://localhost:11434/v1"),
        "http://localhost:11434"
    );
}

#[test]
fn trims_trailing_slash_from_base_url() {
    assert_eq!(
        normalize_base_url("https://api.example.com/v1/"),
        "https://api.example.com/v1"
    );
    assert_eq!(
        normalize_base_url("  https://api.example.com/v1  "),
        "https://api.example.com/v1"
    );
}

#[test]
fn validates_provider_health_interval() {
    assert_eq!(normalize_health_interval(None).unwrap(), None);
    assert_eq!(normalize_health_interval(Some(0)).unwrap(), None);
    assert_eq!(normalize_health_interval(Some(30)).unwrap(), Some(30));
    assert!(normalize_health_interval(Some(-1)).is_err());
}

#[test]
fn validates_provider_request_policy() {
    assert_eq!(normalize_provider_timeout(None).unwrap(), None);
    assert_eq!(normalize_provider_timeout(Some(0)).unwrap(), None);
    assert_eq!(normalize_provider_timeout(Some(120)).unwrap(), Some(120));
    assert!(normalize_provider_timeout(Some(-1)).is_err());
    assert!(normalize_provider_timeout(Some(3601)).is_err());

    assert_eq!(normalize_provider_cooldown(None).unwrap(), None);
    assert_eq!(normalize_provider_cooldown(Some(0)).unwrap(), None);
    assert_eq!(normalize_provider_cooldown(Some(900)).unwrap(), Some(900));
    assert!(normalize_provider_cooldown(Some(-1)).is_err());
    assert!(normalize_provider_cooldown(Some(3601)).is_err());

    assert_eq!(normalize_provider_concurrency(None).unwrap(), None);
    assert_eq!(normalize_provider_concurrency(Some(0)).unwrap(), None);
    assert_eq!(normalize_provider_concurrency(Some(4)).unwrap(), Some(4));
    assert!(normalize_provider_concurrency(Some(-1)).is_err());
    assert!(normalize_provider_concurrency(Some(1001)).is_err());

    assert_eq!(normalize_provider_queue_timeout(None).unwrap(), None);
    assert_eq!(normalize_provider_queue_timeout(Some(0)).unwrap(), Some(0));
    assert_eq!(
        normalize_provider_queue_timeout(Some(30)).unwrap(),
        Some(30)
    );
    assert!(normalize_provider_queue_timeout(Some(-1)).is_err());
    assert!(normalize_provider_queue_timeout(Some(301)).is_err());
}
