use super::*;

pub(super) async fn check_circuit_persistence(state: &AppState) -> SelfCheckItem {
    const ID: &str = "circuit_persistence";
    const CATEGORY: &str = "resilience";
    const TITLE: &str = "熔断与冷却状态持久化";
    if !state.resilience_state_persist {
        return warn(
            ID,
            CATEGORY,
            TITLE,
            "已禁用（OPENLLM_RESILIENCE_STATE_PERSIST=0）：重启后熔断状态清空",
            "重启会立刻把流量打回刚失败的提供商；高可用部署建议保持开启",
        );
    }
    let pending = state
        .resilience_dirty
        .lock()
        .map(|dirty| dirty.providers.len() + dirty.targets.len() + dirty.provider_keys.len())
        .unwrap_or(0);
    pass(
        ID,
        CATEGORY,
        TITLE,
        format!("已开启；当前 {pending} 项状态等待写回 SQLite"),
    )
}

pub(super) async fn check_provider_health(state: &AppState) -> SelfCheckItem {
    const ID: &str = "provider_health";
    const CATEGORY: &str = "resilience";
    const TITLE: &str = "启用的上游提供商可路由";
    let enabled = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM providers WHERE enabled = 1")
        .fetch_one(&state.pool)
        .await;
    let enabled = match enabled {
        Ok(enabled) => enabled,
        Err(error) => {
            return fail(
                ID,
                CATEGORY,
                TITLE,
                format!("读取提供商失败：{error}"),
                "检查数据库状态",
            );
        }
    };
    if enabled == 0 {
        return fail(
            ID,
            CATEGORY,
            TITLE,
            "没有启用的提供商，所有请求都会以 502 失败",
            "在控制台「提供商」页启用至少一个上游提供商",
        );
    }

    let threshold = crate::state::provider_open_threshold();
    let now = std::time::Instant::now();
    let cooling = state
        .provider_cooldown
        .lock()
        .await
        .iter()
        .filter(|(_, until)| **until > now)
        .map(|(provider_id, _)| *provider_id)
        .collect::<HashSet<_>>();
    let streaks = state.provider_failure_streak.lock().await.clone();
    let (mut open, mut half_open, mut cooling_only) = (0usize, 0usize, 0usize);
    for provider_id in &cooling {
        match crate::state::provider_circuit_state(
            streaks.get(provider_id).copied().unwrap_or(0),
            true,
            threshold,
        ) {
            "open" => open += 1,
            "half_open" => half_open += 1,
            _ => cooling_only += 1,
        }
    }
    let unavailable = open + half_open + cooling_only;
    let detail = format!(
        "启用 {enabled} 个提供商；熔断打开 {open}、探测中 {half_open}、软冷却 {cooling_only}（阈值 {threshold}）"
    );
    if unavailable >= enabled as usize {
        fail(
            ID,
            CATEGORY,
            TITLE,
            format!("{detail}；所有启用的提供商都不可路由"),
            "查看「设置 → 冷却管理」确认原因，修复上游或清空冷却后重试",
        )
    } else if unavailable > 0 {
        warn(
            ID,
            CATEGORY,
            TITLE,
            detail,
            "仍有可用提供商在承接流量；确认上游故障是否已恢复",
        )
    } else {
        pass(ID, CATEGORY, TITLE, detail)
    }
}

pub(super) async fn check_provider_credentials(state: &AppState) -> SelfCheckItem {
    const ID: &str = "provider_credentials";
    const CATEGORY: &str = "resilience";
    const TITLE: &str = "上游凭据可用";
    // Whether an upstream needs credentials is not knowable from here: a local
    // vLLM/Ollama endpoint legitimately has none, and a provider may carry its
    // auth in a custom header instead of a key. So a missing credential set is
    // reported as a warning (it breaks requests against most hosted upstreams)
    // and the check only escalates when keys exist but all of them are broken.
    let rows = sqlx::query_as::<_, (i64, String, String, i64)>(
        "SELECT p.id, COALESCE(p.api_key, ''), COALESCE(p.headers, ''), \
            (SELECT COUNT(*) FROM provider_api_keys k \
                WHERE k.provider_id = p.id AND k.enabled = 1) \
         FROM providers p WHERE p.enabled = 1 ORDER BY p.id",
    )
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            return fail(
                ID,
                CATEGORY,
                TITLE,
                format!("读取上游凭据失败：{error}"),
                "检查数据库状态",
            );
        }
    };
    let providers = rows.len();
    let with_material = rows
        .iter()
        .filter(|(_, api_key, headers, keys)| {
            !api_key.trim().is_empty() || *keys > 0 || headers_carry_a_credential(headers)
        })
        .count();
    let stats = sqlx::query_as::<_, (i64, i64)>(
        "SELECT \
            (SELECT COUNT(*) FROM provider_api_keys k \
                JOIN providers p ON p.id = k.provider_id \
                WHERE k.enabled = 1 AND p.enabled = 1), \
            (SELECT COUNT(*) FROM provider_api_keys k \
                JOIN providers p ON p.id = k.provider_id \
                WHERE k.enabled = 1 AND p.enabled = 1 AND k.last_error IS NOT NULL)",
    )
    .fetch_one(&state.pool)
    .await;
    let (usable, errored) = match stats {
        Ok(row) => row,
        Err(error) => {
            return fail(
                ID,
                CATEGORY,
                TITLE,
                format!("读取上游凭据失败：{error}"),
                "检查数据库状态",
            );
        }
    };
    if providers == 0 {
        return pass(ID, CATEGORY, TITLE, "没有启用的提供商，无需检查凭据");
    }
    if usable == 0 {
        if with_material == 0 {
            return warn(
                ID,
                CATEGORY,
                TITLE,
                format!("{providers} 个启用的提供商都没有配置上游凭据"),
                "免鉴权的本地上游可忽略；若上游需要鉴权，请求会返回 401/403",
            );
        }
        return pass(
            ID,
            CATEGORY,
            TITLE,
            format!("{providers} 个启用的提供商通过自定义请求头或旧版字段提供凭据"),
        );
    }
    if errored >= usable {
        return warn(
            ID,
            CATEGORY,
            TITLE,
            format!("{usable} 个可用凭据全部带最近一次错误记录"),
            "在「提供商」页运行密钥测试，确认上游凭据是否已失效",
        );
    }
    pass(
        ID,
        CATEGORY,
        TITLE,
        format!("{usable} 个可用凭据（{errored} 个带最近错误记录）"),
    )
}

/// True when a provider's custom headers already carry an authenticating value.
pub(super) fn headers_carry_a_credential(headers: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(headers) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    object
        .iter()
        .any(|(name, value)| is_secret_header(name) && !value.is_null())
}
