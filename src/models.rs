use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Openai,
    Anthropic,
    Ollama,
    Custom,
}

impl ProviderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Custom => "custom",
        }
    }
}

impl std::str::FromStr for ProviderType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "ollama" => Ok(Self::Ollama),
            "custom" => Ok(Self::Custom),
            _ => Err(format!("unsupported provider type: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    Priority,
    Weighted,
    RoundRobin,
}

impl RouteStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Weighted => "weighted",
            Self::RoundRobin => "round_robin",
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
            _ => Err(format!("unsupported route strategy: {value}")),
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Provider {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub model_prefix: String,
    pub models_dev_id: Option<String>,
    pub api_key: Option<String>,
    pub headers: String,
    pub enabled: i64,
    pub tool_search_supported: i64,
    pub models_synced_at: Option<String>,
    pub models_sync_error: Option<String>,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<i64>,
    pub last_test_latency_ms: Option<i64>,
    pub last_test_checked: Option<String>,
    pub last_test_message: Option<String>,
    pub health_check_interval_minutes: Option<i64>,
    pub health_check_model: Option<String>,
    pub models_sync_interval_minutes: Option<i64>,
    pub models_sync_attempted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderView {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub model_prefix: String,
    pub models_dev_id: Option<String>,
    pub headers: serde_json::Value,
    pub enabled: bool,
    pub api_key_set: bool,
    pub api_keys: Vec<ProviderApiKeyView>,
    pub tool_search_supported: bool,
    pub models: Vec<String>,
    pub models_synced_at: Option<String>,
    pub models_sync_error: Option<String>,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<bool>,
    pub last_test_latency_ms: Option<i64>,
    pub last_test_checked: Option<String>,
    pub last_test_message: Option<String>,
    pub health_check_interval_minutes: Option<i64>,
    pub health_check_model: Option<String>,
    pub quota_kind: Option<String>,
    pub models_sync_interval_minutes: Option<i64>,
    pub models_sync_attempted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderQuotaView {
    pub kind: String,
    pub title: String,
    pub plan_name: Option<String>,
    pub key_id: Option<i64>,
    pub key_name: Option<String>,
    pub key_suffix: Option<String>,
    pub source_url: Option<String>,
    pub items: Vec<ProviderQuotaItem>,
    pub details: Vec<ProviderQuotaDetail>,
    pub prices: Vec<ProviderPriceView>,
    pub fetched_at: String,
}

#[derive(Debug, Deserialize)]
pub struct ProviderQuotaQuery {
    #[serde(default)]
    pub key_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ProviderQuotaItem {
    pub key: String,
    pub label: String,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: String,
    pub percent: Option<f64>,
    pub reset_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderQuotaDetail {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderPriceView {
    pub model_name: String,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ProviderApiKeyRecord {
    pub id: i64,
    pub name: String,
    pub secret: String,
    pub enabled: i64,
    pub last_used_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<i64>,
    pub last_test_latency_ms: Option<i64>,
    pub last_test_checked: Option<String>,
    pub last_test_message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderApiKeyView {
    pub id: i64,
    pub name: String,
    pub api_key_set: bool,
    pub api_key_suffix: String,
    pub enabled: bool,
    pub last_used_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<bool>,
    pub last_test_latency_ms: Option<i64>,
    pub last_test_checked: Option<String>,
    pub last_test_message: Option<String>,
    pub requests: i64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cooldown_seconds: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderApiKeyInput {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProviderInput {
    pub name: String,
    pub provider_type: ProviderType,
    pub base_url: String,
    #[serde(default)]
    pub model_prefix: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_keys: Vec<ProviderApiKeyInput>,
    #[serde(default)]
    pub headers: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub tool_search_supported: bool,
    #[serde(default = "default_true")]
    pub auto_sync_models: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub health_check_interval_minutes: Option<i64>,
    #[serde(default)]
    pub health_check_model: Option<String>,
    #[serde(default)]
    pub models_sync_interval_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderUpdate {
    pub name: Option<String>,
    pub provider_type: Option<ProviderType>,
    pub base_url: Option<String>,
    pub model_prefix: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: Option<bool>,
    #[serde(default)]
    pub api_keys: Option<Vec<ProviderApiKeyInput>>,
    pub headers: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub tool_search_supported: Option<bool>,
    pub auto_sync_models: Option<bool>,
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub health_check_interval_minutes: Option<i64>,
    #[serde(default)]
    pub health_check_model: Option<String>,
    #[serde(default)]
    pub models_sync_interval_minutes: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ProviderModelLimitView {
    pub model_name: String,
    pub enabled: bool,
    /// Effective endpoint paths after applying any manual override.
    pub supported_endpoints: Vec<String>,
    /// Manual endpoint list. `None` means the synchronized value is used.
    pub supported_endpoints_override: Option<Vec<String>>,
    /// Effective values after applying any manual overrides.
    pub context_limit: Option<i64>,
    pub input_limit: Option<i64>,
    pub output_limit: Option<i64>,
    /// Manual values. `None` means the synced value is used.
    pub context_override: Option<i64>,
    pub input_override: Option<i64>,
    pub output_override: Option<i64>,
    /// Effective USD-per-million-token prices after applying overrides.
    pub cost_input: Option<f64>,
    pub cost_output: Option<f64>,
    pub cost_cache_read: Option<f64>,
    pub cost_cache_write: Option<f64>,
    /// Manual price values. `None` keeps the synchronized price.
    pub cost_input_override: Option<f64>,
    pub cost_output_override: Option<f64>,
    pub cost_cache_read_override: Option<f64>,
    pub cost_cache_write_override: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderModelLimitInput {
    pub model_name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub supported_endpoints_override: Option<Vec<String>>,
    #[serde(default)]
    pub context_limit: Option<i64>,
    #[serde(default)]
    pub input_limit: Option<i64>,
    #[serde(default)]
    pub output_limit: Option<i64>,
    #[serde(default)]
    pub cost_input_override: Option<f64>,
    #[serde(default)]
    pub cost_output_override: Option<f64>,
    #[serde(default)]
    pub cost_cache_read_override: Option<f64>,
    #[serde(default)]
    pub cost_cache_write_override: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderModelLimitsUpdate {
    pub models: Vec<ProviderModelLimitInput>,
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
    pub tool_search_supported: i64,
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
    pub targets: Vec<RouteDiagnoseTarget>,
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

#[derive(Debug, Serialize)]
pub struct PublicModel {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: &'static str,
    /// Upstream provider the model resolves to. Absent on route entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The upstream model name this entry forwards to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    /// Effective capability envelope. For routes this is the barrel (strictest
    /// common) intersection across every enabled target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
    /// Number of enabled targets behind a route entry. Absent for a model that
    /// resolves directly to a single provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
    /// Present on route entries. `false` means at least one target lacks
    /// metadata, so `capabilities` is a lower bound rather than a verified
    /// guarantee that every target accepts the advertised limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits_verified: Option<bool>,
    /// Flat, de-facto-standard limit names.
    ///
    /// OpenAI-compatible clients (Hermes, LiteLLM, assorted routers) read
    /// these specific keys, and several only walk top-level fields. They mirror
    /// the nested `capabilities` values so both styles of client work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i64>,
    /// Friendly label for clients that show one (Anthropic's `display_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Endpoint paths this model is known to support.
    ///
    /// Omitted means the provider did not declare endpoint capabilities and
    /// the gateway treats the model as eligible for any compatible endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_endpoints: Option<Vec<String>>,
}

impl PublicModel {
    /// Fills the flat limit fields from a capability envelope.
    ///
    /// `context_limit` is the total window, so it doubles as the input ceiling
    /// when no separate input limit is published; that matches how clients
    /// interpret `max_input_tokens`.
    pub fn with_flat_limits(mut self) -> Self {
        if let Some(capabilities) = self.capabilities.as_ref() {
            self.context_length = capabilities.context_limit;
            self.max_input_tokens = capabilities.input_limit.or(capabilities.context_limit);
            self.max_output_tokens = capabilities.output_limit;
            self.max_completion_tokens = capabilities.output_limit;
        }
        self
    }
}

/// Capability metadata mirrored from models.dev. Every field is optional so an
/// unknown value stays distinguishable from a known `false` or `0`, which
/// matters when intersecting several targets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_weights: Option<bool>,
    /// Accepted input modalities, e.g. `["text", "image"]`.
    ///
    /// Deliberately a top-level list rather than nested under an
    /// `input`/`output` key: OpenAI-compatible clients walk nested dicts
    /// looking for `input`/`output` *price* fields, and a list value there
    /// raises `TypeError: unhashable type: 'list'`, which silently discards the
    /// whole endpoint's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_model_id: Option<String>,
    /// The model's full context window, when it is larger than the input the
    /// client may actually send.
    ///
    /// `context_limit` reports the safe input capacity so every context-named
    /// key tells a client the same thing. The raw window is kept here so the
    /// information is not lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_context_tokens: Option<i64>,
}

impl ModelCapabilities {
    /// Collapses the window/input distinction into one conservative input cap.
    ///
    /// Providers report both a total window and a (smaller) maximum input, and
    /// clients read either key to decide how much to send. Exposing different
    /// numbers invites a client to pick the optimistic one and exceed the real
    /// limit, so both are published as the smaller value; the untouched window
    /// moves to `total_context_tokens`.
    pub fn with_effective_input_limit(mut self) -> Self {
        if let Some(window) = self.context_limit {
            // A declared input limit is authoritative when it is the stricter
            // of the two; otherwise the window is the cap.
            let effective = match self.input_limit {
                Some(input) => input.min(window),
                None => window,
            };
            if effective != window {
                self.total_context_tokens = Some(window);
            }
            self.context_limit = Some(effective);
            self.input_limit = Some(effective);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Barrel/intersection: the strictest common envelope across `targets`.
    ///
    /// Numeric limits take the minimum of the known values, booleans are only
    /// true when every target that reports the flag says true, and modality
    /// lists keep only what all targets accept. Descriptive fields (family,
    /// cost, release dates) are intentionally dropped because a route can span
    /// unrelated models where they have no common meaning.
    pub fn intersect<'a>(targets: impl IntoIterator<Item = &'a ModelCapabilities>) -> Option<Self> {
        let targets = targets.into_iter().collect::<Vec<_>>();
        if targets.is_empty() {
            return None;
        }
        let min = |f: fn(&ModelCapabilities) -> Option<i64>| -> Option<i64> {
            targets.iter().filter_map(|c| f(c)).min()
        };
        let all_true = |f: fn(&ModelCapabilities) -> Option<bool>| -> Option<bool> {
            let known = targets.iter().filter_map(|c| f(c)).collect::<Vec<_>>();
            if known.is_empty() {
                None
            } else {
                Some(known.iter().all(|v| *v))
            }
        };
        let capabilities = Self {
            context_limit: min(|c| c.context_limit),
            output_limit: min(|c| c.output_limit),
            input_limit: min(|c| c.input_limit),
            attachment: all_true(|c| c.attachment),
            reasoning: all_true(|c| c.reasoning),
            tool_call: all_true(|c| c.tool_call),
            structured_output: all_true(|c| c.structured_output),
            temperature: all_true(|c| c.temperature),
            open_weights: all_true(|c| c.open_weights),
            input_modalities: intersect_modalities(&targets, |c| c.input_modalities.as_ref()),
            output_modalities: intersect_modalities(&targets, |c| c.output_modalities.as_ref()),
            cost: None,
            family: None,
            knowledge: None,
            release_date: None,
            last_updated: None,
            canonical_model_id: None,
            // The raw window is intersected like any other ceiling so a route
            // never advertises a larger window than its narrowest target.
            total_context_tokens: min(|c| c.total_context_tokens),
        };
        (!capabilities.is_empty()).then_some(capabilities)
    }
}

/// Keeps only the modalities supported by every target that declares them.
/// Targets with no modality data are ignored rather than treated as "nothing",
/// and if none declare anything the result is `None`.
fn intersect_modalities(
    targets: &[&ModelCapabilities],
    get: fn(&ModelCapabilities) -> Option<&Vec<String>>,
) -> Option<Vec<String>> {
    let declared = targets.iter().filter_map(|c| get(c)).collect::<Vec<_>>();
    if declared.is_empty() {
        return None;
    }
    let mut sets = declared.iter().map(|items| {
        items
            .iter()
            .map(|item| item.to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>()
    });
    let mut result = sets.next().unwrap_or_default();
    for group in sets {
        result = result.intersection(&group).cloned().collect();
    }
    Some(result.into_iter().collect())
}

#[derive(Debug, Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<PublicModel>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ApiKeyRecord {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub key_suffix: String,
    pub enabled: i64,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub daily_token_limit: Option<i64>,
    pub daily_cost_limit_micros: Option<i64>,
    pub requests_per_minute: Option<i64>,
    pub max_concurrency: Option<i64>,
    pub allowed_models: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyView {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub key_suffix: String,
    pub enabled: bool,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub unpriced_requests: i64,
    pub daily_token_limit: Option<i64>,
    pub daily_cost_limit_micros: Option<i64>,
    pub requests_per_minute: Option<i64>,
    pub max_concurrency: Option<i64>,
    pub today_requests: i64,
    pub today_tokens: i64,
    pub today_prompt_tokens: i64,
    pub today_completion_tokens: i64,
    pub today_cost_micros: Option<i64>,
    pub requests_this_minute: i64,
    pub current_in_flight: i64,
    pub allowed_models: Vec<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, FromRow)]
pub struct ApiKeyStatsRow {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub key_suffix: String,
    pub enabled: i64,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub unpriced_requests: i64,
    pub daily_token_limit: Option<i64>,
    pub daily_cost_limit_micros: Option<i64>,
    pub requests_per_minute: Option<i64>,
    pub max_concurrency: Option<i64>,
    pub today_requests: i64,
    pub today_tokens: i64,
    pub today_prompt_tokens: i64,
    pub today_completion_tokens: i64,
    pub today_cost_micros: Option<i64>,
    pub requests_this_minute: i64,
    pub current_in_flight: i64,
    pub allowed_models: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyInput {
    pub name: String,
    #[serde(default)]
    pub daily_token_limit: Option<i64>,
    #[serde(default)]
    pub daily_cost_limit_micros: Option<i64>,
    #[serde(default)]
    pub requests_per_minute: Option<i64>,
    #[serde(default)]
    pub max_concurrency: Option<i64>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyUpdate {
    pub enabled: bool,
    #[serde(default)]
    pub daily_token_limit: Option<i64>,
    #[serde(default)]
    pub daily_cost_limit_micros: Option<i64>,
    #[serde(default)]
    pub requests_per_minute: Option<i64>,
    #[serde(default)]
    pub max_concurrency: Option<i64>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyCreated {
    pub key: String,
    pub item: ApiKeyView,
}

#[derive(Debug, FromRow)]
pub struct UsageLog {
    pub id: i64,
    pub request_id: String,
    pub api_key_id: Option<i64>,
    pub route_id: Option<i64>,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub estimated_cost_micros: Option<i64>,
    pub latency_ms: i64,
    /// Time to first content token, in milliseconds. Non-streamed responses
    /// use the total response time because no incremental token timestamp is
    /// available, while failed requests leave this empty.
    pub first_token_ms: Option<i64>,
    pub status_code: i64,
    pub in_flight: i64,
    pub success: i64,
    pub streamed: i64,
    pub error_message: Option<String>,
    pub response_preview: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct UsageLogView {
    pub id: i64,
    pub request_id: String,
    pub api_key_id: Option<i64>,
    pub api_key_name: Option<String>,
    pub route_id: Option<i64>,
    pub route_name: Option<String>,
    pub provider_id: Option<i64>,
    pub provider_name: Option<String>,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    /// Estimated request cost in micro-US dollars; `None` means unpriced.
    pub estimated_cost_micros: Option<i64>,
    pub latency_ms: i64,
    pub first_token_ms: Option<i64>,
    /// Generation throughput in tokens per second.
    ///
    /// For streamed requests this measures the generation phase only, i.e. it
    /// excludes the wait for the first token, which is what "tokens per second"
    /// means in practice. Non-streamed requests use the whole request time.
    /// `None` when it cannot be derived (no output, or no elapsed time).
    pub output_tps: Option<f64>,
    pub status_code: i64,
    pub in_flight: bool,
    pub success: bool,
    pub streamed: bool,
    pub error_message: Option<String>,
    pub response_preview: Option<String>,
    pub created_at: String,
}

impl From<UsageLog> for UsageLogView {
    fn from(value: UsageLog) -> Self {
        let streamed = value.streamed != 0;
        let output_tps = output_tps_of(
            value.completion_tokens,
            value.latency_ms,
            value.first_token_ms,
            streamed,
        );
        Self {
            id: value.id,
            request_id: value.request_id,
            api_key_id: value.api_key_id,
            api_key_name: None,
            route_id: value.route_id,
            route_name: None,
            provider_id: value.provider_id,
            provider_name: None,
            provider_api_key_id: value.provider_api_key_id,
            provider_api_key_name: value.provider_api_key_name,
            requested_model: value.requested_model,
            upstream_model: value.upstream_model,
            endpoint: value.endpoint,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            estimated_cost_micros: value.estimated_cost_micros,
            latency_ms: value.latency_ms,
            first_token_ms: value.first_token_ms,
            output_tps,
            status_code: value.status_code,
            in_flight: value.in_flight != 0,
            success: value.success != 0,
            streamed,
            error_message: value.error_message,
            response_preview: value.response_preview,
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, FromRow)]
pub struct UsageLogDetailRow {
    pub id: i64,
    pub request_id: String,
    pub api_key_id: Option<i64>,
    pub api_key_name: Option<String>,
    pub route_id: Option<i64>,
    pub route_name: Option<String>,
    pub provider_id: Option<i64>,
    pub provider_name: Option<String>,
    pub provider_api_key_id: Option<i64>,
    pub provider_api_key_name: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub estimated_cost_micros: Option<i64>,
    pub latency_ms: i64,
    pub first_token_ms: Option<i64>,
    pub status_code: i64,
    pub in_flight: i64,
    pub success: i64,
    pub streamed: i64,
    pub error_message: Option<String>,
    pub response_preview: Option<String>,
    pub created_at: String,
}

impl From<UsageLogDetailRow> for UsageLogView {
    fn from(value: UsageLogDetailRow) -> Self {
        let streamed = value.streamed != 0;
        let output_tps = output_tps_of(
            value.completion_tokens,
            value.latency_ms,
            value.first_token_ms,
            streamed,
        );
        Self {
            id: value.id,
            request_id: value.request_id,
            api_key_id: value.api_key_id,
            api_key_name: value.api_key_name,
            route_id: value.route_id,
            route_name: value.route_name,
            provider_id: value.provider_id,
            provider_name: value.provider_name,
            provider_api_key_id: value.provider_api_key_id,
            provider_api_key_name: value.provider_api_key_name,
            requested_model: value.requested_model,
            upstream_model: value.upstream_model,
            endpoint: value.endpoint,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            estimated_cost_micros: value.estimated_cost_micros,
            latency_ms: value.latency_ms,
            first_token_ms: value.first_token_ms,
            output_tps,
            status_code: value.status_code,
            in_flight: value.in_flight != 0,
            success: value.success != 0,
            streamed,
            error_message: value.error_message,
            response_preview: value.response_preview,
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
    pub provider_id: Option<i64>,
    pub provider_api_key_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub route_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub endpoint: Option<String>,
    pub success: Option<bool>,
    pub in_flight: Option<bool>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UsagePage {
    pub items: Vec<UsageLogView>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
pub struct UsageCleanup {
    /// Delete usage rows older than this many days. Must be at least 1 so a
    /// stray request cannot wipe the whole history.
    pub older_than_days: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageCleanupResult {
    pub deleted: u64,
    pub older_than_days: i64,
    pub cutoff: String,
}

#[derive(Debug, Deserialize)]
pub struct OverviewQuery {
    /// Minutes to add to UTC to reach the caller's local time (e.g. +480 for
    /// UTC+8). Lets "today" respect the operator's timezone instead of UTC.
    #[serde(default)]
    pub tz_offset_minutes: i64,
    /// Inclusive start of the dashboard range as an RFC3339 timestamp.
    pub from: Option<String>,
    /// Exclusive end of the dashboard range as an RFC3339 timestamp.
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub requests_today: i64,
    pub tokens_today: i64,
    pub prompt_tokens_today: i64,
    pub completion_tokens_today: i64,
    /// Prompt tokens served from cache today, and the resulting hit ratio.
    pub cache_read_today: i64,
    pub cache_write_today: i64,
    pub cache_hit_rate: f64,
    pub requests_total: i64,
    pub tokens_total: i64,
    pub prompt_tokens_total: i64,
    pub completion_tokens_total: i64,
    pub cache_read_total: i64,
    pub cache_write_total: i64,
    pub cost_today_micros: i64,
    pub cost_total_micros: i64,
    pub unpriced_today: i64,
    pub unpriced_total: i64,
    pub range_requests: i64,
    pub range_tokens: i64,
    pub range_prompt_tokens: i64,
    pub range_completion_tokens: i64,
    pub range_cache_read: i64,
    pub range_cache_write: i64,
    pub range_cache_hit_rate: f64,
    pub range_cost_micros: i64,
    pub range_unpriced: i64,
    pub range_success_rate: f64,
    pub range_avg_latency_ms: f64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub active_providers: i64,
    pub active_routes: i64,
    pub healthy_providers: i64,
    pub failed_providers: i64,
    pub untested_providers: i64,
    pub provider_keys_total: i64,
    pub healthy_provider_keys: i64,
    pub failed_provider_keys: i64,
    pub untested_provider_keys: i64,
    pub runtime_error_provider_keys: i64,
    pub cooling_provider_keys: i64,
    pub in_flight_requests: i64,
    pub recent_requests: Vec<UsageLogView>,
    pub provider_usage: Vec<ProviderUsage>,
    pub model_usage: Vec<ModelUsage>,
    pub daily_usage: Vec<DailyUsage>,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ProviderUsage {
    pub provider_id: i64,
    pub provider_name: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micros: Option<i64>,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DailyUsage {
    pub day: String,
    pub requests: i64,
    pub tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

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
    pub usage_logs: i64,
    pub in_flight_requests: i64,
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

#[derive(Debug, Deserialize)]
pub struct AdminTokenQuery {
    pub admin_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderTestResult {
    pub ok: bool,
    pub latency_ms: i64,
    pub message: String,
    /// Which check produced this result, so the UI can explain that a passing
    /// test actually exercised credentials rather than a public listing.
    pub checked: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderKeyTestItem {
    pub key_id: Option<i64>,
    pub key_name: String,
    pub api_key_suffix: String,
    pub ok: bool,
    pub latency_ms: i64,
    pub message: String,
    pub checked: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderKeyTestResult {
    pub provider_id: i64,
    pub provider_name: String,
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub model: Option<String>,
    pub results: Vec<ProviderKeyTestItem>,
}

#[derive(Debug, Serialize)]
pub struct ProviderKeyTestSummary {
    pub provider_id: i64,
    pub provider_name: String,
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub model: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderKeyTestAllResult {
    pub total_providers: usize,
    pub tested_providers: usize,
    pub healthy_providers: usize,
    pub failed_providers: usize,
    pub total_keys: usize,
    pub healthy_keys: usize,
    pub failed_keys: usize,
    pub results: Vec<ProviderKeyTestSummary>,
}

#[derive(Debug, Serialize)]
pub struct ProviderTestSummary {
    pub provider_id: i64,
    pub provider_name: String,
    pub ok: bool,
    pub latency_ms: i64,
    pub message: String,
    pub checked: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderTestAllResult {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub results: Vec<ProviderTestSummary>,
}

#[derive(Debug, Serialize)]
pub struct ModelSyncResult {
    pub ok: bool,
    pub provider_id: i64,
    pub models: Vec<String>,
    pub count: usize,
    pub synced_at: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ModelSyncPreview {
    pub provider_id: i64,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<ModelSyncChange>,
    pub retained: usize,
    pub disabled_retained: usize,
}

#[derive(Debug, Serialize)]
pub struct ModelSyncChange {
    pub model_name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Prompt tokens served from the provider's cache, billed at a discount.
    pub cache_read_tokens: i64,
    /// Prompt tokens written into the cache for later reuse.
    pub cache_write_tokens: i64,
}

impl Usage {
    /// Convenience constructor for the common case with no cache traffic.
    pub fn new(prompt_tokens: i64, completion_tokens: i64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }

    pub fn normalized(mut self) -> Self {
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens + self.completion_tokens;
        }
        // Defensive: a provider reporting a negative or absurd cache count must
        // not corrupt aggregates.
        self.cache_read_tokens = self.cache_read_tokens.max(0);
        self.cache_write_tokens = self.cache_write_tokens.max(0);
        self
    }

    pub fn has_tokens(&self) -> bool {
        self.prompt_tokens > 0 || self.completion_tokens > 0
    }
}

/// Merges manual price overrides into a models.dev cost object.
///
/// Overrides are applied to both the base object and every tier, so a manual
/// correction wins regardless of which context tier the request selects.
pub fn effective_cost_value(
    cost: Option<&serde_json::Value>,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
) -> Option<serde_json::Value> {
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return cost.cloned().filter(|value| value.is_object());
    }
    let mut object = cost
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in [
        ("input", input),
        ("output", output),
        ("cache_read", cache_read),
        ("cache_write", cache_write),
    ] {
        let Some(value) = value else {
            continue;
        };
        let value = serde_json::json!(value);
        object.insert(key.to_string(), value.clone());
        if let Some(tiers) = object
            .get_mut("tiers")
            .and_then(serde_json::Value::as_array_mut)
        {
            for tier in tiers {
                if let Some(tier) = tier.as_object_mut() {
                    tier.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    Some(serde_json::Value::Object(object))
}

/// Reads a base, non-tiered price for display in the management console.
pub fn cost_base_price(cost: &serde_json::Value, key: &str) -> Option<f64> {
    cost_price(cost, cost, key)
}

/// Estimates a request's cost in micro-US dollars from models.dev pricing.
///
/// Prices are expressed per million tokens, so multiplying a token count by
/// the price directly yields micro-dollars. Cache traffic is separated from
/// fresh input because providers bill it at different rates. Tiered context
/// pricing uses the largest published tier that the prompt has crossed.
pub fn estimate_cost_micros(cost: Option<&serde_json::Value>, usage: Usage) -> Option<i64> {
    let cost = cost?;
    let usage = usage.normalized();
    let selected = select_cost_tier(cost, usage.prompt_tokens);
    let input = cost_price(selected, cost, "input");
    let output = cost_price(selected, cost, "output");
    let cache_read = cost_price(selected, cost, "cache_read").or(input);
    let cache_write = cost_price(selected, cost, "cache_write").or(input);
    if input.is_none() && output.is_none() && cache_read.is_none() && cache_write.is_none() {
        return None;
    }

    let prompt = usage.prompt_tokens.max(0);
    let read = usage.cache_read_tokens.max(0).min(prompt);
    let write = usage.cache_write_tokens.max(0).min(prompt - read);
    let fresh = prompt - read - write;
    let completion = usage.completion_tokens.max(0);

    let amount = (fresh as f64) * input.unwrap_or(0.0)
        + (read as f64) * cache_read.unwrap_or(0.0)
        + (write as f64) * cache_write.unwrap_or(0.0)
        + (completion as f64) * output.unwrap_or(0.0);
    Some(amount.round().max(0.0).min(i64::MAX as f64) as i64)
}

fn select_cost_tier(cost: &serde_json::Value, prompt_tokens: i64) -> &serde_json::Value {
    let Some(tiers) = cost.get("tiers").and_then(serde_json::Value::as_array) else {
        return cost;
    };
    let mut selected = cost;
    let mut selected_size = -1;
    for tier in tiers {
        let Some(size) = tier
            .pointer("/tier/size")
            .and_then(serde_json::Value::as_i64)
        else {
            continue;
        };
        if tier
            .pointer("/tier/type")
            .and_then(serde_json::Value::as_str)
            != Some("context")
            || size > prompt_tokens
            || size < selected_size
        {
            continue;
        }
        selected = tier;
        selected_size = size;
    }
    selected
}

fn cost_price(selected: &serde_json::Value, base: &serde_json::Value, key: &str) -> Option<f64> {
    selected
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .or_else(|| base.get(key).and_then(serde_json::Value::as_f64))
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Derives output throughput from a completed request.
///
/// Streamed requests measure the generation phase alone (total latency minus
/// the first-token wait), since including queue time would understate a fast
/// model behind a slow first token. Requests without a first-token timestamp
/// fall back to the full latency. Returns `None` rather than infinity or zero
/// when there is nothing meaningful to report, so the UI can show a dash.
fn output_tps_of(
    completion_tokens: i64,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    streamed: bool,
) -> Option<f64> {
    if completion_tokens <= 0 {
        return None;
    }
    let elapsed_ms = if streamed {
        match first_token_ms {
            // Guard against a first-token stamp that exceeds the total
            // (possible with clock granularity): fall back to full latency.
            Some(first) if latency_ms > first => latency_ms - first,
            _ => latency_ms,
        }
    } else {
        latency_ms
    };
    if elapsed_ms <= 0 {
        return None;
    }
    let tps = completion_tokens as f64 / (elapsed_ms as f64 / 1000.0);
    tps.is_finite().then_some(tps)
}

impl From<Provider> for ProviderView {
    fn from(value: Provider) -> Self {
        let headers =
            serde_json::from_str(&value.headers).unwrap_or_else(|_| serde_json::json!({}));
        let quota_kind = crate::api::provider_quota_kind(&value.base_url).map(ToOwned::to_owned);
        Self {
            id: value.id,
            name: value.name,
            provider_type: value.provider_type,
            base_url: value.base_url,
            model_prefix: value.model_prefix,
            models_dev_id: value.models_dev_id,
            headers,
            enabled: value.enabled != 0,
            api_key_set: value.api_key.as_deref().is_some_and(|key| !key.is_empty()),
            api_keys: Vec::new(),
            tool_search_supported: value.tool_search_supported != 0,
            models: Vec::new(),
            models_synced_at: value.models_synced_at,
            models_sync_error: value.models_sync_error,
            last_test_at: value.last_test_at,
            last_test_ok: value.last_test_ok.map(|value| value != 0),
            last_test_latency_ms: value.last_test_latency_ms,
            last_test_checked: value.last_test_checked,
            last_test_message: value.last_test_message,
            health_check_interval_minutes: value.health_check_interval_minutes,
            health_check_model: value.health_check_model,
            quota_kind,
            models_sync_interval_minutes: value.models_sync_interval_minutes,
            models_sync_attempted_at: value.models_sync_attempted_at,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_weight() -> i64 {
    100
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    fn capabilities(
        output: Option<i64>,
        tools: Option<bool>,
        context: Option<i64>,
    ) -> ModelCapabilities {
        ModelCapabilities {
            output_limit: output,
            tool_call: tools,
            context_limit: context,
            ..Default::default()
        }
    }

    #[test]
    fn intersection_takes_strictest_common_envelope() {
        let wide = capabilities(Some(128000), Some(true), Some(400000));
        let narrow = capabilities(Some(8000), Some(true), Some(32000));
        let result = ModelCapabilities::intersect([&wide, &narrow]).unwrap();
        assert_eq!(result.output_limit, Some(8000));
        assert_eq!(result.context_limit, Some(32000));
        assert_eq!(result.tool_call, Some(true));
    }

    #[test]
    fn intersection_reports_false_when_any_target_lacks_a_capability() {
        let with_tools = capabilities(Some(8000), Some(true), None);
        let without_tools = capabilities(Some(8000), Some(false), None);
        let result = ModelCapabilities::intersect([&with_tools, &without_tools]).unwrap();
        assert_eq!(result.tool_call, Some(false));
    }

    #[test]
    fn intersection_ignores_unknown_values_instead_of_zeroing_them() {
        let known = capabilities(Some(8000), None, Some(32000));
        let unknown = ModelCapabilities::default();
        let result = ModelCapabilities::intersect([&known, &unknown]).unwrap();
        // An unknown limit must not collapse the known one to zero.
        assert_eq!(result.output_limit, Some(8000));
        assert_eq!(result.context_limit, Some(32000));
        assert_eq!(result.tool_call, None);
    }

    #[test]
    fn intersection_keeps_only_shared_modalities() {
        let multi = ModelCapabilities {
            input_modalities: Some(vec![
                "text".to_string(),
                "image".to_string(),
                "pdf".to_string(),
            ]),
            output_modalities: Some(vec!["text".to_string()]),
            ..Default::default()
        };
        let text_only = ModelCapabilities {
            input_modalities: Some(vec!["text".to_string()]),
            output_modalities: Some(vec!["text".to_string()]),
            ..Default::default()
        };
        let result = ModelCapabilities::intersect([&multi, &text_only]).unwrap();
        assert_eq!(result.input_modalities, Some(vec!["text".to_string()]));
        assert_eq!(result.output_modalities, Some(vec!["text".to_string()]));
    }

    #[test]
    fn empty_intersection_returns_none() {
        assert!(ModelCapabilities::intersect(std::iter::empty()).is_none());
        let empty = ModelCapabilities::default();
        assert!(ModelCapabilities::intersect([&empty]).is_none());
    }

    #[test]
    fn tps_measures_generation_phase_for_streams() {
        // 100 tokens emitted over 2s after a 3s first-token wait:
        // generation phase is 2s, so 50 tok/s (not 20, which the total would give).
        let tps = output_tps_of(100, 5000, Some(3000), true).unwrap();
        assert!((tps - 50.0).abs() < 0.001, "got {tps}");
    }

    #[test]
    fn tps_falls_back_to_full_latency_for_non_streams() {
        // Non-streamed responses use the whole request time even when the log
        // carries the same timestamp in `first_token_ms`.
        let tps = output_tps_of(100, 2000, Some(2000), false).unwrap();
        assert!((tps - 50.0).abs() < 0.001, "got {tps}");
    }

    #[test]
    fn tps_is_absent_when_undefined() {
        // No output tokens means no throughput to report.
        assert_eq!(output_tps_of(0, 1000, Some(100), true), None);
        // Zero elapsed time would divide by zero.
        assert_eq!(output_tps_of(10, 0, None, false), None);
        // A first-token stamp beyond the total falls back to full latency
        // instead of underflowing into a negative duration.
        let tps = output_tps_of(10, 100, Some(5000), true).unwrap();
        assert!((tps - 100.0).abs() < 0.001, "got {tps}");
    }

    #[test]
    fn effective_input_limit_collapses_window_and_input() {
        // The reported bug: window 1050000 but only 922000 inputs accepted.
        // Both context-named keys must agree on the safe value.
        let capabilities = ModelCapabilities {
            context_limit: Some(1_050_000),
            input_limit: Some(922_000),
            ..Default::default()
        }
        .with_effective_input_limit();
        assert_eq!(capabilities.context_limit, Some(922_000));
        assert_eq!(capabilities.input_limit, Some(922_000));
        // The raw window is preserved rather than discarded.
        assert_eq!(capabilities.total_context_tokens, Some(1_050_000));
    }

    #[test]
    fn effective_input_limit_is_conservative_when_input_exceeds_window() {
        // Also reported: window 400000 with a larger declared input (922000).
        // The smaller of the two is the safe ceiling.
        let capabilities = ModelCapabilities {
            context_limit: Some(400_000),
            input_limit: Some(922_000),
            ..Default::default()
        }
        .with_effective_input_limit();
        assert_eq!(capabilities.context_limit, Some(400_000));
        assert_eq!(capabilities.input_limit, Some(400_000));
    }

    #[test]
    fn effective_input_limit_keeps_equal_values_untouched() {
        let capabilities = ModelCapabilities {
            context_limit: Some(128_000),
            input_limit: Some(128_000),
            ..Default::default()
        }
        .with_effective_input_limit();
        assert_eq!(capabilities.context_limit, Some(128_000));
        assert_eq!(capabilities.input_limit, Some(128_000));
        // Nothing was hidden, so no raw window needs recording.
        assert_eq!(capabilities.total_context_tokens, None);
    }

    #[test]
    fn estimates_cost_with_cache_prices() {
        let cost = serde_json::json!({
            "input": 1.0,
            "output": 2.0,
            "cache_read": 0.1,
            "cache_write": 1.25
        });
        let usage = Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            cache_read_tokens: 400_000,
            cache_write_tokens: 100_000,
        };
        // 500K fresh input + 400K cache read + 100K cache write + 1M output.
        assert_eq!(
            estimate_cost_micros(Some(&cost), usage),
            Some(500_000 + 40_000 + 125_000 + 2_000_000)
        );
    }

    #[test]
    fn cost_overrides_apply_to_base_and_tiered_prices() {
        let synced = serde_json::json!({
            "input": 2.0,
            "output": 10.0,
            "tiers": [
                {"input": 4.0, "output": 20.0}
            ]
        });
        let effective =
            effective_cost_value(Some(&synced), Some(1.5), None, Some(0.25), None).unwrap();
        assert_eq!(cost_base_price(&effective, "input"), Some(1.5));
        assert_eq!(cost_base_price(&effective, "cache_read"), Some(0.25));
        assert_eq!(cost_base_price(&effective, "output"), Some(10.0));
        assert_eq!(effective["tiers"][0]["input"], serde_json::json!(1.5));
        assert_eq!(effective["tiers"][0]["cache_read"], serde_json::json!(0.25));
        assert_eq!(effective["tiers"][0]["output"], serde_json::json!(20.0));
    }

    #[test]
    fn applies_context_price_tier() {
        let cost = serde_json::json!({
            "input": 1.0,
            "output": 2.0,
            "tiers": [
                { "input": 5.0, "output": 10.0, "tier": { "type": "context", "size": 1000 } }
            ]
        });
        let usage = Usage::new(2_000, 0);
        assert_eq!(estimate_cost_micros(Some(&cost), usage), Some(10_000));
    }

    #[test]
    fn cost_is_unknown_without_numeric_prices() {
        let cost = serde_json::json!({ "currency": "USD" });
        assert_eq!(estimate_cost_micros(Some(&cost), Usage::new(100, 50)), None);
        assert_eq!(estimate_cost_micros(None, Usage::new(100, 50)), None);
    }
}
