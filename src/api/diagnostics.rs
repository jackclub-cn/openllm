use super::*;
use std::sync::atomic::Ordering::Relaxed;

/// A check that passed and needs no operator attention.
const CHECK_PASS: &str = "pass";
/// A check that works but is configured in a way an operator should know about.
const CHECK_WARN: &str = "warn";
/// A check that will break requests (or already is).
const CHECK_FAIL: &str = "fail";

/// How many failed requests a snapshot carries.
const MAX_SNAPSHOT_FAILURES: i64 = 20;
/// How many characters of an error/warning message a snapshot keeps. Upstream
/// error bodies can be large and are not worth carrying whole.
const MAX_SNAPSHOT_MESSAGE_CHARS: usize = 400;

/// Aggregated result of the operator-facing self-check.
#[derive(Debug, serde::Serialize)]
pub struct SelfCheckReport {
    pub generated_at: String,
    /// `fail` if any check failed, else `warn` if any check warned, else `pass`.
    pub overall: &'static str,
    pub counts: SelfCheckCounts,
    pub checks: Vec<SelfCheckItem>,
}

#[derive(Debug, serde::Serialize)]
pub struct SelfCheckCounts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

/// One structured self-check result.
///
/// Every item answers three questions an on-call operator asks first: what was
/// checked (`title`), what the gateway observed (`detail`), and what to do about
/// it (`hint`). `id` is stable so a runbook can grep for it.
#[derive(Debug, serde::Serialize)]
pub struct SelfCheckItem {
    pub id: &'static str,
    pub category: &'static str,
    pub title: &'static str,
    pub status: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

fn check(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    status: &'static str,
    detail: impl Into<String>,
    hint: Option<&'static str>,
) -> SelfCheckItem {
    SelfCheckItem {
        id,
        category,
        title,
        status,
        detail: detail.into(),
        hint,
    }
}

fn pass(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    detail: impl Into<String>,
) -> SelfCheckItem {
    check(id, category, title, CHECK_PASS, detail, None)
}

fn warn(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    detail: impl Into<String>,
    hint: &'static str,
) -> SelfCheckItem {
    check(id, category, title, CHECK_WARN, detail, Some(hint))
}

fn fail(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    detail: impl Into<String>,
    hint: &'static str,
) -> SelfCheckItem {
    check(id, category, title, CHECK_FAIL, detail, Some(hint))
}

/// Runs every check and aggregates the verdict.
///
/// The endpoint never fails: a broken database becomes a failing check instead
/// of an error response, because a support engineer asking "what is wrong?" must
/// still get an answer when the answer is "the database is unreachable".
pub async fn self_check(State(state): State<AppState>) -> Json<SelfCheckReport> {
    Json(run_self_check(&state).await)
}

pub(crate) async fn run_self_check(state: &AppState) -> SelfCheckReport {
    let mut checks = Vec::new();
    checks.push(check_database(state).await);
    checks.push(check_migrations(state).await);
    checks.push(check_instance_lease(state).await);
    checks.push(check_shutdown_config(state).await);
    checks.push(check_circuit_persistence(state).await);
    checks.push(check_provider_health(state).await);
    checks.push(check_provider_credentials(state).await);
    checks.push(check_routes(state).await);
    checks.push(check_gateway_keys(state).await);
    checks.push(check_capacity(state).await);
    checks.push(check_runtime_lag(state).await);
    checks.push(check_maintenance(state).await);
    checks.push(check_webhooks(state).await);

    let mut counts = SelfCheckCounts {
        pass: 0,
        warn: 0,
        fail: 0,
    };
    for item in &checks {
        match item.status {
            CHECK_FAIL => counts.fail += 1,
            CHECK_WARN => counts.warn += 1,
            _ => counts.pass += 1,
        }
    }
    let overall = if counts.fail > 0 {
        CHECK_FAIL
    } else if counts.warn > 0 {
        CHECK_WARN
    } else {
        CHECK_PASS
    };
    SelfCheckReport {
        generated_at: Utc::now().to_rfc3339(),
        overall,
        counts,
        checks,
    }
}

async fn check_database(state: &AppState) -> SelfCheckItem {
    const ID: &str = "database";
    const CATEGORY: &str = "startup";
    const TITLE: &str = "数据库可读写";
    match load_database_stats(state).await {
        Ok(stats) => {
            let detail = format!(
                "SQLite 正常；请求日志 {} 行、审计 {} 行；文件 {:.1} MiB（内部空闲 {:.1} MiB）",
                stats.usage_logs,
                stats.audit_logs,
                stats.size_bytes as f64 / 1_048_576.0,
                stats.free_bytes as f64 / 1_048_576.0,
            );
            // A very large request log is the one database problem the gateway
            // can see on its own: it slows writes and every backup. Disk space
            // itself is the host's business, not SQLite's page counters.
            if stats.usage_logs > 5_000_000 {
                warn(
                    ID,
                    CATEGORY,
                    TITLE,
                    detail,
                    "清理历史请求日志或调低 OPENLLM_USAGE_RETENTION_DAYS",
                )
            } else {
                pass(ID, CATEGORY, TITLE, detail)
            }
        }
        Err(error) => fail(
            ID,
            CATEGORY,
            TITLE,
            format!("数据库查询失败：{error}"),
            "检查数据目录权限、磁盘空间与 SQLite 文件是否被其他进程独占",
        ),
    }
}

async fn check_migrations(state: &AppState) -> SelfCheckItem {
    const ID: &str = "migrations";
    const CATEGORY: &str = "startup";
    const TITLE: &str = "数据库结构已是最新";
    let migrator = sqlx::migrate!("./migrations");
    match crate::startup::pending_migrations(&state.pool, &migrator).await {
        Ok(pending) if pending.is_empty() => pass(
            ID,
            CATEGORY,
            TITLE,
            format!("{} 个迁移全部已应用", migrator.iter().count()),
        ),
        Ok(pending) => {
            let list = pending
                .iter()
                .map(|migration| format!("{} {}", migration.version, migration.description))
                .collect::<Vec<_>>()
                .join(", ");
            fail(
                ID,
                CATEGORY,
                TITLE,
                format!("存在 {} 个待执行迁移：{list}", pending.len()),
                "重启进程会自动执行迁移；先确认已生成迁移前备份，且没有第二个实例在写同一数据库",
            )
        }
        Err(error) => fail(
            ID,
            CATEGORY,
            TITLE,
            format!("读取迁移状态失败：{error}"),
            "确认 _sqlx_migrations 表存在且可读",
        ),
    }
}

async fn check_instance_lease(state: &AppState) -> SelfCheckItem {
    const ID: &str = "instance_lease";
    const CATEGORY: &str = "startup";
    const TITLE: &str = "实例租约唯一且心跳新鲜";
    let ttl = crate::startup::lease_ttl_secs();
    let rows = sqlx::query_as::<_, (String, i64, String, i64)>(
        "SELECT id, pid, host, \
                CAST((julianday('now') - julianday(heartbeat_at)) * 86400 AS INTEGER) \
         FROM instance_leases ORDER BY heartbeat_at DESC",
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
                format!("读取实例租约失败：{error}"),
                "确认 instance_leases 表存在（启动时会自动创建）",
            );
        }
    };
    let host = crate::startup::host_name();
    match rows.as_slice() {
        [] => warn(
            ID,
            CATEGORY,
            TITLE,
            "未发现实例租约行，无法确认当前进程是否独占数据库",
            "重启进程会重新申领租约",
        ),
        [(id, pid, lease_host, age)] => {
            let detail = format!(
                "租约 {id} 由 {lease_host}(pid {pid}) 持有，心跳 {} 秒前（TTL {ttl} 秒）",
                age.max(&0)
            );
            if *age > ttl {
                warn(
                    ID,
                    CATEGORY,
                    TITLE,
                    format!("{detail}；心跳已超过 TTL"),
                    "若进程仍在运行说明心跳任务被阻塞，请查看日志中的 instance lease 心跳错误",
                )
            } else if lease_host != &host {
                warn(
                    ID,
                    CATEGORY,
                    TITLE,
                    format!("{detail}；租约主机与当前主机 {host} 不一致"),
                    "确认是否挂载了其他实例的数据目录",
                )
            } else {
                pass(ID, CATEGORY, TITLE, detail)
            }
        }
        _ => warn(
            ID,
            CATEGORY,
            TITLE,
            format!("存在 {} 条实例租约，可能有异常退出的实例", rows.len()),
            "确认没有第二个进程指向同一数据库；过期租约会在下次启动时被接管",
        ),
    }
}

async fn check_shutdown_config(state: &AppState) -> SelfCheckItem {
    const ID: &str = "shutdown_config";
    const CATEGORY: &str = "startup";
    const TITLE: &str = "排空与超时配置";
    let limits = state.runtime_limits();
    let mut warnings = Vec::new();
    if limits.shutdown_grace_secs == 0 {
        warnings.push("排空等待无上限（OPENLLM_SHUTDOWN_GRACE_SECS=0），停止时会一直等在途请求");
    }
    if limits.stream_max_secs.is_none() && limits.request_timeout_secs.is_none() {
        warnings.push("未设置上游流总时长与请求总时长上限，慢上游可能长期占用并发槽位");
    }
    let detail = format!(
        "排空等待 {} 秒；上游空闲超时 {} 秒；流总时长 {}；请求总时长 {}",
        limits.shutdown_grace_secs,
        limits.upstream_idle_timeout_secs,
        limits
            .stream_max_secs
            .map(|value| format!("{value} 秒"))
            .unwrap_or_else(|| "不限".to_string()),
        limits
            .request_timeout_secs
            .map(|value| format!("{value} 秒"))
            .unwrap_or_else(|| "不限".to_string()),
    );
    if warnings.is_empty() {
        pass(ID, CATEGORY, TITLE, detail)
    } else {
        warn(
            ID,
            CATEGORY,
            TITLE,
            format!("{detail}；{}", warnings.join("；")),
            "按 docs/DEPLOYMENT.md 的升级清单核对排空与超时参数",
        )
    }
}

async fn check_circuit_persistence(state: &AppState) -> SelfCheckItem {
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

async fn check_provider_health(state: &AppState) -> SelfCheckItem {
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

async fn check_provider_credentials(state: &AppState) -> SelfCheckItem {
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
fn headers_carry_a_credential(headers: &str) -> bool {
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

async fn check_routes(state: &AppState) -> SelfCheckItem {
    const ID: &str = "routes";
    const CATEGORY: &str = "routing";
    const TITLE: &str = "路由与目标模型可解析";
    let row = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT \
            (SELECT COUNT(*) FROM routes WHERE enabled = 1), \
            (SELECT COUNT(*) FROM route_targets t \
                JOIN routes r ON r.id = t.route_id \
                JOIN providers p ON p.id = t.provider_id \
                WHERE t.enabled = 1 AND r.enabled = 1 AND p.enabled = 1), \
            (SELECT COUNT(*) FROM provider_models pm \
                JOIN providers p ON p.id = pm.provider_id \
                WHERE pm.enabled = 1 AND p.enabled = 1)",
    )
    .fetch_one(&state.pool)
    .await;
    let (routes, targets, models) = match row {
        Ok(row) => row,
        Err(error) => {
            return fail(
                ID,
                CATEGORY,
                TITLE,
                format!("读取路由失败：{error}"),
                "检查数据库状态",
            );
        }
    };
    let detail =
        format!("启用路由 {routes} 条 · 启用路由目标 {targets} 个 · 可路由模型 {models} 个");
    // With no explicit route/target row the gateway still routes by model
    // straight to any enabled provider exposing it (verified against the real
    // binary), so the only genuinely fatal state is having nothing to route to.
    if models == 0 {
        fail(
            ID,
            CATEGORY,
            TITLE,
            format!("{detail}；没有任何可路由的模型，请求会以 502 失败"),
            "在「提供商」页同步或添加模型，并确认提供商处于启用状态",
        )
    } else if routes == 0 {
        pass(
            ID,
            CATEGORY,
            TITLE,
            format!("{detail}；未配置路由规则，请求按模型名直接选择提供商"),
        )
    } else {
        pass(ID, CATEGORY, TITLE, detail)
    }
}

async fn check_gateway_keys(state: &AppState) -> SelfCheckItem {
    const ID: &str = "gateway_keys";
    const CATEGORY: &str = "routing";
    const TITLE: &str = "网关访问密钥";
    let row =
        sqlx::query_as::<_, (i64, i64)>("SELECT COUNT(*), COALESCE(SUM(enabled), 0) FROM api_keys")
            .fetch_one(&state.pool)
            .await;
    let (total, enabled) = match row {
        Ok(row) => row,
        Err(error) => {
            return fail(
                ID,
                CATEGORY,
                TITLE,
                format!("读取访问密钥失败：{error}"),
                "检查数据库状态",
            );
        }
    };
    match (total, enabled) {
        (0, _) => warn(
            ID,
            CATEGORY,
            TITLE,
            "没有配置任何访问密钥：网关对调用方不做鉴权",
            "公网或共享环境请创建访问密钥；本地自用可忽略",
        ),
        (_, 0) => fail(
            ID,
            CATEGORY,
            TITLE,
            format!("已配置 {total} 个访问密钥但全部被禁用，所有代理请求都会 401"),
            "在「访问密钥」页启用或新建一个密钥",
        ),
        _ => pass(
            ID,
            CATEGORY,
            TITLE,
            format!("{enabled}/{total} 个访问密钥可用"),
        ),
    }
}

async fn check_capacity(state: &AppState) -> SelfCheckItem {
    const ID: &str = "capacity";
    const CATEGORY: &str = "capacity";
    const TITLE: &str = "准入与内存保护";
    let limits = state.runtime_limits();
    let in_flight =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_logs WHERE in_flight = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
    let memory_limit = state
        .memory_pressure
        .as_ref()
        .map(|pressure| pressure.limit_bytes())
        .unwrap_or(0);
    let memory_used = state
        .memory_pressure
        .as_ref()
        .and_then(|pressure| pressure.used_bytes())
        .or_else(crate::state::sample_process_memory_bytes)
        .unwrap_or(0);
    let pressure = state
        .memory_pressure
        .as_ref()
        .is_some_and(|pressure| pressure.is_shedding());
    let detail = format!(
        "在途请求 {in_flight}/{}；内存 {:.1}/{:.1} MiB（来源 {}）；准入等待 {} ms；并发卸载 {}、字节卸载 {}、内存卸载 {}",
        if limits.max_concurrent_requests == 0 {
            "不限".to_string()
        } else {
            limits.max_concurrent_requests.to_string()
        },
        memory_used as f64 / 1_048_576.0,
        memory_limit as f64 / 1_048_576.0,
        state.memory_limit_source,
        limits.admission_wait_ms,
        state.requests_shed.load(Relaxed),
        state.request_bytes_shed.load(Relaxed),
        state.memory_shed.load(Relaxed),
    );
    if limits.max_concurrent_requests == 0 && state.memory_pressure.is_none() {
        warn(
            ID,
            CATEGORY,
            TITLE,
            format!("{detail}；并发上限与内存保护均未启用"),
            "设置 OPENLLM_MAX_CONCURRENT_REQUESTS 与 OPENLLM_MEMORY_LIMIT_MIB，避免突发流量耗尽内存",
        )
    } else if pressure {
        warn(
            ID,
            CATEGORY,
            TITLE,
            format!("{detail}；当前处于内存压力状态，正在卸载请求"),
            "降低并发上限、调低请求体上限或提升内存上限",
        )
    } else {
        pass(ID, CATEGORY, TITLE, detail)
    }
}

async fn check_runtime_lag(state: &AppState) -> SelfCheckItem {
    const ID: &str = "runtime_lag";
    const CATEGORY: &str = "capacity";
    const TITLE: &str = "事件循环延迟";
    let lag = state.runtime_lag_millis.load(Relaxed);
    let detail = format!("最近一次采样的运行时阻塞 {lag} ms");
    if lag > 20_000 {
        fail(
            ID,
            CATEGORY,
            TITLE,
            detail,
            "事件循环长时间阻塞会使健康检查与排空超时；检查是否有同步阻塞调用或磁盘 IO 卡顿",
        )
    } else if lag > 5_000 {
        warn(
            ID,
            CATEGORY,
            TITLE,
            detail,
            "观察是否与维护任务或大请求日志清理同时发生",
        )
    } else {
        pass(ID, CATEGORY, TITLE, detail)
    }
}

async fn check_maintenance(state: &AppState) -> SelfCheckItem {
    const ID: &str = "maintenance";
    const CATEGORY: &str = "observability";
    const TITLE: &str = "维护任务运行中";
    let runs = state.maintenance_runs.load(Relaxed);
    let last_run = state.maintenance_last_run_unix.load(Relaxed);
    let uptime = state.started_at.elapsed().as_secs();
    let now = Utc::now().timestamp().max(0) as u64;
    let age = if last_run == 0 {
        None
    } else {
        Some(now.saturating_sub(last_run))
    };
    let detail = format!(
        "已运行 {runs} 次；最近一次 {}（进程已运行 {uptime} 秒）",
        age.map(|age| format!("{age} 秒前"))
            .unwrap_or_else(|| "从未运行".to_string())
    );
    if runs == 0 && uptime > 300 {
        fail(
            ID,
            CATEGORY,
            TITLE,
            detail,
            "维护任务每 60 秒运行一次；查看日志中的 maintenance 任务错误",
        )
    } else if age.is_some_and(|age| age > 300) {
        warn(
            ID,
            CATEGORY,
            TITLE,
            detail,
            "维护任务可能被阻塞；查看日志中的 maintenance 错误或运行时阻塞告警",
        )
    } else {
        pass(ID, CATEGORY, TITLE, detail)
    }
}

async fn check_webhooks(state: &AppState) -> SelfCheckItem {
    const ID: &str = "webhooks";
    const CATEGORY: &str = "observability";
    const TITLE: &str = "Webhook 投递";
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT \
            (SELECT COUNT(*) FROM webhooks WHERE enabled = 1), \
            (SELECT COUNT(*) FROM webhook_deliveries \
                WHERE error IS NOT NULL \
                  AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day'))",
    )
    .fetch_one(&state.pool)
    .await;
    let (enabled, failed) = match row {
        Ok(row) => row,
        Err(error) => {
            return fail(
                ID,
                CATEGORY,
                TITLE,
                format!("读取 Webhook 状态失败：{error}"),
                "检查数据库状态",
            );
        }
    };
    if enabled == 0 {
        return pass(ID, CATEGORY, TITLE, "没有启用的 Webhook");
    }
    let detail = format!("启用 {enabled} 个 Webhook；最近 24 小时失败投递 {failed} 次");
    if failed > 0 {
        warn(
            ID,
            CATEGORY,
            TITLE,
            detail,
            "在「Webhook」页查看投递记录与错误原因，确认接收端可达",
        )
    } else {
        pass(ID, CATEGORY, TITLE, detail)
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct SnapshotQuery {
    /// Opt-in: include upstream provider credentials in plaintext. Off by
    /// default so a snapshot can be attached to a ticket safely.
    #[serde(default)]
    pub include_secrets: bool,
}

/// A support-ready bundle of everything an operator has to gather by hand today.
#[derive(Debug, serde::Serialize)]
pub struct DiagnosticSnapshot {
    pub generated_at: String,
    pub version: &'static str,
    pub host: String,
    pub draining: bool,
    pub uptime_seconds: u64,
    pub instance_lease: Option<LeaseView>,
    pub status: RuntimeStatus,
    pub limits: RuntimeLimits,
    pub settings: SettingsView,
    pub self_check: SelfCheckReport,
    pub resilience: CooldownSnapshot,
    pub pools: Vec<crate::allocation::PoolUsageView>,
    pub providers: Vec<ProviderView>,
    pub recent_failures: Vec<FailureView>,
    pub requests_24h: RequestOutcomeSummary,
    /// Whether `secrets` is populated. Always explicit so a reader can tell a
    /// redacted file from a secret-bearing one.
    pub secrets_included: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<SnapshotSecrets>,
    pub notes: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct LeaseView {
    pub id: String,
    pub pid: i64,
    pub host: String,
    pub started_at: String,
    pub heartbeat_at: String,
    pub heartbeat_age_seconds: i64,
    /// Whether the lease belongs to this host (the only supported topology).
    pub ours: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct FailureView {
    pub request_id: String,
    pub created_at: String,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub provider_id: Option<i64>,
    pub status_code: i64,
    pub latency_ms: i64,
    pub error_message: Option<String>,
    pub warning_message: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct RequestOutcomeSummary {
    pub total: i64,
    pub successful: i64,
    pub failed: i64,
    /// Failed requests per HTTP status code, most frequent first.
    pub failures_by_status: Vec<StatusCount>,
}

#[derive(Debug, serde::Serialize)]
pub struct StatusCount {
    pub status_code: i64,
    pub count: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct SnapshotSecrets {
    pub provider_credentials: Vec<ProviderCredentialSecret>,
    pub provider_headers: Vec<ProviderHeaderSecret>,
}

#[derive(Debug, serde::Serialize)]
pub struct ProviderCredentialSecret {
    pub provider_id: i64,
    pub provider_name: String,
    pub key_id: i64,
    pub key_name: String,
    pub secret: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ProviderHeaderSecret {
    pub provider_id: i64,
    pub provider_name: String,
    pub headers: Value,
}

/// Exports the diagnostic snapshot.
///
/// Redaction is the default: upstream credentials and provider headers are
/// dropped, and request/response previews never enter the file at all. Passing
/// `include_secrets=true` is an explicit, audited action.
pub async fn diagnostic_snapshot(
    State(state): State<AppState>,
    Query(query): Query<SnapshotQuery>,
) -> AppResult<Json<DiagnosticSnapshot>> {
    let limits = state.runtime_limits();
    let generated_at = Utc::now().to_rfc3339();

    let mut providers = sqlx::query_as::<_, Provider>(
        "SELECT * FROM providers ORDER BY enabled DESC, name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(ProviderView::from)
    .collect::<Vec<_>>();
    hydrate_provider_views(&state, &mut providers).await?;
    let raw_headers = providers
        .iter()
        .map(|provider| (provider.id, provider.name.clone(), provider.headers.clone()))
        .collect::<Vec<_>>();
    if !query.include_secrets {
        for provider in &mut providers {
            provider.headers = redact_headers(&provider.headers);
        }
    }

    let lease = load_lease_view(&state).await;
    let (secrets, legacy_secrets) = if query.include_secrets {
        let (secrets, legacy) = load_secrets(&state, raw_headers).await?;
        record_audit(
            &state,
            "export",
            "diagnostics",
            None,
            "导出了包含明文上游凭据的诊断快照",
            None,
        )
        .await;
        (Some(secrets), legacy)
    } else {
        (None, 0)
    };

    let notes = if query.include_secrets {
        let mut notes = vec![
            "本文件包含明文上游凭据，请按机密文件处理，用完即删。".to_string(),
            "请求与响应预览、提示词内容不在快照内。".to_string(),
        ];
        if legacy_secrets > 0 {
            notes.push(format!(
                "另有 {legacy_secrets} 条凭据仅存在于旧版 providers.api_key 字段，以 key_id=0 导出。"
            ));
        }
        notes
    } else {
        vec![
            "上游凭据仅以 api_key_set/后缀形式出现，不含明文；带上 include_secrets=true 才会导出明文（会记入审计日志）。".to_string(),
            "请求与响应预览、提示词内容不在快照内。".to_string(),
        ]
    };

    Ok(Json(DiagnosticSnapshot {
        generated_at,
        version: env!("CARGO_PKG_VERSION"),
        host: crate::startup::host_name(),
        draining: state.shutting_down.load(Relaxed),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        instance_lease: lease,
        status: collect_status(&state).await,
        limits,
        settings: SettingsView {
            admin_auth_enabled: state.admin_token.is_some(),
            database: "sqlite",
            version: env!("CARGO_PKG_VERSION"),
            database_stats: load_database_stats(&state).await?,
        },
        self_check: run_self_check(&state).await,
        resilience: cooldown_snapshot(&state).await?,
        pools: crate::allocation::all_pool_usage(&state)
            .await
            .unwrap_or_default(),
        providers,
        recent_failures: load_recent_failures(&state).await,
        requests_24h: load_request_outcomes(&state).await,
        secrets_included: secrets.is_some(),
        secrets,
        notes,
    }))
}

async fn load_lease_view(state: &AppState) -> Option<LeaseView> {
    let host = crate::startup::host_name();
    sqlx::query_as::<_, (String, i64, String, String, String, i64)>(
        "SELECT id, pid, host, started_at, heartbeat_at, \
                CAST((julianday('now') - julianday(heartbeat_at)) * 86400 AS INTEGER) \
         FROM instance_leases ORDER BY heartbeat_at DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .map(
        |(id, pid, lease_host, started_at, heartbeat_at, heartbeat_age_seconds)| LeaseView {
            ours: lease_host == host,
            id,
            pid,
            host: lease_host,
            started_at,
            heartbeat_at,
            heartbeat_age_seconds,
        },
    )
}

async fn load_recent_failures(state: &AppState) -> Vec<FailureView> {
    sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            i64,
            i64,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT request_id, created_at, requested_model, upstream_model, provider_id, \
                status_code, latency_ms, error_message, warning_message \
         FROM usage_logs WHERE success = 0 ORDER BY id DESC LIMIT ?",
    )
    .bind(MAX_SNAPSHOT_FAILURES)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(
        |(
            request_id,
            created_at,
            requested_model,
            upstream_model,
            provider_id,
            status_code,
            latency_ms,
            error_message,
            warning_message,
        )| FailureView {
            request_id,
            created_at,
            requested_model,
            upstream_model,
            provider_id,
            status_code,
            latency_ms,
            error_message: truncate_message(error_message),
            warning_message: truncate_message(warning_message),
        },
    )
    .collect()
}

async fn load_request_outcomes(state: &AppState) -> RequestOutcomeSummary {
    let totals = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), COALESCE(SUM(success), 0) FROM usage_logs \
         WHERE created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or((0, 0));
    let failures_by_status = sqlx::query_as::<_, (i64, i64)>(
        "SELECT status_code, COUNT(*) FROM usage_logs \
         WHERE success = 0 \
           AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day') \
         GROUP BY status_code ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(status_code, count)| StatusCount { status_code, count })
    .collect();
    RequestOutcomeSummary {
        total: totals.0,
        successful: totals.1,
        failed: totals.0.saturating_sub(totals.1),
        failures_by_status,
    }
}

async fn load_secrets(
    state: &AppState,
    headers: Vec<(i64, String, Value)>,
) -> AppResult<(SnapshotSecrets, i64)> {
    let mut provider_credentials = sqlx::query_as::<_, (i64, String, i64, String, String)>(
        "SELECT p.id, p.name, k.id, k.name, k.secret \
         FROM provider_api_keys k JOIN providers p ON p.id = k.provider_id \
         ORDER BY p.id, k.id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(
        |(provider_id, provider_name, key_id, key_name, secret)| ProviderCredentialSecret {
            provider_id,
            provider_name,
            key_id,
            key_name,
            secret,
        },
    )
    .collect::<Vec<_>>();
    // Any key created through the API is mirrored into provider_api_keys, but a
    // database carried forward from an older version can still hold a secret in
    // the legacy column only. Export it too (key_id 0) so an operator never
    // restores a provider from a snapshot without its credential.
    let legacy = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, name, api_key FROM providers \
         WHERE COALESCE(api_key, '') <> '' ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut legacy_exported = 0;
    for (provider_id, provider_name, secret) in legacy {
        if provider_credentials
            .iter()
            .any(|item| item.provider_id == provider_id && item.secret == secret)
        {
            continue;
        }
        legacy_exported += 1;
        provider_credentials.push(ProviderCredentialSecret {
            provider_id,
            provider_name,
            key_id: 0,
            key_name: "旧版 api_key 字段".to_string(),
            secret,
        });
    }
    let provider_headers = headers
        .into_iter()
        .map(
            |(provider_id, provider_name, headers)| ProviderHeaderSecret {
                provider_id,
                provider_name,
                headers,
            },
        )
        .collect();
    Ok((
        SnapshotSecrets {
            provider_credentials,
            provider_headers,
        },
        legacy_exported,
    ))
}

fn truncate_message(message: Option<String>) -> Option<String> {
    message.map(|message| {
        if message.chars().count() <= MAX_SNAPSHOT_MESSAGE_CHARS {
            message
        } else {
            let head = message
                .chars()
                .take(MAX_SNAPSHOT_MESSAGE_CHARS)
                .collect::<String>();
            format!("{head}…")
        }
    })
}

/// Replaces the value of any header that looks like a credential.
///
/// Providers let operators add arbitrary upstream headers, and a gateway
/// operator may well have put a token there. Keys are matched
/// case-insensitively on the usual credential names; everything else is kept so
/// the snapshot stays useful.
pub(crate) fn redact_headers(headers: &Value) -> Value {
    match headers {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(name, value)| {
                    if is_secret_header(name) {
                        (name.clone(), Value::String("***".to_string()))
                    } else {
                        (name.clone(), redact_headers(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_headers).collect()),
        other => other.clone(),
    }
}

fn is_secret_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "api-key",
        "apikey",
        "token",
        "secret",
        "cookie",
    ]
    .iter()
    .any(|needle| name.contains(needle))
}

#[cfg(test)]
#[path = "../../tests/unit/diagnostics.rs"]
mod tests;
