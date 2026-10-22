use super::*;
use crate::models::{ContextAgentUsage, ContextAnalytics, ContextBreakdown, ContextCategoryUsage};
use futures_util::TryStreamExt;
use sqlx::FromRow;
use std::collections::HashMap;

const CONTEXT_CATEGORY_ORDER: [&str; 7] = [
    "system",
    "conversation",
    "tool_results",
    "tools",
    "tool_calls",
    "reasoning",
    "unattributed",
];

#[derive(Debug, FromRow)]
struct AgentRow {
    api_key_id: Option<i64>,
    label: String,
    requests: i64,
    sessions: i64,
    context_samples: i64,
    measured_samples: i64,
    estimated_samples: i64,
    prompt_tokens: i64,
    avg_prompt_tokens: f64,
    max_prompt_tokens: i64,
    cache_read: i64,
    last_seen: String,
}

#[derive(Debug, FromRow)]
struct ModelRow {
    api_key_id: Option<i64>,
    model: String,
}

#[derive(Debug, FromRow)]
struct TotalsRow {
    requests: i64,
    sessions: i64,
    context_requests: i64,
    measured_requests: i64,
    estimated_requests: i64,
}

#[derive(Default)]
struct ContextAggregate {
    categories: HashMap<String, (i64, i64)>,
    unmetered_items: i64,
    opaque_reasoning_items: i64,
}

impl ContextAggregate {
    fn add(&mut self, context: ContextBreakdown) {
        for category in context.categories {
            let entry = self.categories.entry(category.key).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(category.tokens.max(0));
            entry.1 = entry
                .1
                .saturating_add(i64::try_from(category.items).unwrap_or(i64::MAX));
        }
        self.unmetered_items = self
            .unmetered_items
            .saturating_add(i64::try_from(context.unmetered_items).unwrap_or(i64::MAX));
        self.opaque_reasoning_items = self
            .opaque_reasoning_items
            .saturating_add(i64::try_from(context.opaque_reasoning_items).unwrap_or(i64::MAX));
    }

    fn categories(self) -> Vec<ContextCategoryUsage> {
        let mut categories = self
            .categories
            .into_iter()
            .map(|(key, (tokens, items))| ContextCategoryUsage { key, tokens, items })
            .collect::<Vec<_>>();
        categories.sort_by(|left, right| {
            let left_order = category_order(&left.key);
            let right_order = category_order(&right.key);
            left_order
                .cmp(&right_order)
                .then_with(|| right.tokens.cmp(&left.tokens))
                .then_with(|| left.key.cmp(&right.key))
        });
        categories
    }
}

fn category_order(key: &str) -> usize {
    CONTEXT_CATEGORY_ORDER
        .iter()
        .position(|candidate| *candidate == key)
        .unwrap_or(CONTEXT_CATEGORY_ORDER.len())
}

fn cache_hit_rate(prompt_tokens: i64, cache_read: i64) -> f64 {
    if prompt_tokens <= 0 {
        return 0.0;
    }
    (cache_read as f64 / prompt_tokens as f64 * 100.0).clamp(0.0, 100.0)
}

/// Aggregate the already-persisted context breakdown without exposing prompt
/// content. This stays a separate query from overview so opening the analytics
/// landing page does not pay for JSON parsing across the selected range.
pub async fn analytics_context(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> AppResult<Json<ContextAnalytics>> {
    let window = overview_window(&query)?;
    let range_start_text = window.range_start.to_rfc3339();
    let range_end_text = window.range_end.to_rfc3339();
    let filters = query.filters();
    let filters = &filters;

    let mut totals_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT COUNT(*) AS requests,
               COUNT(DISTINCT u.session_id) AS sessions,
               COALESCE(SUM(CASE WHEN u.context_breakdown_json IS NOT NULL
                                      AND TRIM(u.context_breakdown_json) <> ''
                                 THEN 1 ELSE 0 END), 0) AS context_requests,
               COALESCE(SUM(CASE WHEN json_valid(u.context_breakdown_json)
                                      AND json_extract(
                                          u.context_breakdown_json,
                                          '$.input_estimated'
                                      ) = 0
                                 THEN 1 ELSE 0 END), 0) AS measured_requests,
               COALESCE(SUM(CASE WHEN json_valid(u.context_breakdown_json)
                                      AND json_extract(
                                          u.context_breakdown_json,
                                          '$.input_estimated'
                                      ) = 1
                                 THEN 1 ELSE 0 END), 0) AS estimated_requests
        FROM usage_logs u
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(&mut totals_sql, filters, &range_start_text, &range_end_text);

    let mut agents_sql = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT u.api_key_id AS api_key_id,
               COALESCE(k.name, '匿名调用') AS label,
               COUNT(*) AS requests,
               COUNT(DISTINCT u.session_id) AS sessions,
               COALESCE(SUM(CASE WHEN u.context_breakdown_json IS NOT NULL
                                      AND TRIM(u.context_breakdown_json) <> ''
                                 THEN 1 ELSE 0 END), 0) AS context_samples,
               COALESCE(SUM(CASE WHEN json_valid(u.context_breakdown_json)
                                      AND json_extract(
                                          u.context_breakdown_json,
                                          '$.input_estimated'
                                      ) = 0
                                 THEN 1 ELSE 0 END), 0) AS measured_samples,
               COALESCE(SUM(CASE WHEN json_valid(u.context_breakdown_json)
                                      AND json_extract(
                                          u.context_breakdown_json,
                                          '$.input_estimated'
                                      ) = 1
                                 THEN 1 ELSE 0 END), 0) AS estimated_samples,
               COALESCE(SUM(u.prompt_tokens), 0) AS prompt_tokens,
               COALESCE(AVG(u.prompt_tokens), 0.0) AS avg_prompt_tokens,
               COALESCE(MAX(u.prompt_tokens), 0) AS max_prompt_tokens,
               COALESCE(SUM(u.cache_read_tokens), 0) AS cache_read,
               MAX(u.created_at) AS last_seen
        FROM usage_logs u
        LEFT JOIN api_keys k ON k.id = u.api_key_id
        WHERE u.in_flight = 0
        "#,
    );
    push_range_where(&mut agents_sql, filters, &range_start_text, &range_end_text);
    agents_sql.push(
        " GROUP BY u.api_key_id, k.name \
          ORDER BY prompt_tokens DESC, requests DESC LIMIT 50",
    );

    let mut models_sql = QueryBuilder::<Sqlite>::new(
        r#"
        WITH ranked AS (
            SELECT u.api_key_id AS api_key_id,
                   u.requested_model AS model,
                   ROW_NUMBER() OVER (
                       PARTITION BY u.api_key_id
                       ORDER BY COUNT(*) DESC, u.requested_model
                   ) AS position
            FROM usage_logs u
            WHERE u.in_flight = 0
        "#,
    );
    push_range_where(&mut models_sql, filters, &range_start_text, &range_end_text);
    models_sql.push(
        " GROUP BY u.api_key_id, u.requested_model \
          ) SELECT api_key_id, model FROM ranked WHERE position <= 3",
    );

    let (totals, agents, models) = tokio::try_join!(
        totals_sql
            .build_query_as::<TotalsRow>()
            .fetch_one(&state.pool),
        agents_sql
            .build_query_as::<AgentRow>()
            .fetch_all(&state.pool),
        models_sql
            .build_query_as::<ModelRow>()
            .fetch_all(&state.pool),
    )?;

    let mut context_rows_sql = QueryBuilder::<Sqlite>::new(
        "SELECT u.api_key_id, u.context_breakdown_json FROM usage_logs u \
         WHERE u.in_flight = 0 \
           AND u.context_breakdown_json IS NOT NULL \
           AND TRIM(u.context_breakdown_json) <> ''",
    );
    push_range_where(
        &mut context_rows_sql,
        filters,
        &range_start_text,
        &range_end_text,
    );
    let mut rows = context_rows_sql.build().fetch(&state.pool);
    let mut context_by_agent: HashMap<i64, ContextAggregate> = HashMap::new();
    while let Some(row) = rows.try_next().await? {
        let api_key_id: Option<i64> = row.try_get("api_key_id")?;
        let raw: String = row.try_get("context_breakdown_json")?;
        let Ok(context) = serde_json::from_str::<ContextBreakdown>(&raw) else {
            continue;
        };
        context_by_agent
            .entry(api_key_id.unwrap_or(0))
            .or_default()
            .add(context);
    }

    let mut models_by_agent: HashMap<i64, Vec<String>> = HashMap::new();
    for row in models {
        models_by_agent
            .entry(row.api_key_id.unwrap_or(0))
            .or_default()
            .push(row.model);
    }

    let agents = agents
        .into_iter()
        .map(|row| {
            let key = row.api_key_id.unwrap_or(0);
            let aggregate = context_by_agent.remove(&key).unwrap_or_default();
            let unmetered_items = aggregate.unmetered_items;
            let opaque_reasoning_items = aggregate.opaque_reasoning_items;
            ContextAgentUsage {
                api_key_id: row.api_key_id,
                label: row.label,
                requests: row.requests,
                sessions: row.sessions,
                context_samples: row.context_samples,
                measured_samples: row.measured_samples,
                estimated_samples: row.estimated_samples,
                prompt_tokens: row.prompt_tokens,
                avg_prompt_tokens: row.avg_prompt_tokens,
                max_prompt_tokens: row.max_prompt_tokens,
                cache_hit_rate: cache_hit_rate(row.prompt_tokens, row.cache_read),
                category_tokens: aggregate.categories(),
                unmetered_items,
                opaque_reasoning_items,
                models: models_by_agent.remove(&key).unwrap_or_default(),
                last_seen: row.last_seen,
            }
        })
        .collect();

    Ok(Json(ContextAnalytics {
        requests: totals.requests,
        sessions: totals.sessions,
        context_requests: totals.context_requests,
        measured_requests: totals.measured_requests,
        estimated_requests: totals.estimated_requests,
        agents,
    }))
}
