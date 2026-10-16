use super::*;

/// Identity of one provider-side quota reading.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderQuotaSnapshotKey {
    pub provider_id: i64,
    pub provider_key_id: Option<i64>,
    pub model: String,
    pub window_key: String,
    pub unit: String,
    pub source: String,
}

/// Persisted quota reading. Fields are optional because many upstreams expose
/// only one side of the equation, such as remaining balance without a limit.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProviderQuotaSnapshotRecord {
    pub provider_id: i64,
    pub provider_key_id: Option<i64>,
    pub model: String,
    pub window_key: String,
    #[sqlx(rename = "limit_value")]
    pub limit: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: String,
    pub reset_at: Option<String>,
    pub source: String,
    pub confidence: String,
    pub observed_at: String,
    pub valid_until: String,
}

impl ProviderQuotaSnapshotRecord {
    pub(crate) fn key(&self) -> ProviderQuotaSnapshotKey {
        ProviderQuotaSnapshotKey {
            provider_id: self.provider_id,
            provider_key_id: self.provider_key_id,
            model: self.model.clone(),
            window_key: self.window_key.clone(),
            unit: self.unit.clone(),
            source: self.source.clone(),
        }
    }
}

/// API-facing quota reading with a derived state. `state` is intentionally
/// separate from `source`: a stale provider API reading is still a provider API
/// reading, but its routing state is `unknown`.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderQuotaSnapshotView {
    pub provider_id: i64,
    pub provider_key_id: Option<i64>,
    pub model: String,
    pub window_key: String,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: String,
    pub percent: Option<f64>,
    pub reset_at: Option<String>,
    pub source: String,
    pub confidence: String,
    pub observed_at: String,
    pub valid_until: String,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct ProviderQuotaSnapshotInput {
    #[serde(default)]
    pub key_id: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    pub window_key: String,
    #[serde(default)]
    pub used: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub remaining: Option<f64>,
    pub unit: String,
    #[serde(default)]
    pub reset_at: Option<String>,
    #[serde(default)]
    pub valid_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderQuotaSnapshotRequest {
    pub snapshots: Vec<ProviderQuotaSnapshotInput>,
}
