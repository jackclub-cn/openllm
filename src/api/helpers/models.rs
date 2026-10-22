use super::*;

/// Wraps plain model names (manually curated lists, Ollama) with empty upstream
/// metadata so they share one insert path with synced entries.
pub(in crate::api) fn names_to_entries(models: &[String]) -> Vec<(String, UpstreamModelInfo)> {
    models
        .iter()
        .map(|name| (name.clone(), UpstreamModelInfo::default()))
        .collect()
}

pub(super) enum ModelAutoIncludeMatcher {
    Glob(globset::GlobMatcher),
    Regex(regex::Regex),
}

pub(super) fn compile_model_auto_include_rule(rule: &str) -> AppResult<ModelAutoIncludeMatcher> {
    if let Some(pattern) = rule.strip_prefix("re:") {
        if pattern.is_empty() {
            return Err(AppError::BadRequest(
                "model auto-include regex must not be empty".to_string(),
            ));
        }
        return regex::Regex::new(pattern)
            .map(ModelAutoIncludeMatcher::Regex)
            .map_err(|error| {
                AppError::BadRequest(format!("invalid model auto-include regex: {error}"))
            });
    }
    globset::GlobBuilder::new(rule)
        .literal_separator(false)
        .build()
        .map(|glob| ModelAutoIncludeMatcher::Glob(glob.compile_matcher()))
        .map_err(|error| AppError::BadRequest(format!("invalid model auto-include glob: {error}")))
}

pub(super) fn compile_model_auto_include_rules(
    rules: &[String],
) -> AppResult<Vec<ModelAutoIncludeMatcher>> {
    rules
        .iter()
        .map(|rule| compile_model_auto_include_rule(rule))
        .collect()
}

pub(in crate::api) fn normalize_model_auto_include_rules(
    rules: &[String],
) -> AppResult<Vec<String>> {
    if rules.len() > 64 {
        return Err(AppError::BadRequest(
            "at most 64 model auto-include rules are allowed".to_string(),
        ));
    }
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for raw in rules {
        let rule = raw.trim();
        if rule.is_empty() {
            continue;
        }
        if rule.chars().count() > 256 {
            return Err(AppError::BadRequest(
                "model auto-include rules must be at most 256 characters".to_string(),
            ));
        }
        if seen.insert(rule.to_string()) {
            normalized.push(rule.to_string());
        }
    }
    compile_model_auto_include_rules(&normalized)?;
    Ok(normalized)
}

pub(super) fn model_matches_auto_include_rules(
    model: &str,
    rules: &[ModelAutoIncludeMatcher],
) -> bool {
    rules.iter().any(|rule| match rule {
        ModelAutoIncludeMatcher::Glob(glob) => glob.is_match(model),
        ModelAutoIncludeMatcher::Regex(regex) => regex.is_match(model),
    })
}

/// Serialises the split modality fields back into the nested shape used for
/// storage, keeping one canonical on-disk representation.
pub(in crate::api) fn modalities_to_storage(capabilities: &ModelCapabilities) -> Option<String> {
    if capabilities.input_modalities.is_none() && capabilities.output_modalities.is_none() {
        return None;
    }
    let mut value = serde_json::Map::new();
    if let Some(input) = &capabilities.input_modalities {
        value.insert("input".to_string(), json!(input));
    }
    if let Some(output) = &capabilities.output_modalities {
        value.insert("output".to_string(), json!(output));
    }
    serde_json::to_string(&Value::Object(value)).ok()
}

pub(in crate::api) async fn replace_provider_models(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    models: &[(String, UpstreamModelInfo)],
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
) -> AppResult<()> {
    write_provider_models(tx, provider_id, models, catalog, provider_hint, true, &[])
        .await
        .map(|_| ())
}

pub(in crate::api) async fn merge_provider_models(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    models: &[(String, UpstreamModelInfo)],
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
    auto_include_rules: &[String],
) -> AppResult<usize> {
    write_provider_models(
        tx,
        provider_id,
        models,
        catalog,
        provider_hint,
        false,
        auto_include_rules,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn write_provider_models(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider_id: i64,
    models: &[(String, UpstreamModelInfo)],
    catalog: Option<&models_dev::Catalog>,
    provider_hint: Option<&str>,
    replace: bool,
    auto_include_rules: &[String],
) -> AppResult<usize> {
    let existing_overrides = sqlx::query_as::<_, ProviderModelOverride>(
        "SELECT model_name, enabled, context_override, input_override, output_override, \
                supported_endpoints_override, cost_input_override, cost_output_override, \
                cost_cache_read_override, cost_cache_write_override \
         FROM provider_models WHERE provider_id = ?",
    )
    .bind(provider_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| (row.model_name.clone(), row))
    .collect::<HashMap<_, _>>();
    if replace {
        sqlx::query("DELETE FROM provider_models WHERE provider_id = ?")
            .bind(provider_id)
            .execute(&mut **tx)
            .await?;
    }
    let auto_include_matchers = if replace {
        Vec::new()
    } else {
        compile_model_auto_include_rules(auto_include_rules)?
    };
    let mut seen = HashSet::new();
    let mut written = 0;
    let synced_at = Utc::now().to_rfc3339();
    for (model, upstream) in models {
        let model = model.trim();
        if model.is_empty() || !seen.insert(model.to_string()) {
            continue;
        }
        if !replace
            && !existing_overrides.contains_key(model)
            && !model_matches_auto_include_rules(model, &auto_include_matchers)
        {
            continue;
        }
        let found = catalog.and_then(|catalog| catalog.lookup(provider_hint, model));
        let has_capabilities = found.is_some();
        let capabilities = found.unwrap_or_default().with_effective_input_limit();
        // The provider's own context window is more trustworthy than a
        // models.dev guess, and is the only source for models models.dev lacks.
        // A provider may publish both a total window and a smaller accepted
        // input ceiling; keep the strictest value so clients never see an
        // optimistic limit.
        let context_limit = min_known(upstream.context_limit, capabilities.context_limit);
        let input_limit = min_known(context_limit, capabilities.input_limit);
        let overrides = existing_overrides.get(model).cloned().unwrap_or_default();
        sqlx::query(
            r#"
            INSERT INTO provider_models (
                provider_id, model_name, enabled, context_limit, output_limit,
                input_limit, attachment, reasoning, reasoning_efforts,
                tool_call, structured_output,
                temperature, open_weights, modalities, cost, family, knowledge,
                release_date, last_updated, canonical_model_id, capabilities_synced_at,
                upstream_context_limit, supported_endpoints, display_name,
                context_override, input_override, output_override,
                supported_endpoints_override, cost_input_override, cost_output_override,
                cost_cache_read_override, cost_cache_write_override
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(provider_id, model_name) DO UPDATE SET
                enabled = excluded.enabled,
                context_limit = excluded.context_limit,
                output_limit = excluded.output_limit,
                input_limit = excluded.input_limit,
                attachment = excluded.attachment,
                reasoning = excluded.reasoning,
                reasoning_efforts = excluded.reasoning_efforts,
                tool_call = excluded.tool_call,
                structured_output = excluded.structured_output,
                temperature = excluded.temperature,
                open_weights = excluded.open_weights,
                modalities = excluded.modalities,
                cost = excluded.cost,
                family = excluded.family,
                knowledge = excluded.knowledge,
                release_date = excluded.release_date,
                last_updated = excluded.last_updated,
                canonical_model_id = excluded.canonical_model_id,
                capabilities_synced_at = excluded.capabilities_synced_at,
                upstream_context_limit = excluded.upstream_context_limit,
                supported_endpoints = excluded.supported_endpoints,
                display_name = excluded.display_name,
                context_override = excluded.context_override,
                input_override = excluded.input_override,
                output_override = excluded.output_override,
                supported_endpoints_override = excluded.supported_endpoints_override,
                cost_input_override = excluded.cost_input_override,
                cost_output_override = excluded.cost_output_override,
                cost_cache_read_override = excluded.cost_cache_read_override,
                cost_cache_write_override = excluded.cost_cache_write_override
            "#,
        )
        .bind(provider_id)
        .bind(model)
        .bind(overrides.enabled.unwrap_or(1))
        .bind(context_limit)
        .bind(capabilities.output_limit)
        .bind(input_limit)
        .bind(capabilities.attachment.map(i64::from))
        .bind(capabilities.reasoning.map(i64::from))
        .bind(
            capabilities
                .reasoning_efforts
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .unwrap_or_default(),
        )
        .bind(capabilities.tool_call.map(i64::from))
        .bind(capabilities.structured_output.map(i64::from))
        .bind(capabilities.temperature.map(i64::from))
        .bind(capabilities.open_weights.map(i64::from))
        // Persisted in models.dev's nested shape; the read path flattens it.
        .bind(modalities_to_storage(&capabilities))
        .bind(
            capabilities
                .cost
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .unwrap_or_default(),
        )
        .bind(capabilities.family)
        .bind(capabilities.knowledge)
        .bind(capabilities.release_date)
        .bind(capabilities.last_updated)
        .bind(capabilities.canonical_model_id)
        .bind((has_capabilities || upstream.context_limit.is_some()).then_some(synced_at.clone()))
        .bind(upstream.context_limit)
        .bind(
            (!upstream.supported_endpoints.is_empty())
                .then(|| serde_json::to_string(&upstream.supported_endpoints))
                .transpose()
                .unwrap_or_default(),
        )
        .bind(upstream.display_name.clone())
        .bind(overrides.context_override)
        .bind(overrides.input_override)
        .bind(overrides.output_override)
        .bind(overrides.supported_endpoints_override)
        .bind(overrides.cost_input_override)
        .bind(overrides.cost_output_override)
        .bind(overrides.cost_cache_read_override)
        .bind(overrides.cost_cache_write_override)
        .execute(&mut **tx)
        .await?;
        written += 1;
    }
    Ok(written)
}

/// Extra facts a provider reports about a model in its own `/models` response.
///
/// These are kept separate from models.dev metadata so the upstream's own
/// numbers can win: a provider knows its real context window even when
/// models.dev has never heard of the model.
#[derive(Debug, Clone, Default)]
pub(crate) struct UpstreamModelInfo {
    pub(crate) context_limit: Option<i64>,
    pub(crate) supported_endpoints: Vec<String>,
    /// Provider-supplied label, e.g. "DeepSeek V4.1 Flash".
    pub(crate) display_name: Option<String>,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub(in crate::api) struct ProviderModelOverride {
    pub(super) model_name: String,
    pub(super) enabled: Option<i64>,
    pub(super) context_override: Option<i64>,
    pub(super) input_override: Option<i64>,
    pub(super) output_override: Option<i64>,
    pub(super) supported_endpoints_override: Option<String>,
    pub(super) cost_input_override: Option<f64>,
    pub(super) cost_output_override: Option<f64>,
    pub(super) cost_cache_read_override: Option<f64>,
    pub(super) cost_cache_write_override: Option<f64>,
}

/// Reads every common context/input spelling and keeps the strictest value.
///
/// OpenAI-compatible providers are inconsistent here: some expose
/// `context_length`, others `context_window`, and several publish both a total
/// window and a smaller `max_input_tokens`. Taking the minimum keeps the
/// gateway conservative when those fields disagree.
pub(in crate::api) fn upstream_context_limit(model: &Value) -> Option<i64> {
    [
        "context_length",
        "context_window",
        "context_size",
        "max_input_tokens",
        "max_context_window",
        "max_context_tokens",
    ]
    .iter()
    .filter_map(|key| model.get(*key).and_then(Value::as_i64))
    .filter(|value| *value > 0)
    .min()
}

pub(in crate::api) fn min_known(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// Parses an OpenAI-style model list, preserving each entry's name and any
/// upstream-reported limits.
pub(in crate::api) fn parse_openai_model_entries(
    value: &Value,
) -> Vec<(String, UpstreamModelInfo)> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model
                .get("id")
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)?
                .trim();
            if name.is_empty() {
                return None;
            }
            let context_limit = upstream_context_limit(model);
            let supported_endpoints = model
                .get("supported_endpoints")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            // Providers commonly send the friendly label as `name`. Only keep
            // it when it adds information beyond the id itself.
            let display_name = model
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty() && *label != name)
                .map(ToOwned::to_owned);
            Some((
                name.to_string(),
                UpstreamModelInfo {
                    context_limit,
                    supported_endpoints,
                    display_name,
                },
            ))
        })
        .collect()
}

pub(in crate::api) fn parse_ollama_models(value: &Value) -> Vec<String> {
    value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("name").or_else(|| model.get("model")))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}
