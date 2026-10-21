use super::*;

use tempfile::tempdir;
#[path = "lifecycle/release_feed.rs"]
mod release_feed;

const SEED_SQL: &str = r#"
INSERT INTO providers (id, name, provider_type, base_url, model_prefix, model_auto_include_rules, headers, api_key, enabled, timeout_seconds, cooldown_seconds, max_concurrency, queue_timeout_seconds)
VALUES (1, 'alpha', 'openai', 'https://alpha.example/v1', '', '["gpt-*","re:^claude-"]', '{"Authorization":"Bearer sk-alpha-secret","X-Custom":"keep-me"}', 'sk-alpha-secret', 1, 30, 120, 4, 5);
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
    sqlx::query_scalar(sql)
        .fetch_one(&state.pool)
        .await
        .unwrap()
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
#[path = "lifecycle/export.rs"]
mod export;

#[path = "lifecycle/import.rs"]
mod import;

#[path = "lifecycle/backups.rs"]
mod backups;

#[path = "lifecycle/migrations.rs"]
mod migrations;

#[path = "lifecycle/upgrade.rs"]
mod upgrade;
