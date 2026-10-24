//! Fair-share allocation of a provider's capacity across gateway access keys.
//!
//! One upstream provider is shared by every gateway key that routes to it, but
//! the provider's own limit is global: a single client's burst can consume the
//! whole window, after which every other key receives `429`s even though it
//! barely used the provider. A *provider pool* declares that shared capacity
//! and optionally reserves a weighted share of it for specific gateway keys.
//!
//! The decision mirrors OmniRoute's quota-sharing engine:
//!
//! * below the saturation threshold the pool is **generous** and any key may
//!   borrow idle capacity up to the pool limit (work-conserving: unused
//!   headroom is never wasted);
//! * at or above the threshold the pool turns **strict** and every key with an
//!   allocation is held to `limit × weight%`: `hard` blocks the excess, `soft`
//!   only deprioritises it, `burst` may still use remaining headroom;
//! * keys without an allocation are borrowers: they use leftover capacity but
//!   can never push the pool past its limit.
//!
//! Accounting is in-memory over a sliding window (buckets of a fraction of the
//! window) because the check runs per candidate target on the request path.
//! Requests are charged when an upstream attempt starts — a failed attempt
//! still consumed provider capacity, and charging only successes would let a
//! fallback storm bypass the pool entirely. Tokens are charged when a response
//! completes, which is when their exact count is known. A restart therefore
//! resets the window; that is deliberate, since the provider's own window is
//! what actually persists, and a cold gateway does not hold a burst's worth of
//! history.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};
use crate::models::{ApiKeyRecord, ProviderPool, ProviderPoolAllocation, RouteTarget};
use crate::state::AppState;

/// Default rolling window for a pool, in seconds.
pub(crate) const DEFAULT_POOL_WINDOW_SECONDS: i64 = 60;
/// Longest accepted rolling window, in seconds.
pub(crate) const MAX_POOL_WINDOW_SECONDS: i64 = 86_400;
/// Default share of the pool capacity reserved for one key, in percent.
pub(crate) const DEFAULT_ALLOCATION_WEIGHT_PCT: i64 = 100;
/// Default usage ratio at which fair shares become strict, in percent.
pub(crate) const DEFAULT_SATURATION_THRESHOLD_PCT: i64 = 50;
/// Number of buckets a window is divided into. More buckets track the window
/// more closely at the cost of a little bookkeeping.
const BUCKETS_PER_WINDOW: u64 = 12;
/// Key id used for anonymous traffic, which never has an allocation row.
pub(crate) const ANONYMOUS_API_KEY_ID: i64 = 0;

/// How a key's traffic is treated once the pool is saturated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationPolicy {
    /// Block traffic above the reserved share.
    Hard,
    /// Allow it, but try this key after keys that are inside their share.
    Soft,
    /// Allow it while the pool still has headroom.
    Burst,
}

impl AllocationPolicy {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hard" => Some(Self::Hard),
            "soft" => Some(Self::Soft),
            "burst" => Some(Self::Burst),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Soft => "soft",
            Self::Burst => "burst",
        }
    }
}

/// One key's reserved share of a pool.
#[derive(Debug, Clone)]
pub(crate) struct PoolAllocationSpec {
    pub weight_pct: u64,
    pub policy: AllocationPolicy,
}

/// One pool, resolved into the shape the hot path needs.
#[derive(Debug, Clone)]
pub(crate) struct PoolSpec {
    pub id: i64,
    pub provider_id: i64,
    pub name: String,
    /// Rolling window the limits apply to.
    pub window: Duration,
    /// Bucket width in milliseconds; a slice of the window.
    pub bucket_width_ms: u64,
    pub request_limit: Option<u64>,
    pub token_limit: Option<u64>,
    pub saturation_threshold_pct: u64,
    pub allocations: HashMap<i64, PoolAllocationSpec>,
}

impl PoolSpec {
    pub(crate) fn from_rows(pool: &ProviderPool, allocations: &[ProviderPoolAllocation]) -> Self {
        let window_seconds = pool
            .window_seconds
            .clamp(1, MAX_POOL_WINDOW_SECONDS)
            .unsigned_abs();
        let window = Duration::from_secs(window_seconds);
        let allocations = allocations
            .iter()
            .filter(|allocation| allocation.pool_id == pool.id)
            .filter_map(|allocation| {
                let policy = AllocationPolicy::parse(&allocation.policy)?;
                Some((
                    allocation.api_key_id,
                    PoolAllocationSpec {
                        // A zero weight would reserve nothing at all, which is
                        // almost never what an operator means by "add an
                        // allocation"; one percent keeps the key inside the
                        // pool instead of locked out of it.
                        weight_pct: allocation.weight_pct.clamp(1, 100) as u64,
                        policy,
                    },
                ))
            })
            .collect();
        Self {
            id: pool.id,
            provider_id: pool.provider_id,
            name: pool.name.clone(),
            window,
            bucket_width_ms: (window.as_millis() as u64 / BUCKETS_PER_WINDOW).max(1),
            request_limit: pool
                .request_limit
                .filter(|limit| *limit > 0)
                .map(|limit| limit as u64),
            token_limit: pool
                .token_limit
                .filter(|limit| *limit > 0)
                .map(|limit| limit as u64),
            saturation_threshold_pct: pool.saturation_threshold_pct.clamp(1, 100).unsigned_abs(),
            allocations,
        }
    }

    /// Whether this pool constrains anything at all.
    pub(crate) fn is_enforcing(&self) -> bool {
        self.request_limit.is_some() || self.token_limit.is_some()
    }

    /// Capacity reserved for a key, in the pool's own units.
    ///
    /// At least one unit, so a declared share is never a hard lock-out.
    pub(crate) fn share_of(&self, limit: u64, weight_pct: u64) -> u64 {
        (limit.saturating_mul(weight_pct) / 100).max(1)
    }

    /// Whether `used` has reached the ratio at which shares become strict.
    fn is_saturated(&self, used: u64, limit: u64) -> bool {
        used.saturating_mul(100) >= limit.saturating_mul(self.saturation_threshold_pct)
    }
}

/// Every enabled pool, indexed by the provider it constrains.
#[derive(Debug, Default)]
pub struct PoolCatalog {
    by_provider: HashMap<i64, Vec<PoolSpec>>,
}

impl PoolCatalog {
    pub(crate) fn is_empty(&self) -> bool {
        self.by_provider.is_empty()
    }

    pub(crate) fn pools_for_provider(&self, provider_id: i64) -> &[PoolSpec] {
        self.by_provider
            .get(&provider_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Every enforcing pool, in a stable order (by pool id).
    pub(crate) fn pools(&self) -> Vec<&PoolSpec> {
        let mut specs: Vec<&PoolSpec> = self.by_provider.values().flatten().collect();
        specs.sort_by_key(|spec| spec.id);
        specs
    }

    /// Loads the enabled pool configuration.
    ///
    /// Pools without any limit are dropped here: they cannot deny or penalise
    /// anything, so keeping them would only cost a lookup per request.
    pub(crate) async fn load(pool: &SqlitePool) -> AppResult<Self> {
        let pools = sqlx::query_as::<_, ProviderPool>(
            "SELECT * FROM provider_pools WHERE enabled = 1 ORDER BY id",
        )
        .fetch_all(pool)
        .await?;
        if pools.is_empty() {
            return Ok(Self::default());
        }
        let allocations = sqlx::query_as::<_, ProviderPoolAllocation>(
            "SELECT * FROM provider_pool_allocations ORDER BY id",
        )
        .fetch_all(pool)
        .await?;
        let mut by_provider: HashMap<i64, Vec<PoolSpec>> = HashMap::new();
        for pool in &pools {
            let spec = PoolSpec::from_rows(pool, &allocations);
            if !spec.is_enforcing() {
                continue;
            }
            by_provider.entry(spec.provider_id).or_default().push(spec);
        }
        Ok(Self { by_provider })
    }
}

/// What the pool says about one candidate request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PoolDecision {
    Allow,
    /// The key is above its share while the pool is saturated. The request may
    /// proceed, but it should be tried after traffic that is inside its share.
    Penalize {
        reason: String,
    },
    /// The request must not use this provider right now.
    Deny {
        reason: String,
        retry_after: Duration,
    },
}

impl PoolDecision {
    fn deny(reason: String, retry_after: Duration) -> Self {
        Self::Deny {
            reason,
            retry_after: retry_after.max(Duration::from_secs(1)),
        }
    }
}

/// Sliding-window usage of one pool, both in total and for one key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PoolUsage {
    pub requests: u64,
    pub tokens: u64,
    pub key_requests: u64,
    pub key_tokens: u64,
}

#[derive(Debug, Default)]
struct Bucket {
    requests: u64,
    tokens: u64,
    key_requests: HashMap<i64, u64>,
    key_tokens: HashMap<i64, u64>,
}

#[derive(Debug, Default)]
struct PoolWindow {
    buckets: HashMap<u64, Bucket>,
}

/// Rolling-window counters for every pool that has seen traffic.
///
/// The origin is fixed at construction so bucket indices are stable and tests
/// can advance time without sleeping.
#[derive(Debug)]
pub struct AllocationLedger {
    origin: Instant,
    windows: HashMap<i64, PoolWindow>,
    /// Cumulative denials per pool since startup. Counted at the request path
    /// (never in `decide`, which the diagnose preview also calls), so an
    /// operator can alert on a key being throttled by its fair share.
    denials: HashMap<i64, u64>,
}

impl AllocationLedger {
    pub(crate) fn new(origin: Instant) -> Self {
        Self {
            origin,
            windows: HashMap::new(),
            denials: HashMap::new(),
        }
    }

    /// Counts one denied attempt against a pool.
    pub(crate) fn record_denial(&mut self, pool_id: i64) {
        *self.denials.entry(pool_id).or_insert(0) += 1;
    }

    /// Cumulative denials for a pool since startup.
    pub(crate) fn denials(&self, pool_id: i64) -> u64 {
        self.denials.get(&pool_id).copied().unwrap_or(0)
    }

    pub(crate) fn remove_pool(&mut self, pool_id: i64) {
        self.windows.remove(&pool_id);
        self.denials.remove(&pool_id);
    }

    fn bucket_index(&self, spec: &PoolSpec, now: Instant) -> u64 {
        let elapsed_ms = now.saturating_duration_since(self.origin).as_millis() as u64;
        elapsed_ms / spec.bucket_width_ms.max(1)
    }

    /// Drops buckets that have aged out of the window.
    fn prune(window: &mut PoolWindow, index: u64) {
        let oldest = index.saturating_sub(BUCKETS_PER_WINDOW);
        window.buckets.retain(|bucket, _| *bucket >= oldest);
    }

    fn with_bucket<T>(
        &mut self,
        spec: &PoolSpec,
        index: u64,
        update: impl FnOnce(&mut Bucket) -> T,
    ) -> T {
        let window = self.windows.entry(spec.id).or_default();
        Self::prune(window, index);
        let bucket = window.buckets.entry(index).or_default();
        update(bucket)
    }

    /// Charges `count` upstream attempts for a key.
    pub(crate) fn charge_requests(
        &mut self,
        spec: &PoolSpec,
        api_key_id: i64,
        count: u64,
        now: Instant,
    ) {
        if count == 0 {
            return;
        }
        let index = self.bucket_index(spec, now);
        self.with_bucket(spec, index, |bucket| {
            bucket.requests = bucket.requests.saturating_add(count);
            let key = bucket.key_requests.entry(api_key_id).or_default();
            *key = key.saturating_add(count);
        });
    }

    /// Charges tokens a completed response actually consumed.
    pub(crate) fn charge_tokens(
        &mut self,
        spec: &PoolSpec,
        api_key_id: i64,
        tokens: u64,
        now: Instant,
    ) {
        if tokens == 0 {
            return;
        }
        let index = self.bucket_index(spec, now);
        self.with_bucket(spec, index, |bucket| {
            bucket.tokens = bucket.tokens.saturating_add(tokens);
            let key = bucket.key_tokens.entry(api_key_id).or_default();
            *key = key.saturating_add(tokens);
        });
    }

    /// Current usage of `spec` in its rolling window.
    pub(crate) fn usage(&mut self, spec: &PoolSpec, api_key_id: i64, now: Instant) -> PoolUsage {
        let index = self.bucket_index(spec, now);
        let window = self.windows.entry(spec.id).or_default();
        Self::prune(window, index);
        let mut usage = PoolUsage::default();
        for bucket in window.buckets.values() {
            usage.requests = usage.requests.saturating_add(bucket.requests);
            usage.tokens = usage.tokens.saturating_add(bucket.tokens);
            usage.key_requests = usage
                .key_requests
                .saturating_add(bucket.key_requests.get(&api_key_id).copied().unwrap_or(0));
            usage.key_tokens = usage
                .key_tokens
                .saturating_add(bucket.key_tokens.get(&api_key_id).copied().unwrap_or(0));
        }
        usage
    }

    /// How long until enough of this pool's window has drained for a denied
    /// request to fit again.
    ///
    /// Reported as `Retry-After` on a pool denial so a client backs off by the
    /// window rather than hammering a limit that cannot free up any sooner.
    /// Buckets are fixed slices of the window, so a bucket stops counting only
    /// once the window index has moved `BUCKETS_PER_WINDOW` past it — waiting
    /// for the current bucket alone would understate the wait by the number of
    /// buckets in the window.
    pub(crate) fn retry_after(&self, spec: &PoolSpec, now: Instant) -> Duration {
        let elapsed_ms = now.saturating_duration_since(self.origin).as_millis() as u64;
        let width = spec.bucket_width_ms.max(1);
        let index = self.bucket_index(spec, now);
        let oldest = self
            .windows
            .get(&spec.id)
            .and_then(|window| window.buckets.keys().copied().min());
        let drains_at_ms = match oldest {
            Some(oldest) => (oldest + BUCKETS_PER_WINDOW) * width,
            None => (index + 1) * width,
        };
        Duration::from_millis(drains_at_ms.saturating_sub(elapsed_ms).max(1))
    }

    /// Decides whether one more request may use this pool.
    ///
    /// Both dimensions are evaluated and the first denial wins; a soft excess
    /// keeps the request but marks it so the caller can try it last.
    pub(crate) fn decide(
        &mut self,
        spec: &PoolSpec,
        api_key_id: i64,
        now: Instant,
    ) -> PoolDecision {
        if !spec.is_enforcing() {
            return PoolDecision::Allow;
        }
        let usage = self.usage(spec, api_key_id, now);
        let allocation = spec.allocations.get(&api_key_id);
        let mut penalize: Option<String> = None;

        if let Some(limit) = spec.request_limit {
            if usage.requests >= limit {
                return PoolDecision::deny(
                    format!(
                        "provider pool '{}' request window is exhausted ({}/{limit} in {}s)",
                        spec.name,
                        usage.requests,
                        spec.window.as_secs()
                    ),
                    self.retry_after(spec, now),
                );
            }
            if spec.is_saturated(usage.requests, limit)
                && let Some(allocation) = allocation
            {
                let share = spec.share_of(limit, allocation.weight_pct);
                if usage.key_requests >= share {
                    match allocation.policy {
                        AllocationPolicy::Hard => {
                            return PoolDecision::deny(
                                format!(
                                    "provider pool '{}' fair share exceeded for this API key \
                                     ({}/{share} requests in {}s at {}% pool usage)",
                                    spec.name,
                                    usage.key_requests,
                                    spec.window.as_secs(),
                                    spec.saturation_threshold_pct
                                ),
                                self.retry_after(spec, now),
                            );
                        }
                        AllocationPolicy::Soft => {
                            penalize = Some(format!(
                                "provider pool '{}' is saturated and this API key is above its \
                                 soft share ({}/{share} requests)",
                                spec.name, usage.key_requests
                            ));
                        }
                        AllocationPolicy::Burst => {}
                    }
                }
            }
        }

        if let Some(limit) = spec.token_limit {
            if usage.tokens >= limit {
                return PoolDecision::deny(
                    format!(
                        "provider pool '{}' token window is exhausted ({}/{limit} in {}s)",
                        spec.name,
                        usage.tokens,
                        spec.window.as_secs()
                    ),
                    self.retry_after(spec, now),
                );
            }
            if spec.is_saturated(usage.tokens, limit)
                && let Some(allocation) = allocation
            {
                let share = spec.share_of(limit, allocation.weight_pct);
                if usage.key_tokens >= share {
                    match allocation.policy {
                        AllocationPolicy::Hard => {
                            return PoolDecision::deny(
                                format!(
                                    "provider pool '{}' token fair share exceeded for this API key \
                                     ({}/{share} tokens in {}s)",
                                    spec.name,
                                    usage.key_tokens,
                                    spec.window.as_secs()
                                ),
                                self.retry_after(spec, now),
                            );
                        }
                        AllocationPolicy::Soft => {
                            penalize = Some(format!(
                                "provider pool '{}' is saturated and this API key is above its \
                                 soft token share ({}/{share} tokens)",
                                spec.name, usage.key_tokens
                            ));
                        }
                        AllocationPolicy::Burst => {}
                    }
                }
            }
        }

        match penalize {
            Some(reason) => PoolDecision::Penalize { reason },
            None => PoolDecision::Allow,
        }
    }
}

/// What filtering the candidate targets did.
#[derive(Debug, Default)]
pub(crate) struct AllocationFilterOutcome {
    /// Targets removed because a pool denied them.
    pub removed: usize,
    /// Readable decisions, written to the request log so an operator can see
    /// why a candidate was held back without reading process logs.
    pub notes: Vec<String>,
    /// Back-off hint when a denial produced one.
    pub retry_after: Option<Duration>,
}

/// Drops or deprioritises candidates that are over their provider's allocation.
///
/// Runs before target ordering, next to the budget filter: a denied candidate
/// is simply not attempted, so the request still falls back to a provider with
/// headroom instead of failing. A pool denial is a local routing decision and
/// never marks the provider as failed, so it cannot trip a circuit.
pub(crate) async fn apply_allocation_filter(
    state: &AppState,
    api_key: Option<&ApiKeyRecord>,
    targets: &mut Vec<RouteTarget>,
) -> AppResult<AllocationFilterOutcome> {
    let catalog = state.pool_catalog().await?;
    if catalog.is_empty() || targets.is_empty() {
        return Ok(AllocationFilterOutcome::default());
    }
    let api_key_id = api_key.map(|key| key.id).unwrap_or(ANONYMOUS_API_KEY_ID);
    let now = Instant::now();
    let mut outcome = AllocationFilterOutcome::default();
    let mut kept = Vec::with_capacity(targets.len());
    let mut penalized = Vec::new();

    for target in std::mem::take(targets) {
        let pools = catalog.pools_for_provider(target.provider_id);
        if pools.is_empty() {
            kept.push(target);
            continue;
        }
        let mut denial: Option<(String, Duration)> = None;
        let mut penalty: Option<String> = None;
        for spec in pools {
            let decision = {
                let mut ledger = state.allocation_ledger.lock().await;
                let decision = ledger.decide(spec, api_key_id, now);
                if matches!(decision, PoolDecision::Deny { .. }) {
                    ledger.record_denial(spec.id);
                }
                decision
            };
            match decision {
                PoolDecision::Allow => {}
                PoolDecision::Penalize { reason } => {
                    penalty.get_or_insert(reason);
                }
                PoolDecision::Deny {
                    reason,
                    retry_after,
                } => {
                    denial = Some((reason, retry_after));
                    break;
                }
            }
        }
        match (denial, penalty) {
            (Some((reason, retry_after)), _) => {
                outcome.removed += 1;
                outcome.notes.push(format!(
                    "excluded provider '{}': {reason}",
                    target.provider_name
                ));
                outcome.retry_after = Some(
                    outcome
                        .retry_after
                        .map_or(retry_after, |current| current.max(retry_after)),
                );
            }
            (None, Some(reason)) => {
                outcome.notes.push(format!(
                    "deprioritised provider '{}': {reason}",
                    target.provider_name
                ));
                penalized.push(target);
            }
            (None, None) => kept.push(target),
        }
    }
    kept.extend(penalized);
    *targets = kept;

    if targets.is_empty() && outcome.removed > 0 {
        return Err(AppError::UpstreamStatus {
            status: axum::http::StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "no route target is inside its provider allocation: {}",
                outcome.notes.join("; ")
            ),
            retry_after: outcome.retry_after,
        });
    }
    Ok(outcome)
}

/// Charges one upstream attempt against every pool covering the provider, or
/// reports why the attempt is not inside its allocation.
///
/// Called when an attempt starts rather than when it succeeds: a failed
/// attempt still consumed provider capacity, and charging only successes would
/// let a fallback storm slip past the pool. It is called *after* the local
/// admission gates (concurrency slots, circuit breaker) so a request the
/// gateway rejects locally never burns pool quota, and it decides and charges
/// under one lock so two concurrent requests cannot both take the last free
/// slot of a hard limit.
///
/// Returns `Some((reason, retry_after))` when the attempt was denied and no
/// quota was consumed.
pub(crate) async fn charge_target_attempt(
    state: &AppState,
    provider_id: i64,
    api_key_id: Option<i64>,
) -> Option<(String, Duration)> {
    let Ok(catalog) = state.pool_catalog().await else {
        return None;
    };
    let pools = catalog.pools_for_provider(provider_id);
    if pools.is_empty() {
        return None;
    }
    let api_key_id = api_key_id.unwrap_or(ANONYMOUS_API_KEY_ID);
    let now = Instant::now();
    let mut ledger = state.allocation_ledger.lock().await;
    let mut denial: Option<(String, Duration)> = None;
    for spec in pools {
        if let PoolDecision::Deny {
            reason,
            retry_after,
        } = ledger.decide(spec, api_key_id, now)
        {
            ledger.record_denial(spec.id);
            denial = Some((reason, retry_after));
            break;
        }
    }
    if denial.is_none() {
        for spec in pools {
            ledger.charge_requests(spec, api_key_id, 1, now);
        }
    }
    denial
}

/// Charges the tokens a completed response consumed.
pub(crate) async fn charge_token_usage(
    state: &AppState,
    provider_id: Option<i64>,
    api_key_id: Option<i64>,
    tokens: i64,
) {
    let (Some(provider_id), tokens) = (provider_id, tokens.max(0) as u64) else {
        return;
    };
    if tokens == 0 {
        return;
    }
    let Ok(catalog) = state.pool_catalog().await else {
        return;
    };
    let pools = catalog.pools_for_provider(provider_id);
    if pools.is_empty() {
        return;
    }
    let api_key_id = api_key_id.unwrap_or(ANONYMOUS_API_KEY_ID);
    let now = Instant::now();
    let mut ledger = state.allocation_ledger.lock().await;
    for spec in pools {
        ledger.charge_tokens(spec, api_key_id, tokens, now);
    }
}

/// Snapshot of one pool's live usage, for the console and diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct PoolUsageView {
    pub pool_id: i64,
    pub provider_id: i64,
    pub name: String,
    pub window_seconds: u64,
    pub request_limit: Option<u64>,
    pub token_limit: Option<u64>,
    pub requests: u64,
    pub tokens: u64,
    pub saturated: bool,
    pub denials: u64,
    pub allocations: Vec<PoolAllocationUsageView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolAllocationUsageView {
    pub api_key_id: i64,
    pub weight_pct: u64,
    pub policy: &'static str,
    pub request_share: Option<u64>,
    pub token_share: Option<u64>,
    pub key_requests: u64,
    pub key_tokens: u64,
}

/// Live usage of every enforcing pool covering `provider_id`.
pub(crate) async fn pool_usage(
    state: &AppState,
    provider_id: i64,
) -> AppResult<Vec<PoolUsageView>> {
    let catalog = state.pool_catalog().await?;
    let pools = catalog.pools_for_provider(provider_id);
    if pools.is_empty() {
        return Ok(Vec::new());
    }
    let now = Instant::now();
    let specs: Vec<&PoolSpec> = pools.iter().collect();
    let mut ledger = state.allocation_ledger.lock().await;
    Ok(pool_views(&specs, &mut ledger, now))
}

/// Live usage of every enforcing pool, for the metrics endpoint and diagnostics.
pub(crate) async fn all_pool_usage(state: &AppState) -> AppResult<Vec<PoolUsageView>> {
    let catalog = state.pool_catalog().await?;
    let specs = catalog.pools();
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let now = Instant::now();
    let mut ledger = state.allocation_ledger.lock().await;
    Ok(pool_views(&specs, &mut ledger, now))
}

fn pool_views(
    specs: &[&PoolSpec],
    ledger: &mut AllocationLedger,
    now: Instant,
) -> Vec<PoolUsageView> {
    let mut views = Vec::with_capacity(specs.len());
    for spec in specs {
        let usage = ledger.usage(spec, ANONYMOUS_API_KEY_ID, now);
        let saturated = spec
            .request_limit
            .is_some_and(|limit| spec.is_saturated(usage.requests, limit))
            || spec
                .token_limit
                .is_some_and(|limit| spec.is_saturated(usage.tokens, limit));
        let allocations = spec
            .allocations
            .iter()
            .map(|(api_key_id, allocation)| {
                let key_usage = ledger.usage(spec, *api_key_id, now);
                PoolAllocationUsageView {
                    api_key_id: *api_key_id,
                    weight_pct: allocation.weight_pct,
                    policy: allocation.policy.as_str(),
                    request_share: spec
                        .request_limit
                        .map(|limit| spec.share_of(limit, allocation.weight_pct)),
                    token_share: spec
                        .token_limit
                        .map(|limit| spec.share_of(limit, allocation.weight_pct)),
                    key_requests: key_usage.key_requests,
                    key_tokens: key_usage.key_tokens,
                }
            })
            .collect();
        views.push(PoolUsageView {
            pool_id: spec.id,
            provider_id: spec.provider_id,
            name: spec.name.clone(),
            window_seconds: spec.window.as_secs(),
            request_limit: spec.request_limit,
            token_limit: spec.token_limit,
            requests: usage.requests,
            tokens: usage.tokens,
            saturated,
            denials: ledger.denials(spec.id),
            allocations,
        });
    }
    views
}

#[cfg(test)]
#[path = "../tests/unit/allocation.rs"]
mod tests;
