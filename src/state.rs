use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use reqwest::Client;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify, RwLock, Semaphore, broadcast};

use crate::allocation::{AllocationLedger, PoolCatalog};
use crate::capabilities::CapabilityCache;
use crate::error::AppResult;
use crate::models::{
    GuardrailSettings, InspectorSettings, ProviderQuotaSnapshotKey, ProviderQuotaSnapshotRecord,
    ResilienceSettings, RuntimeLimits,
};
use crate::resilience_state::DirtyResilienceState;
use crate::session_affinity::{SessionAffinityConfig, SessionAffinityStore};

pub(crate) const SETTING_GUARDRAILS: &str = "guardrails";
pub(crate) const SETTING_INSPECTOR: &str = "inspector";
pub(crate) const SETTING_RESILIENCE: &str = "resilience";

#[derive(Clone, Debug)]
pub struct FailureMetadata {
    pub scope: &'static str,
    pub retry: &'static str,
    pub confidence: &'static str,
    pub reason_code: &'static str,
    pub observed_at: Instant,
}

/// A short-lived target penalty for 429/rate-limit or local saturation.
///
/// A sink is not a failure circuit: the target remains eligible and can clear
/// itself with a successful request, but it is tried after healthy siblings.
pub type TargetSinkKey = (i64, Option<i64>, String);

#[derive(Clone, Debug)]
pub struct TargetSinkEntry {
    pub until: Instant,
    pub reason_code: &'static str,
    pub observed_at: Instant,
}

/// RAII guard for one bounded all-targets-busy wait.
///
/// Counters use a standard mutex because the guard is dropped from synchronous
/// cancellation paths. The actual wait remains async in the request handler.
pub struct BusyWaitLease {
    total: Arc<AtomicUsize>,
    by_route: Arc<std::sync::Mutex<HashMap<i64, usize>>>,
    route_id: i64,
}

impl Drop for BusyWaitLease {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::AcqRel);
        let mut by_route = self
            .by_route
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(count) = by_route.get_mut(&self.route_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                by_route.remove(&self.route_id);
            }
        }
    }
}

/// Process-wide ceiling for requests waiting on an all-busy route.
pub(crate) const MAX_BUSY_WAITERS: usize = 64;

pub type FailureClassifications = HashMap<(String, String, String), u64>;
pub type ModelConcurrency = HashMap<(i64, String), Arc<Semaphore>>;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub client: Client,
    pub admin_token: Option<String>,
    pub round_robin: Arc<Mutex<HashMap<i64, usize>>>,
    /// Process-local prompt/session affinity, optionally mirrored to SQLite.
    pub(crate) session_affinity: Option<Arc<SessionAffinityStore>>,
    /// Per-provider cursor used to rotate upstream API keys across requests.
    pub provider_key_cursor: Arc<Mutex<HashMap<i64, usize>>>,
    /// In-memory cooldowns for providers that recently failed at runtime.
    pub provider_cooldown: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Classification of the failure that created each provider cooldown.
    pub provider_failure_meta: Arc<Mutex<HashMap<i64, FailureMetadata>>>,
    /// Consecutive provider failures, used to escalate runtime cooldowns.
    pub provider_failure_streak: Arc<Mutex<HashMap<i64, u32>>>,
    /// Providers whose circuit is half-open, mapped to the instant the current
    /// recovery probe was admitted. While an entry is younger than
    /// `PROVIDER_PROBE_LEASE` the provider is treated as still open so only one
    /// request tests recovery at a time.
    pub provider_probe: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Provider circuit hard-opens (trips) since startup, by provider id. Used
    /// for the `openllm_provider_circuit_trips_total` counter so flapping
    /// providers stand out.
    pub provider_circuit_trips: Arc<Mutex<HashMap<i64, u64>>>,
    /// In-memory cooldowns for a specific provider model. This keeps one
    /// overloaded model from removing every sibling model on the provider.
    pub target_cooldown: Arc<Mutex<HashMap<(i64, String), Instant>>>,
    /// Classification of the failure that created each model cooldown.
    pub target_failure_meta: Arc<Mutex<HashMap<(i64, String), FailureMetadata>>>,
    /// Successful requests whose first token was slow. These targets are still
    /// routable, but are tried after healthy siblings while the soft penalty
    /// is active. This catches a provider that keeps returning 200 while its
    /// time-to-first-token collapses under quota pressure.
    pub target_slow: Arc<Mutex<HashMap<(i64, String), TargetSlowState>>>,
    /// In-memory cooldowns for provider keys that recently failed.
    pub provider_key_cooldown: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Classification of the failure that created each key cooldown.
    pub provider_key_failure_meta: Arc<Mutex<HashMap<i64, FailureMetadata>>>,
    /// Short-lived target sinks. Unlike cooldowns these remain routable.
    pub target_sink: Arc<Mutex<HashMap<TargetSinkKey, TargetSinkEntry>>>,
    /// Number of requests currently waiting because every target was busy.
    pub busy_waiting_total: Arc<AtomicUsize>,
    /// Per-route wait counters used to bound one route without a global lock.
    pub busy_waiting_by_route: Arc<std::sync::Mutex<HashMap<i64, usize>>>,
    /// Cumulative upstream failure classifications since process start.
    pub failure_classifications: Arc<Mutex<FailureClassifications>>,
    /// Latest provider-side quota readings. Request routing only reads this
    /// cache; SQLite is refreshed by explicit management calls or workers.
    pub quota_snapshots: Arc<Mutex<HashMap<ProviderQuotaSnapshotKey, ProviderQuotaSnapshotRecord>>>,
    /// Learned upstream request-shape support, scoped by provider/model/endpoint.
    pub(crate) capability_profiles: Arc<Mutex<CapabilityCache>>,
    /// Provider keys whose latest persisted runtime state contains an error.
    /// A later successful request clears the database field only once.
    pub provider_key_error_state: Arc<Mutex<HashSet<i64>>>,
    /// Throttles provider-key usage timestamps on the request hot path.
    pub provider_key_touched: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Rolling 60-second starts for each upstream credential.
    pub provider_key_rpm: Arc<Mutex<HashMap<i64, VecDeque<Instant>>>>,
    /// Shared semaphores enforcing each upstream credential's concurrency cap.
    pub provider_key_concurrency: Arc<Mutex<HashMap<i64, Arc<Semaphore>>>>,
    /// Optional semaphores bounding each credential's waiting queue.
    pub provider_key_queue: Arc<Mutex<HashMap<i64, Arc<Semaphore>>>>,
    /// Shared semaphores enforcing each provider's concurrency cap.
    pub provider_concurrency: Arc<Mutex<HashMap<i64, Arc<Semaphore>>>>,
    /// Shared semaphores enforcing each provider model's concurrency cap.
    pub model_concurrency: Arc<Mutex<ModelConcurrency>>,
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
    /// How long an over-capacity request waits for an admission slot before it
    /// is shed. Zero sheds immediately.
    pub admission_wait: Duration,
    /// How long the gateway will spend reading a request body before giving up.
    /// `None` leaves the read unbounded.
    pub body_read_timeout: Option<Duration>,
    /// Wall-clock budget for a request's entire fallback loop. Once it elapses
    /// the gateway stops starting further route targets and returns the last
    /// error, so a route full of failing targets cannot pin an admission slot
    /// for minutes. `None` disables the budget.
    pub request_timeout: Option<Duration>,
    /// Opt-in process-memory pressure guard; `None` disables it.
    pub memory_pressure: Option<Arc<MemoryPressure>>,
    /// Where the memory ceiling came from: `"off"`, `"env"`, or `"cgroup"`.
    pub memory_limit_source: &'static str,
    /// Configured memory shed ratio, kept for the runtime-limits view even when
    /// the guard is disabled.
    pub memory_shed_ratio: f64,
    /// Requests shed by the memory-pressure guard since startup.
    pub memory_shed: Arc<AtomicU64>,
    /// How late, in milliseconds, the async runtime ran its last 1s timer.
    /// Surfaces a blocked runtime (sync I/O or a CPU-bound call on the reactor).
    pub runtime_lag_millis: Arc<AtomicU64>,
    /// Unix timestamp of the last completed maintenance cycle; `0` means none
    /// has run yet.
    pub maintenance_last_run_unix: Arc<AtomicU64>,
    /// Completed maintenance cycles since startup.
    pub maintenance_runs: Arc<AtomicU64>,
    /// Wall-clock instant the process state was created, for uptime.
    pub started_at: Instant,
    /// Set once a shutdown signal arrives. The readiness probe reports
    /// `not_ready` while this is set so a load balancer drains the instance
    /// before its connections close.
    pub shutting_down: Arc<AtomicBool>,
    /// Unix timestamp at startup, for the Prometheus start-time gauge.
    pub started_unix: i64,
    pub events: broadcast::Sender<UsageEvent>,
    /// Provider circuit transitions, consumed by the webhook dispatcher.
    pub circuit_events: broadcast::Sender<ProviderCircuitEvent>,
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
    /// Whether circuits and cooldowns are mirrored into SQLite so they survive
    /// a restart (`OPENLLM_RESILIENCE_STATE_PERSIST=0` disables the mirror).
    pub resilience_state_persist: bool,
    /// Entities whose persisted resilience mirror is stale. The writer task
    /// drains this set, so the hot path only pays for a small in-memory mark.
    pub resilience_dirty: Arc<std::sync::Mutex<DirtyResilienceState>>,
    /// Wakes the resilience-state writer after a state change.
    pub resilience_flush: Arc<Notify>,
    /// Cached provider-pool configuration. `None` means it must be re-read from
    /// SQLite after a pool or allocation change.
    pub pool_catalog: Arc<RwLock<Option<Arc<PoolCatalog>>>>,
    /// Rolling-window usage per pool, shared by every gateway access key.
    pub allocation_ledger: Arc<Mutex<AllocationLedger>>,
}

/// A fetched models.dev catalog and the instant it was retrieved.
pub type CatalogCache = (Instant, Arc<crate::models_dev::Catalog>);

mod limits;
pub(crate) use limits::*;
mod memory;
pub use memory::MemoryPressure;
use memory::cgroup_memory_max_bytes;
pub(crate) use memory::sample_process_memory_bytes;
mod events;

pub use events::*;
mod admission;
pub use admission::*;
mod app;
