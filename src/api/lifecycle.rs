//! Lifecycle APIs: configuration export/import, database backups, the schema
//! pre-check and the upgrade check.
//!
//! Everything in this module is admin-only and is meant to make a gateway
//! reproducible: a configuration document you can keep in git, a consistent
//! backup you can restore into an empty data directory, a report of the
//! migrations a start would run, and a version comparison.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use super::*;

// -------------------------------------------------------------------------
// Configuration export
// -------------------------------------------------------------------------

pub async fn export_config(
    State(state): State<AppState>,
    Query(query): Query<ExportConfigQuery>,
) -> AppResult<Json<ConfigDocument>> {
    Ok(Json(
        build_config_document(&state, query.include_secrets).await?,
    ))
}

/// Build the portable configuration document.
///
/// `include_secrets = false` masks every credential-shaped value instead of
/// omitting the field, so the file still documents *that* a credential exists
/// without being a secret store itself. Masked values are never applied by the
/// importer, so a masked round-trip cannot overwrite a real credential.
pub(crate) async fn build_config_document(
    state: &AppState,
    include_secrets: bool,
) -> AppResult<ConfigDocument> {
    let mut notes = vec![
        "网关 API Key 只保存哈希，导出文件仅包含名称与限额，导入时按“跳过”处理。".to_string(),
        "用量、审计与 Webhook 投递历史属于运行时数据，不在导出范围内。".to_string(),
        "并发、内存、超时等上限来自环境变量，不在导出范围内。".to_string(),
    ];
    if include_secrets {
        notes.push(
            "该文件包含明文上游凭据与 Webhook 签名，请按密钥管理并限制访问权限。".to_string(),
        );
    } else {
        notes.push(
            "上游凭据、Webhook 签名与凭据类请求头已掩码为 ***；导入时不会被覆盖。".to_string(),
        );
    }

    let settings = ConfigSettings {
        guardrails: stored_setting(state, crate::state::SETTING_GUARDRAILS, &mut notes).await?,
        inspector: stored_setting(state, crate::state::SETTING_INSPECTOR, &mut notes).await?,
        resilience: stored_setting(state, crate::state::SETTING_RESILIENCE, &mut notes).await?,
        usage_retention_days: stored_usage_retention_days(state).await?,
    };

    // Disabled models are not part of a `ProviderView`, so they are read
    // separately: without them a round-trip would silently re-enable models an
    // operator had turned off.
    let mut disabled_models = HashMap::<i64, Vec<String>>::new();
    for (provider_id, model_name) in sqlx::query_as::<_, (i64, String)>(
        "SELECT provider_id, model_name FROM provider_models WHERE enabled = 0 \
         ORDER BY provider_id, model_name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?
    {
        disabled_models
            .entry(provider_id)
            .or_default()
            .push(model_name);
    }

    let mut provider_keys = HashMap::<i64, Vec<ConfigProviderKey>>::new();
    // Key names are not unique inside a provider (only the secret is), so the
    // document is built from one ordered row per key: collapsing the rows into
    // a name-keyed map would drop the secret of a second key with the same
    // name.
    for (
        provider_id,
        name,
        enabled,
        secret,
        max_concurrency,
        requests_per_minute,
        queue_limit,
        queue_wait_ms,
    ) in sqlx::query_as::<
        _,
        (
            i64,
            String,
            i64,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ),
    >(
        "SELECT provider_id, name, enabled, secret, max_concurrency, \
                requests_per_minute, queue_limit, queue_wait_ms \
         FROM provider_api_keys ORDER BY provider_id, id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        provider_keys
            .entry(provider_id)
            .or_default()
            .push(ConfigProviderKey {
                name,
                enabled: enabled != 0,
                max_concurrency,
                requests_per_minute,
                queue_limit,
                queue_wait_ms,
                api_key: include_secrets.then_some(secret),
            });
    }

    let mut legacy_keys = HashMap::<i64, String>::new();
    if include_secrets {
        for (id, api_key) in sqlx::query_as::<_, (i64, String)>(
            "SELECT id, api_key FROM providers WHERE COALESCE(api_key, '') <> ''",
        )
        .fetch_all(&state.pool)
        .await?
        {
            legacy_keys.insert(id, api_key);
        }
    }

    let provider_rows = sqlx::query_as::<_, Provider>("SELECT * FROM providers ORDER BY id")
        .fetch_all(&state.pool)
        .await?;
    let mut views = provider_rows
        .into_iter()
        .map(ProviderView::from)
        .collect::<Vec<_>>();
    hydrate_provider_views(state, &mut views).await?;

    let providers = views
        .into_iter()
        .map(|view| ConfigProvider {
            name: view.name.clone(),
            provider_type: view.provider_type.as_str().to_string(),
            base_url: view.base_url.clone(),
            model_prefix: view.model_prefix.clone(),
            enabled: view.enabled,
            // Deliberately false: the document already carries the full model
            // list, so an import must not depend on reaching the provider.
            auto_sync_models: false,
            models: view.models.clone(),
            disabled_models: disabled_models.remove(&view.id).unwrap_or_default(),
            model_auto_include_rules: view.model_auto_include_rules.clone(),
            health_check_interval_minutes: view.health_check_interval_minutes,
            health_check_model: view.health_check_model.clone(),
            models_sync_interval_minutes: view.models_sync_interval_minutes,
            timeout_seconds: view.timeout_seconds,
            configured_cooldown_seconds: view.configured_cooldown_seconds,
            max_concurrency: view.max_concurrency,
            queue_timeout_seconds: view.queue_timeout_seconds,
            headers: if include_secrets {
                view.headers.clone()
            } else {
                redact_headers(&view.headers)
            },
            api_key_set: view.api_key_set,
            api_key: legacy_keys.remove(&view.id),
            provider_keys: provider_keys.remove(&view.id).unwrap_or_default(),
        })
        .collect();

    // Provider and gateway-key names are exported instead of ids so the file
    // can be imported into a database where the ids differ.
    let mut provider_names = HashMap::<i64, String>::new();
    for (id, name) in
        sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM providers ORDER BY id")
            .fetch_all(&state.pool)
            .await?
    {
        provider_names.insert(id, name);
    }
    let mut api_key_names = HashMap::<i64, String>::new();
    for (id, name) in
        sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM api_keys ORDER BY id")
            .fetch_all(&state.pool)
            .await?
    {
        api_key_names.insert(id, name);
    }

    let mut route_targets = HashMap::<i64, Vec<ConfigRouteTarget>>::new();
    for (route_id, provider_id, upstream_model, weight, priority, enabled) in
        sqlx::query_as::<_, (i64, i64, String, i64, i64, i64)>(
            "SELECT route_id, provider_id, upstream_model, weight, priority, enabled \
             FROM route_targets ORDER BY route_id, priority, id",
        )
        .fetch_all(&state.pool)
        .await?
    {
        route_targets
            .entry(route_id)
            .or_default()
            .push(ConfigRouteTarget {
                provider: provider_names
                    .get(&provider_id)
                    .cloned()
                    .unwrap_or_else(|| format!("provider #{provider_id}")),
                upstream_model,
                weight,
                priority,
                enabled: enabled != 0,
            });
    }

    let routes = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            i64,
            i64,
            i64,
            i64,
        ),
    >(
        "SELECT id, name, model_pattern, \
                CASE WHEN strategy_ext <> '' THEN strategy_ext ELSE strategy END, enabled, \
                context_override, output_override, \
                all_targets_busy_wait_seconds, retry_jitter_ms, \
                max_waiting_requests, keepalive_during_wait \
         FROM routes ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(
        |(
            id,
            name,
            model_pattern,
            strategy,
            enabled,
            context_override,
            output_override,
            all_targets_busy_wait_seconds,
            retry_jitter_ms,
            max_waiting_requests,
            keepalive_during_wait,
        )| {
            ConfigRoute {
                name,
                model_pattern,
                strategy,
                enabled: enabled != 0,
                context_override,
                output_override,
                all_targets_busy_wait_seconds,
                retry_jitter_ms,
                max_waiting_requests,
                keepalive_during_wait: keepalive_during_wait != 0,
                targets: route_targets.remove(&id).unwrap_or_default(),
            }
        },
    )
    .collect();

    let mut pool_allocations = HashMap::<i64, Vec<ConfigPoolAllocation>>::new();
    for (pool_id, api_key_id, weight_pct, policy) in sqlx::query_as::<_, (i64, i64, i64, String)>(
        "SELECT pool_id, api_key_id, weight_pct, policy \
         FROM provider_pool_allocations ORDER BY pool_id, id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        pool_allocations
            .entry(pool_id)
            .or_default()
            .push(ConfigPoolAllocation {
                api_key: api_key_names
                    .get(&api_key_id)
                    .cloned()
                    .unwrap_or_else(|| format!("api key #{api_key_id}")),
                weight_pct,
                policy,
            });
    }

    let provider_pools =
        sqlx::query_as::<_, (i64, i64, String, i64, i64, Option<i64>, Option<i64>, i64)>(
            "SELECT id, provider_id, name, enabled, window_seconds, request_limit, token_limit, \
                saturation_threshold_pct \
         FROM provider_pools ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .map(
            |(
                id,
                provider_id,
                name,
                enabled,
                window_seconds,
                request_limit,
                token_limit,
                threshold,
            )| {
                ConfigPool {
                    provider: provider_names
                        .get(&provider_id)
                        .cloned()
                        .unwrap_or_else(|| format!("provider #{provider_id}")),
                    name,
                    enabled: enabled != 0,
                    window_seconds,
                    request_limit,
                    token_limit,
                    saturation_threshold_pct: threshold,
                    allocations: pool_allocations.remove(&id).unwrap_or_default(),
                }
            },
        )
        .collect();

    let mut webhook_secrets = HashMap::<i64, String>::new();
    let mut webhook_headers = HashMap::<i64, String>::new();
    for (id, secret, headers) in sqlx::query_as::<_, (i64, Option<String>, String)>(
        "SELECT id, secret, headers FROM webhooks ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?
    {
        if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
            webhook_secrets.insert(id, secret);
        }
        webhook_headers.insert(id, headers);
    }

    let webhooks = sqlx::query_as::<_, (i64, String, String, String, String, String, i64)>(
        "SELECT id, name, url, event_types, kind, body_fields, enabled \
         FROM webhooks ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(id, name, url, event_types, kind, body_fields, enabled)| {
        let headers = webhook_headers.get(&id).cloned().unwrap_or_default();
        let headers = parse_json_or_empty(&headers);
        ConfigWebhook {
            name,
            url,
            enabled: enabled != 0,
            kind,
            // Destination parameters such as a Telegram chat id are
            // configuration, not credentials, so they travel with the
            // document; the signing secret and credential-bearing headers
            // keep the existing masking rules.
            body_fields: parse_json_or_empty(&body_fields),
            event_types: parse_string_list(&event_types),
            headers: if include_secrets {
                headers.clone()
            } else {
                redact_headers(&headers)
            },
            secret_set: webhook_secrets.contains_key(&id),
            // Masked by omission: a `***` placeholder would be applied by an
            // import if the operator ticked "apply", replacing the signature.
            secret: if include_secrets {
                webhook_secrets.get(&id).cloned()
            } else {
                None
            },
        }
    })
    .collect();

    let gateway_keys = sqlx::query_as::<
        _,
        (
            String,
            i64,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ),
    >(
        "SELECT name, enabled, allowed_models, daily_token_limit, daily_cost_limit_micros, \
                requests_per_minute, max_concurrency, expires_at \
         FROM api_keys ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(
        |(
            name,
            enabled,
            allowed_models,
            daily_token_limit,
            daily_cost_limit_micros,
            requests_per_minute,
            max_concurrency,
            expires_at,
        )| ConfigGatewayKey {
            name,
            enabled: enabled != 0,
            allowed_models: parse_string_list(allowed_models.as_deref().unwrap_or_default()),
            daily_token_limit,
            daily_cost_limit_micros,
            requests_per_minute,
            max_concurrency,
            expires_at,
        },
    )
    .collect();

    Ok(ConfigDocument {
        openllm_config_version: crate::models::CONFIG_DOCUMENT_VERSION,
        exported_at: Utc::now().to_rfc3339(),
        gateway_version: env!("CARGO_PKG_VERSION").to_string(),
        secrets_included: include_secrets,
        notes,
        settings,
        providers,
        routes,
        provider_pools,
        webhooks,
        gateway_keys,
    })
}

/// Read a settings row as JSON, keeping the difference between "absent" (the
/// built-in default applies) and "explicitly set to the default".
async fn stored_setting<T: serde::de::DeserializeOwned>(
    state: &AppState,
    key: &str,
    notes: &mut Vec<String>,
) -> AppResult<Option<T>> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(&state.pool)
        .await?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    match serde_json::from_str::<T>(&raw) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            notes.push(format!("设置 {key} 无法解析，已跳过：{error}"));
            Ok(None)
        }
    }
}

async fn stored_usage_retention_days(state: &AppState) -> AppResult<Option<i64>> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(super::SETTING_USAGE_RETENTION_DAYS)
        .fetch_optional(&state.pool)
        .await?;
    Ok(raw.and_then(|raw| raw.trim().parse::<i64>().ok()))
}

fn parse_json_or_empty(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

fn parse_string_list(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

// -------------------------------------------------------------------------
// Configuration import
// -------------------------------------------------------------------------

pub async fn import_config(
    State(state): State<AppState>,
    Json(request): Json<ImportConfigRequest>,
) -> AppResult<Json<ImportPlan>> {
    let document = request.document;
    let mut run = ImportRun::new(&state, request.apply, document.secrets_included);
    run.load_lookups().await?;

    if document.openllm_config_version > crate::models::CONFIG_DOCUMENT_VERSION {
        run.blocked = true;
        run.problems.push(format!(
            "配置文档版本 {} 高于本程序支持的 {}，请先升级程序",
            document.openllm_config_version,
            crate::models::CONFIG_DOCUMENT_VERSION
        ));
    }
    if document.openllm_config_version < 1 {
        run.blocked = true;
        run.problems.push(format!(
            "配置文档版本 {} 无效",
            document.openllm_config_version
        ));
    }

    if !run.blocked {
        run.settings(document.settings).await;
        run.providers(&document.providers).await;
        run.routes(&document.routes).await;
        run.pools(&document.provider_pools).await;
        run.webhooks(&document.webhooks).await;
    }
    for key in &document.gateway_keys {
        run.record(
            "gateway_key",
            &key.name,
            ACTION_SKIP,
            "网关 API Key 只保存哈希，无法导入；请在“API 密钥”页重新创建".to_string(),
        );
    }

    let plan = run.finish(document.openllm_config_version);
    if plan.applied {
        let has_secrets = plan.secrets_included && (plan.counts.create + plan.counts.update) > 0;
        record_audit(
            &state,
            "import",
            "config",
            None,
            if has_secrets {
                "导入了配置文档（含明文上游凭据）"
            } else {
                "导入了配置文档"
            },
            Some(json!({
                "create": plan.counts.create,
                "update": plan.counts.update,
                "unchanged": plan.counts.unchanged,
                "skip": plan.counts.skip,
                "error": plan.counts.error,
                "secrets_included": plan.secrets_included,
            })),
        )
        .await;
    }
    Ok(Json(plan))
}

const ACTION_CREATE: &str = "create";
const ACTION_UPDATE: &str = "update";
const ACTION_UNCHANGED: &str = "unchanged";
const ACTION_SKIP: &str = "skip";
const ACTION_ERROR: &str = "error";

/// A provider as it currently exists, reduced to the fields the importer
/// compares. Secrets are loaded only when the document carries secrets.
struct ProviderFacts {
    provider_type: String,
    base_url: String,
    model_prefix: String,
    enabled: bool,
    health_check_interval_minutes: Option<i64>,
    health_check_model: Option<String>,
    models_sync_interval_minutes: Option<i64>,
    model_auto_include_rules: Vec<String>,
    timeout_seconds: Option<i64>,
    configured_cooldown_seconds: Option<i64>,
    max_concurrency: Option<i64>,
    queue_timeout_seconds: Option<i64>,
    models_enabled: Vec<String>,
    models_disabled: Vec<String>,
    key_names: Vec<String>,
    headers: Value,
    legacy_api_key: Option<String>,
    /// Every secret per key name: provider key names are not unique (only the
    /// secret is), so a name can map to more than one secret.
    key_secrets: HashMap<String, Vec<String>>,
    /// Existing key rows by secret, used to keep a key's identity (and with it
    /// its lifetime statistics and usage attribution) across an import.
    key_ids_by_secret: HashMap<String, i64>,
    key_channels: Vec<(
        String,
        bool,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    )>,
}

/// A webhook as it currently exists, reduced to the fields the importer
/// compares. The signing secret is loaded only when the document carries
/// secrets; `kind` and `body_fields` are configuration and always compared.
struct WebhookFacts {
    url: String,
    enabled: bool,
    event_types: Vec<String>,
    headers: Value,
    kind: String,
    body_fields: Value,
    secret: Option<String>,
}

struct ImportRun<'a> {
    state: &'a AppState,
    apply: bool,
    secrets_included: bool,
    counts: ImportCounts,
    items: Vec<ImportItem>,
    problems: Vec<String>,
    /// A blocking problem (an unusable document version) stops every write;
    /// advisory problems only skip the affected item.
    blocked: bool,
    providers: HashMap<String, i64>,
    api_keys: HashMap<String, Vec<i64>>,
    routes: Vec<(
        i64,
        String,
        String,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        i64,
        i64,
    )>,
    pools: Vec<(i64, i64, String)>,
    webhooks: Vec<(i64, String, String)>,
}

impl<'a> ImportRun<'a> {
    fn new(state: &'a AppState, apply: bool, secrets_included: bool) -> Self {
        Self {
            state,
            apply,
            secrets_included,
            counts: ImportCounts::default(),
            items: Vec::new(),
            problems: Vec::new(),
            blocked: false,
            providers: HashMap::new(),
            api_keys: HashMap::new(),
            routes: Vec::new(),
            pools: Vec::new(),
            webhooks: Vec::new(),
        }
    }

    async fn load_lookups(&mut self) -> AppResult<()> {
        for (id, name) in
            sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM providers ORDER BY id")
                .fetch_all(&self.state.pool)
                .await?
        {
            self.providers.entry(name).or_insert(id);
        }
        for (id, name) in
            sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM api_keys ORDER BY id")
                .fetch_all(&self.state.pool)
                .await?
        {
            self.api_keys.entry(name).or_default().push(id);
        }
        self.routes = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Option<i64>,
                Option<i64>,
                i64,
                i64,
                i64,
                i64,
            ),
        >(
            "SELECT id, name, model_pattern, context_override, output_override, \
                    all_targets_busy_wait_seconds, retry_jitter_ms, \
                    max_waiting_requests, keepalive_during_wait \
             FROM routes ORDER BY id",
        )
        .fetch_all(&self.state.pool)
        .await?;
        self.pools = sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT id, provider_id, name FROM provider_pools ORDER BY id",
        )
        .fetch_all(&self.state.pool)
        .await?;
        self.webhooks = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, name, url FROM webhooks ORDER BY id",
        )
        .fetch_all(&self.state.pool)
        .await?;
        Ok(())
    }

    fn record(&mut self, kind: &'static str, name: &str, action: &'static str, detail: String) {
        match action {
            ACTION_CREATE => self.counts.create += 1,
            ACTION_UPDATE => self.counts.update += 1,
            ACTION_UNCHANGED => self.counts.unchanged += 1,
            ACTION_SKIP => self.counts.skip += 1,
            _ => self.counts.error += 1,
        }
        self.items.push(ImportItem {
            kind,
            name: name.to_string(),
            action,
            detail,
        });
    }

    async fn settings(&mut self, settings: ConfigSettings) {
        if let Some(guardrails) = settings.guardrails {
            let unchanged = self
                .state
                .guardrail_settings()
                .await
                .map(|current| current == guardrails)
                .unwrap_or(false);
            if unchanged {
                self.record(
                    "setting",
                    "guardrails",
                    ACTION_UNCHANGED,
                    "护栏设置未变化".to_string(),
                );
            } else if !self.apply {
                self.record(
                    "setting",
                    "guardrails",
                    ACTION_UPDATE,
                    "将更新护栏设置".to_string(),
                );
            } else {
                let result =
                    super::update_guardrails_settings(State(self.state.clone()), Json(guardrails))
                        .await;
                self.settings_result("guardrails", "护栏设置", result);
            }
        }
        if let Some(inspector) = settings.inspector {
            let unchanged = self
                .state
                .inspector_settings()
                .await
                .map(|current| current == inspector)
                .unwrap_or(false);
            if unchanged {
                self.record(
                    "setting",
                    "inspector",
                    ACTION_UNCHANGED,
                    "请求预览设置未变化".to_string(),
                );
            } else if !self.apply {
                self.record(
                    "setting",
                    "inspector",
                    ACTION_UPDATE,
                    "将更新请求预览设置".to_string(),
                );
            } else {
                let result =
                    super::update_inspector_settings(State(self.state.clone()), Json(inspector))
                        .await;
                self.settings_result("inspector", "请求预览设置", result);
            }
        }
        if let Some(resilience) = settings.resilience {
            let unchanged = self
                .state
                .resilience_settings()
                .await
                .map(|current| current == resilience)
                .unwrap_or(false);
            if unchanged {
                self.record(
                    "setting",
                    "resilience",
                    ACTION_UNCHANGED,
                    "重试与流恢复设置未变化".to_string(),
                );
            } else if !self.apply {
                self.record(
                    "setting",
                    "resilience",
                    ACTION_UPDATE,
                    "将更新重试与流恢复设置".to_string(),
                );
            } else {
                let result =
                    super::update_resilience_settings(State(self.state.clone()), Json(resilience))
                        .await;
                self.settings_result("resilience", "重试与流恢复设置", result);
            }
        }
        if let Some(days) = settings.usage_retention_days {
            let current = stored_usage_retention_days(self.state).await.ok().flatten();
            if current == Some(days) {
                self.record(
                    "setting",
                    "usage_retention_days",
                    ACTION_UNCHANGED,
                    format!("用量日志保留 {days} 天，未变化"),
                );
            } else if self.apply {
                let result = super::update_runtime_settings(
                    State(self.state.clone()),
                    Json(RuntimeSettingsUpdate {
                        usage_retention_days: Some(days),
                    }),
                )
                .await;
                self.settings_result("usage_retention_days", "用量日志保留设置", result);
            } else {
                self.record(
                    "setting",
                    "usage_retention_days",
                    ACTION_UPDATE,
                    format!("将用量日志保留时间设置为 {days} 天"),
                );
            }
        }
    }

    fn settings_result<T: std::fmt::Debug>(
        &mut self,
        key: &'static str,
        label: &str,
        result: AppResult<T>,
    ) {
        match result {
            Ok(_) => self.record("setting", key, ACTION_UPDATE, format!("已更新{label}")),
            Err(error) => {
                let detail = format!("更新{label}失败：{error}");
                self.record("setting", key, ACTION_ERROR, detail.clone());
                self.problems.push(detail);
            }
        }
    }

    async fn facts(&mut self, id: i64) -> AppResult<ProviderFacts> {
        let provider = sqlx::query_as::<_, Provider>("SELECT * FROM providers WHERE id = ?")
            .bind(id)
            .fetch_one(&self.state.pool)
            .await?;
        let mut models_enabled = Vec::new();
        let mut models_disabled = Vec::new();
        for (model_name, enabled) in sqlx::query_as::<_, (String, i64)>(
            "SELECT model_name, enabled FROM provider_models WHERE provider_id = ? \
             ORDER BY model_name COLLATE NOCASE",
        )
        .bind(id)
        .fetch_all(&self.state.pool)
        .await?
        {
            if enabled != 0 {
                models_enabled.push(model_name);
            } else {
                models_disabled.push(model_name);
            }
        }
        let mut key_names = Vec::new();
        let mut key_secrets: HashMap<String, Vec<String>> = HashMap::new();
        let mut key_ids_by_secret = HashMap::new();
        let mut key_channels = Vec::new();
        for (
            id,
            name,
            enabled,
            secret,
            max_concurrency,
            requests_per_minute,
            queue_limit,
            queue_wait_ms,
        ) in sqlx::query_as::<
            _,
            (
                i64,
                String,
                i64,
                String,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
            ),
        >(
            "SELECT id, name, enabled, secret, max_concurrency, \
                    requests_per_minute, queue_limit, queue_wait_ms \
             FROM provider_api_keys WHERE provider_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.state.pool)
        .await?
        {
            if enabled != 0 {
                key_names.push(name.clone());
            }
            key_ids_by_secret.insert(secret.clone(), id);
            key_secrets.entry(name.clone()).or_default().push(secret);
            key_channels.push((
                name,
                enabled != 0,
                max_concurrency,
                requests_per_minute,
                queue_limit,
                queue_wait_ms,
            ));
        }
        Ok(ProviderFacts {
            provider_type: provider.provider_type,
            base_url: provider.base_url,
            model_prefix: provider.model_prefix,
            enabled: provider.enabled != 0,
            health_check_interval_minutes: provider.health_check_interval_minutes,
            health_check_model: provider.health_check_model,
            models_sync_interval_minutes: provider.models_sync_interval_minutes,
            model_auto_include_rules: serde_json::from_str(&provider.model_auto_include_rules)
                .unwrap_or_default(),
            timeout_seconds: provider.timeout_seconds,
            configured_cooldown_seconds: provider.cooldown_seconds,
            max_concurrency: provider.max_concurrency,
            queue_timeout_seconds: provider.queue_timeout_seconds,
            models_enabled,
            models_disabled,
            key_names,
            headers: parse_json_or_empty(&provider.headers),
            legacy_api_key: provider.api_key.filter(|key| !key.is_empty()),
            key_secrets,
            key_ids_by_secret,
            key_channels,
        })
    }

    fn facts_match(&self, facts: &ProviderFacts, doc: &ConfigProvider) -> bool {
        facts.provider_type == doc.provider_type
            && facts.base_url == doc.base_url
            && facts.model_prefix == doc.model_prefix
            && facts.enabled == doc.enabled
            && opt_matches(
                facts.health_check_interval_minutes,
                doc.health_check_interval_minutes,
            )
            && opt_matches(
                facts.health_check_model.as_deref(),
                doc.health_check_model.as_deref(),
            )
            && opt_matches(
                facts.models_sync_interval_minutes,
                doc.models_sync_interval_minutes,
            )
            && opt_matches(facts.timeout_seconds, doc.timeout_seconds)
            && opt_matches(
                facts.configured_cooldown_seconds,
                doc.configured_cooldown_seconds,
            )
            && opt_matches(facts.max_concurrency, doc.max_concurrency)
            && opt_matches(facts.queue_timeout_seconds, doc.queue_timeout_seconds)
            && same_set(&facts.models_enabled, &doc.models)
            && same_set(&facts.models_disabled, &doc.disabled_models)
            && same_set(
                &facts.model_auto_include_rules,
                &doc.model_auto_include_rules,
            )
            && same_set(&facts.key_names, &self.document_key_names(doc))
            && (doc.provider_keys.is_empty()
                || facts.key_channels
                    == doc
                        .provider_keys
                        .iter()
                        .map(|key| {
                            (
                                key.name.clone(),
                                key.enabled,
                                key.max_concurrency,
                                key.requests_per_minute,
                                key.queue_limit,
                                key.queue_wait_ms,
                            )
                        })
                        .collect::<Vec<_>>())
            && (!self.secrets_included || header_values_match(&facts.headers, &doc.headers))
            && self.credentials_match(facts, doc)
    }

    /// With secrets in the document a credential that already matches must not
    /// look like a change; an omitted secret means "leave it alone".
    fn credentials_match(&self, facts: &ProviderFacts, doc: &ConfigProvider) -> bool {
        if !self.secrets_included {
            return true;
        }
        if let Some(api_key) = doc.api_key.as_deref().filter(|value| !value.is_empty())
            && facts.legacy_api_key.as_deref() != Some(api_key)
        {
            return false;
        }
        doc.provider_keys.iter().all(|key| {
            match key.api_key.as_deref().filter(|value| !value.is_empty()) {
                Some(secret) => facts
                    .key_secrets
                    .get(&key.name)
                    .is_some_and(|secrets| secrets.iter().any(|known| known == secret)),
                None => true,
            }
        })
    }

    fn document_key_names(&self, doc: &ConfigProvider) -> Vec<String> {
        doc.provider_keys
            .iter()
            .filter(|key| key.enabled)
            .map(|key| key.name.clone())
            .collect()
    }

    /// The provider key payload for an import.
    ///
    /// With secrets in the document, an existing row is matched by secret —
    /// unique per provider, unlike the name — so passing its id keeps the key
    /// identity and with it the lifetime statistics and the usage attribution
    /// that live on that row. A key that matches nothing is created. Without
    /// secrets nothing is applied on the update path (a masked import would
    /// otherwise overwrite a real credential), so the ids are irrelevant there.
    fn provider_key_inputs(
        &self,
        doc: &ConfigProvider,
        facts: Option<&ProviderFacts>,
    ) -> Vec<ProviderApiKeyInput> {
        doc.provider_keys
            .iter()
            .map(|key| {
                let secret = if self.secrets_included {
                    key.api_key.clone().filter(|value| !value.is_empty())
                } else {
                    None
                };
                let id = match (secret.as_deref(), facts) {
                    (Some(secret), Some(facts)) => facts.key_ids_by_secret.get(secret).copied(),
                    _ => None,
                };
                ProviderApiKeyInput {
                    id,
                    name: key.name.clone(),
                    api_key: secret,
                    enabled: key.enabled,
                    max_concurrency: key.max_concurrency,
                    requests_per_minute: key.requests_per_minute,
                    queue_limit: key.queue_limit,
                    queue_wait_ms: key.queue_wait_ms,
                }
            })
            .collect()
    }

    async fn providers(&mut self, providers: &[ConfigProvider]) {
        for (index, doc) in providers.iter().enumerate() {
            let name = doc.name.trim().to_string();
            if name.is_empty() {
                self.record(
                    "provider",
                    "(未命名)",
                    ACTION_ERROR,
                    "提供商名称为空".to_string(),
                );
                continue;
            }
            let provider_type = match ProviderType::from_str(&doc.provider_type) {
                Ok(value) => value,
                Err(error) => {
                    self.record("provider", &name, ACTION_ERROR, error);
                    continue;
                }
            };
            // Enabled and disabled models are sent together so the model set is
            // replaced wholesale, then the disabled ones are turned off. Doing
            // it in this order also survives `replace_provider_models`, which
            // preserves the previous `enabled` flag of a known model name.
            let mut models = dedup_sorted(doc.models.clone());
            for model in &doc.disabled_models {
                if !models.iter().any(|existing| existing == model) {
                    models.push(model.clone());
                }
            }
            let headers = headers_to_apply(&doc.headers, self.secrets_included);

            match self.providers.get(&name).copied() {
                Some(id) => {
                    let facts = match self.facts(id).await {
                        Ok(facts) => facts,
                        Err(error) => {
                            self.record(
                                "provider",
                                &name,
                                ACTION_ERROR,
                                format!("读取现有提供商失败：{error}"),
                            );
                            continue;
                        }
                    };
                    if self.facts_match(&facts, doc) {
                        self.record(
                            "provider",
                            &name,
                            ACTION_UNCHANGED,
                            "提供商配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "provider",
                            &name,
                            ACTION_UPDATE,
                            describe_provider_change(&facts, doc, self.secrets_included),
                        );
                        continue;
                    }
                    let keys = self.provider_key_inputs(doc, Some(&facts));
                    let update = ProviderUpdate {
                        name: Some(name.clone()),
                        provider_type: Some(provider_type),
                        base_url: Some(doc.base_url.clone()),
                        model_prefix: Some(doc.model_prefix.clone()),
                        api_key: if self.secrets_included {
                            doc.api_key.clone()
                        } else {
                            None
                        },
                        clear_api_key: None,
                        api_keys: if self.secrets_included && !keys.is_empty() {
                            Some(keys)
                        } else {
                            None
                        },
                        headers: headers.clone(),
                        enabled: Some(doc.enabled),
                        auto_sync_models: Some(false),
                        models: Some(models.clone()),
                        model_auto_include_rules: Some(doc.model_auto_include_rules.clone()),
                        health_check_interval_minutes: doc.health_check_interval_minutes,
                        health_check_model: doc.health_check_model.clone(),
                        models_sync_interval_minutes: doc.models_sync_interval_minutes,
                        timeout_seconds: doc.timeout_seconds,
                        configured_cooldown_seconds: doc.configured_cooldown_seconds,
                        max_concurrency: doc.max_concurrency,
                        queue_timeout_seconds: doc.queue_timeout_seconds,
                    };
                    match super::update_provider(State(self.state.clone()), Path(id), Json(update))
                        .await
                    {
                        Ok(_) => {
                            self.apply_model_flags(id, doc, &models).await;
                            self.record(
                                "provider",
                                &name,
                                ACTION_UPDATE,
                                describe_provider_change(&facts, doc, self.secrets_included),
                            );
                        }
                        Err(error) => self.record(
                            "provider",
                            &name,
                            ACTION_ERROR,
                            format!("更新失败：{error}"),
                        ),
                    }
                }
                None => {
                    if !self.apply {
                        // Remember a placeholder id so routes and pools later in
                        // the same document can still resolve this provider by
                        // name: a preview must show what *would* be linked.
                        self.providers.insert(name.clone(), -(index as i64) - 1);
                        self.record(
                            "provider",
                            &name,
                            ACTION_CREATE,
                            format!(
                                "将新建 {} 提供商，{} 个模型（其中 {} 个禁用）",
                                doc.provider_type,
                                models.len(),
                                doc.disabled_models.len()
                            ),
                        );
                        continue;
                    }
                    let input = ProviderInput {
                        name: name.clone(),
                        provider_type,
                        base_url: doc.base_url.clone(),
                        model_prefix: doc.model_prefix.clone(),
                        api_key: if self.secrets_included {
                            doc.api_key.clone()
                        } else {
                            None
                        },
                        api_keys: self.provider_key_inputs(doc, None),
                        headers: headers.unwrap_or_else(|| json!({})),
                        enabled: doc.enabled,
                        auto_sync_models: false,
                        models: models.clone(),
                        model_auto_include_rules: doc.model_auto_include_rules.clone(),
                        health_check_interval_minutes: doc.health_check_interval_minutes,
                        health_check_model: doc.health_check_model.clone(),
                        models_sync_interval_minutes: doc.models_sync_interval_minutes,
                        timeout_seconds: doc.timeout_seconds,
                        configured_cooldown_seconds: doc.configured_cooldown_seconds,
                        max_concurrency: doc.max_concurrency,
                        queue_timeout_seconds: doc.queue_timeout_seconds,
                    };
                    match super::create_provider(State(self.state.clone()), Json(input)).await {
                        Ok((_, Json(view))) => {
                            self.providers.insert(name.clone(), view.id);
                            self.apply_model_flags(view.id, doc, &models).await;
                            self.record(
                                "provider",
                                &name,
                                ACTION_CREATE,
                                format!("已新建提供商（#{}）", view.id),
                            );
                        }
                        Err(error) => self.record(
                            "provider",
                            &name,
                            ACTION_ERROR,
                            format!("创建失败：{error}"),
                        ),
                    }
                }
            }
        }
    }

    /// Re-assert `enabled` per model: the provider handlers preserve an
    /// existing override, so the document is authoritative here.
    async fn apply_model_flags(&mut self, provider_id: i64, doc: &ConfigProvider, all: &[String]) {
        for model in all {
            let enabled = if doc.disabled_models.iter().any(|name| name == model) {
                0
            } else {
                1
            };
            if let Err(error) = sqlx::query(
                "UPDATE provider_models SET enabled = ? WHERE provider_id = ? AND model_name = ?",
            )
            .bind(enabled)
            .bind(provider_id)
            .bind(model)
            .execute(&self.state.pool)
            .await
            {
                self.problems
                    .push(format!("更新模型 {model} 的启用状态失败：{error}"));
            }
        }
    }

    async fn routes(&mut self, routes: &[ConfigRoute]) {
        for doc in routes {
            let name = doc.name.trim().to_string();
            let strategy = match RouteStrategy::from_str(doc.strategy.trim()) {
                Ok(value) => value,
                Err(error) => {
                    self.record("route", &name, ACTION_ERROR, error);
                    continue;
                }
            };
            let mut targets = Vec::new();
            // Parallel to `targets`, but with provider ids resolved: this is
            // what the current database rows are compared against.
            let mut desired = Vec::new();
            let mut missing = None;
            for target in &doc.targets {
                match self.providers.get(&target.provider).copied() {
                    Some(provider_id) => {
                        targets.push(RouteTargetInput {
                            id: None,
                            provider_id,
                            upstream_model: target.upstream_model.clone(),
                            weight: target.weight,
                            priority: target.priority,
                            enabled: target.enabled,
                        });
                        desired.push((
                            provider_id,
                            target.upstream_model.clone(),
                            target.weight,
                            target.priority,
                            target.enabled,
                        ));
                    }
                    None => missing = Some(target.provider.clone()),
                }
            }
            if let Some(provider) = missing {
                self.record(
                    "route",
                    &name,
                    ACTION_SKIP,
                    format!("目标引用了不存在的提供商 '{provider}'，整条路由已跳过"),
                );
                continue;
            }
            let existing = self
                .routes
                .iter()
                .find(|(_, existing_name, _, _, _, _, _, _, _)| existing_name == &name)
                .or_else(|| {
                    self.routes
                        .iter()
                        .find(|(_, _, pattern, _, _, _, _, _, _)| pattern == &doc.model_pattern)
                })
                .map(
                    |(
                        id,
                        _,
                        _,
                        context_override,
                        output_override,
                        all_targets_busy_wait_seconds,
                        retry_jitter_ms,
                        max_waiting_requests,
                        keepalive_during_wait,
                    )| {
                        (
                            *id,
                            *context_override,
                            *output_override,
                            *all_targets_busy_wait_seconds,
                            *retry_jitter_ms,
                            *max_waiting_requests,
                            *keepalive_during_wait,
                        )
                    },
                );
            match existing {
                Some((
                    id,
                    current_context_override,
                    current_output_override,
                    current_all_targets_busy_wait_seconds,
                    current_retry_jitter_ms,
                    current_max_waiting_requests,
                    current_keepalive_during_wait,
                )) => {
                    let current = self.route_targets(id).await.unwrap_or_default();
                    if route_targets_match(&current, &desired)
                        && current_context_override == doc.context_override
                        && current_output_override == doc.output_override
                        && current_all_targets_busy_wait_seconds
                            == doc.all_targets_busy_wait_seconds
                        && current_retry_jitter_ms == doc.retry_jitter_ms
                        && current_max_waiting_requests == doc.max_waiting_requests
                        && (current_keepalive_during_wait != 0) == doc.keepalive_during_wait
                    {
                        self.record(
                            "route",
                            &name,
                            ACTION_UNCHANGED,
                            "路由配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "route",
                            &name,
                            ACTION_UPDATE,
                            format!("将更新路由（{} 个目标）", doc.targets.len()),
                        );
                        continue;
                    }
                    let update = RouteUpdate {
                        name: Some(name.clone()),
                        model_pattern: Some(doc.model_pattern.clone()),
                        strategy: Some(strategy),
                        enabled: Some(doc.enabled),
                        context_override: Some(doc.context_override),
                        output_override: Some(doc.output_override),
                        all_targets_busy_wait_seconds: Some(doc.all_targets_busy_wait_seconds),
                        retry_jitter_ms: Some(doc.retry_jitter_ms),
                        max_waiting_requests: Some(doc.max_waiting_requests),
                        keepalive_during_wait: Some(doc.keepalive_during_wait),
                        targets: Some(targets),
                    };
                    match super::update_route(State(self.state.clone()), Path(id), Json(update))
                        .await
                    {
                        Ok(_) => self.record(
                            "route",
                            &name,
                            ACTION_UPDATE,
                            format!("已更新路由（{} 个目标）", doc.targets.len()),
                        ),
                        Err(error) => {
                            self.record("route", &name, ACTION_ERROR, format!("更新失败：{error}"))
                        }
                    }
                }
                None => {
                    if !self.apply {
                        self.record(
                            "route",
                            &name,
                            ACTION_CREATE,
                            format!("将新建路由（{} 个目标）", doc.targets.len()),
                        );
                        continue;
                    }
                    let input = RouteInput {
                        name: name.clone(),
                        model_pattern: doc.model_pattern.clone(),
                        strategy,
                        enabled: doc.enabled,
                        context_override: doc.context_override,
                        output_override: doc.output_override,
                        all_targets_busy_wait_seconds: Some(doc.all_targets_busy_wait_seconds),
                        retry_jitter_ms: Some(doc.retry_jitter_ms),
                        max_waiting_requests: Some(doc.max_waiting_requests),
                        keepalive_during_wait: Some(doc.keepalive_during_wait),
                        targets,
                    };
                    match super::create_route(State(self.state.clone()), Json(input)).await {
                        Ok((_, Json(view))) => {
                            self.routes.push((
                                view.id,
                                name.clone(),
                                doc.model_pattern.clone(),
                                view.context_override,
                                view.output_override,
                                view.all_targets_busy_wait_seconds,
                                view.retry_jitter_ms,
                                view.max_waiting_requests,
                                i64::from(view.keepalive_during_wait),
                            ));
                            self.record("route", &name, ACTION_CREATE, "已新建路由".to_string());
                        }
                        Err(error) => {
                            self.record("route", &name, ACTION_ERROR, format!("创建失败：{error}"))
                        }
                    }
                }
            }
        }
    }

    async fn route_targets(&self, route_id: i64) -> AppResult<Vec<(i64, String, i64, i64, bool)>> {
        Ok(sqlx::query_as::<_, (i64, String, i64, i64, i64)>(
            "SELECT provider_id, upstream_model, weight, priority, enabled \
             FROM route_targets WHERE route_id = ? ORDER BY priority, id",
        )
        .bind(route_id)
        .fetch_all(&self.state.pool)
        .await?
        .into_iter()
        .map(|(provider_id, model, weight, priority, enabled)| {
            (provider_id, model, weight, priority, enabled != 0)
        })
        .collect())
    }

    async fn pools(&mut self, pools: &[ConfigPool]) {
        for doc in pools {
            let label = format!("{} / {}", doc.provider, doc.name);
            let Some(provider_id) = self.providers.get(&doc.provider).copied() else {
                self.record(
                    "provider_pool",
                    &label,
                    ACTION_SKIP,
                    format!("提供商 '{}' 不存在，配额池已跳过", doc.provider),
                );
                continue;
            };
            let mut allocations = Vec::new();
            for allocation in &doc.allocations {
                match self.api_keys.get(&allocation.api_key) {
                    Some(ids) if ids.len() == 1 => allocations.push(ProviderPoolAllocationInput {
                        api_key_id: ids[0],
                        weight_pct: allocation.weight_pct,
                        policy: allocation.policy.clone(),
                    }),
                    Some(ids) if ids.is_empty() => self.problems.push(format!(
                        "配额池 '{}' 的分配引用了不存在的网关 API Key '{}'，该分配已跳过",
                        doc.name, allocation.api_key
                    )),
                    Some(_) => self.problems.push(format!(
                        "配额池 '{}' 的分配引用了不唯一的网关 API Key '{}'，该分配已跳过",
                        doc.name, allocation.api_key
                    )),
                    None => self.problems.push(format!(
                        "配额池 '{}' 的分配引用了不存在的网关 API Key '{}'，该分配已跳过",
                        doc.name, allocation.api_key
                    )),
                }
            }
            let existing = self
                .pools
                .iter()
                .find(|(_, existing_provider, existing_name)| {
                    *existing_provider == provider_id && existing_name == &doc.name
                })
                .map(|(id, _, _)| *id);
            match existing {
                Some(id) => {
                    let current = self.pool_allocations(id).await.unwrap_or_default();
                    if pool_allocations_match(&current, &allocations) {
                        self.record(
                            "provider_pool",
                            &label,
                            ACTION_UNCHANGED,
                            "配额池配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "provider_pool",
                            &label,
                            ACTION_UPDATE,
                            format!("将更新配额池（{} 个分配）", allocations.len()),
                        );
                        continue;
                    }
                    let update = ProviderPoolUpdate {
                        name: Some(doc.name.clone()),
                        enabled: Some(doc.enabled),
                        window_seconds: Some(doc.window_seconds),
                        request_limit: Some(doc.request_limit),
                        token_limit: Some(doc.token_limit),
                        saturation_threshold_pct: Some(doc.saturation_threshold_pct),
                        allocations: Some(allocations),
                    };
                    match super::update_provider_pool(
                        State(self.state.clone()),
                        Path(id),
                        Json(update),
                    )
                    .await
                    {
                        Ok(_) => self.record(
                            "provider_pool",
                            &label,
                            ACTION_UPDATE,
                            "已更新配额池".to_string(),
                        ),
                        Err(error) => self.record(
                            "provider_pool",
                            &label,
                            ACTION_ERROR,
                            format!("更新失败：{error}"),
                        ),
                    }
                }
                None => {
                    if !self.apply {
                        self.record(
                            "provider_pool",
                            &label,
                            ACTION_CREATE,
                            format!("将新建配额池（{} 个分配）", allocations.len()),
                        );
                        continue;
                    }
                    let input = ProviderPoolInput {
                        provider_id,
                        name: doc.name.clone(),
                        enabled: doc.enabled,
                        window_seconds: doc.window_seconds,
                        request_limit: doc.request_limit,
                        token_limit: doc.token_limit,
                        saturation_threshold_pct: doc.saturation_threshold_pct,
                        allocations,
                    };
                    match super::create_provider_pool(State(self.state.clone()), Json(input)).await
                    {
                        Ok(Json(view)) => {
                            self.pools.push((view.id, provider_id, doc.name.clone()));
                            self.record(
                                "provider_pool",
                                &label,
                                ACTION_CREATE,
                                "已新建配额池".to_string(),
                            );
                        }
                        Err(error) => self.record(
                            "provider_pool",
                            &label,
                            ACTION_ERROR,
                            format!("创建失败：{error}"),
                        ),
                    }
                }
            }
        }
    }

    async fn pool_allocations(&self, pool_id: i64) -> AppResult<Vec<(i64, i64, String)>> {
        Ok(sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT api_key_id, weight_pct, policy FROM provider_pool_allocations \
             WHERE pool_id = ? ORDER BY api_key_id",
        )
        .bind(pool_id)
        .fetch_all(&self.state.pool)
        .await?)
    }

    async fn webhooks(&mut self, webhooks: &[ConfigWebhook]) {
        for doc in webhooks {
            let name = doc.name.trim().to_string();
            let headers = headers_to_apply(&doc.headers, self.secrets_included);
            let existing = self
                .webhooks
                .iter()
                .find(|(_, _, url)| url == &doc.url)
                .or_else(|| {
                    self.webhooks
                        .iter()
                        .find(|(_, existing, _)| existing == &name)
                })
                .map(|(id, _, _)| *id);
            match existing {
                Some(id) => {
                    let facts = match self.webhook_facts(id).await {
                        Ok(facts) => facts,
                        Err(error) => {
                            self.record(
                                "webhook",
                                &name,
                                ACTION_ERROR,
                                format!("读取现有 Webhook 失败：{error}"),
                            );
                            continue;
                        }
                    };
                    if facts.url == doc.url
                        && facts.enabled == doc.enabled
                        && same_set(&facts.event_types, &doc.event_types)
                        && facts.kind == doc.kind
                        && facts.body_fields == doc.body_fields
                        && (!self.secrets_included
                            || header_values_match(&facts.headers, &doc.headers))
                        && (!self.secrets_included
                            || facts.secret.as_deref() == doc.secret.as_deref())
                    {
                        self.record(
                            "webhook",
                            &name,
                            ACTION_UNCHANGED,
                            "Webhook 配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "webhook",
                            &name,
                            ACTION_UPDATE,
                            "将更新 Webhook".to_string(),
                        );
                        continue;
                    }
                    let update = WebhookUpdate {
                        name: Some(name.clone()),
                        url: Some(doc.url.clone()),
                        secret: if self.secrets_included {
                            doc.secret.clone()
                        } else {
                            None
                        },
                        headers,
                        kind: Some(doc.kind.clone()),
                        body_fields: Some(doc.body_fields.clone()),
                        clear_secret: None,
                        event_types: Some(doc.event_types.clone()),
                        enabled: Some(doc.enabled),
                    };
                    match super::update_webhook(State(self.state.clone()), Path(id), Json(update))
                        .await
                    {
                        Ok(_) => self.record(
                            "webhook",
                            &name,
                            ACTION_UPDATE,
                            "已更新 Webhook".to_string(),
                        ),
                        Err(error) => self.record(
                            "webhook",
                            &name,
                            ACTION_ERROR,
                            format!("更新失败：{error}"),
                        ),
                    }
                }
                None => {
                    if !self.apply {
                        self.record(
                            "webhook",
                            &name,
                            ACTION_CREATE,
                            "将新建 Webhook".to_string(),
                        );
                        continue;
                    }
                    let input = WebhookInput {
                        name: name.clone(),
                        url: doc.url.clone(),
                        secret: if self.secrets_included {
                            doc.secret.clone()
                        } else {
                            None
                        },
                        headers,
                        kind: Some(doc.kind.clone()),
                        body_fields: Some(doc.body_fields.clone()),
                        event_types: Some(doc.event_types.clone()),
                        enabled: doc.enabled,
                    };
                    match super::create_webhook(State(self.state.clone()), Json(input)).await {
                        Ok((_, Json(view))) => {
                            self.webhooks.push((view.id, name.clone(), doc.url.clone()));
                            self.record(
                                "webhook",
                                &name,
                                ACTION_CREATE,
                                "已新建 Webhook".to_string(),
                            );
                        }
                        Err(error) => self.record(
                            "webhook",
                            &name,
                            ACTION_ERROR,
                            format!("创建失败：{error}"),
                        ),
                    }
                }
            }
        }
    }

    /// Current state of one webhook, used to decide whether a document would
    /// change anything.
    async fn webhook_facts(&self, id: i64) -> AppResult<WebhookFacts> {
        let (url, enabled, event_types, headers, kind, body_fields, secret) =
            sqlx::query_as::<_, (String, i64, String, String, String, String, Option<String>)>(
                "SELECT url, enabled, event_types, headers, kind, body_fields, secret \
             FROM webhooks WHERE id = ?",
            )
            .bind(id)
            .fetch_one(&self.state.pool)
            .await?;
        Ok(WebhookFacts {
            url,
            enabled: enabled != 0,
            event_types: parse_string_list(&event_types),
            headers: parse_json_or_empty(&headers),
            kind,
            body_fields: parse_json_or_empty(&body_fields),
            secret: secret.filter(|secret| !secret.is_empty()),
        })
    }

    fn finish(self, document_version: i64) -> ImportPlan {
        let applied = self.apply && !self.blocked && self.counts.error == 0;
        let mut notes = Vec::new();
        if self.secrets_included {
            notes.push(
                "上游密钥按其密钥值匹配现有记录，未匹配到的密钥会新建，新建密钥的历史用量统计从零开始。"
                    .to_string(),
            );
        }
        if applied {
            notes.push(
                "导入逐项写入（设置→提供商→配额池→路由→Webhook），中途失败会保留已写入的部分。"
                    .to_string(),
            );
        }
        ImportPlan {
            dry_run: !self.apply,
            // The writes already happened by now, so `applied` only reports
            // whether they were allowed to; `valid` additionally demands a
            // clean document (no skipped references).
            applied,
            valid: !self.blocked && self.counts.error == 0 && self.problems.is_empty(),
            document_version,
            secrets_included: self.secrets_included,
            counts: self.counts,
            items: self.items,
            problems: self.problems,
            notes,
        }
    }
}

/// `null` in the document means "leave the current value alone": the update
/// handlers cannot clear an optional field, so treating it as a match keeps a
/// repeated import idempotent instead of reporting the same change forever.
fn opt_matches<T: PartialEq>(current: Option<T>, document: Option<T>) -> bool {
    match document {
        None => true,
        Some(value) => current == Some(value),
    }
}

fn dedup_sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort_by_key(|value| value.to_lowercase());
    values.dedup();
    values
}

fn same_set<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len() && left.iter().all(|item| right.contains(item))
}

/// Headers to write, or `None` when the document may not change them.
///
/// When secrets were not exported every credential-looking value is `***`, so
/// applying the document would overwrite a real credential with a mask. In that
/// mode the header map is left untouched entirely.
fn headers_to_apply(value: &Value, include_secrets: bool) -> Option<Value> {
    if !include_secrets {
        return None;
    }
    let object = value.as_object();
    match object {
        Some(map) if map.is_empty() => None,
        Some(_) => Some(value.clone()),
        None => None,
    }
}

fn header_values_match(current: &Value, document: &Value) -> bool {
    current == document
}

fn route_targets_match(
    current: &[(i64, String, i64, i64, bool)],
    desired: &[(i64, String, i64, i64, bool)],
) -> bool {
    current.len() == desired.len() && current.iter().all(|target| desired.contains(target))
}

fn pool_allocations_match(
    current: &[(i64, i64, String)],
    doc: &[ProviderPoolAllocationInput],
) -> bool {
    current.len() == doc.len()
        && current.iter().all(|(api_key_id, weight_pct, policy)| {
            doc.iter().any(|allocation| {
                allocation.api_key_id == *api_key_id
                    && allocation.weight_pct == *weight_pct
                    && allocation.policy == *policy
            })
        })
}

fn describe_provider_change(
    facts: &ProviderFacts,
    doc: &ConfigProvider,
    include_secrets: bool,
) -> String {
    let mut changes = Vec::new();
    if facts.base_url != doc.base_url {
        changes.push("接入地址");
    }
    if facts.provider_type != doc.provider_type {
        changes.push("提供商类型");
    }
    if facts.enabled != doc.enabled {
        changes.push("启用状态");
    }
    if !same_set(&facts.models_enabled, &doc.models)
        || !same_set(&facts.models_disabled, &doc.disabled_models)
    {
        changes.push("模型清单");
    }
    if facts.timeout_seconds != doc.timeout_seconds
        || facts.configured_cooldown_seconds != doc.configured_cooldown_seconds
        || facts.max_concurrency != doc.max_concurrency
        || facts.queue_timeout_seconds != doc.queue_timeout_seconds
        || facts.health_check_interval_minutes != doc.health_check_interval_minutes
        || facts.health_check_model != doc.health_check_model
        || facts.models_sync_interval_minutes != doc.models_sync_interval_minutes
    {
        changes.push("超时与并发参数");
    }
    if include_secrets && facts.headers != doc.headers {
        changes.push("请求头");
    }
    if changes.is_empty() {
        "提供商配置将重新写入".to_string()
    } else {
        format!("将更新：{}", changes.join("、"))
    }
}

// -------------------------------------------------------------------------
// Backups
// -------------------------------------------------------------------------

/// The file behind the live pool, or an error for an in-memory database.
fn database_file(state: &AppState) -> AppResult<PathBuf> {
    let options = state.pool.connect_options();
    let path = options.get_filename().to_path_buf();
    // sqlx has no `in_memory` getter, so the marker it writes into the
    // filename is what identifies a `sqlite::memory:` pool; a real database
    // never has a `file:` filename.
    let filename = options.get_filename().to_path_buf();
    let name = filename.to_string_lossy();
    if name.is_empty() || name == ":memory:" || name.starts_with("file:sqlx-in-memory-") {
        return Err(AppError::BadRequest(
            "该实例使用内存数据库，没有可备份的文件".to_string(),
        ));
    }
    Ok(path)
}

pub async fn create_backup(
    State(state): State<AppState>,
) -> AppResult<(StatusCode, Json<BackupInfo>)> {
    let path = database_file(&state)?;
    let written = crate::startup::create_backup(&state.pool, &path)
        .await
        .map_err(AppError::Internal)?;
    let name = written
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let info = crate::startup::list_backups(&path)
        .await
        .map_err(AppError::Internal)?
        .into_iter()
        .find(|backup| backup.name == name)
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("备份文件写入后未能列出")))?;
    record_audit(
        &state,
        "backup",
        "database",
        Some(&info.name),
        "created a database backup",
        Some(json!({ "size_bytes": info.size_bytes })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(info)))
}

pub async fn list_backups(State(state): State<AppState>) -> AppResult<Json<Vec<BackupInfo>>> {
    let path = database_file(&state)?;
    Ok(Json(
        crate::startup::list_backups(&path)
            .await
            .map_err(AppError::Internal)?,
    ))
}

/// Verify a backup without touching it: read-only open, integrity check and
/// row counts, so an operator knows the file is restorable before stopping the
/// gateway to restore it.
pub async fn verify_backup(
    State(state): State<AppState>,
    Json(input): Json<VerifyBackupRequest>,
) -> AppResult<Json<DatabaseFileReport>> {
    let path = database_file(&state)?;
    let backup = crate::startup::resolve_backup_path(&path, &input.name, false)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    if !backup.is_file() {
        return Err(AppError::NotFound(format!(
            "备份文件 {} 不存在",
            input.name
        )));
    }
    let report = crate::startup::verify_database_file(&backup)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(report))
}

// -------------------------------------------------------------------------
// Migration pre-check and upgrade check
// -------------------------------------------------------------------------

/// What a start would do to the schema, answered from the live connection.
pub async fn migration_status(State(state): State<AppState>) -> AppResult<Json<MigrationReport>> {
    let path = database_file(&state)?;
    let file = path.to_string_lossy().replace('\\', "/");
    let migrator = sqlx::migrate!("./migrations");
    let mut problems = Vec::new();

    let applied_versions = sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_else(|error| {
            problems.push(format!("读取迁移记录失败：{error}"));
            Vec::new()
        });

    let pending = crate::startup::pending_migrations(&state.pool, &migrator)
        .await
        .map_err(AppError::Internal)?
        .into_iter()
        .map(|migration| MigrationEntry {
            version: migration.version,
            description: migration.description.clone(),
        })
        .collect::<Vec<_>>();

    let unknown_applied = {
        let mut unknown = applied_versions
            .iter()
            .filter(|version| !migrator.iter().any(|m| m.version == **version))
            .copied()
            .collect::<Vec<_>>();
        unknown.sort_unstable();
        unknown
    };
    if !unknown_applied.is_empty() {
        problems.push(format!(
            "数据库中记录了本程序不认识的迁移 {unknown_applied:?}，说明该库由更新的版本写入",
        ));
    }

    let applied = migrator
        .iter()
        .filter(|migration| applied_versions.contains(&migration.version))
        .map(|migration| MigrationEntry {
            version: migration.version,
            description: migration.description.to_string(),
        })
        .collect::<Vec<_>>();

    let data_present = sqlx::query_scalar::<_, i64>(
        "SELECT (SELECT COUNT(*) FROM providers) + (SELECT COUNT(*) FROM routes) \
                + (SELECT COUNT(*) FROM api_keys)",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0)
        > 0;

    Ok(Json(MigrationReport {
        database: file,
        up_to_date: pending.is_empty(),
        applied,
        backup_would_be_written: data_present && !pending.is_empty(),
        pending,
        unknown_applied,
        data_present,
        problems,
    }))
}

/// Compare the running version with a newer one.
///
/// `?latest=` makes the check testable and usable on an air-gapped host; when
/// it is absent `OPENLLM_UPGRADE_FEED_URL` is fetched (a GitHub-release shaped
/// document with `tag_name` or `version`). With neither, the answer is
/// `unknown` — a gateway without a repository field in `Cargo.toml` has no
/// sensible default feed and must not guess.
pub async fn upgrade_check(
    State(state): State<AppState>,
    Query(query): Query<UpgradeCheckQuery>,
) -> AppResult<Json<UpgradeStatus>> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let pending = crate::startup::pending_migrations(&state.pool, &sqlx::migrate!("./migrations"))
        .await
        .map_err(AppError::Internal)?;

    let (latest, source, mut problems) = match query
        .latest
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(version) => (Some(normalize_version(version)), "query", Vec::new()),
        None => match upgrade_feed_url() {
            Some(url) => match fetch_latest_version(&state.client, &url).await {
                Ok(version) => (Some(version), "feed", Vec::new()),
                Err(error) => (
                    None,
                    "feed",
                    vec![format!("无法从 {url} 获取最新版本：{error}")],
                ),
            },
            None => (
                None,
                "none",
                vec![
                    "未提供 latest 参数，也未设置 OPENLLM_UPGRADE_FEED_URL，无法判断是否有新版本"
                        .to_string(),
                ],
            ),
        },
    };

    let comparison = match latest.as_deref() {
        Some(latest) if !is_numeric_version(latest) => {
            problems.push(format!("无法解析版本号 '{latest}'，请手动核对发布说明"));
            None
        }
        Some(latest) => Some(compare_versions(&current, latest)),
        None => None,
    };
    let (status, update_available, message) = match comparison {
        Some(std::cmp::Ordering::Less) => (
            "update_available",
            true,
            format!(
                "有新版本可用：当前 {current}，最新 {}{}",
                latest.clone().unwrap_or_default(),
                if pending.is_empty() {
                    String::new()
                } else {
                    format!("；升级后将执行 {} 个迁移", pending.len())
                }
            ),
        ),
        Some(std::cmp::Ordering::Equal) => {
            ("up_to_date", false, format!("当前 {current} 已是最新版本"))
        }
        Some(std::cmp::Ordering::Greater) => (
            "ahead",
            false,
            format!(
                "当前 {current} 高于对比版本 {}（开发版或回滚场景）",
                latest.clone().unwrap_or_default()
            ),
        ),
        None => {
            problems.push("无法比较版本，请手动核对发布说明".to_string());
            (
                "unknown",
                false,
                format!("当前 {current}；未能确定可用版本"),
            )
        }
    };
    let message = if problems.is_empty() {
        message
    } else {
        format!("{message}；{}", problems.join("；"))
    };

    Ok(Json(UpgradeStatus {
        current_version: current,
        latest_version: latest,
        update_available,
        status,
        source,
        checked_at: Utc::now().to_rfc3339(),
        message,
        pending_migrations: pending.len() as i64,
    }))
}

fn upgrade_feed_url() -> Option<String> {
    std::env::var("OPENLLM_UPGRADE_FEED_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Accept `v0.4.0` as well as `0.4.0`: release tags usually carry the prefix.
fn normalize_version(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
}

/// Whether a version string is plain dotted numbers. `compare_versions` treats
/// anything else as "inconclusive", which must not be reported as "up to
/// date": a typo in the release tag would otherwise look like good news.
fn is_numeric_version(value: &str) -> bool {
    let value = normalize_version(value);
    !value.is_empty()
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Compare dotted numeric versions. Missing segments count as zero, so `0.4`
/// and `0.4.0` are equal; a non-numeric segment makes the comparison
/// inconclusive (`None`) rather than wrong.
pub(crate) fn compare_versions(current: &str, latest: &str) -> std::cmp::Ordering {
    fn parts(value: &str) -> Option<Vec<u64>> {
        // Release tags usually carry a `v` prefix; strip it here so a caller
        // that forgot to normalize still compares numerically.
        let value = normalize_version(value);
        value
            .split(['.', '-', '+'])
            .map(|part| {
                if part.is_empty() {
                    Some(0)
                } else {
                    part.parse::<u64>().ok()
                }
            })
            .collect()
    }
    let (Some(mut current), Some(mut latest)) = (parts(current), parts(latest)) else {
        return std::cmp::Ordering::Equal;
    };
    while current.len() < latest.len() {
        current.push(0);
    }
    while latest.len() < current.len() {
        latest.push(0);
    }
    current.cmp(&latest)
}

async fn fetch_latest_version(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    let body: Value = response.json().await?;
    let version = body
        .get("tag_name")
        .or_else(|| body.get("version"))
        .or_else(|| body.get("latest"))
        .and_then(Value::as_str)
        .map(normalize_version)
        .filter(|version| !version.is_empty());
    version.ok_or_else(|| anyhow::anyhow!("响应里没有 tag_name/version 字段"))
}

#[cfg(test)]
#[path = "../../tests/unit/lifecycle.rs"]
mod tests;
