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
    pub api_key: Option<String>,
    pub headers: String,
    pub enabled: i64,
    pub models_synced_at: Option<String>,
    pub models_sync_error: Option<String>,
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
    pub headers: serde_json::Value,
    pub enabled: bool,
    pub api_key_set: bool,
    pub models: Vec<String>,
    pub models_synced_at: Option<String>,
    pub models_sync_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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
    pub headers: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_sync_models: bool,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderUpdate {
    pub name: Option<String>,
    pub provider_type: Option<ProviderType>,
    pub base_url: Option<String>,
    pub model_prefix: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub auto_sync_models: Option<bool>,
    pub models: Option<Vec<String>>,
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
    pub upstream_model: String,
    pub weight: i64,
    pub priority: i64,
    pub enabled: i64,
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

#[derive(Debug, Serialize)]
pub struct PublicModel {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: &'static str,
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
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyInput {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyUpdate {
    pub enabled: bool,
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
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub latency_ms: i64,
    pub status_code: i64,
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
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub latency_ms: i64,
    pub status_code: i64,
    pub success: bool,
    pub streamed: bool,
    pub error_message: Option<String>,
    pub response_preview: Option<String>,
    pub created_at: String,
}

impl From<UsageLog> for UsageLogView {
    fn from(value: UsageLog) -> Self {
        Self {
            id: value.id,
            request_id: value.request_id,
            api_key_id: value.api_key_id,
            api_key_name: None,
            route_id: value.route_id,
            route_name: None,
            provider_id: value.provider_id,
            provider_name: None,
            requested_model: value.requested_model,
            upstream_model: value.upstream_model,
            endpoint: value.endpoint,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            latency_ms: value.latency_ms,
            status_code: value.status_code,
            success: value.success != 0,
            streamed: value.streamed != 0,
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
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub endpoint: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub latency_ms: i64,
    pub status_code: i64,
    pub success: i64,
    pub streamed: i64,
    pub error_message: Option<String>,
    pub response_preview: Option<String>,
    pub created_at: String,
}

impl From<UsageLogDetailRow> for UsageLogView {
    fn from(value: UsageLogDetailRow) -> Self {
        Self {
            id: value.id,
            request_id: value.request_id,
            api_key_id: value.api_key_id,
            api_key_name: value.api_key_name,
            route_id: value.route_id,
            route_name: value.route_name,
            provider_id: value.provider_id,
            provider_name: value.provider_name,
            requested_model: value.requested_model,
            upstream_model: value.upstream_model,
            endpoint: value.endpoint,
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            latency_ms: value.latency_ms,
            status_code: value.status_code,
            success: value.success != 0,
            streamed: value.streamed != 0,
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
    pub route_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub success: Option<bool>,
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
}

#[derive(Debug, Serialize)]
pub struct Overview {
    pub requests_today: i64,
    pub tokens_today: i64,
    pub requests_total: i64,
    pub tokens_total: i64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub active_providers: i64,
    pub active_routes: i64,
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
    pub success_rate: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: i64,
    pub tokens: i64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DailyUsage {
    pub day: String,
    pub requests: i64,
    pub tokens: i64,
}

#[derive(Debug, Serialize)]
pub struct SettingsView {
    pub admin_auth_enabled: bool,
    pub database: &'static str,
    pub version: &'static str,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

impl Usage {
    pub fn normalized(mut self) -> Self {
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens + self.completion_tokens;
        }
        self
    }
}

impl From<Provider> for ProviderView {
    fn from(value: Provider) -> Self {
        let headers =
            serde_json::from_str(&value.headers).unwrap_or_else(|_| serde_json::json!({}));
        Self {
            id: value.id,
            name: value.name,
            provider_type: value.provider_type,
            base_url: value.base_url,
            model_prefix: value.model_prefix,
            headers,
            enabled: value.enabled != 0,
            api_key_set: value.api_key.as_deref().is_some_and(|key| !key.is_empty()),
            models: Vec::new(),
            models_synced_at: value.models_synced_at,
            models_sync_error: value.models_sync_error,
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
