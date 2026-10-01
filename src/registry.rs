//! Read side of the model registry.
//!
//! Both the public OpenAI-compatible `/v1/models` endpoint and the admin
//! console need the same view: every reachable model plus its capability
//! envelope. Keeping the queries here guarantees the two never drift apart.

use std::collections::HashMap;

use serde_json::Value;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::error::AppResult;
use crate::models::ModelCapabilities;

/// A model exposed by a provider, addressed as `prefix + upstream model`.
#[derive(Debug, Clone)]
pub struct SyncedModel {
    pub id: String,
    pub upstream_model: String,
    pub provider_name: String,
    /// Provider-supplied label, when it differs from the model id.
    pub display_name: Option<String>,
    /// When the model row was created, so the Anthropic shape can report a
    /// real `created_at` instead of null.
    pub created_at: Option<String>,
    pub capabilities: Option<ModelCapabilities>,
    /// Effective endpoint paths after applying any manual override.
    pub supported_endpoints: Option<Vec<String>>,
}

/// A route, whose advertised capabilities are the barrel (strictest common)
/// intersection across its enabled targets.
#[derive(Debug, Clone)]
pub struct RouteModel {
    pub id: String,
    /// The route's human name, used as the display label (e.g. "Hermes").
    pub display_name: Option<String>,
    /// When the route was created, so the Anthropic shape can report a real
    /// `created_at` instead of null.
    pub created_at: Option<String>,
    pub target_count: usize,
    pub capabilities: Option<ModelCapabilities>,
    /// Endpoint paths supported by every target. `None` means at least one
    /// target did not declare endpoint metadata.
    pub supported_endpoints: Option<Vec<String>>,
    /// True when at least one target is missing metadata, meaning the
    /// intersection is a lower bound rather than a verified envelope.
    pub incomplete: bool,
}

/// Capability columns, in the order [`CapabilityRow`] declares them. Kept in
/// one place so the struct and its query cannot drift.
const CAPABILITY_COLUMNS: &str = "COALESCE(pm.context_override, pm.context_limit) AS context_limit, \
     COALESCE(pm.output_override, pm.output_limit) AS output_limit, \
     COALESCE(pm.input_override, pm.input_limit) AS input_limit, \
     pm.attachment AS attachment, pm.reasoning AS reasoning, \
     pm.tool_call AS tool_call, pm.structured_output AS structured_output, \
     pm.temperature AS temperature, pm.open_weights AS open_weights, \
     pm.modalities AS modalities, pm.cost AS cost, \
     pm.cost_input_override AS cost_input_override, \
     pm.cost_output_override AS cost_output_override, \
     pm.cost_cache_read_override AS cost_cache_read_override, \
     pm.cost_cache_write_override AS cost_cache_write_override, \
     pm.family AS family, \
     pm.knowledge AS knowledge, pm.release_date AS release_date, \
     pm.last_updated AS last_updated, pm.canonical_model_id AS canonical_model_id";

#[derive(Debug, sqlx::FromRow)]
struct CapabilityRow {
    context_limit: Option<i64>,
    output_limit: Option<i64>,
    input_limit: Option<i64>,
    attachment: Option<i64>,
    reasoning: Option<i64>,
    tool_call: Option<i64>,
    structured_output: Option<i64>,
    temperature: Option<i64>,
    open_weights: Option<i64>,
    modalities: Option<String>,
    cost: Option<String>,
    cost_input_override: Option<f64>,
    cost_output_override: Option<f64>,
    cost_cache_read_override: Option<f64>,
    cost_cache_write_override: Option<f64>,
    family: Option<String>,
    knowledge: Option<String>,
    release_date: Option<String>,
    last_updated: Option<String>,
    canonical_model_id: Option<String>,
}

impl CapabilityRow {
    fn into_capabilities(self) -> Option<ModelCapabilities> {
        let boolean = |value: Option<i64>| value.map(|value| value != 0);
        let cost = parse_json_column(self.cost);
        let capabilities = ModelCapabilities {
            context_limit: self.context_limit,
            output_limit: self.output_limit,
            input_limit: self.input_limit,
            attachment: boolean(self.attachment),
            reasoning: boolean(self.reasoning),
            tool_call: boolean(self.tool_call),
            structured_output: boolean(self.structured_output),
            temperature: boolean(self.temperature),
            open_weights: boolean(self.open_weights),
            input_modalities: stored_modality(&self.modalities, "input"),
            output_modalities: stored_modality(&self.modalities, "output"),
            cost: crate::models::effective_cost_value(
                cost.as_ref(),
                self.cost_input_override,
                self.cost_output_override,
                self.cost_cache_read_override,
                self.cost_cache_write_override,
            ),
            family: self.family,
            knowledge: self.knowledge,
            release_date: self.release_date,
            last_updated: self.last_updated,
            canonical_model_id: self.canonical_model_id,
            total_context_tokens: None,
        };
        // Collapse the window/input distinction at the read boundary so every
        // consumer sees one consistent, conservative input cap even for rows
        // written before this rule existed.
        let capabilities = capabilities.with_effective_input_limit();
        (!capabilities.is_empty()).then_some(capabilities)
    }
}

/// Extracts one modality list from the JSON blob persisted in `provider_models`,
/// whose shape mirrors models.dev (`{"input": [...], "output": [...]}`).
fn stored_modality(raw: &Option<String>, key: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(raw.as_ref()?).ok()?;
    let items = value.get(key)?.as_array()?;
    let values = items
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}

fn parse_json_column(value: Option<String>) -> Option<Value> {
    value
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .filter(|value| !value.is_null())
}

#[derive(Debug, sqlx::FromRow)]
struct SyncedRow {
    id: String,
    upstream_model: String,
    provider_name: String,
    display_name: Option<String>,
    created_at: Option<String>,
    supported_endpoints: Option<String>,
    #[sqlx(flatten)]
    capabilities: CapabilityRow,
}

/// One enabled target of a route, joined to its synced capability metadata.
#[derive(Debug, sqlx::FromRow)]
struct RouteTargetRow {
    route_id: i64,
    supported_endpoints: Option<String>,
    #[sqlx(flatten)]
    capabilities: CapabilityRow,
}

#[derive(Debug, sqlx::FromRow)]
struct TargetCapabilityRow {
    provider_id: i64,
    model_name: String,
    #[sqlx(flatten)]
    capabilities: CapabilityRow,
}

pub async fn synced_models(pool: &SqlitePool) -> AppResult<Vec<SyncedModel>> {
    let query = format!(
        r#"
        SELECT p.model_prefix || pm.model_name AS id,
               pm.model_name AS upstream_model,
               p.name AS provider_name,
               pm.display_name AS display_name,
               pm.created_at AS created_at,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               {CAPABILITY_COLUMNS}
        FROM providers p
        JOIN provider_models pm ON pm.provider_id = p.id AND pm.enabled = 1
        WHERE p.enabled = 1
        ORDER BY p.model_prefix || pm.model_name
        "#
    );
    let rows = sqlx::query_as::<_, SyncedRow>(&query)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| SyncedModel {
            id: row.id,
            upstream_model: row.upstream_model,
            provider_name: row.provider_name,
            display_name: row.display_name,
            created_at: row.created_at,
            capabilities: row.capabilities.into_capabilities(),
            supported_endpoints: parse_endpoints(row.supported_endpoints.as_deref()),
        })
        .collect())
}

/// Barrel envelope for a set of concrete targets: the strictest common
/// capability across all of them, plus whether every target reported metadata.
#[derive(Debug, Clone)]
pub struct BarrelEnvelope {
    pub capabilities: Option<ModelCapabilities>,
    /// True when at least one target has no metadata, so `capabilities` is a
    /// lower bound rather than a verified envelope.
    pub incomplete: bool,
    pub target_count: usize,
}

pub async fn barrel_for_targets(
    pool: &SqlitePool,
    targets: &[(i64, String)],
) -> AppResult<BarrelEnvelope> {
    let mut found = HashMap::<(i64, String), ModelCapabilities>::new();
    for chunk in targets.chunks(256) {
        let mut query = QueryBuilder::<Sqlite>::new(format!(
            "SELECT pm.provider_id, pm.model_name, {CAPABILITY_COLUMNS} \
             FROM provider_models pm WHERE pm.enabled = 1 AND ("
        ));
        for (index, (provider_id, model_name)) in chunk.iter().enumerate() {
            if index > 0 {
                query.push(" OR ");
            }
            query
                .push("(pm.provider_id = ")
                .push_bind(*provider_id)
                .push(" AND pm.model_name = ")
                .push_bind(model_name.clone())
                .push(")");
        }
        query.push(")");
        for row in query
            .build_query_as::<TargetCapabilityRow>()
            .fetch_all(pool)
            .await?
        {
            if let Some(capabilities) = row.capabilities.into_capabilities() {
                found.insert((row.provider_id, row.model_name), capabilities);
            }
        }
    }

    let mut known = Vec::with_capacity(targets.len());
    let mut incomplete = false;
    for target in targets {
        match found.get(target) {
            Some(capabilities) => known.push(capabilities.clone()),
            None => incomplete = true,
        }
    }
    let capabilities = ModelCapabilities::intersect(known.iter());
    Ok(BarrelEnvelope {
        capabilities,
        incomplete,
        target_count: targets.len(),
    })
}

pub async fn route_models(pool: &SqlitePool) -> AppResult<Vec<RouteModel>> {
    let routes = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT id, model_pattern, name, created_at FROM routes WHERE enabled = 1 \
         ORDER BY model_pattern COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;

    let query = format!(
        r#"
        SELECT rt.route_id,
               COALESCE(pm.supported_endpoints_override, pm.supported_endpoints)
                   AS supported_endpoints,
               {CAPABILITY_COLUMNS}
        FROM route_targets rt
        JOIN routes r ON r.id = rt.route_id AND r.enabled = 1
        JOIN providers p ON p.id = rt.provider_id AND p.enabled = 1
        LEFT JOIN provider_models pm
               ON pm.provider_id = rt.provider_id
              AND pm.model_name = rt.upstream_model
              AND pm.enabled = 1
        WHERE rt.enabled = 1
          AND NOT EXISTS (
              SELECT 1 FROM provider_models disabled
              WHERE disabled.provider_id = rt.provider_id
                AND disabled.model_name = rt.upstream_model
                AND disabled.enabled = 0
          )
        ORDER BY rt.route_id, rt.id
        "#
    );
    let mut targets_by_route = HashMap::<i64, Vec<RouteTargetRow>>::new();
    for target in sqlx::query_as::<_, RouteTargetRow>(&query)
        .fetch_all(pool)
        .await?
    {
        targets_by_route
            .entry(target.route_id)
            .or_default()
            .push(target);
    }

    let mut models = Vec::with_capacity(routes.len());
    for (route_id, pattern, route_name, route_created_at) in routes {
        let targets = targets_by_route.remove(&route_id).unwrap_or_default();
        let target_count = targets.len();
        if target_count == 0 {
            continue;
        }
        let capabilities = targets
            .iter()
            .map(|row| capability_from_row(&row.capabilities))
            .collect::<Vec<_>>();
        let supported_endpoints = intersect_endpoints(
            targets
                .iter()
                .map(|row| parse_endpoints(row.supported_endpoints.as_deref())),
        );
        // A target without metadata widens the true envelope, so the
        // intersection is only a lower bound in that case.
        let incomplete = capabilities.iter().any(Option::is_none);
        let known = capabilities.iter().flatten().collect::<Vec<_>>();
        models.push(RouteModel {
            id: pattern,
            display_name: Some(route_name),
            created_at: Some(route_created_at),
            target_count,
            capabilities: ModelCapabilities::intersect(known),
            supported_endpoints,
            incomplete,
        });
    }
    Ok(models)
}

/// Clones a borrowed row into owned capabilities. The rows come back by value
/// in `route_models`, this keeps the intersection helper borrowing-friendly.
fn capability_from_row(row: &CapabilityRow) -> Option<ModelCapabilities> {
    let boolean = |value: Option<i64>| value.map(|value| value != 0);
    let cost = row
        .cost
        .as_ref()
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    let capabilities = ModelCapabilities {
        context_limit: row.context_limit,
        output_limit: row.output_limit,
        input_limit: row.input_limit,
        attachment: boolean(row.attachment),
        reasoning: boolean(row.reasoning),
        tool_call: boolean(row.tool_call),
        structured_output: boolean(row.structured_output),
        temperature: boolean(row.temperature),
        open_weights: boolean(row.open_weights),
        input_modalities: stored_modality(&row.modalities, "input"),
        output_modalities: stored_modality(&row.modalities, "output"),
        cost: crate::models::effective_cost_value(
            cost.as_ref(),
            row.cost_input_override,
            row.cost_output_override,
            row.cost_cache_read_override,
            row.cost_cache_write_override,
        ),
        family: row.family.clone(),
        knowledge: row.knowledge.clone(),
        release_date: row.release_date.clone(),
        last_updated: row.last_updated.clone(),
        canonical_model_id: row.canonical_model_id.clone(),
        total_context_tokens: None,
    };
    let capabilities = capabilities.with_effective_input_limit();
    (!capabilities.is_empty()).then_some(capabilities)
}

fn parse_endpoints(raw: Option<&str>) -> Option<Vec<String>> {
    let endpoints = raw
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())?
        .into_iter()
        .map(|endpoint| endpoint.trim().trim_end_matches('/').to_string())
        .filter(|endpoint| !endpoint.is_empty())
        .collect::<Vec<_>>();
    (!endpoints.is_empty()).then_some(endpoints)
}

fn intersect_endpoints(
    endpoints: impl IntoIterator<Item = Option<Vec<String>>>,
) -> Option<Vec<String>> {
    let mut declared = Vec::new();
    for endpoints in endpoints {
        let endpoints = endpoints?;
        declared.push(
            endpoints
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
        );
    }
    let mut declared = declared.into_iter();
    let mut result = declared.next()?;
    for endpoints in declared {
        result = result.intersection(&endpoints).cloned().collect();
    }
    (!result.is_empty()).then(|| result.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn route_models_batches_targets_and_keeps_barrel_limits() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        for (id, name) in [(1, "Primary"), (2, "Backup")] {
            sqlx::query(
                "INSERT INTO providers (id, name, provider_type, base_url)
                 VALUES (?, ?, 'openai', 'https://example.com/v1')",
            )
            .bind(id)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        }
        for (provider_id, model_name, context_limit, output_limit) in [
            (1, "primary-model", 100_000, 50_000),
            (2, "backup-model", 80_000, 40_000),
        ] {
            sqlx::query(
                "INSERT INTO provider_models (
                    provider_id, model_name, enabled, context_limit, output_limit
                 ) VALUES (?, ?, 1, ?, ?)",
            )
            .bind(provider_id)
            .bind(model_name)
            .bind(context_limit)
            .bind(output_limit)
            .execute(&pool)
            .await
            .unwrap();
        }
        for (id, pattern) in [(1, "barrel-model"), (2, "single-model")] {
            sqlx::query(
                "INSERT INTO routes (id, name, model_pattern, strategy, enabled)
                 VALUES (?, ?, ?, 'priority', 1)",
            )
            .bind(id)
            .bind(pattern)
            .bind(pattern)
            .execute(&pool)
            .await
            .unwrap();
        }
        for (route_id, provider_id, upstream_model) in [
            (1, 1, "primary-model"),
            (1, 2, "backup-model"),
            (2, 1, "primary-model"),
        ] {
            sqlx::query(
                "INSERT INTO route_targets (
                    route_id, provider_id, upstream_model, weight, priority, enabled
                 ) VALUES (?, ?, ?, 100, 0, 1)",
            )
            .bind(route_id)
            .bind(provider_id)
            .bind(upstream_model)
            .execute(&pool)
            .await
            .unwrap();
        }

        let models = route_models(&pool).await.unwrap();
        let barrel = models
            .iter()
            .find(|model| model.id == "barrel-model")
            .unwrap();
        let single = models
            .iter()
            .find(|model| model.id == "single-model")
            .unwrap();
        assert_eq!(barrel.target_count, 2);
        assert_eq!(
            barrel.capabilities.as_ref().unwrap().context_limit,
            Some(80_000)
        );
        assert_eq!(
            barrel.capabilities.as_ref().unwrap().output_limit,
            Some(40_000)
        );
        assert_eq!(single.target_count, 1);
        assert_eq!(
            single.capabilities.as_ref().unwrap().context_limit,
            Some(100_000)
        );

        let direct = barrel_for_targets(
            &pool,
            &[
                (1, "primary-model".to_string()),
                (2, "backup-model".to_string()),
            ],
        )
        .await
        .unwrap();
        assert!(!direct.incomplete);
        assert_eq!(direct.target_count, 2);
        assert_eq!(
            direct.capabilities.as_ref().unwrap().context_limit,
            Some(80_000)
        );

        let incomplete = barrel_for_targets(
            &pool,
            &[
                (1, "primary-model".to_string()),
                (99, "missing-model".to_string()),
            ],
        )
        .await
        .unwrap();
        assert!(incomplete.incomplete);
        assert_eq!(incomplete.target_count, 2);
        assert_eq!(
            incomplete.capabilities.as_ref().unwrap().context_limit,
            Some(100_000)
        );
    }
}
