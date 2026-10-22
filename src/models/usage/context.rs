use super::*;

#[derive(Debug, Serialize)]
pub struct ContextAnalytics {
    pub requests: i64,
    pub sessions: i64,
    pub context_requests: i64,
    pub measured_requests: i64,
    pub estimated_requests: i64,
    pub agents: Vec<ContextAgentUsage>,
}

#[derive(Debug, Serialize)]
pub struct ContextAgentUsage {
    pub api_key_id: Option<i64>,
    pub label: String,
    pub requests: i64,
    pub sessions: i64,
    pub context_samples: i64,
    pub measured_samples: i64,
    pub estimated_samples: i64,
    pub prompt_tokens: i64,
    pub avg_prompt_tokens: f64,
    pub max_prompt_tokens: i64,
    pub cache_hit_rate: f64,
    pub category_tokens: Vec<ContextCategoryUsage>,
    pub unmetered_items: i64,
    pub opaque_reasoning_items: i64,
    pub models: Vec<String>,
    pub last_seen: String,
}

#[derive(Debug, Serialize)]
pub struct ContextCategoryUsage {
    pub key: String,
    pub tokens: i64,
    pub items: i64,
}
