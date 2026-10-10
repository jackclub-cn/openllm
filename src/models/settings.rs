use super::*;

#[derive(Debug, Serialize)]
pub struct SettingsView {
    pub admin_auth_enabled: bool,
    pub database: &'static str,
    pub version: &'static str,
    pub database_stats: DatabaseStats,
}

#[derive(Debug, Serialize)]
pub struct DatabaseStats {
    pub path: Option<String>,
    pub size_bytes: i64,
    pub free_bytes: i64,
    pub providers: i64,
    pub provider_models: i64,
    pub provider_api_keys: i64,
    pub routes: i64,
    pub access_keys: i64,
    pub webhooks: i64,
    pub webhook_deliveries: i64,
    pub audit_logs: i64,
    pub usage_logs: i64,
    pub in_flight_requests: i64,
}

#[derive(Debug, Serialize)]
pub struct DatabaseVacuumResult {
    pub reclaimed_bytes: i64,
    pub database_stats: DatabaseStats,
}

#[derive(Debug, Serialize)]
pub struct RuntimeSettingsView {
    pub usage_retention_days: Option<i64>,
    /// Effective, environment-derived limits. Read-only; reported so an
    /// operator can confirm what the running process actually picked up.
    pub limits: RuntimeLimits,
}

/// Effective, environment-derived runtime limits.
#[derive(Debug, Serialize)]
pub struct RuntimeLimits {
    /// Idle gap allowed between upstream bytes.
    pub upstream_idle_timeout_secs: u64,
    /// Post-signal drain grace; `0` waits forever.
    pub shutdown_grace_secs: u64,
    /// Buffered request-body cap.
    pub max_request_body_mib: usize,
    /// Buffered upstream-response cap.
    pub max_upstream_body_mib: usize,
    /// Downstream SSE keep-alive interval; `None` disables it.
    pub sse_keepalive_secs: Option<u64>,
    /// Global concurrent request cap; `0` means unlimited.
    pub max_concurrent_requests: usize,
    /// In-flight request-body byte budget in MiB; `0` means unlimited.
    pub max_inflight_request_mib: usize,
    /// Admission wait in milliseconds before an over-capacity request is shed;
    /// `0` sheds immediately.
    pub admission_wait_ms: u64,
    /// Hard cap on an upstream stream's total lifetime in seconds; `None`
    /// disables it.
    pub stream_max_secs: Option<u64>,
    /// Bound on reading a client request body in seconds; `None` disables it.
    pub body_read_timeout_secs: Option<u64>,
    /// Wall-clock budget for a request's whole fallback loop in seconds;
    /// `None` disables it.
    pub request_timeout_secs: Option<u64>,
    /// Consecutive provider failures that hard-open its circuit; `0` keeps the
    /// soft cooldown only.
    pub provider_open_threshold: u32,
    /// Process-memory ceiling that arms the pressure guard, in MiB; `0`
    /// disables it.
    pub memory_limit_mib: u64,
    /// Where the memory ceiling came from: `"off"`, `"env"`, or `"cgroup"`.
    pub memory_limit_source: String,
    /// Fraction of the ceiling at which new requests are shed, as a percent.
    pub memory_shed_ratio_pct: u64,
    /// SQLite connection-pool size.
    pub db_max_connections: u32,
    /// SQLite write-contention timeout, in seconds.
    pub db_busy_timeout_secs: u64,
    /// Wait for a pooled SQLite connection before a request fails, in seconds.
    pub db_acquire_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeSettingsUpdate {
    #[serde(default)]
    pub usage_retention_days: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuardrailSettings {
    #[serde(default)]
    pub blocked_terms: Vec<String>,
    #[serde(default)]
    pub max_prompt_tokens: Option<i64>,
}

impl GuardrailSettings {
    pub fn is_empty(&self) -> bool {
        self.blocked_terms.is_empty() && self.max_prompt_tokens.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectorSettings {
    #[serde(default)]
    pub capture_request_previews: bool,
    #[serde(default = "default_preview_max_chars")]
    pub request_preview_max_chars: i64,
}

impl Default for InspectorSettings {
    fn default() -> Self {
        Self {
            capture_request_previews: false,
            request_preview_max_chars: default_preview_max_chars(),
        }
    }
}

fn default_preview_max_chars() -> i64 {
    4000
}

/// Availability policy for transient upstream failures.
///
/// A transient failure (connection error, timeout, or a retryable 5xx) is
/// first retried against the same target before the request falls back to the
/// next route target. This keeps a single-target route serving through a blip
/// instead of failing outright, while the bounded budget keeps a hard outage
/// from stalling the request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResilienceSettings {
    /// Hold the opening stream window briefly so a cutoff before any client
    /// bytes can be retried invisibly. Disabled by default because it delays
    /// stream headers by up to the holdback duration.
    #[serde(default)]
    pub stream_recovery_enabled: bool,
    /// Extra attempts for a stream that ended before any content reached the
    /// client. Kept separate from `max_retries` so the recovery toggle works on
    /// its own instead of silently doing nothing when same-target retries are
    /// turned off.
    #[serde(default = "default_stream_recovery_max_retries")]
    pub stream_recovery_max_retries: i64,
    /// Extra attempts against the same target. `0` disables same-target retry.
    #[serde(default = "default_max_retries")]
    pub max_retries: i64,
    /// First backoff delay; each further retry doubles it.
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    /// Ceiling for the exponential backoff.
    #[serde(default = "default_retry_max_backoff_ms")]
    pub retry_max_backoff_ms: i64,
}

fn default_max_retries() -> i64 {
    1
}

/// Upper bound for `ResilienceSettings::stream_recovery_max_retries`; also used
/// by the settings validator and the data path.
pub const STREAM_RECOVERY_MAX_RETRIES: i64 = 4;

fn default_stream_recovery_max_retries() -> i64 {
    2
}

fn default_retry_backoff_ms() -> i64 {
    200
}

fn default_retry_max_backoff_ms() -> i64 {
    2_000
}

impl Default for ResilienceSettings {
    fn default() -> Self {
        Self {
            stream_recovery_enabled: false,
            stream_recovery_max_retries: default_stream_recovery_max_retries(),
            max_retries: default_max_retries(),
            retry_backoff_ms: default_retry_backoff_ms(),
            retry_max_backoff_ms: default_retry_max_backoff_ms(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AdminTokenQuery {
    pub admin_token: Option<String>,
}
