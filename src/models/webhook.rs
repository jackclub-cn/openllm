use super::*;

/// Event types selected when a webhook does not specify any.
pub const WEBHOOK_DEFAULT_EVENT_TYPES: [&str; 2] = ["request.completed", "request.failed"];
/// Event types a webhook can subscribe to.
pub const WEBHOOK_EVENT_TYPES: [&str; 4] = [
    "request.completed",
    "request.failed",
    "provider.circuit_open",
    "provider.circuit_closed",
];

/// Body shapes a webhook can post.
///
/// `generic` is the gateway's own event envelope; every other kind renders a
/// message for that chat platform, so an operator can point a webhook straight
/// at a channel instead of writing an adapter.
pub const WEBHOOK_CHANNEL_KINDS: [&str; 6] = [
    "generic", "slack", "discord", "telegram", "feishu", "dingtalk",
];

/// The kind used when a webhook does not name one.
pub const WEBHOOK_DEFAULT_CHANNEL_KIND: &str = "generic";

#[derive(Debug, Clone, FromRow)]
pub struct Webhook {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub headers: String,
    pub kind: String,
    pub body_fields: String,
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
    pub kind: String,
    /// Extra static fields merged into the rendered channel body.
    pub body_fields: serde_json::Value,
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
    pub kind: Option<String>,
    #[serde(default)]
    pub body_fields: Option<serde_json::Value>,
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
    pub kind: Option<String>,
    #[serde(default)]
    pub body_fields: Option<serde_json::Value>,
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

    /// Extra body fields as an object, falling back to an empty one when the
    /// stored JSON is unreadable, so a corrupted row still delivers.
    pub fn body_fields_object(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(&self.body_fields)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    /// True when this webhook renders a chat channel body rather than the
    /// generic event envelope.
    pub fn is_chat_channel(&self) -> bool {
        self.kind != WEBHOOK_DEFAULT_CHANNEL_KIND
    }
}
