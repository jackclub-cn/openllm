use super::*;

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

pub(super) const ACTION_CREATE: &str = "create";
pub(super) const ACTION_UPDATE: &str = "update";
pub(super) const ACTION_UNCHANGED: &str = "unchanged";
pub(super) const ACTION_SKIP: &str = "skip";
pub(super) const ACTION_ERROR: &str = "error";

type ProviderKeyChannel = (
    String,
    bool,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);
type RouteLookup = (
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    i64,
    i64,
    i64,
    i64,
);

/// A provider as it currently exists, reduced to the fields the importer
/// compares. Secrets are loaded only when the document carries secrets.
pub(super) struct ProviderFacts {
    pub(super) provider_type: String,
    pub(super) base_url: String,
    pub(super) model_prefix: String,
    pub(super) enabled: bool,
    pub(super) health_check_interval_minutes: Option<i64>,
    pub(super) health_check_model: Option<String>,
    pub(super) models_sync_interval_minutes: Option<i64>,
    pub(super) model_auto_include_rules: Vec<String>,
    pub(super) timeout_seconds: Option<i64>,
    pub(super) configured_cooldown_seconds: Option<i64>,
    pub(super) max_concurrency: Option<i64>,
    pub(super) queue_timeout_seconds: Option<i64>,
    pub(super) models_enabled: Vec<String>,
    pub(super) models_disabled: Vec<String>,
    pub(super) key_names: Vec<String>,
    pub(super) headers: Value,
    pub(super) legacy_api_key: Option<String>,
    /// Every secret per key name: provider key names are not unique (only the
    /// secret is), so a name can map to more than one secret.
    pub(super) key_secrets: HashMap<String, Vec<String>>,
    /// Existing key rows by secret, used to keep a key's identity (and with it
    /// its lifetime statistics and usage attribution) across an import.
    pub(super) key_ids_by_secret: HashMap<String, i64>,
    pub(super) key_channels: Vec<ProviderKeyChannel>,
}

/// A webhook as it currently exists, reduced to the fields the importer
/// compares. The signing secret is loaded only when the document carries
/// secrets; `kind` and `body_fields` are configuration and always compared.
pub(super) struct WebhookFacts {
    pub(super) url: String,
    pub(super) enabled: bool,
    pub(super) event_types: Vec<String>,
    pub(super) headers: Value,
    pub(super) kind: String,
    pub(super) body_fields: Value,
    pub(super) secret: Option<String>,
}

pub(super) struct ImportRun<'a> {
    pub(super) state: &'a AppState,
    pub(super) apply: bool,
    pub(super) secrets_included: bool,
    pub(super) counts: ImportCounts,
    pub(super) items: Vec<ImportItem>,
    pub(super) problems: Vec<String>,
    /// A blocking problem (an unusable document version) stops every write;
    /// advisory problems only skip the affected item.
    pub(super) blocked: bool,
    pub(super) providers: HashMap<String, i64>,
    pub(super) api_keys: HashMap<String, Vec<i64>>,
    pub(super) routes: Vec<RouteLookup>,
    pub(super) pools: Vec<(i64, i64, String)>,
    pub(super) webhooks: Vec<(i64, String, String)>,
}

impl<'a> ImportRun<'a> {
    pub(super) fn new(state: &'a AppState, apply: bool, secrets_included: bool) -> Self {
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

    pub(super) async fn load_lookups(&mut self) -> AppResult<()> {
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

    pub(super) fn record(
        &mut self,
        kind: &'static str,
        name: &str,
        action: &'static str,
        detail: String,
    ) {
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

    pub(super) fn finish(self, document_version: i64) -> ImportPlan {
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

mod pools;
mod providers;
mod routes;
mod settings;
mod webhooks;
