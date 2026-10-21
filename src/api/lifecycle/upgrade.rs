use super::*;

/// Compare the running version with a newer one.
///
/// `?latest=` makes the check testable and usable on an air-gapped host; when
/// it is absent `OPENLLM_UPGRADE_FEED_URL` is fetched (a GitHub-release shaped
/// document with `tag_name` or `version`). `?official=true` opts into the
/// project's stable GitHub releases when no custom feed is configured.
pub async fn upgrade_check(
    State(state): State<AppState>,
    Query(query): Query<UpgradeCheckQuery>,
) -> AppResult<Json<UpgradeStatus>> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let pending = crate::startup::pending_migrations(&state.pool, &sqlx::migrate!("./migrations"))
        .await
        .map_err(AppError::Internal)?;
    let feed_url = upgrade_feed_url().or_else(|| {
        query
            .official
            .then(|| crate::release_feed::OFFICIAL_RELEASE_FEED.to_string())
    });

    let (latest, source, mut problems) = match query
        .latest
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(version) => (Some(normalize_version(version)), "query", Vec::new()),
        None => match feed_url {
            Some(url) => match state.release_feed.latest(&state.client, &url).await {
                Ok(version) => (Some(normalize_version(&version)), "feed", Vec::new()),
                Err(error) => (
                    None,
                    "feed",
                    vec![format!("无法从 {url} 获取最新版本：{error}")],
                ),
            },
            None => (
                None,
                "none",
                vec![
                    "未提供 latest 参数、未设置 OPENLLM_UPGRADE_FEED_URL，也未启用官方发布源，无法判断是否有新版本"
                        .to_string(),
                ],
            ),
        },
    };

    let comparison = match latest.as_deref() {
        Some(latest) if !is_numeric_version(latest) => {
            problems.push(format!("无法解析版本号 '{latest}'，请手动核对发布说明"));
            None
        }
        Some(latest) => Some(compare_versions(&current, latest)),
        None => None,
    };
    let (status, update_available, message) = match comparison {
        Some(std::cmp::Ordering::Less) => (
            "update_available",
            true,
            format!(
                "有新版本可用：当前 {current}，最新 {}{}",
                latest.clone().unwrap_or_default(),
                if pending.is_empty() {
                    String::new()
                } else {
                    format!("；升级后将执行 {} 个迁移", pending.len())
                }
            ),
        ),
        Some(std::cmp::Ordering::Equal) => {
            ("up_to_date", false, format!("当前 {current} 已是最新版本"))
        }
        Some(std::cmp::Ordering::Greater) => (
            "ahead",
            false,
            format!(
                "当前 {current} 高于对比版本 {}（开发版或回滚场景）",
                latest.clone().unwrap_or_default()
            ),
        ),
        None => {
            problems.push("无法比较版本，请手动核对发布说明".to_string());
            (
                "unknown",
                false,
                format!("当前 {current}；未能确定可用版本"),
            )
        }
    };
    let message = if problems.is_empty() {
        message
    } else {
        format!("{message}；{}", problems.join("；"))
    };

    Ok(Json(UpgradeStatus {
        current_version: current,
        latest_version: latest,
        update_available,
        status,
        source,
        checked_at: Utc::now().to_rfc3339(),
        message,
        pending_migrations: pending.len() as i64,
    }))
}

pub(super) fn upgrade_feed_url() -> Option<String> {
    std::env::var("OPENLLM_UPGRADE_FEED_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Accept `v0.4.0` as well as `0.4.0`: release tags usually carry the prefix.
pub(super) fn normalize_version(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
}

/// Whether a version string is plain dotted numbers. `compare_versions` treats
/// anything else as "inconclusive", which must not be reported as "up to
/// date": a typo in the release tag would otherwise look like good news.
pub(super) fn is_numeric_version(value: &str) -> bool {
    let value = normalize_version(value);
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u64>().is_ok()
        })
}

/// Compare dotted numeric versions. Missing segments count as zero, so `0.4`
/// and `0.4.0` are equal; a non-numeric segment makes the comparison
/// inconclusive (`None`) rather than wrong.
pub(crate) fn compare_versions(current: &str, latest: &str) -> std::cmp::Ordering {
    fn parts(value: &str) -> Option<Vec<u64>> {
        // Release tags usually carry a `v` prefix; strip it here so a caller
        // that forgot to normalize still compares numerically.
        let value = normalize_version(value);
        value
            .split(['.', '-', '+'])
            .map(|part| {
                if part.is_empty() {
                    Some(0)
                } else {
                    part.parse::<u64>().ok()
                }
            })
            .collect()
    }
    let (Some(mut current), Some(mut latest)) = (parts(current), parts(latest)) else {
        return std::cmp::Ordering::Equal;
    };
    while current.len() < latest.len() {
        current.push(0);
    }
    while latest.len() < current.len() {
        latest.push(0);
    }
    current.cmp(&latest)
}
