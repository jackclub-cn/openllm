use super::*;

/// Neutral factor score used when a signal has no data.
///
/// Unknown must never penalise a candidate: a provider that has never served a
/// request, or that publishes no price, would otherwise rank behind a candidate
/// with mediocre known metrics just because less is known about it.
pub(super) const NEUTRAL_FACTOR_SCORE: f64 = 0.5;

/// One explainable component of a candidate's score.
#[derive(Debug, Clone)]
pub(crate) struct TargetScoreFactor {
    /// Stable factor name (`circuit`, `capacity`, `allocation`, ...).
    pub(crate) name: &'static str,
    /// `0.0..=1.0`, or `None` when nothing is known. Unknown factors contribute
    /// [`NEUTRAL_FACTOR_SCORE`].
    pub(crate) score: Option<f64>,
    pub(crate) weight: f64,
    /// `weight × (score or the neutral value)`, so an operator can see exactly
    /// which factor moved a candidate.
    pub(crate) contribution: f64,
    pub(crate) detail: String,
}

/// Why a candidate ranks where it does.
#[derive(Debug, Clone)]
pub(crate) struct TargetAssessment {
    /// `0..=100`, higher is better; 50 means "no known signal either way".
    pub(crate) score: f64,
    pub(crate) factors: Vec<TargetScoreFactor>,
    /// Set when the request path will not use this candidate at all right now.
    pub(crate) exclusion: Option<String>,
    /// Usable, but tried after candidates without the penalty (a soft pool
    /// share, a failed health check, saturated concurrency, a slow successful
    /// first token, or a cooling circuit that has no healthy alternative).
    pub(crate) deprioritized: bool,
}

impl TargetAssessment {
    pub(super) fn new(factors: Vec<TargetScoreFactor>) -> Self {
        let mut weight_sum = 0.0;
        let mut score_sum = 0.0;
        for factor in &factors {
            weight_sum += factor.weight;
            score_sum += factor.contribution;
        }
        let score = if weight_sum > 0.0 {
            score_sum / weight_sum * 100.0
        } else {
            NEUTRAL_FACTOR_SCORE * 100.0
        };
        Self {
            score: (score * 100.0).round() / 100.0,
            factors,
            exclusion: None,
            deprioritized: false,
        }
    }

    pub(super) fn excluded(mut self, reason: String) -> Self {
        self.exclusion = Some(reason);
        self
    }

    pub(super) fn penalized(mut self) -> Self {
        self.deprioritized = true;
        self
    }
}

pub(super) fn score_factor(
    name: &'static str,
    weight: f64,
    score: Option<f64>,
    detail: impl Into<String>,
) -> TargetScoreFactor {
    let score = score.map(|value| value.clamp(0.0, 1.0));
    TargetScoreFactor {
        name,
        weight,
        score,
        contribution: weight * score.unwrap_or(NEUTRAL_FACTOR_SCORE),
        detail: detail.into(),
    }
}

/// Seconds left on a monotonic cooldown deadline, rounded up and never below 1
/// so a displayed "remaining" is never a misleading zero while still cooling.
pub(super) fn cooldown_remaining_seconds(until: Instant, now: Instant) -> u64 {
    let remaining = until.saturating_duration_since(now);
    (remaining.as_secs_f64().ceil() as u64).max(1)
}

/// Fraction of a `(available, limit)` pair that is still free.
pub(super) fn headroom_ratio(headroom: &(usize, usize)) -> f64 {
    if headroom.1 == 0 {
        1.0
    } else {
        headroom.0 as f64 / headroom.1 as f64
    }
}

/// Concurrency headroom of the worst-limited dimension, as
/// `(available, limit)`; `None` when the target caps nothing.
///
/// A limit that was configured but never used has no semaphore yet, which
/// means every slot is free — not that the target is saturated.
pub(super) async fn concurrency_headroom(
    state: &AppState,
    target: &RouteTarget,
) -> Option<(usize, usize)> {
    let mut worst: Option<(usize, usize)> = None;
    if let Some(provider_api_key_id) = target.provider_api_key_id {
        if let Some(limit) = target
            .provider_key_max_concurrency
            .filter(|limit| *limit > 0)
        {
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let available = {
                let semaphores = state.provider_key_concurrency.lock().await;
                semaphores
                    .get(&provider_api_key_id)
                    .map(|semaphore| semaphore.available_permits())
                    .unwrap_or(limit)
            };
            let candidate = (available, limit);
            if headroom_ratio(&candidate)
                < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
            {
                worst = Some(candidate);
            }
        }
        if let Some(limit) = target
            .provider_key_requests_per_minute
            .filter(|limit| *limit > 0)
        {
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let (used, _) = provider_key_rpm_window(state, provider_api_key_id).await;
            let candidate = (limit.saturating_sub(used), limit);
            if headroom_ratio(&candidate)
                < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
            {
                worst = Some(candidate);
            }
        }
    }
    if let Some(limit) = target.max_concurrency.filter(|limit| *limit > 0) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let available = {
            let semaphores = state.provider_concurrency.lock().await;
            semaphores
                .get(&target.provider_id)
                .map(|semaphore| semaphore.available_permits())
                .unwrap_or(limit)
        };
        let candidate = (available, limit);
        if headroom_ratio(&candidate)
            < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
        {
            worst = Some(candidate);
        }
    }
    if let Some(limit) = target.model_max_concurrency.filter(|limit| *limit > 0) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let available = {
            let semaphores = state.model_concurrency.lock().await;
            semaphores
                .get(&(target.provider_id, target.upstream_model.clone()))
                .map(|semaphore| semaphore.available_permits())
                .unwrap_or(limit)
        };
        let candidate = (available, limit);
        if headroom_ratio(&candidate)
            < worst.map_or(f64::INFINITY, |current| headroom_ratio(&current))
        {
            worst = Some(candidate);
        }
    }
    worst
}

/// Pool headroom for one key, plus a denial reason or a soft penalty.
pub(super) async fn allocation_signal(
    state: &AppState,
    provider_id: i64,
    api_key_id: Option<i64>,
) -> (Option<TargetScoreFactor>, Option<String>, bool) {
    let Ok(catalog) = state.pool_catalog().await else {
        return (None, None, false);
    };
    let pools = catalog.pools_for_provider(provider_id);
    if pools.is_empty() {
        return (None, None, false);
    }
    let api_key_id = api_key_id.unwrap_or(crate::allocation::ANONYMOUS_API_KEY_ID);
    let now = Instant::now();
    let mut headroom = f64::INFINITY;
    let mut details = Vec::new();
    let mut denied = None;
    let mut penalized = false;
    for spec in pools {
        let decision = {
            let mut ledger = state.allocation_ledger.lock().await;
            ledger.decide(spec, api_key_id, now)
        };
        match decision {
            PoolDecision::Allow => {}
            PoolDecision::Penalize { reason } => {
                penalized = true;
                details.push(reason);
            }
            PoolDecision::Deny { reason, .. } => {
                denied = Some(reason);
            }
        }
        let usage = {
            let mut ledger = state.allocation_ledger.lock().await;
            ledger.usage(spec, api_key_id, now)
        };
        if let Some(limit) = spec.request_limit {
            headroom = headroom.min(limit.saturating_sub(usage.requests) as f64 / limit as f64);
            details.push(format!(
                "池 '{}' 请求余量 {}/{}",
                spec.name,
                limit.saturating_sub(usage.requests),
                limit
            ));
        }
        if let Some(limit) = spec.token_limit {
            headroom = headroom.min(limit.saturating_sub(usage.tokens) as f64 / limit as f64);
            details.push(format!(
                "池 '{}' token 余量 {}/{}",
                spec.name,
                limit.saturating_sub(usage.tokens),
                limit
            ));
        }
    }
    if let Some(reason) = denied {
        return (
            Some(score_factor("allocation", 2.0, Some(0.0), reason.clone())),
            Some(reason),
            penalized,
        );
    }
    let score = headroom.is_finite().then_some(headroom);
    (
        Some(score_factor("allocation", 2.0, score, details.join("；"))),
        None,
        penalized,
    )
}

/// Scores every candidate with an explainable factor breakdown.
///
/// Performs no upstream request: every signal comes from the database, the
/// in-memory control state, or recent usage rows.
pub(crate) async fn assess_targets(
    state: &AppState,
    strategy: RouteStrategy,
    targets: &[RouteTarget],
    api_key_id: Option<i64>,
    affinity: Option<&SessionAffinityContext>,
) -> AppResult<HashMap<(i64, String), TargetAssessment>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let metrics = runtime_target_metrics(
        state,
        targets,
        strategy == RouteStrategy::P95LatencyOptimized,
    )
    .await?;
    let quota_states = crate::quota::target_scope_states(state, targets).await;
    let quota_now = chrono::Utc::now();
    let key_of = |target: &RouteTarget| (target.provider_id, target.upstream_model.clone());

    // Relative signals are only meaningful next to the other candidates.
    let cheapest = targets
        .iter()
        .filter_map(target_cost_score)
        .filter(|cost| *cost > 0.0)
        .fold(f64::INFINITY, f64::min);
    let fastest = targets
        .iter()
        .filter_map(|target| {
            metrics
                .get(&key_of(target))
                .and_then(|metric| metric.avg_latency_ms)
        })
        .filter(|latency| *latency > 0.0)
        .fold(f64::INFINITY, f64::min);
    let fastest_p95 = targets
        .iter()
        .filter_map(|target| {
            metrics
                .get(&key_of(target))
                .and_then(|metric| metric.p95_latency_ms)
        })
        .filter(|latency| *latency > 0.0)
        .fold(f64::INFINITY, f64::min);
    let busiest = targets
        .iter()
        .filter_map(|target| metrics.get(&key_of(target)).map(|metric| metric.requests))
        .max()
        .unwrap_or(0);
    let best_priority = targets
        .iter()
        .map(|target| target.priority)
        .min()
        .unwrap_or(0);
    let highest_weight = targets
        .iter()
        .map(|target| target.weight)
        .max()
        .unwrap_or(1);

    let now = Instant::now();
    let threshold = provider_open_threshold();
    let streaks = state.provider_failure_streak.lock().await.clone();
    let provider_cooldowns = {
        let mut cooldowns = state.provider_cooldown.lock().await;
        cooldowns.retain(|_, until| *until + PROVIDER_PROBE_LEASE > now);
        cooldowns.clone()
    };
    let model_cooldowns = {
        let mut cooldowns = state.target_cooldown.lock().await;
        cooldowns.retain(|_, until| *until + PROVIDER_PROBE_LEASE > now);
        cooldowns.clone()
    };
    let slow_targets = {
        let mut targets = state.target_slow.lock().await;
        targets.retain(|_, entry| {
            entry.until.is_some_and(|until| until > now)
                || entry.last_slow_at.is_some_and(|last| {
                    now.saturating_duration_since(last) <= SLOW_TARGET_STREAK_WINDOW
                })
        });
        targets.clone()
    };
    let target_sinks = {
        let mut sinks = state.target_sink.lock().await;
        sinks.retain(|_, entry| entry.until > now);
        sinks.clone()
    };

    let mut assessments = HashMap::with_capacity(targets.len());
    for target in targets {
        let key = key_of(target);
        let metric = metrics.get(&key);
        let mut factors = Vec::new();
        let target_sink_key = target_sink_key(target);
        let sink_penalized = target_sinks.contains_key(&target_sink_key);
        if sink_penalized {
            factors.push(score_factor(
                "sink",
                1.25,
                Some(0.0),
                "短期限流 sink，仍可重试但排在健康目标之后",
            ));
        }

        // 1. Circuit state: an open provider is short-circuited before any
        //    upstream call, so it must score at the bottom.
        let streak = streaks.get(&target.provider_id).copied().unwrap_or(0);
        let provider_until = provider_cooldowns.get(&target.provider_id).copied();
        let cooling = provider_until.is_some_and(|until| until > now);
        let circuit_state = crate::state::provider_circuit_state(streak, cooling, threshold);
        let (circuit_score, circuit_detail) = match circuit_state {
            "open" => (
                0.0,
                format!(
                    "熔断中（连续失败 {streak} 次，冷却剩余 {}s）",
                    cooldown_remaining_seconds(provider_until.unwrap_or(now), now)
                ),
            ),
            "half_open" => (
                0.5,
                format!("半开，仅放行一个探测请求（连续失败 {streak} 次）"),
            ),
            _ => (1.0, "熔断关闭".to_string()),
        };
        factors.push(score_factor(
            "circuit",
            2.5,
            Some(circuit_score),
            circuit_detail,
        ));

        // 2. Live concurrency capacity.
        let capacity = concurrency_headroom(state, target).await;
        let capacity_score = capacity.map(|headroom| headroom_ratio(&headroom));
        let capacity_detail = match capacity {
            Some((available, limit)) => format!("并发余量 {available}/{limit}"),
            None => "未设置并发上限".to_string(),
        };
        let saturated = capacity.is_some_and(|(available, _)| available == 0);
        factors.push(score_factor(
            "capacity",
            2.0,
            capacity_score,
            capacity_detail,
        ));

        // 3. Pool allocation: denial is a hard exclusion, a soft share only
        //    deprioritises.
        let (allocation_factor, allocation_denied, allocation_penalized) =
            allocation_signal(state, target.provider_id, api_key_id).await;

        // 4. Provider health from the last test or scheduled probe.
        let (health_score, health_detail, health_failed) = match target.provider_health {
            Some(0) => (0.0, "最近一次健康检查失败".to_string(), true),
            Some(_) => (1.0, "最近一次健康检查通过".to_string(), false),
            None => (NEUTRAL_FACTOR_SCORE, "尚未做过健康检查".to_string(), false),
        };
        factors.push(score_factor(
            "health",
            2.5,
            Some(health_score),
            health_detail,
        ));

        // 5. Recent upstream-caused success rate.
        let reliability = metric.and_then(|metric| metric.success_rate());
        let reliability_detail = match (metric, reliability) {
            (Some(metric), Some(rate)) => format!(
                "近期成功率 {:.1}%（{}/{} 次上游结果）",
                rate * 100.0,
                metric.successes,
                metric.successes + metric.upstream_faults
            ),
            _ => "近期没有可判定的上游结果".to_string(),
        };
        factors.push(score_factor(
            "reliability",
            2.0,
            reliability,
            reliability_detail,
        ));

        // 6. Latency relative to the fastest known candidate.
        let latency = metric
            .and_then(|metric| metric.avg_latency_ms)
            .filter(|value| *value > 0.0);
        let latency_score = latency.map(|latency| {
            if fastest.is_finite() && fastest > 0.0 {
                (fastest / latency).clamp(0.0, 1.0)
            } else {
                NEUTRAL_FACTOR_SCORE
            }
        });
        let latency_detail = match latency {
            Some(latency) => format!("近期平均 {latency:.0}ms（最快 {fastest:.0}ms）"),
            None => "近期没有延迟样本".to_string(),
        };
        factors.push(score_factor(
            "latency",
            1.0,
            latency_score,
            latency_detail.clone(),
        ));
        let p95_latency = metric
            .and_then(|metric| metric.p95_latency_ms)
            .filter(|value| *value > 0.0);
        let p95_latency_score = p95_latency.map(|latency| {
            if fastest_p95.is_finite() && fastest_p95 > 0.0 {
                (fastest_p95 / latency).clamp(0.0, 1.0)
            } else {
                NEUTRAL_FACTOR_SCORE
            }
        });
        let p95_latency_detail = match p95_latency {
            Some(latency) => format!("近期 P95 {latency:.0}ms（最快 {fastest_p95:.0}ms）"),
            None => "近期没有 P95 延迟样本".to_string(),
        };

        // Only the latency-first strategy lets a recent slow first token
        // reshuffle the order; every other strategy keeps the order the
        // operator configured.
        let slow_state = slow_targets.get(&key).filter(|entry| {
            strategy.is_latency_first() && entry.until.is_some_and(|until| until > now)
        });
        if let Some(slow_state) = slow_state {
            factors.push(score_factor(
                "first_token",
                1.5,
                Some(0.0),
                format!(
                    "最近一次首字 {}ms，软降权剩余 {}s",
                    slow_state.last_first_token_ms,
                    cooldown_remaining_seconds(slow_state.until.unwrap_or(now), now)
                ),
            ));
        }

        // 7. Cost relative to the cheapest known candidate.
        let cost = target_cost_score(target);
        let cost_score = cost.map(|cost| {
            if cheapest.is_finite() && cheapest > 0.0 {
                (cheapest / cost).clamp(0.0, 1.0)
            } else {
                NEUTRAL_FACTOR_SCORE
            }
        });
        let cost_detail = match cost {
            Some(cost) => format!("成本指数 {cost:.4}（最低 {cheapest:.4}）"),
            None => "未公布价格".to_string(),
        };
        factors.push(score_factor("cost", 1.0, cost_score, cost_detail.clone()));

        let quota = quota_states.get(&crate::quota::quota_target_key(target));
        let quota_score = crate::quota::quota_pacing_score(quota, quota_now);
        let quota_detail = quota_detail(quota);
        let quota_low = quota.is_some_and(|quota| quota.state == "low");
        let quota_exhausted = quota.is_some_and(|quota| quota.state == "exhausted");
        let quota_alternative = targets.iter().any(|other| {
            quota_states
                .get(&crate::quota::quota_target_key(other))
                .is_none_or(|quota| quota.state != "exhausted")
        });
        if strategy == RouteStrategy::QuotaPaced {
            factors.push(score_factor(
                "quota",
                2.0,
                Some(quota_score),
                quota_detail.clone(),
            ));
        }

        // 8. The strategy's own preference, so the score agrees with the order
        //    the request path will use.
        let (strategy_score, strategy_detail) = match strategy {
            RouteStrategy::Priority => {
                let rank = target.priority.saturating_sub(best_priority);
                (
                    Some(1.0 / (1.0 + rank as f64)),
                    format!("优先级 {}（最低 {best_priority}）", target.priority),
                )
            }
            RouteStrategy::Weighted => (
                Some(target.weight.max(0) as f64 / highest_weight.max(1) as f64),
                format!("权重 {}（最高 {highest_weight}）", target.weight),
            ),
            RouteStrategy::RoundRobin => (Some(1.0), "按轮询游标顺序".to_string()),
            RouteStrategy::CostOptimized => (cost_score, cost_detail.clone()),
            RouteStrategy::LatencyOptimized => (latency_score, latency_detail.clone()),
            RouteStrategy::P95LatencyOptimized => (p95_latency_score, p95_latency_detail.clone()),
            RouteStrategy::LeastUsed => {
                let requests = metric.map(|metric| metric.requests).unwrap_or(0);
                (
                    Some(1.0 - requests as f64 / busiest.max(1) as f64),
                    format!("近期 {requests} 次请求（最多 {busiest} 次）"),
                )
            }
            RouteStrategy::QuotaPaced => (Some(quota_score), quota_detail),
        };
        factors.push(score_factor(
            "strategy",
            1.5,
            strategy_score,
            strategy_detail,
        ));
        if let Some(affinity) = affinity {
            let pinned_metrics = affinity.pinned.as_ref().and_then(|pinned| {
                metrics.get(&(pinned.provider_id, pinned.upstream_model.clone()))
            });
            factors.push(session_factor(affinity, target, pinned_metrics));
        }

        let mut assessment = TargetAssessment::new(factors);
        if health_failed
            || saturated
            || allocation_penalized
            || slow_state.is_some()
            || quota_low
            || sink_penalized
        {
            assessment = assessment.penalized();
        }

        // A model cooling down is skipped while a healthy alternative exists,
        // an open circuit is short-circuited before the upstream call, and a
        // pool denial removes the candidate outright.
        let model_until = model_cooldowns
            .get(&(target.provider_id, target.upstream_model.clone()))
            .copied();
        let model_cooling = model_until.is_some_and(|until| until > now);
        let sibling_exists = targets.iter().any(|other| {
            other.provider_id != target.provider_id || other.upstream_model != target.upstream_model
        });
        if let Some(reason) = allocation_denied {
            assessment = assessment.excluded(format!("提供商配额池拒绝：{reason}"));
        } else if circuit_state == "open" {
            assessment = assessment.excluded("提供商熔断中，请求会被立即短路".to_string());
        } else if model_cooling && sibling_exists {
            assessment = assessment.excluded(format!(
                "该模型正在冷却（剩余 {}s），存在可用替代目标",
                cooldown_remaining_seconds(model_until.unwrap_or(now), now)
            ));
        } else if strategy == RouteStrategy::QuotaPaced && quota_exhausted && quota_alternative {
            assessment = assessment.excluded("额度已耗尽，存在可用替代目标".to_string());
        }
        if let Some(factor) = allocation_factor {
            // Keep the factor list in a stable order for the UI.
            let mut factors = assessment.factors;
            factors.insert(3, factor);
            assessment.factors = factors;
        }
        assessments.insert(key, assessment);
    }
    Ok(assessments)
}
