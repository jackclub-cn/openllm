use super::*;

pub(in crate::proxy) fn stable_hash64(parts: &[&[u8]]) -> u64 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 digests contain at least eight bytes"),
    )
}

pub(in crate::proxy) fn session_target_hash(
    session_id: &str,
    ordering_key: i64,
    target: &RouteTarget,
) -> u64 {
    stable_hash64(&[
        b"openllm-session-target-v1",
        &ordering_key.to_be_bytes(),
        &target.id.to_be_bytes(),
        &target.provider_id.to_be_bytes(),
        target.upstream_model.as_bytes(),
        session_id.as_bytes(),
    ])
}

pub(in crate::proxy) fn session_provider_key_hash(
    session_id: &str,
    provider_id: i64,
    key_id: i64,
) -> u64 {
    stable_hash64(&[
        b"openllm-session-provider-key-v1",
        &provider_id.to_be_bytes(),
        &key_id.to_be_bytes(),
        session_id.as_bytes(),
    ])
}

/// Orders weighted and round-robin targets without storing per-session state.
///
/// Weighted routes use weighted rendezvous hashing, so sessions still spread
/// according to target weights while a given session keeps the same preference
/// order. Priority routes already have a deterministic order and are left
/// untouched.
pub(in crate::proxy) fn order_targets_for_session(
    session_id: &str,
    ordering_key: i64,
    strategy: RouteStrategy,
    targets: &mut Vec<RouteTarget>,
) {
    match strategy {
        RouteStrategy::Priority => {}
        RouteStrategy::Weighted => {
            let mut ranked = targets
                .drain(..)
                .map(|target| {
                    let hash = session_target_hash(session_id, ordering_key, &target);
                    let uniform = (hash as f64 + 1.0) / (u64::MAX as f64 + 1.0);
                    let score = -uniform.ln() / target.weight.max(1) as f64;
                    (score, target)
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                right
                    .0
                    .total_cmp(&left.0)
                    .then_with(|| left.1.id.cmp(&right.1.id))
            });
            targets.extend(ranked.into_iter().map(|(_, target)| target));
        }
        RouteStrategy::RoundRobin => {
            let mut ranked = targets
                .drain(..)
                .map(|target| {
                    (
                        session_target_hash(session_id, ordering_key, &target),
                        target,
                    )
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.id.cmp(&right.1.id))
            });
            targets.extend(ranked.into_iter().map(|(_, target)| target));
        }
        RouteStrategy::CostOptimized
        | RouteStrategy::LatencyOptimized
        | RouteStrategy::P95LatencyOptimized
        | RouteStrategy::LeastUsed
        | RouteStrategy::QuotaPaced => {}
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionAffinityContext {
    pub(super) scope: crate::session_affinity::SessionAffinityScope,
    pub(super) pinned: Option<crate::session_affinity::SessionAffinityTarget>,
    pub(super) record_bind: bool,
}

pub(super) async fn session_affinity_context(
    state: &AppState,
    route_id: i64,
    session_key: Option<&str>,
    api_key_id: Option<i64>,
    record_metrics: bool,
) -> Option<SessionAffinityContext> {
    let store = state.session_affinity.as_ref()?;
    let session_key = session_key
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let scope =
        crate::session_affinity::SessionAffinityScope::new(api_key_id, route_id, session_key);
    let pinned = if record_metrics {
        store.lookup(&state.pool, &scope).await
    } else {
        store.peek(&state.pool, &scope).await
    };
    Some(SessionAffinityContext {
        scope,
        pinned,
        record_bind: record_metrics,
    })
}

pub(super) fn same_affinity_target(
    target: &RouteTarget,
    affinity: &crate::session_affinity::SessionAffinityTarget,
) -> bool {
    target.provider_id == affinity.provider_id && target.upstream_model == affinity.upstream_model
}

pub(super) fn affinity_target(
    target: &RouteTarget,
) -> crate::session_affinity::SessionAffinityTarget {
    crate::session_affinity::SessionAffinityTarget {
        provider_id: target.provider_id,
        upstream_model: target.upstream_model.clone(),
    }
}

pub(super) fn session_factor(
    context: &SessionAffinityContext,
    target: &RouteTarget,
    cache_metrics: Option<&RuntimeTargetMetricRow>,
) -> TargetScoreFactor {
    let cache_roi = cache_metrics.and_then(RuntimeTargetMetricRow::cache_roi);
    let cache_evidence = cache_roi.is_some();
    let weight = cache_roi.map_or(0.25, |roi| 0.75 + roi * 1.75);
    let cache_detail = cache_metrics
        .zip(cache_roi)
        .map(|(metrics, roi)| {
            format!(
                "，近一天缓存收益估值约 {:.1}%（命中率 {:.1}%，读取 {} / 写入 {} / 输入 {}）",
                roi * 100.0,
                metrics.cache_hit_rate().unwrap_or(0.0) * 100.0,
                metrics.cache_read_tokens,
                metrics.cache_write_tokens,
                metrics.prompt_tokens
            )
        })
        .unwrap_or_default();
    match &context.pinned {
        Some(pinned) if same_affinity_target(target, pinned) => score_factor(
            "session",
            weight,
            Some(1.0),
            if cache_evidence {
                format!(
                    "命中会话粘性与缓存收益{cache_detail}，优先回到 {} / {}",
                    target.provider_name, target.upstream_model,
                )
            } else {
                format!(
                    "命中会话粘性（暂无缓存收益证据），{} / {} 只保留普通优先级",
                    target.provider_name, target.upstream_model
                )
            },
        ),
        Some(pinned) => score_factor(
            "session",
            weight,
            Some(0.0),
            if cache_evidence {
                format!(
                    "本会话已有缓存收益{cache_detail}，优先回到 provider {} / {}",
                    pinned.provider_id, pinned.upstream_model,
                )
            } else {
                format!(
                    "本会话原先在 provider {} / {}，但没有缓存收益证据，不阻塞策略排序",
                    pinned.provider_id, pinned.upstream_model
                )
            },
        ),
        None => score_factor(
            "session",
            0.25,
            None,
            "该会话尚未建立粘性目标；首个健康候选会被记住",
        ),
    }
}
