use super::*;

/// An declared share of one provider's capacity.
#[derive(Debug, Clone, FromRow)]
pub struct ProviderPool {
    pub id: i64,
    pub provider_id: i64,
    pub name: String,
    pub enabled: i64,
    pub window_seconds: i64,
    pub request_limit: Option<i64>,
    pub token_limit: Option<i64>,
    pub saturation_threshold_pct: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One gateway access key's reserved share of a pool.
#[derive(Debug, Clone, FromRow)]
pub struct ProviderPoolAllocation {
    pub id: i64,
    pub pool_id: i64,
    pub api_key_id: i64,
    pub weight_pct: i64,
    pub policy: String,
}

/// Create payload for a provider pool.
#[derive(Debug, Deserialize)]
pub struct ProviderPoolInput {
    pub provider_id: i64,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_pool_window_seconds")]
    pub window_seconds: i64,
    #[serde(default)]
    pub request_limit: Option<i64>,
    #[serde(default)]
    pub token_limit: Option<i64>,
    #[serde(default = "default_pool_saturation_threshold_pct")]
    pub saturation_threshold_pct: i64,
    /// Replaces the pool's allocations when present.
    #[serde(default)]
    pub allocations: Vec<ProviderPoolAllocationInput>,
}

/// Update payload; every field is optional so a single attribute can change.
#[derive(Debug, Deserialize)]
pub struct ProviderPoolUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub window_seconds: Option<i64>,
    #[serde(default)]
    pub request_limit: Option<Option<i64>>,
    #[serde(default)]
    pub token_limit: Option<Option<i64>>,
    #[serde(default)]
    pub saturation_threshold_pct: Option<i64>,
    #[serde(default)]
    pub allocations: Option<Vec<ProviderPoolAllocationInput>>,
}

/// One key's reserved share inside a pool payload.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderPoolAllocationInput {
    pub api_key_id: i64,
    #[serde(default = "default_pool_allocation_weight_pct")]
    pub weight_pct: i64,
    #[serde(default = "default_pool_allocation_policy")]
    pub policy: String,
}

pub fn default_pool_window_seconds() -> i64 {
    crate::allocation::DEFAULT_POOL_WINDOW_SECONDS
}

pub fn default_pool_saturation_threshold_pct() -> i64 {
    crate::allocation::DEFAULT_SATURATION_THRESHOLD_PCT
}

pub fn default_pool_allocation_weight_pct() -> i64 {
    crate::allocation::DEFAULT_ALLOCATION_WEIGHT_PCT
}

pub fn default_pool_allocation_policy() -> String {
    "hard".to_string()
}

/// A pool with its allocations, as returned by the management API.
#[derive(Debug, Serialize)]
pub struct ProviderPoolView {
    pub id: i64,
    pub provider_id: i64,
    pub provider_name: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub window_seconds: i64,
    pub request_limit: Option<i64>,
    pub token_limit: Option<i64>,
    pub saturation_threshold_pct: i64,
    pub allocations: Vec<ProviderPoolAllocationView>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderPoolAllocationView {
    pub id: i64,
    pub api_key_id: i64,
    pub api_key_name: Option<String>,
    pub weight_pct: i64,
    pub policy: String,
    /// Effective capacity reserved for this key, in the pool's own units.
    pub request_share: Option<i64>,
    pub token_share: Option<i64>,
}
