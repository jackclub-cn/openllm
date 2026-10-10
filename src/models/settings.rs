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
