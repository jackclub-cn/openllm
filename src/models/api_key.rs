use super::*;

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
    /// JSON-encoded [`ApiKeyRoutingPolicy`]; `None` means no stored policy.
    pub routing_policy: Option<String>,
}

/// Routing constraints an API key enforces on every request it authenticates.
///
/// These act as defaults: a request may still override any single field with
/// the matching `x-openllm-*` header.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyRoutingPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_providers: Vec<String>,
}

impl ApiKeyRoutingPolicy {
    pub fn is_empty(&self) -> bool {
        self.strategy.is_none() && self.provider.is_none() && self.exclude_providers.is_empty()
    }
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
    pub routing_policy: ApiKeyRoutingPolicy,
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
    pub routing_policy: Option<String>,
}

/// Parses a stored routing policy, falling back to "no policy" when the column
/// is empty or holds malformed JSON so a bad row cannot break routing.
pub fn parse_routing_policy(raw: Option<&str>) -> ApiKeyRoutingPolicy {
    raw.and_then(|raw| serde_json::from_str::<ApiKeyRoutingPolicy>(raw).ok())
        .unwrap_or_default()
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
    #[serde(default)]
    pub routing_policy: Option<ApiKeyRoutingPolicy>,
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
    #[serde(default)]
    pub routing_policy: Option<ApiKeyRoutingPolicy>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyCreated {
    pub key: String,
    pub item: ApiKeyView,
}
