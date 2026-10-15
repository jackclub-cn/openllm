use super::*;

use tempfile::tempdir;

const SEED_SQL: &str = r#"
INSERT INTO providers (id, name, provider_type, base_url, model_prefix, headers, api_key, enabled, timeout_seconds, cooldown_seconds, max_concurrency, queue_timeout_seconds)
VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1', '', '{"Authorization":"Bearer sk-alpha-secret","X-Custom":"keep-me"}', 'sk-alpha-secret', 1, 30, 120, 4, 5);
INSERT INTO provider_api_keys (provider_id, name, secret, enabled)
VALUES (1, 'Default', 'sk-alpha-secret', 1), (1, 'backup', 'sk-alpha-backup', 1);
INSERT INTO provider_models (provider_id, model_name, enabled)
VALUES (1, 'gpt-4o', 1), (1, 'gpt-4o-mini', 0);
INSERT INTO routes (id, name, model_pattern, strategy, enabled) VALUES (1, 'default', '*', 'priority', 1);
INSERT INTO route_targets (route_id, provider_id, upstream_model, weight, priority, enabled)
VALUES (1, 1, 'gpt-4o', 100, 0, 1);
INSERT INTO provider_pools (provider_id, name, enabled, window_seconds, request_limit, token_limit, saturation_threshold_pct)
VALUES (1, 'shared', 1, 60, 100, NULL, 50);
INSERT INTO provider_pool_allocations (pool_id, api_key_id, weight_pct, policy) VALUES (1, 1, 100, 'hard');
INSERT INTO webhooks (name, url, secret, event_types, headers, enabled)
VALUES ('hook', 'https://hook.example/x', 'whsec-1', '["request.completed"]', '{}', 1);
INSERT INTO settings (key, value)
VALUES ('guardrails', '{"blocked_terms":["bad-word"],"max_prompt_tokens":null}'), ('usage_retention_days', '30');
"#;

async fn exec(state: &AppState, sql: &str) {
    sqlx::raw_sql(sql).execute(&state.pool).await.unwrap();
}

async fn memory_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, None)
}

/// A gateway access key row, so pool allocations can be resolved by name.
async fn seed_gateway_key(state: &AppState, name: &str) {
    exec(
        state,
        &format!(
            "INSERT INTO api_keys (name, key_hash, key_prefix, key_suffix, enabled) \
             VALUES ('{name}', 'hash-{name}', 'sk-', 'abcd', 1)"
        ),
    )
    .await;
}

async fn seeded_state() -> AppState {
    let state = memory_state().await;
    seed_gateway_key(&state, "caller").await;
    exec(&state, SEED_SQL).await;
    state
}

/// A target database that already has the gateway key the document's pool
/// allocation refers to (gateway keys themselves are never imported).
async fn empty_state() -> AppState {
    let state = memory_state().await;
    seed_gateway_key(&state, "caller").await;
    state
}

async fn file_state(directory: &std::path::Path) -> (AppState, PathBuf) {
    let path = directory.join("openllm.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    (AppState::new(pool, None), path)
}

async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&state.pool).await.unwrap()
}

async fn export(state: &AppState, include_secrets: bool) -> ConfigDocument {
    export_config(
        State(state.clone()),
        Query(ExportConfigQuery { include_secrets }),
    )
    .await
    .unwrap()
    .0
}

async fn import(state: &AppState, document: ConfigDocument, apply: bool) -> ImportPlan {
    import_config(
        State(state.clone()),
        Json(ImportConfigRequest { document, apply }),
    )
    .await
    .unwrap()
    .0
}

fn item<'a>(plan: &'a ImportPlan, kind: &str, name: &str) -> &'a ImportItem {
    plan.items
        .iter()
        .find(|item| item.kind == kind && item.name == name)
        .unwrap_or_else(|| panic!("no {kind} item named {name} in {:#?}", plan.items))
}

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
    assert!(provider.provider_keys.iter().all(|key| key.api_key.is_none()));
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
        document
            .notes
            .iter()
            .any(|note| note.contains("API Key")),
        "the document must say that gateway keys cannot be restored: {:?}",
        document.notes
    );
}

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

    let headers: String =
        sqlx::query_scalar("SELECT headers FROM providers WHERE name = 'alpha'")
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
        vec![
            ("gpt-4o".to_string(), 1),
            ("gpt-4o-mini".to_string(), 0),
        ]
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

    let (url, secret) =
        sqlx::query_as::<_, (String, String)>("SELECT url, secret FROM webhooks")
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
    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, snapshot);
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
    assert_eq!(item(&plan, "provider_pool", "alpha / shared").action, "create");
    assert_eq!(item(&plan, "webhook", "hook").action, "create");
    assert_eq!(item(&plan, "gateway_key", "caller").action, "skip");

    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, 0);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM routes").await, 0);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM provider_pools").await, 0);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM webhooks").await, 0);
    assert_eq!(
        count(&target, "SELECT COUNT(*) FROM settings WHERE key = 'guardrails'").await,
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
    assert!(!plan.valid, "a document with skipped references is not clean");
    assert_eq!(plan.counts.error, 0, "{:#?}", plan.items);
    let route = item(&plan, "route", "default");
    assert_eq!(route.action, "skip");
    assert!(route.detail.contains("ghost"), "{}", route.detail);
    assert!(
        plan.problems.iter().any(|problem| problem.contains("nobody")),
        "{:#?}",
        plan.problems
    );

    // The provider, the pool (without the unresolvable allocation) and the
    // webhook were still created; only the broken route was dropped.
    assert_eq!(count(&target, "SELECT COUNT(*) FROM providers").await, 1);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM routes").await, 0);
    assert_eq!(count(&target, "SELECT COUNT(*) FROM provider_pools").await, 1);
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
    assert_eq!(audits, 0, "a rejected document must not be audited as applied");
}

// -------------------------------------------------------------------------
// Backups
// -------------------------------------------------------------------------

#[tokio::test]
async fn a_backup_is_created_listed_verified_and_still_restorable() {
    let directory = tempdir().unwrap();
    let (state, database) = file_state(directory.path()).await;
    seed_gateway_key(&state, "caller").await;
    exec(&state, SEED_SQL).await;

    let (status, Json(info)) = create_backup(State(state.clone())).await.unwrap();
    assert_eq!(status, StatusCode::CREATED);
    assert!(info.name.starts_with("openllm-manual-"), "{}", info.name);
    assert!(info.name.ends_with(".db"));
    assert!(info.size_bytes > 0);
    assert_eq!(info.kind, "manual");
    let backup = PathBuf::from(&info.path);
    assert!(backup.exists(), "{backup:?}");

    let listed = list_backups(State(state.clone())).await.unwrap().0;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, info.name);
    assert_eq!(listed[0].kind, "manual");

    let report = verify_backup(
        State(state.clone()),
        Json(VerifyBackupRequest {
            name: info.name.clone(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(report.ok, "{:#?}", report.problems);
    assert_eq!(report.integrity, "ok");
    assert!(report.size_bytes > 0);
    assert!(report.missing_tables.is_empty(), "{:#?}", report.missing_tables);
    assert!(report.applied_migrations > 0);
    assert!(report.schema_version > 0);
    assert_eq!(report.providers, 1);
    assert_eq!(report.routes, 1);
    assert_eq!(report.webhooks, 1);
    assert_eq!(report.provider_pools, 1);
    assert_eq!(report.file, info.path);

    // The backup is a self-contained database: opening it read-only shows the
    // same rows, and the original file still exists untouched.
    assert!(database.exists());
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        backup.to_string_lossy().replace('\\', "/")
    ))
    .await
    .unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM providers WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "alpha");
    pool.close().await;
}

#[tokio::test]
async fn backup_verification_rejects_traversal_and_unknown_names() {
    let directory = tempdir().unwrap();
    let (state, _database) = file_state(directory.path()).await;
    let (_, Json(_)) = create_backup(State(state.clone())).await.unwrap();

    for name in ["../openllm.db", "backups/openllm-manual-x.db", ".."] {
        let result = verify_backup(
            State(state.clone()),
            Json(VerifyBackupRequest {
                name: name.to_string(),
            }),
        )
        .await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "{name} must be rejected"
        );
    }

    let missing = verify_backup(
        State(state.clone()),
        Json(VerifyBackupRequest {
            name: "openllm-manual-19990101-000000.db".to_string(),
        }),
    )
    .await;
    assert!(matches!(missing, Err(AppError::NotFound(_))));

    // A file that is not a database is rejected, not reported as healthy.
    std::fs::write(directory.path().join("backups").join("junk.db"), b"not a database")
        .unwrap();
    let junk = verify_backup(
        State(state.clone()),
        Json(VerifyBackupRequest {
            name: "junk.db".to_string(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(!junk.ok);
    assert!(!junk.problems.is_empty());
}

#[tokio::test]
async fn database_operations_need_a_file_database() {
    let state = memory_state().await;
    let result = create_backup(State(state.clone())).await;
    assert!(
        matches!(result, Err(AppError::BadRequest(_))),
        "unexpected result: {result:?}"
    );
    assert!(matches!(
        migration_status(State(state.clone())).await,
        Err(AppError::BadRequest(_))
    ));
    assert!(matches!(
        verify_backup(
            State(state.clone()),
            Json(VerifyBackupRequest {
                name: "any.db".to_string()
            })
        )
        .await,
        Err(AppError::BadRequest(_))
    ));
}

#[tokio::test]
async fn old_backups_are_pruned_but_the_newest_are_kept() {
    let directory = tempdir().unwrap();
    let (state, database) = file_state(directory.path()).await;
    let backups = directory.path().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    for index in 0..8 {
        std::fs::write(
            backups.join(format!("openllm-manual-2020010{index}-000000.db")),
            b"old",
        )
        .unwrap();
    }
    assert!(database.exists());

    let (_, Json(_)) = create_backup(State(state.clone())).await.unwrap();
    let listed = list_backups(State(state.clone())).await.unwrap().0;
    let manual = listed
        .iter()
        .filter(|info| info.kind == "manual")
        .count();
    assert!(
        manual <= crate::startup::MAX_MANUAL_BACKUPS,
        "manual backups must be pruned: {manual}"
    );
    assert!(
        listed
            .iter()
            .any(|info| info.name.contains(&chrono::Utc::now().format("%Y").to_string())),
        "the newest backup must survive: {:#?}",
        listed
    );
}

// -------------------------------------------------------------------------
// Migration pre-check
// -------------------------------------------------------------------------

#[tokio::test]
async fn the_migration_precheck_reports_pending_migrations_without_touching_the_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("openllm.db");

    // A file that exists but was never migrated: every migration is pending.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    pool.close().await;
    let before = std::fs::read(&path).unwrap();

    let config = crate::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: directory.path().to_path_buf(),
        database_url: Some(format!("sqlite://{}", path.to_string_lossy().replace('\\', "/"))),
        admin_token: None,
    };
    let report = crate::startup::migration_report(&config).await.unwrap();
    assert!(!report.up_to_date);
    assert!(report.applied.is_empty());
    assert!(!report.pending.is_empty());
    assert!(!report.data_present);
    assert!(!report.backup_would_be_written, "an empty file needs no backup");
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    assert!(
        report.pending.iter().any(|entry| entry.version == 1),
        "the first migration must be listed"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the pre-check must not modify the database file"
    );

    // A missing file is reported the same way instead of being created.
    let missing = directory.path().join("nope.db");
    let config = crate::Config {
        database_url: Some(format!(
            "sqlite://{}",
            missing.to_string_lossy().replace('\\', "/")
        )),
        ..config
    };
    let report = crate::startup::migration_report(&config).await.unwrap();
    assert!(!report.up_to_date);
    assert!(!report.pending.is_empty());
    assert!(!missing.exists());

    // The in-memory state is a fully migrated database, so the API reports it
    // as up to date when pointed at a real file.
    let (state, database) = file_state(directory.path()).await;
    let report = migration_status(State(state.clone())).await.unwrap().0;
    assert!(report.up_to_date, "{:#?}", report.problems);
    assert!(report.pending.is_empty());
    assert!(!report.applied.is_empty());
    assert!(report.unknown_applied.is_empty());
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    assert!(database.exists());
    assert!(
        !report.data_present,
        "a migrated but empty database holds no user data"
    );
    assert!(!report.backup_would_be_written);

    // With rows in it, the same report says a backup would be written before
    // any pending migration runs.
    exec(
        &state,
        "INSERT INTO providers (name, provider_type, base_url, enabled) \
         VALUES ('alpha', 'openai', 'https://alpha.example/v1', 1)",
    )
    .await;
    let report = migration_status(State(state.clone())).await.unwrap().0;
    assert!(report.data_present);
    assert!(report.up_to_date);
    assert!(
        !report.backup_would_be_written,
        "an up-to-date database needs no pre-migration backup"
    );
    drop(state);
}

// -------------------------------------------------------------------------
// Upgrade check
// -------------------------------------------------------------------------

#[tokio::test]
async fn the_upgrade_check_compares_against_a_given_version() {
    let state = memory_state().await;

    let newer = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some("v0.4.0".to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(newer.current_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(newer.latest_version.as_deref(), Some("0.4.0"));
    assert!(newer.update_available);
    assert_eq!(newer.status, "update_available");
    assert_eq!(newer.source, "query");
    assert!(!newer.message.is_empty());

    // A tag that cannot be parsed must not be reported as "up to date".
    let garbage = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some("latest-release".to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(garbage.status, "unknown");
    assert!(!garbage.update_available);
    assert!(
        garbage.message.contains("latest-release"),
        "the message should say what could not be parsed: {}",
        garbage.message
    );

    let same = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(!same.update_available);
    assert_eq!(same.status, "up_to_date");

    let older = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some("0.1.0".to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(!older.update_available);
    assert_eq!(older.status, "ahead");

    if std::env::var("OPENLLM_UPGRADE_FEED_URL").is_err() {
        let unknown = upgrade_check(State(state.clone()), Query(UpgradeCheckQuery { latest: None }))
            .await
            .unwrap()
            .0;
        assert_eq!(unknown.status, "unknown");
        assert_eq!(unknown.source, "none");
        assert!(unknown.latest_version.is_none());
        assert!(!unknown.message.is_empty());
    }
}

#[test]
fn version_comparison_is_numeric_and_tolerates_a_v_prefix() {
    use std::cmp::Ordering;

    assert_eq!(compare_versions("0.3.14", "0.4.0"), Ordering::Less);
    assert_eq!(compare_versions("v0.3.14", "0.3.14"), Ordering::Equal);
    assert_eq!(compare_versions("0.3.14", "v0.3.13"), Ordering::Greater);
    assert_eq!(compare_versions("0.10.0", "0.9.9"), Ordering::Greater);
    assert_eq!(compare_versions("0.3.14", "0.3.14.1"), Ordering::Less);
    assert_eq!(compare_versions("0.4", "0.4.0"), Ordering::Equal);
    // A version the comparison cannot read keeps the current version: the
    // upgrade check turns that into "unknown" instead of "up to date".
    assert_eq!(compare_versions("0.3.14", "not-a-version"), Ordering::Equal);
    assert!(!is_numeric_version("not-a-version"));
    assert!(!is_numeric_version("1.0.0-rc1"));
    assert!(!is_numeric_version(""));
    assert!(is_numeric_version("v1.2.3"));
    assert!(is_numeric_version(" 2 "));
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
    let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_api_keys WHERE provider_id = 1")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(keys, 2, "no duplicate key was created");
}
