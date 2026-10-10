use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use reqwest::Client;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, Semaphore, broadcast};

use crate::error::AppResult;
use crate::models::{GuardrailSettings, InspectorSettings, ResilienceSettings, RuntimeLimits};

pub(crate) const SETTING_GUARDRAILS: &str = "guardrails";
pub(crate) const SETTING_INSPECTOR: &str = "inspector";
pub(crate) const SETTING_RESILIENCE: &str = "resilience";

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub client: Client,
    pub admin_token: Option<String>,
    pub round_robin: Arc<Mutex<HashMap<i64, usize>>>,
    /// Per-provider cursor used to rotate upstream API keys across requests.
    pub provider_key_cursor: Arc<Mutex<HashMap<i64, usize>>>,
    /// In-memory cooldowns for providers that recently failed at runtime.
    pub provider_cooldown: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Consecutive provider failures, used to escalate runtime cooldowns.
    pub provider_failure_streak: Arc<Mutex<HashMap<i64, u32>>>,
    /// In-memory cooldowns for a specific provider model. This keeps one
    /// overloaded model from removing every sibling model on the provider.
    pub target_cooldown: Arc<Mutex<HashMap<(i64, String), Instant>>>,
    /// In-memory cooldowns for provider keys that recently failed.
    pub provider_key_cooldown: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Provider keys whose latest persisted runtime state contains an error.
    /// A later successful request clears the database field only once.
    pub provider_key_error_state: Arc<Mutex<HashSet<i64>>>,
    /// Throttles provider-key usage timestamps on the request hot path.
    pub provider_key_touched: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Shared semaphores enforcing each provider's concurrency cap.
    pub provider_concurrency: Arc<Mutex<HashMap<i64, Arc<Semaphore>>>>,
    /// Shared semaphores enforcing each provider model's concurrency cap.
    pub model_concurrency: Arc<Mutex<HashMap<(i64, String), Arc<Semaphore>>>>,
    /// Optional global cap on concurrent proxied requests. Enforced at
    /// admission so a burst of streams cannot exhaust memory before the
    /// per-provider and per-key limits get a chance to shed load.
    pub request_capacity: Option<Arc<Semaphore>>,
    /// Configured global request cap, `0` when disabled. Kept for metrics.
    pub request_capacity_limit: usize,
    /// Requests refused by the global admission cap since startup.
    pub requests_shed: Arc<AtomicU64>,
    /// Opt-in global budget on the total bytes of in-flight request bodies.
    /// `None` disables the byte budget (see `OPENLLM_MAX_INFLIGHT_REQUEST_MIB`).
    pub inflight_request_bytes: Option<Arc<RequestByteBudget>>,
    /// Configured in-flight request-byte budget in bytes, `0` when disabled.
    /// Kept separately from the budget so metrics can report the limit even
    /// while it is disabled.
    pub request_bytes_limit: usize,
    /// Requests refused by the in-flight byte budget since startup.
    pub request_bytes_shed: Arc<AtomicU64>,
    /// Wall-clock instant the process state was created, for uptime.
    pub started_at: Instant,
    /// Unix timestamp at startup, for the Prometheus start-time gauge.
    pub started_unix: i64,
    pub events: broadcast::Sender<UsageEvent>,
    /// Cached "does the gateway require an API key" flag. `None` means it must
    /// be re-read from SQLite. Invalidated whenever keys are mutated.
    pub auth_required: Arc<RwLock<Option<bool>>>,
    /// Cached request-guardrail policy. `None` means it must be re-read from
    /// SQLite after a settings update or the first request.
    pub guardrails: Arc<RwLock<Option<GuardrailSettings>>>,
    /// Cached request-inspector policy. `None` means it must be re-read from
    /// SQLite after a settings update or the first request.
    pub inspector: Arc<RwLock<Option<InspectorSettings>>>,
    /// Cached transient-failure retry policy. `None` means it must be re-read
    /// from SQLite after a settings update or the first request.
    pub resilience: Arc<RwLock<Option<ResilienceSettings>>>,
    /// Interval between downstream SSE keep-alive comments on a stream that has
    /// gone quiet. `None` disables them.
    pub sse_keepalive: Option<Duration>,
    /// Throttles `last_used_at` writes so the hot request path does not take a
    /// SQLite write lock on every single call.
    pub key_touched: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Throttles automatic usage-retention cleanup to once per hour.
    pub retention_last_run: Arc<Mutex<Option<Instant>>>,
    /// Cached models.dev capability catalog plus the instant it was fetched.
    /// Held behind a read-mostly lock because provider sync refreshes it only
    /// once per TTL.
    pub models_dev: Arc<RwLock<Option<CatalogCache>>>,
    /// Prevents a manual sync and the scheduled sync worker from fetching the
    /// same provider concurrently.
    pub provider_model_sync: Arc<Mutex<HashSet<i64>>>,
    /// Prevents overlapping provider health probes from overwriting each
    /// other's persisted result.
    pub provider_health_check: Arc<Mutex<HashSet<i64>>>,
}

/// A fetched models.dev catalog and the instant it was retrieved.
pub type CatalogCache = (Instant, Arc<crate::models_dev::Catalog>);

/// Default request-body cap in MiB.
///
/// The gateway buffers the whole body in memory, so this is a memory-safety
/// guard rather than a protocol limit. Anthropic accepts up to 32 MiB, and a
/// handful of base64 images or a PDF easily exceeds the previous 16 MiB, which
/// made the gateway reject payloads the upstream would have served. Raise it
/// with `OPENLLM_MAX_BODY_MIB` when larger bodies are expected.
pub(crate) const DEFAULT_MAX_BODY_MIB: usize = 32;

/// Default cap on a buffered (non-streaming) upstream response in MiB.
///
/// LLM responses are small JSON documents, so this is a memory-safety guard
/// against a broken or hostile upstream streaming an unbounded body into
/// memory, not a protocol limit. Raise it with `OPENLLM_MAX_UPSTREAM_BODY_MIB`
/// on the rare endpoint that legitimately returns more.
pub(crate) const DEFAULT_MAX_UPSTREAM_BODY_MIB: usize = 64;

/// Default in-flight request-byte budget in MiB.
///
/// The gateway buffers every request body in memory, so a burst of large
/// coding-agent payloads can exhaust RAM long before the request-count cap
/// trips. This is an opt-in budget: `0` disables it, matching the request-count
/// cap. Set `OPENLLM_MAX_INFLIGHT_REQUEST_MIB` to a positive value to shed
/// requests whose combined buffered bodies would exceed it.
pub(crate) const DEFAULT_MAX_INFLIGHT_REQUEST_MIB: usize = 0;

/// Default idle gap allowed between bytes from an upstream.
///
/// A total-request timeout would abort long generations (reasoning models
/// routinely run past five minutes), so only the gap between reads is bounded.
/// Raise it with `OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS` for upstreams that stay
/// silent for longer before the first byte.
pub(crate) const DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS: u64 = 300;

/// Default gap after which a silent downstream SSE stream gets a keep-alive
/// comment so load balancers, proxies, and clients do not drop the connection
/// while the model is still thinking. Zero disables the comments.
///
/// Override with `OPENLLM_SSE_KEEPALIVE_SECS`.
pub(crate) const DEFAULT_SSE_KEEPALIVE_SECS: u64 = 15;

/// Default grace period to let in-flight requests finish after a shutdown
/// signal before the remaining connections are dropped. Zero waits forever.
///
/// Override with `OPENLLM_SHUTDOWN_GRACE_SECS`.
pub(crate) const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 30;

/// Parses an operator-provided request-body cap, falling back to the default
/// for empty, unparsable, or non-positive values.
pub(crate) fn parse_max_body_mib(value: Option<&str>) -> usize {
    parse_body_mib(value, DEFAULT_MAX_BODY_MIB)
}

/// Parses a MiB cap, falling back to `default` for empty, unparsable, or
/// non-positive values so a typo can never remove the guard.
pub(crate) fn parse_body_mib(value: Option<&str>, default: usize) -> usize {
    parse_positive(value)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

/// Parses an operator-provided idle timeout in seconds, falling back to
/// `default` for empty, unparsable, or zero values.
pub(crate) fn parse_positive_secs(value: Option<&str>, default: u64) -> u64 {
    parse_positive(value).unwrap_or(default)
}

/// Parses an operator-provided SSE keep-alive interval.
///
/// Unlike the idle timeout, `0` is meaningful here: it disables the comments.
/// Anything missing or unparsable keeps the default so a typo cannot silently
/// turn the protection off.
pub(crate) fn parse_keepalive_secs(value: Option<&str>) -> Option<Duration> {
    match value.map(str::trim) {
        None | Some("") => Some(Duration::from_secs(DEFAULT_SSE_KEEPALIVE_SECS)),
        Some("0") => None,
        Some(raw) => match raw.parse::<u64>() {
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(_) => Some(Duration::from_secs(DEFAULT_SSE_KEEPALIVE_SECS)),
        },
    }
}

/// Parses an operator-provided global concurrency cap.
///
/// Empty, zero, and unparsable values all disable the cap, matching the
/// opt-in default: no admission limit unless an operator asks for one.
pub(crate) fn parse_concurrency_limit(value: Option<&str>) -> Option<usize> {
    value
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|limit| *limit > 0)
}

/// Parses an operator-provided in-flight request-byte budget in MiB.
///
/// Empty, zero, and unparsable values all disable the budget, matching the
/// opt-in default so an upgrade never starts shedding bodies by surprise.
pub(crate) fn parse_inflight_bytes_mib(value: Option<&str>) -> Option<usize> {
    parse_concurrency_limit(value).map(|mib| mib.saturating_mul(1024 * 1024))
}

/// Parses the post-signal drain grace period.
///
/// A missing or unparsable value keeps the default; an explicit `0` waits
/// indefinitely, preserving the pre-deadline behavior for operators who want
/// every stream to finish.
pub(crate) fn parse_shutdown_grace_secs(value: Option<&str>) -> u64 {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_SHUTDOWN_GRACE_SECS,
        Some(raw) => raw.parse::<u64>().unwrap_or(DEFAULT_SHUTDOWN_GRACE_SECS),
    }
}

fn parse_positive(value: Option<&str>) -> Option<u64> {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
}

/// Resolves the configured request-body cap in bytes.
pub(crate) fn max_request_body_bytes() -> usize {
    parse_max_body_mib(std::env::var("OPENLLM_MAX_BODY_MIB").ok().as_deref())
        .saturating_mul(1024 * 1024)
}

/// Resolves the configured buffered-upstream-response cap in bytes.
pub(crate) fn max_upstream_body_bytes() -> usize {
    parse_body_mib(
        std::env::var("OPENLLM_MAX_UPSTREAM_BODY_MIB").ok().as_deref(),
        DEFAULT_MAX_UPSTREAM_BODY_MIB,
    )
        .saturating_mul(1024 * 1024)
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub id: i64,
    pub request_id: String,
    pub success: bool,
    pub streamed: bool,
}

/// Opt-in global budget on the total bytes of in-flight request bodies.
///
/// The gateway buffers each request body in memory, so the number of requests
/// is a poor proxy for memory pressure: a handful of large coding-agent
/// payloads can use more RAM than hundreds of small ones. This budget charges
/// the actual bytes of every admitted body and releases them once the response
/// finishes, so the process can shed over-budget requests at admission instead
/// of buffering past its memory headroom.
pub struct RequestByteBudget {
    limit: usize,
    used: AtomicUsize,
}

impl RequestByteBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            used: AtomicUsize::new(0),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Bytes currently reserved by admitted requests.
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// Headroom left in the budget, saturating at zero.
    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used())
    }

    /// Adds `bytes` when the budget still has room, returning `false` otherwise.
    ///
    /// The compare-and-swap loop keeps concurrent admissions from racing past
    /// the limit; a zero-length add always succeeds so empty bodies never trip
    /// the guard.
    fn try_add(&self, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > self.limit {
                return false;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    fn release(&self, bytes: usize) {
        if bytes > 0 {
            self.used.fetch_sub(bytes, Ordering::AcqRel);
        }
    }

    /// Reserves `bytes` against the budget, returning a guard that releases
    /// them on drop. Returns `None` when the reservation would exceed the limit.
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Option<RequestByteGuard> {
        let mut guard = RequestByteGuard {
            budget: Arc::clone(self),
            reserved: 0,
        };
        guard.extend(bytes).then_some(guard)
    }
}

/// Holds reserved bytes against a [`RequestByteBudget`] until it is dropped.
///
/// A single guard accumulates every charge for one request, so it can be moved
/// into the response body and released only once the whole request (body read,
/// upstream work, and any streamed response) has finished.
pub struct RequestByteGuard {
    budget: Arc<RequestByteBudget>,
    reserved: usize,
}

impl RequestByteGuard {
    /// Bytes currently reserved by this guard.
    pub fn reserved(&self) -> usize {
        self.reserved
    }

    /// Charges `bytes` more against the budget, returning `false` when the
    /// request would exceed the limit (leaving the reservation unchanged).
    pub fn extend(&mut self, bytes: usize) -> bool {
        if self.budget.try_add(bytes) {
            self.reserved += bytes;
            true
        } else {
            false
        }
    }

    /// Releases any reservation above `bytes`, keeping the accounting exact
    /// when a declared `Content-Length` overstates the actual body size.
    pub fn shrink_to(&mut self, bytes: usize) {
        if bytes < self.reserved {
            self.budget.release(self.reserved - bytes);
            self.reserved = bytes;
        }
    }
}

impl Drop for RequestByteGuard {
    fn drop(&mut self) {
        self.budget.release(self.reserved);
    }
}

impl AppState {
    pub fn new(pool: SqlitePool, admin_token: Option<String>) -> Self {
        let idle_timeout = parse_positive_secs(
            std::env::var("OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS")
                .ok()
                .as_deref(),
            DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS,
        );
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // A total-request timeout would abort long streaming generations
            // (reasoning models routinely run past five minutes). Bound the
            // gap between reads instead, which still catches a stalled upstream
            // without capping total stream duration.
            .read_timeout(Duration::from_secs(idle_timeout))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build HTTP client");

        let (events, _) = broadcast::channel(128);
        let request_capacity_limit = parse_concurrency_limit(
            std::env::var("OPENLLM_MAX_CONCURRENT_REQUESTS")
                .ok()
                .as_deref(),
        )
        .unwrap_or(0);
        let request_capacity = (request_capacity_limit > 0)
            .then(|| Arc::new(Semaphore::new(request_capacity_limit)));
        let request_bytes_limit = parse_inflight_bytes_mib(
            std::env::var("OPENLLM_MAX_INFLIGHT_REQUEST_MIB")
                .ok()
                .as_deref(),
        )
        .unwrap_or(DEFAULT_MAX_INFLIGHT_REQUEST_MIB * 1024 * 1024);
        let inflight_request_bytes =
            (request_bytes_limit > 0).then(|| Arc::new(RequestByteBudget::new(request_bytes_limit)));

        Self {
            pool,
            client,
            admin_token: admin_token.filter(|token| !token.is_empty()),
            round_robin: Arc::new(Mutex::new(HashMap::new())),
            provider_key_cursor: Arc::new(Mutex::new(HashMap::new())),
            provider_cooldown: Arc::new(Mutex::new(HashMap::new())),
            provider_failure_streak: Arc::new(Mutex::new(HashMap::new())),
            target_cooldown: Arc::new(Mutex::new(HashMap::new())),
            provider_key_cooldown: Arc::new(Mutex::new(HashMap::new())),
            provider_key_error_state: Arc::new(Mutex::new(HashSet::new())),
            provider_key_touched: Arc::new(Mutex::new(HashMap::new())),
            provider_concurrency: Arc::new(Mutex::new(HashMap::new())),
            model_concurrency: Arc::new(Mutex::new(HashMap::new())),
            request_capacity,
            request_capacity_limit,
            requests_shed: Arc::new(AtomicU64::new(0)),
            inflight_request_bytes,
            request_bytes_limit,
            request_bytes_shed: Arc::new(AtomicU64::new(0)),
            started_at: Instant::now(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(0),
            events,
            auth_required: Arc::new(RwLock::new(None)),
            guardrails: Arc::new(RwLock::new(None)),
            inspector: Arc::new(RwLock::new(None)),
            resilience: Arc::new(RwLock::new(None)),
            sse_keepalive: parse_keepalive_secs(
                std::env::var("OPENLLM_SSE_KEEPALIVE_SECS").ok().as_deref(),
            ),
            key_touched: Arc::new(Mutex::new(HashMap::new())),
            retention_last_run: Arc::new(Mutex::new(None)),
            models_dev: Arc::new(RwLock::new(None)),
            provider_model_sync: Arc::new(Mutex::new(HashSet::new())),
            provider_health_check: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn load_provider_key_error_state(&self) -> Result<(), sqlx::Error> {
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM provider_api_keys WHERE last_error IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut errors = self.provider_key_error_state.lock().await;
        errors.clear();
        errors.extend(ids);
        Ok(())
    }

    /// Returns the cached guardrail policy, loading it from SQLite on first
    /// use or after a settings update invalidates the cache.
    pub async fn guardrail_settings(&self) -> AppResult<GuardrailSettings> {
        if let Some(settings) = self.guardrails.read().await.clone() {
            return Ok(settings);
        }

        let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTING_GUARDRAILS)
            .fetch_optional(&self.pool)
            .await?;
        let settings = raw
            .as_deref()
            .and_then(|raw| match serde_json::from_str::<GuardrailSettings>(raw) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    tracing::warn!(%error, "ignoring invalid guardrail settings");
                    None
                }
            })
            .unwrap_or_default();
        *self.guardrails.write().await = Some(settings.clone());
        Ok(settings)
    }

    /// Returns the cached request-inspector policy, loading it from SQLite on
    /// first use or after a settings update invalidates the cache.
    pub async fn inspector_settings(&self) -> AppResult<InspectorSettings> {
        if let Some(settings) = self.inspector.read().await.clone() {
            return Ok(settings);
        }

        let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTING_INSPECTOR)
            .fetch_optional(&self.pool)
            .await?;
        let settings = raw
            .as_deref()
            .and_then(|raw| match serde_json::from_str::<InspectorSettings>(raw) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    tracing::warn!(%error, "ignoring invalid inspector settings");
                    None
                }
            })
            .unwrap_or_default();
        *self.inspector.write().await = Some(settings.clone());
        Ok(settings)
    }

    /// Returns the cached same-target retry policy, loading it from SQLite on
    /// first use or after a settings update invalidates the cache.
    pub async fn resilience_settings(&self) -> AppResult<ResilienceSettings> {
        if let Some(settings) = self.resilience.read().await.clone() {
            return Ok(settings);
        }

        let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTING_RESILIENCE)
            .fetch_optional(&self.pool)
            .await?;
        let settings = raw
            .as_deref()
            .and_then(
                |raw| match serde_json::from_str::<ResilienceSettings>(raw) {
                    Ok(settings) => Some(settings),
                    Err(error) => {
                        tracing::warn!(%error, "ignoring invalid resilience settings");
                        None
                    }
                },
            )
            .unwrap_or_default();
        *self.resilience.write().await = Some(settings.clone());
        Ok(settings)
    }

    /// Reports the effective environment-derived runtime limits.
    ///
    /// Re-derives the same values the process parsed at startup so the console
    /// and operators can confirm them without reading the environment.
    pub(crate) fn runtime_limits(&self) -> RuntimeLimits {
        RuntimeLimits {
            upstream_idle_timeout_secs: parse_positive_secs(
                std::env::var("OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS")
                    .ok()
                    .as_deref(),
                DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS,
            ),
            shutdown_grace_secs: parse_shutdown_grace_secs(
                std::env::var("OPENLLM_SHUTDOWN_GRACE_SECS").ok().as_deref(),
            ),
            max_request_body_mib: parse_max_body_mib(
                std::env::var("OPENLLM_MAX_BODY_MIB").ok().as_deref(),
            ),
            max_upstream_body_mib: parse_body_mib(
                std::env::var("OPENLLM_MAX_UPSTREAM_BODY_MIB").ok().as_deref(),
                DEFAULT_MAX_UPSTREAM_BODY_MIB,
            ),
            sse_keepalive_secs: self.sse_keepalive.map(|interval| interval.as_secs()),
            max_concurrent_requests: self.request_capacity_limit,
            max_inflight_request_mib: self.request_bytes_limit / (1024 * 1024),
        }
    }
}
