use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    Priority,
    Weighted,
    RoundRobin,
    CostOptimized,
    LatencyOptimized,
    LeastUsed,
}

impl RouteStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Weighted => "weighted",
            Self::RoundRobin => "round_robin",
            Self::CostOptimized => "cost_optimized",
            Self::LatencyOptimized => "latency_optimized",
            Self::LeastUsed => "least_used",
        }
    }

    /// Values written to the original constrained `strategy` column and the
    /// extension column. SQLite cannot widen a CHECK constraint in place, so
    /// advanced strategies use `priority` as their storage fallback.
    pub fn storage_values(self) -> (&'static str, &'static str) {
        match self {
            Self::CostOptimized | Self::LatencyOptimized | Self::LeastUsed => {
                ("priority", self.as_str())
            }
            _ => (self.as_str(), ""),
        }
    }
}

impl std::str::FromStr for RouteStrategy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "priority" => Ok(Self::Priority),
            "weighted" => Ok(Self::Weighted),
            "round_robin" => Ok(Self::RoundRobin),
            "cost_optimized" => Ok(Self::CostOptimized),
            "latency_optimized" => Ok(Self::LatencyOptimized),
            "least_used" => Ok(Self::LeastUsed),
            _ => Err(format!("unsupported route strategy: {value}")),
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Route {
    pub id: i64,
    pub name: String,
    pub model_pattern: String,
    pub strategy: String,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct RouteTarget {
    pub id: i64,
    pub route_id: Option<i64>,
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_type: String,
    pub base_url: String,
    pub model_prefix: String,
    pub api_key: Option<String>,
    pub provider_headers: String,
    /// JSON array reported by the provider's models endpoint.
    pub supported_endpoints: Option<String>,
    pub cost: Option<String>,
    #[sqlx(default)]
    pub cost_input_override: Option<f64>,
    #[sqlx(default)]
    pub cost_output_override: Option<f64>,
    #[sqlx(default)]
    pub cost_cache_read_override: Option<f64>,
    #[sqlx(default)]
    pub cost_cache_write_override: Option<f64>,
    #[sqlx(default)]
    pub context_limit: Option<i64>,
    #[sqlx(default)]
    pub input_limit: Option<i64>,
    #[sqlx(default)]
    pub output_limit: Option<i64>,
    /// Provider/model availability as loaded by management queries. Runtime
    /// queries may omit these columns because they already filter disabled
    /// targets.
    #[sqlx(default)]
    pub provider_enabled: Option<i64>,
    #[sqlx(default)]
    pub model_enabled: Option<i64>,
    pub tool_search_supported: i64,
    /// Per-provider request timeout override, in seconds.
    #[sqlx(default)]
    pub timeout_seconds: Option<i64>,
    /// Per-provider cooldown override, in seconds.
    #[sqlx(default)]
    pub cooldown_seconds: Option<i64>,
    /// Per-provider upstream concurrency cap.
    #[sqlx(default)]
    pub max_concurrency: Option<i64>,
    /// Per-provider queue wait before a concurrency-limited request falls back.
    #[sqlx(default)]
    pub queue_timeout_seconds: Option<i64>,
    /// Per-model upstream concurrency cap.
    #[sqlx(default)]
    pub model_max_concurrency: Option<i64>,
    /// Per-model queue wait before a concurrency-limited request falls back.
    #[sqlx(default)]
    pub model_queue_timeout_seconds: Option<i64>,
    /// Most recent provider health result; `Some(0)` means explicitly failed.
    pub provider_health: Option<i64>,
    pub upstream_model: String,
    pub weight: i64,
    pub priority: i64,
    pub enabled: i64,
    /// Provider credential selected for this runtime candidate. This is not a
    /// route_targets column and defaults to `None` for database-loaded rows.
    #[sqlx(default)]
    pub provider_api_key_id: Option<i64>,
    /// Provider credential name selected for this runtime candidate.
    #[sqlx(default)]
    pub provider_api_key_name: Option<String>,
    /// Whether an authentication failure should fall through to a later
    /// candidate instead of being returned to the caller.
    #[sqlx(default)]
    pub auth_retryable: bool,
}

#[derive(Debug, Serialize)]
pub struct RouteView {
    pub id: i64,
    pub name: String,
    pub model_pattern: String,
    pub strategy: String,
    pub enabled: bool,
    pub context_limit: Option<i64>,
    pub input_limit: Option<i64>,
    pub output_limit: Option<i64>,
    pub limits_verified: bool,
    pub targets: Vec<RouteTargetView>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct RouteTargetView {
    pub id: i64,
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_type: String,
    pub upstream_model: String,
    /// Effective endpoint paths used when deciding whether this target can
    /// serve a request. Empty means the target did not declare a restriction.
    pub supported_endpoints: Vec<String>,
    pub context_limit: Option<i64>,
    pub input_limit: Option<i64>,
    pub output_limit: Option<i64>,
    pub provider_enabled: bool,
    pub model_enabled: bool,
    pub model_prefix: String,
    pub weight: i64,
    pub priority: i64,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct RouteInput {
    pub name: String,
    pub model_pattern: String,
    pub strategy: RouteStrategy,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub targets: Vec<RouteTargetInput>,
}

#[derive(Debug, Deserialize)]
pub struct RouteUpdate {
    pub name: Option<String>,
    pub model_pattern: Option<String>,
    pub strategy: Option<RouteStrategy>,
    pub enabled: Option<bool>,
    pub targets: Option<Vec<RouteTargetInput>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteTargetInput {
    #[serde(default)]
    pub id: Option<i64>,
    pub provider_id: i64,
    pub upstream_model: String,
    #[serde(default = "default_weight")]
    pub weight: i64,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct RouteDiagnoseInput {
    pub model: String,
    pub endpoint: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteDiagnoseView {
    pub model: String,
    pub endpoint: String,
    pub matched: bool,
    pub resolved: bool,
    pub match_type: String,
    pub route_id: Option<i64>,
    pub route_name: Option<String>,
    pub strategy: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barrel: Option<ModelCapabilities>,
    pub barrel_incomplete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_targets: Option<Vec<RouteDiagnoseRuntimeTarget>>,
    pub targets: Vec<RouteDiagnoseTarget>,
}

#[derive(Debug, Serialize)]
pub struct RouteDiagnoseRuntimeTarget {
    pub order: usize,
    pub provider_id: i64,
    pub provider_name: String,
    pub upstream_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_cost_per_million: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_cost_per_million: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_requests: Option<i64>,
    pub decision_reason: String,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub provider_health: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct RouteDiagnoseTarget {
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_type: String,
    pub upstream_model: String,
    pub eligible: bool,
    pub reason: String,
    pub supported_endpoints: Vec<String>,
    pub provider_health: Option<bool>,
}
