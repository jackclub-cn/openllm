use super::*;

// -------------------------------------------------------------------------
// Export
// -------------------------------------------------------------------------

#[tokio::test]
async fn a_masked_export_documents_credentials_without_carrying_them() {
    let state = seeded_state().await;
    let document = export(&state, false).await;
    assert!(!document.secrets_included);
    assert_eq!(document.openllm_config_version, 1);
    assert_eq!(document.gateway_version, env!("CARGO_PKG_VERSION"));

    let serialized = serde_json::to_string(&document).unwrap();
    for secret in ["sk-alpha-secret", "sk-alpha-backup", "whsec-1"] {
        assert!(
            !serialized.contains(secret),
            "the masked document must not contain {secret}"
        );
    }

    let provider = &document.providers[0];
    assert!(provider.api_key_set);
    assert_eq!(provider.api_key, None);
    assert_eq!(provider.headers["Authorization"], "***");
    assert_eq!(provider.headers["X-Custom"], "keep-me");
    assert_eq!(provider.provider_keys.len(), 2);
    assert!(
        provider
            .provider_keys
            .iter()
            .all(|key| key.api_key.is_none())
    );
    assert!(document.webhooks[0].secret_set);
    assert_eq!(document.webhooks[0].secret, None);
    assert!(
        document.notes.iter().any(|note| note.contains("掩码")),
        "the document should explain that credentials are masked: {:?}",
        document.notes
    );
}

#[tokio::test]
async fn the_export_carries_everything_needed_to_rebuild_the_configuration() {
    let state = seeded_state().await;
    let document = export(&state, true).await;
    assert!(document.secrets_included);
    assert_eq!(document.providers.len(), 1);

    let provider = &document.providers[0];
    assert_eq!(provider.name, "alpha");
    assert_eq!(provider.provider_type, "openai");
    assert_eq!(provider.base_url, "https://alpha.example/v1");
    assert_eq!(provider.api_key.as_deref(), Some("sk-alpha-secret"));
    assert_eq!(provider.headers["Authorization"], "Bearer sk-alpha-secret");
    assert_eq!(provider.models, vec!["gpt-4o".to_string()]);
    assert_eq!(provider.disabled_models, vec!["gpt-4o-mini".to_string()]);
    assert_eq!(
        provider.model_auto_include_rules,
        vec!["gpt-*".to_string(), "re:^claude-".to_string()]
    );
    assert_eq!(provider.timeout_seconds, Some(30));
    assert_eq!(provider.configured_cooldown_seconds, Some(120));
    assert_eq!(provider.max_concurrency, Some(4));
    let mut key_names = provider
        .provider_keys
        .iter()
        .map(|key| key.name.clone())
        .collect::<Vec<_>>();
    key_names.sort();
    assert_eq!(key_names, vec!["Default".to_string(), "backup".to_string()]);

    assert_eq!(document.routes.len(), 1);
    assert_eq!(document.routes[0].targets[0].provider, "alpha");
    assert_eq!(document.routes[0].targets[0].upstream_model, "gpt-4o");
    assert_eq!(document.provider_pools.len(), 1);
    assert_eq!(document.provider_pools[0].allocations[0].api_key, "caller");
    assert_eq!(document.webhooks.len(), 1);
    assert_eq!(document.webhooks[0].secret.as_deref(), Some("whsec-1"));
    assert_eq!(
        document.settings.usage_retention_days,
        Some(30),
        "the retention setting must be part of the document"
    );
    assert_eq!(
        document.settings.guardrails.as_ref().unwrap().blocked_terms,
        vec!["bad-word".to_string()]
    );

    let key = &document.gateway_keys[0];
    assert_eq!(key.name, "caller");
    assert!(key.enabled);
    assert!(
        document.notes.iter().any(|note| note.contains("API Key")),
        "the document must say that gateway keys cannot be restored: {:?}",
        document.notes
    );
}
