use super::*;

/// Event types a webhook can subscribe to.
pub const WEBHOOK_EVENT_TYPES: [&str; 2] = ["request.completed", "request.failed"];

#[derive(Debug, Clone, FromRow)]
pub struct Webhook {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub headers: String,
    pub event_types: String,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct WebhookView {
    pub id: i64,
    pub name: String,
    pub url: String,
    /// Whether a signing secret is configured. The secret itself never leaves
    /// the server.
    pub secret_set: bool,
    pub headers: serde_json::Value,
    pub event_types: Vec<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_delivery_at: Option<String>,
    pub last_delivery_status: Option<i64>,
    pub recent_failures: i64,
}

#[derive(Debug, Deserialize)]
pub struct WebhookInput {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub headers: Option<serde_json::Value>,
    #[serde(default)]
    pub event_types: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct WebhookUpdate {
    pub name: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub headers: Option<serde_json::Value>,
    #[serde(default)]
    pub clear_secret: Option<bool>,
    #[serde(default)]
    pub event_types: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct WebhookDeliveryView {
    pub id: i64,
    pub webhook_id: i64,
    pub event_type: String,
    pub request_id: Option<String>,
    pub status_code: Option<i64>,
    pub attempts: i64,
    pub error: Option<String>,
    pub duration_ms: i64,
    pub created_at: String,
}

impl Webhook {
    /// Event types as a list, falling back to every known type when the stored
    /// JSON is unreadable, so a corrupted row still delivers instead of going
    /// silent.
    pub fn event_type_list(&self) -> Vec<String> {
        serde_json::from_str::<Vec<String>>(&self.event_types)
            .ok()
            .filter(|values| !values.is_empty())
            .unwrap_or_else(|| {
                WEBHOOK_EVENT_TYPES
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect()
            })
    }

    pub fn subscribes_to(&self, event_type: &str) -> bool {
        self.event_type_list()
            .iter()
            .any(|candidate| candidate == event_type)
    }
}
