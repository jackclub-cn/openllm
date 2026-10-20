use super::*;

pub(super) async fn check_capacity(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_runtime_lag(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_maintenance(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_webhooks(state: &AppState) -> SelfCheckItem {
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
