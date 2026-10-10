use super::*;

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
    pub tool_search_checked_at: Option<String>,
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
    pub tool_search_checked_at: Option<String>,
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
    pub cooldown_seconds: Option<i64>,
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
    #[sqlx(default)]
    pub provider_id: Option<i64>,
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
    #[sqlx(default)]
    pub lifetime_requests: i64,
    #[sqlx(default)]
    pub lifetime_successes: i64,
    #[sqlx(default)]
    pub lifetime_latency_ms: i64,
    #[sqlx(default)]
    pub lifetime_prompt_tokens: i64,
    #[sqlx(default)]
    pub lifetime_completion_tokens: i64,
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

#[derive(Debug, FromRow)]
pub struct ModelInventoryRow {
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_type: String,
    pub provider_enabled: bool,
    pub model_prefix: String,
    pub model_name: String,
    pub enabled: bool,
    pub context_limit: Option<i64>,
    pub input_limit: Option<i64>,
    pub output_limit: Option<i64>,
    pub supported_endpoints: Option<String>,
    pub cost: Option<String>,
    pub cost_input_override: Option<f64>,
    pub cost_output_override: Option<f64>,
    pub cost_cache_read_override: Option<f64>,
    pub cost_cache_write_override: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ModelInventoryView {
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_enabled: bool,
    pub model_prefix: String,
    pub model_name: String,
    pub enabled: bool,
    pub context_limit: Option<i64>,
    pub input_limit: Option<i64>,
    pub output_limit: Option<i64>,
    /// Endpoints the upstream itself declares (after any manual override).
    pub supported_endpoints: Vec<String>,
    /// Endpoints the gateway will actually accept, including protocol
    /// translation. This is what routing and `/v1/models` enforce.
    pub served_endpoints: Vec<String>,
    pub cost_input: Option<f64>,
    pub cost_output: Option<f64>,
    pub cost_cache_read: Option<f64>,
    pub cost_cache_write: Option<f64>,
}

impl From<ModelInventoryRow> for ModelInventoryView {
    fn from(value: ModelInventoryRow) -> Self {
        let cost = value
            .cost
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
        let effective_cost = crate::models::effective_cost_value(
            cost.as_ref(),
            value.cost_input_override,
            value.cost_output_override,
            value.cost_cache_read_override,
            value.cost_cache_write_override,
        );
        let declared: Vec<String> = value
            .supported_endpoints
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
            .unwrap_or_default();
        let served_endpoints = (!declared.is_empty())
            .then(|| {
                crate::registry::served_endpoints(&value.provider_type, Some(declared.clone()))
            })
            .flatten()
            .unwrap_or_default();
        Self {
            provider_id: value.provider_id,
            provider_name: value.provider_name,
            provider_enabled: value.provider_enabled,
            model_prefix: value.model_prefix,
            model_name: value.model_name,
            enabled: value.enabled,
            context_limit: value.context_limit,
            input_limit: value.input_limit,
            output_limit: value.output_limit,
            supported_endpoints: declared,
            served_endpoints,
            cost_input: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "input")),
            cost_output: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "output")),
            cost_cache_read: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "cache_read")),
            cost_cache_write: effective_cost
                .as_ref()
                .and_then(|cost| crate::models::cost_base_price(cost, "cache_write")),
        }
    }
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
            tool_search_checked_at: value.tool_search_checked_at,
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
            cooldown_seconds: None,
            quota_kind,
            models_sync_interval_minutes: value.models_sync_interval_minutes,
            models_sync_attempted_at: value.models_sync_attempted_at,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}
