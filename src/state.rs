use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
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
        }
    }
}
