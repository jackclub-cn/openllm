use super::*;

pub(super) async fn check_database(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_migrations(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_instance_lease(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_shutdown_config(state: &AppState) -> SelfCheckItem {
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
