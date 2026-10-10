use super::*;

pub async fn list_models(State(state): State<AppState>) -> AppResult<Json<Vec<PublicModel>>> {
    let routes = crate::registry::route_models(&state.pool).await?;
    let synced = crate::registry::synced_models(&state.pool).await?;
    let auto_summary = crate::registry::auto_model_summary(&state.pool).await?;
    let from = std::time::UNIX_EPOCH
        .elapsed()
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let mut by_id = BTreeMap::new();
    for model in routes {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created: from,
                owned_by: "openllm",
                provider: None,
                upstream_model: None,
                capabilities: model.capabilities,
                target_count: Some(model.target_count),
                limits_verified: Some(!model.incomplete),
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_completion_tokens: None,
                display_name: model.display_name,
                supported_endpoints: model.supported_endpoints,
            }
            .with_flat_limits(),
        );
    }
    if let Some(summary) = auto_summary {
        for (id, display_name, _) in crate::registry::AUTO_MODELS {
            by_id.entry(id.to_string()).or_insert(
                PublicModel {
                    id: id.to_string(),
                    object: "model",
                    created: from,
                    owned_by: "openllm",
                    provider: None,
                    upstream_model: None,
                    capabilities: None,
                    target_count: Some(summary.target_count),
                    limits_verified: Some(false),
                    context_length: None,
                    max_input_tokens: None,
                    max_output_tokens: None,
                    max_completion_tokens: None,
                    display_name: Some(display_name.to_string()),
                    supported_endpoints: summary.supported_endpoints.clone(),
                }
                .with_flat_limits(),
            );
        }
    }
    for model in synced {
        by_id.entry(model.id.clone()).or_insert(
            PublicModel {
                id: model.id,
                object: "model",
                created: from,
                owned_by: "openllm",
                provider: Some(model.provider_name),
                upstream_model: Some(model.upstream_model),
                capabilities: model.capabilities,
                target_count: None,
                limits_verified: None,
                context_length: None,
                max_input_tokens: None,
                max_output_tokens: None,
                max_completion_tokens: None,
                display_name: model.display_name,
                supported_endpoints: model.supported_endpoints,
            }
            .with_flat_limits(),
        );
    }
    Ok(Json(by_id.into_values().collect()))
}
