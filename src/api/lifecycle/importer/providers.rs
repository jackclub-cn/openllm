use super::*;
impl<'a> ImportRun<'a> {
    pub(super) async fn facts(&mut self, id: i64) -> AppResult<ProviderFacts> {
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

    pub(super) fn facts_match(&self, facts: &ProviderFacts, doc: &ConfigProvider) -> bool {
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
    pub(super) fn credentials_match(&self, facts: &ProviderFacts, doc: &ConfigProvider) -> bool {
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

    pub(super) fn document_key_names(&self, doc: &ConfigProvider) -> Vec<String> {
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
    pub(super) fn provider_key_inputs(
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

    pub(super) async fn providers(&mut self, providers: &[ConfigProvider]) {
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
    pub(super) async fn apply_model_flags(
        &mut self,
        provider_id: i64,
        doc: &ConfigProvider,
        all: &[String],
    ) {
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
}
