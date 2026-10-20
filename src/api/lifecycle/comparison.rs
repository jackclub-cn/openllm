use super::*;

/// `null` in the document means "leave the current value alone": the update
/// handlers cannot clear an optional field, so treating it as a match keeps a
/// repeated import idempotent instead of reporting the same change forever.
pub(super) fn opt_matches<T: PartialEq>(current: Option<T>, document: Option<T>) -> bool {
    match document {
        None => true,
        Some(value) => current == Some(value),
    }
}

pub(super) fn dedup_sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort_by_key(|value| value.to_lowercase());
    values.dedup();
    values
}

pub(super) fn same_set<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len() && left.iter().all(|item| right.contains(item))
}

/// Headers to write, or `None` when the document may not change them.
///
/// When secrets were not exported every credential-looking value is `***`, so
/// applying the document would overwrite a real credential with a mask. In that
/// mode the header map is left untouched entirely.
pub(super) fn headers_to_apply(value: &Value, include_secrets: bool) -> Option<Value> {
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

pub(super) fn header_values_match(current: &Value, document: &Value) -> bool {
    current == document
}

pub(super) fn route_targets_match(
    current: &[(i64, String, i64, i64, bool)],
    desired: &[(i64, String, i64, i64, bool)],
) -> bool {
    current.len() == desired.len() && current.iter().all(|target| desired.contains(target))
}

pub(super) fn pool_allocations_match(
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

pub(super) fn describe_provider_change(
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
