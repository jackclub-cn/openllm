use super::*;

pub(super) async fn check_routes(state: &AppState) -> SelfCheckItem {
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

pub(super) async fn check_gateway_keys(state: &AppState) -> SelfCheckItem {
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
