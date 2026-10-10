use super::*;

#[derive(Debug, Clone, FromRow)]
pub struct AuditLog {
    pub id: i64,
    pub created_at: String,
    pub action: String,
    pub entity: String,
    pub entity_id: Option<String>,
    pub summary: String,
    pub detail: Option<String>,
    pub actor: String,
}

#[derive(Debug, Serialize)]
pub struct AuditLogView {
    pub id: i64,
    pub created_at: String,
    pub action: String,
    pub entity: String,
    pub entity_id: Option<String>,
    pub summary: String,
    pub detail: Option<serde_json::Value>,
    pub actor: String,
}

impl From<AuditLog> for AuditLogView {
    fn from(row: AuditLog) -> Self {
        Self {
            id: row.id,
            created_at: row.created_at,
            action: row.action,
            entity: row.entity,
            entity_id: row.entity_id,
            summary: row.summary,
            detail: row
                .detail
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()),
            actor: row.actor,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuditLogPage {
    pub items: Vec<AuditLogView>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub page_size: Option<i64>,
    #[serde(default)]
    pub entity: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
}
