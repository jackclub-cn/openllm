use super::*;

#[tokio::test]
async fn provider_quota_credential_uses_selected_or_first_enabled_key() {
    let state = provider_key_test_state().await;
    let (_, Json(created)) = create_provider(
        State(state.clone()),
        Json(provider_input(
            None,
            vec![
                provider_key_input(None, "Primary", Some("sk-one"), true),
                provider_key_input(None, "Backup", Some("sk-two"), true),
            ],
        )),
    )
    .await
    .unwrap();
    let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
        .bind(created.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();

    let selected = provider_quota_credential(&state, &provider, Some(created.api_keys[1].id))
        .await
        .unwrap();
    assert_eq!(selected.key_id, Some(created.api_keys[1].id));
    assert_eq!(selected.key_name, "Backup");
    assert_eq!(selected.secret, "sk-two");

    let default = provider_quota_credential(&state, &provider, None)
        .await
        .unwrap();
    assert_eq!(default.key_id, Some(created.api_keys[0].id));
    assert_eq!(default.key_name, "Primary");

    sqlx::query("UPDATE provider_api_keys SET enabled = 0 WHERE id = ?")
        .bind(created.api_keys[0].id)
        .execute(&state.pool)
        .await
        .unwrap();
    let fallback = provider_quota_credential(&state, &provider, None)
        .await
        .unwrap();
    assert_eq!(fallback.key_id, Some(created.api_keys[1].id));
    assert_eq!(fallback.key_name, "Backup");

    assert!(
        provider_quota_credential(&state, &provider, Some(999_999))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn quota_snapshots_persist_manual_overrides_and_restore_to_cache() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url) \
         VALUES (1, 'quota-provider', 'openai', 'https://opencode.ai/zen/go/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret) \
         VALUES (11, 1, 'Primary', 'sk-quota')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let view = ProviderQuotaView {
        kind: "opencode_go".to_string(),
        title: "OpenCode Go 额度".to_string(),
        plan_name: Some("Go".to_string()),
        key_id: Some(11),
        key_name: Some("Primary".to_string()),
        key_suffix: Some("quota".to_string()),
        source_url: None,
        items: vec![ProviderQuotaItem {
            key: "rolling".to_string(),
            label: "5 小时".to_string(),
            used: None,
            limit: None,
            remaining: None,
            unit: "%".to_string(),
            percent: Some(95.0),
            reset_at: None,
        }],
        model_quotas: vec![ProviderQuotaModel {
            model_name: "Model A".to_string(),
            model_id: Some("model-a".to_string()),
            monthly_credits: Some(80.0),
            promo_credits: Some(100.0),
            note: None,
        }],
        details: Vec::new(),
        prices: Vec::new(),
        snapshots: Vec::new(),
        fetched_at: Utc::now().to_rfc3339(),
    };

    let snapshots = crate::quota::store_provider_quota_view(&state, 1, Some(11), &view)
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 2);
    let window = snapshots
        .iter()
        .find(|snapshot| snapshot.model.is_empty())
        .unwrap();
    assert_eq!(window.window_key, "5h");
    assert_eq!(window.state, "low");
    assert_eq!(window.percent, Some(95.0));
    let model = snapshots
        .iter()
        .find(|snapshot| snapshot.model == "model-a")
        .unwrap();
    assert_eq!(model.state, "unknown");
    assert_eq!(model.limit, Some(100.0));

    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM provider_quota_snapshots WHERE provider_id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 2);

    let Json(listed) = list_provider_quota_snapshots(
        State(state.clone()),
        Path(1),
        Query(ProviderQuotaQuery { key_id: Some(11) }),
    )
    .await
    .unwrap();
    assert_eq!(listed.len(), 2);

    let Json(updated) = update_provider_quota_snapshots(
        State(state.clone()),
        Path(1),
        Json(ProviderQuotaSnapshotRequest {
            snapshots: vec![ProviderQuotaSnapshotInput {
                key_id: Some(11),
                model: None,
                window_key: "5h".to_string(),
                used: Some(20.0),
                limit: Some(100.0),
                remaining: Some(80.0),
                unit: "percent".to_string(),
                reset_at: None,
                valid_seconds: Some(3_600),
            }],
        }),
    )
    .await
    .unwrap();
    let window = updated
        .iter()
        .find(|snapshot| snapshot.model.is_empty())
        .unwrap();
    assert_eq!(window.source, "manual");
    assert_eq!(window.state, "healthy");
    assert_eq!(window.used, Some(20.0));

    let restarted = AppState::new(state.pool.clone(), None);
    crate::quota::restore(&restarted).await.unwrap();
    let cache = restarted.quota_snapshots.lock().await;
    assert!(cache.contains_key(&ProviderQuotaSnapshotKey {
        provider_id: 1,
        provider_key_id: Some(11),
        model: String::new(),
        window_key: "5h".to_string(),
        unit: "percent".to_string(),
        source: "manual".to_string(),
    }));
}

#[test]
fn provider_quota_refresh_interval_parses_and_clamps() {
    assert_eq!(parse_provider_quota_refresh_interval(Some("0")), None);
    assert_eq!(
        parse_provider_quota_refresh_interval(Some("15")),
        Some(std::time::Duration::from_secs(15 * 60))
    );
    assert_eq!(
        parse_provider_quota_refresh_interval(Some("invalid")),
        Some(std::time::Duration::from_secs(15 * 60))
    );
    assert_eq!(
        parse_provider_quota_refresh_interval(Some("999999999")),
        Some(std::time::Duration::from_secs(7 * 24 * 60 * 60))
    );
}

#[tokio::test]
async fn provider_quota_overview_groups_database_snapshots_without_upstream_calls() {
    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, enabled) VALUES \
         (1, 'OpenCode Primary', 'openai', 'https://opencode.ai/zen/go/v1', 1), \
         (2, 'Unsupported', 'openai', 'https://api.openai.com/v1', 1), \
         (3, 'OpenCode Backup', 'openai', 'https://opencode.ai/zen/go/v1', 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) \
         VALUES (11, 1, 'Primary', 'sk-quota', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    crate::quota::upsert_manual_snapshots(
        &state,
        1,
        &[ProviderQuotaSnapshotInput {
            key_id: Some(11),
            model: None,
            window_key: "5h".to_string(),
            used: Some(20.0),
            limit: Some(100.0),
            remaining: Some(80.0),
            unit: "percent".to_string(),
            reset_at: None,
            valid_seconds: Some(3_600),
        }],
    )
    .await
    .unwrap();

    let Json(overview) = provider_quota_overview(State(state.clone())).await.unwrap();
    assert_eq!(
        overview
            .iter()
            .map(|item| item.provider_id)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(overview[0].provider_name, "OpenCode Primary");
    assert!(overview[0].provider_enabled);
    assert_eq!(overview[0].snapshots.len(), 1);
    let snapshot = &overview[0].snapshots[0];
    assert_eq!(snapshot.key_id, Some(11));
    assert_eq!(snapshot.key_name.as_deref(), Some("Primary"));
    assert_eq!(snapshot.key_suffix.as_deref(), Some("uota"));
    assert_eq!(snapshot.state, "healthy");
    assert_eq!(snapshot.remaining, Some(80.0));
    assert!(!overview[1].provider_enabled);
    assert!(overview[1].snapshots.is_empty());
}
