use super::*;

// -------------------------------------------------------------------------
// Import
// -------------------------------------------------------------------------

#[tokio::test]
async fn a_config_document_round_trips_into_an_empty_database() {
    let source = seeded_state().await;
    let document = export(&source, true).await;
    let target = empty_state().await;

    let plan = import(&target, document, true).await;
    assert!(plan.applied, "{:#?}", plan.items);
    assert!(plan.valid, "{:#?} / {:#?}", plan.problems, plan.items);
    assert_eq!(plan.counts.error, 0, "{:#?}", plan.items);

    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, 1);
    let (base_url, timeout, cooldown, concurrency) = sqlx::query_as::<_, (String, i64, i64, i64)>(
        "SELECT base_url, timeout_seconds, cooldown_seconds, max_concurrency FROM providers WHERE name = 'alpha'",
    )
    .fetch_one(&target.pool)
    .await
    .unwrap();
    assert_eq!(base_url, "https://alpha.example/v1");
    assert_eq!((timeout, cooldown, concurrency), (30, 120, 4));
    let rules: String =
        sqlx::query_scalar("SELECT model_auto_include_rules FROM providers WHERE name = 'alpha'")
            .fetch_one(&target.pool)
            .await
            .unwrap();
    assert_eq!(rules, r#"["gpt-*","re:^claude-"]"#);

    let headers: String = sqlx::query_scalar("SELECT headers FROM providers WHERE name = 'alpha'")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    let headers: Value = serde_json::from_str(&headers).unwrap();
    assert_eq!(headers["Authorization"], "Bearer sk-alpha-secret");
    assert_eq!(headers["X-Custom"], "keep-me");

    // Both named keys survive with their own secret, and the legacy column is
    // still populated for older readers.
    let keys = sqlx::query_as::<_, (String, String)>(
        "SELECT name, secret FROM provider_api_keys ORDER BY name",
    )
    .fetch_all(&target.pool)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec![
            ("Default".to_string(), "sk-alpha-secret".to_string()),
            ("backup".to_string(), "sk-alpha-backup".to_string()),
        ]
    );
    let legacy: String = sqlx::query_scalar("SELECT api_key FROM providers WHERE name = 'alpha'")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    assert_eq!(legacy, "sk-alpha-secret");

    // A disabled model stays disabled instead of silently being re-enabled.
    let models = sqlx::query_as::<_, (String, i64)>(
        "SELECT model_name, enabled FROM provider_models ORDER BY model_name",
    )
    .fetch_all(&target.pool)
    .await
    .unwrap();
    assert_eq!(
        models,
        vec![("gpt-4o".to_string(), 1), ("gpt-4o-mini".to_string(), 0),]
    );

    // The route target points at the freshly created provider id.
    let provider_id: i64 = sqlx::query_scalar("SELECT id FROM providers WHERE name = 'alpha'")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    let target_provider: i64 =
        sqlx::query_scalar("SELECT provider_id FROM route_targets ORDER BY id LIMIT 1")
            .fetch_one(&target.pool)
            .await
            .unwrap();
    assert_eq!(target_provider, provider_id);

    // The pool and its per-key allocation are restored, resolving both the
    // provider and the gateway key by name.
    let (window, limit, threshold) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT window_seconds, request_limit, saturation_threshold_pct FROM provider_pools",
    )
    .fetch_one(&target.pool)
    .await
    .unwrap();
    assert_eq!((window, limit, threshold), (60, 100, 50));
    let caller_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE name = 'caller'")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    let allocation = sqlx::query_as::<_, (i64, i64, String)>(
        "SELECT api_key_id, weight_pct, policy FROM provider_pool_allocations",
    )
    .fetch_one(&target.pool)
    .await
    .unwrap();
    assert_eq!(allocation, (caller_id, 100, "hard".to_string()));

    let (url, secret) = sqlx::query_as::<_, (String, String)>("SELECT url, secret FROM webhooks")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    assert_eq!(url, "https://hook.example/x");
    assert_eq!(secret, "whsec-1");

    let guardrails: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'guardrails'")
            .fetch_one(&target.pool)
            .await
            .unwrap();
    let guardrails: GuardrailSettings = serde_json::from_str(&guardrails).unwrap();
    assert_eq!(guardrails.blocked_terms, vec!["bad-word".to_string()]);
    let retention: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'usage_retention_days'")
            .fetch_one(&target.pool)
            .await
            .unwrap();
    assert_eq!(retention, "30");
}

#[tokio::test]
async fn reimporting_the_same_document_changes_nothing() {
    let source = seeded_state().await;
    let document = export(&source, true).await;
    let target = empty_state().await;
    let first = import(&target, document, true).await;
    assert!(first.applied, "{:#?}", first.items);
    let snapshot = count(&target, "SELECT COUNT(*) FROM providers").await;

    let second = import(&target, export(&target, true).await, false).await;
    assert!(second.dry_run);
    assert!(second.valid, "{:#?}", second.problems);
    assert_eq!(second.counts.create, 0, "{:#?}", second.items);
    assert_eq!(second.counts.update, 0, "{:#?}", second.items);
    assert_eq!(second.counts.error, 0, "{:#?}", second.items);
    assert!(
        second.counts.unchanged >= 6,
        "every entity should report as unchanged: {:#?}",
        second.items
    );
    assert_eq!(
        count(&target, "SELECT COUNT(*) FROM providers").await,
        snapshot
    );
}

/// A chat channel is configuration, not a credential: its kind and extra body
/// fields have to survive a masked export/import round trip, or an alert
/// channel could not be reviewed and re-created from a document.
#[tokio::test]
async fn a_chat_channel_round_trips_through_a_masked_document() {
    let source = seeded_state().await;
    exec(
        &source,
        "UPDATE webhooks SET kind = 'slack', body_fields = '{\"channel\":\"#ops\"}' \
         WHERE name = 'hook'",
    )
    .await;

    // A masked export still carries the channel configuration: only the
    // credentials are held back.
    let masked = export(&source, false).await;
    assert_eq!(masked.webhooks[0].kind, "slack");
    assert_eq!(masked.webhooks[0].body_fields["channel"], "#ops");

    let target = empty_state().await;
    let plan = import(&target, export(&source, true).await, true).await;
    assert!(plan.applied, "{:#?}", plan.items);
    assert_eq!(plan.counts.error, 0, "{:#?}", plan.items);

    let (kind, body_fields) = sqlx::query_as::<_, (String, String)>(
        "SELECT kind, body_fields FROM webhooks WHERE name = 'hook'",
    )
    .fetch_one(&target.pool)
    .await
    .unwrap();
    assert_eq!(kind, "slack");
    let body: Value = serde_json::from_str(&body_fields).unwrap();
    assert_eq!(body["channel"], "#ops");

    // Re-importing must see the row as unchanged, which is what proves the
    // comparison covers the two new columns.
    let again = import(&target, export(&target, true).await, false).await;
    assert!(again.valid, "{:#?}", again.problems);
    assert_eq!(
        item(&again, "webhook", "hook").action,
        "unchanged",
        "{:#?}",
        again.items
    );
}

#[tokio::test]
async fn a_masked_document_never_overwrites_an_existing_credential() {
    let source = seeded_state().await;
    let masked = export(&source, false).await;
    let target = seeded_state().await;
    // The target holds its own, different credentials.
    exec(
        &target,
        "UPDATE providers SET api_key = 'sk-target', headers = '{\"Authorization\":\"Bearer sk-target\"}';
         UPDATE provider_api_keys SET secret = 'sk-target' WHERE name = 'Default';",
    )
    .await;

    let plan = import(&target, masked, true).await;
    assert_eq!(plan.counts.error, 0, "{:#?}", plan.items);
    assert!(!plan.secrets_included);

    let secret: String = sqlx::query_scalar("SELECT api_key FROM providers WHERE name = 'alpha'")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    assert_eq!(
        secret, "sk-target",
        "a masked field must never be applied over a real credential"
    );
    let headers: String = sqlx::query_scalar("SELECT headers FROM providers WHERE name = 'alpha'")
        .fetch_one(&target.pool)
        .await
        .unwrap();
    assert!(headers.contains("sk-target"));
    let key_secret: String =
        sqlx::query_scalar("SELECT secret FROM provider_api_keys WHERE name = 'Default'")
            .fetch_one(&target.pool)
            .await
            .unwrap();
    assert_eq!(key_secret, "sk-target");
}

#[tokio::test]
async fn a_dry_run_reports_a_plan_without_writing_anything() {
    let source = seeded_state().await;
    let document = export(&source, true).await;
    let target = empty_state().await;

    let plan = import(&target, document, false).await;
    assert!(plan.dry_run);
    assert!(!plan.applied);
    assert!(plan.valid, "{:#?}", plan.problems);
    assert!(
        plan.counts.create >= 4,
        "provider, route, pool and webhook should be created: {:#?}",
        plan.items
    );
    assert!(
        plan.counts.update >= 2,
        "the two settings should be reported as updates: {:#?}",
        plan.items
    );
    assert_eq!(item(&plan, "provider", "alpha").action, "create");
    assert_eq!(item(&plan, "route", "default").action, "create");
    assert_eq!(
        item(&plan, "provider_pool", "alpha / shared").action,
        "create"
    );
    assert_eq!(item(&plan, "webhook", "hook").action, "create");
    assert_eq!(item(&plan, "gateway_key", "caller").action, "skip");

    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, 0);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM routes").await, 0);
    assert_eq!(
        count(&target, "SELECT COUNT(*) FROM provider_pools").await,
        0
    );
    assert_eq!(count(&target, "SELECT COUNT(*) FROM webhooks").await, 0);
    assert_eq!(
        count(
            &target,
            "SELECT COUNT(*) FROM settings WHERE key = 'guardrails'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn an_import_skips_references_it_cannot_resolve() {
    let source = seeded_state().await;
    let mut document = export(&source, true).await;
    document.routes[0].targets[0].provider = "ghost".to_string();
    document.provider_pools[0].allocations[0].api_key = "nobody".to_string();

    let target = empty_state().await;
    let plan = import(&target, document, true).await;

    assert!(plan.applied);
    assert!(
        !plan.valid,
        "a document with skipped references is not clean"
    );
    assert_eq!(plan.counts.error, 0, "{:#?}", plan.items);
    let route = item(&plan, "route", "default");
    assert_eq!(route.action, "skip");
    assert!(route.detail.contains("ghost"), "{}", route.detail);
    assert!(
        plan.problems
            .iter()
            .any(|problem| problem.contains("nobody")),
        "{:#?}",
        plan.problems
    );

    // The provider, the pool (without the unresolvable allocation) and the
    // webhook were still created; only the broken route was dropped.
    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, 1);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM routes").await, 0);
    assert_eq!(
        count(&target, "SELECT COUNT(*) FROM provider_pools").await,
        1
    );
    assert_eq!(
        count(&target, "SELECT COUNT(*) FROM provider_pool_allocations").await,
        0
    );
    assert_eq!(count(&target, "SELECT COUNT(*) FROM webhooks").await, 1);
}

#[tokio::test]
async fn a_newer_document_version_is_rejected_before_any_write() {
    let source = seeded_state().await;
    let mut document = export(&source, true).await;
    document.openllm_config_version = 99;

    let target = empty_state().await;
    let plan = import(&target, document, true).await;
    assert!(!plan.applied);
    assert!(!plan.valid);
    assert_eq!(plan.document_version, 99);
    assert!(
        plan.problems.iter().any(|problem| problem.contains("99")),
        "{:#?}",
        plan.problems
    );
    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, 0);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM routes").await, 0);

    // An import is audited, and the audit row must not contain a secret.
    let audits = count(
        &target,
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'import' AND entity = 'config'",
    )
    .await;
    assert_eq!(
        audits, 0,
        "a rejected document must not be audited as applied"
    );
}

/// Provider key names are not unique inside a provider (only the secret is), so
/// the export must not collapse the rows into a name-keyed map: doing that
/// dropped the secret of the second key with the same name, and the resulting
/// document could not even be re-imported.
#[tokio::test]
async fn a_second_key_with_the_same_name_survives_the_round_trip() {
    let state = seeded_state().await;
    exec(
        &state,
        "INSERT INTO provider_api_keys (provider_id, name, secret, enabled) \
         VALUES (1, 'Default', 'sk-alpha-second', 1)",
    )
    .await;

    let document = export(&state, true).await;
    let keys = &document.providers[0].provider_keys;
    let defaults: Vec<&str> = keys
        .iter()
        .filter(|key| key.name == "Default")
        .filter_map(|key| key.api_key.as_deref())
        .collect();
    assert_eq!(
        defaults.len(),
        2,
        "both same-named keys must be exported: {keys:?}"
    );
    assert!(defaults.contains(&"sk-alpha-secret") && defaults.contains(&"sk-alpha-second"));

    let target = empty_state().await;
    let plan = import(&target, document, true).await;
    assert!(plan.applied, "{:?}", plan.problems);
    assert_eq!(plan.counts.error, 0);

    let secrets: Vec<String> = sqlx::query_scalar(
        "SELECT secret FROM provider_api_keys WHERE provider_id = 1 ORDER BY secret",
    )
    .fetch_all(&target.pool)
    .await
    .unwrap();
    assert_eq!(
        secrets,
        vec![
            "sk-alpha-backup".to_string(),
            "sk-alpha-second".to_string(),
            "sk-alpha-secret".to_string()
        ]
    );
}

/// Importing a document that carries secrets must keep the identity of a key
/// that already exists: `replace_provider_api_keys` matches on the id, so a
/// missing id deletes and recreates the row, which resets the key's lifetime
/// statistics and detaches the usage history recorded against it.
#[tokio::test]
async fn importing_secrets_keeps_the_existing_key_rows() {
    let state = seeded_state().await;
    // A marker that only survives an UPDATE, not a delete-and-insert.
    exec(
        &state,
        "UPDATE provider_api_keys SET last_error = 'marker' WHERE secret = 'sk-alpha-secret'",
    )
    .await;
    let before: (i64, Option<String>) = sqlx::query_as(
        "SELECT id, created_at FROM provider_api_keys WHERE secret = 'sk-alpha-secret'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();

    let mut document = export(&state, true).await;
    // Force a provider update so the key payload is actually sent.
    document.providers[0].timeout_seconds = Some(45);

    let plan = import(&state, document, true).await;
    assert!(plan.applied, "{:?}", plan.problems);
    assert!(
        plan.notes.iter().any(|note| note.contains("密钥值匹配")),
        "the plan must explain how keys were matched: {:?}",
        plan.notes
    );

    let after: (i64, Option<String>) = sqlx::query_as(
        "SELECT id, created_at FROM provider_api_keys WHERE secret = 'sk-alpha-secret'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(after.0, before.0, "the key row is updated, not recreated");
    assert_eq!(after.1, before.1, "its creation time is preserved");
    let marker: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM provider_api_keys WHERE id = ?")
            .bind(after.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        marker.as_deref(),
        Some("marker"),
        "columns the import does not touch must survive"
    );
    let keys: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM provider_api_keys WHERE provider_id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(keys, 2, "no duplicate key was created");
}
