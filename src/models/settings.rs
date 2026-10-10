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

#[derive(Debug, Deserialize)]
pub struct AdminTokenQuery {
    pub admin_token: Option<String>,
}
