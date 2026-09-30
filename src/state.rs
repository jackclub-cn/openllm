use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, broadcast};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub client: Client,
    pub admin_token: Option<String>,
    pub round_robin: Arc<Mutex<HashMap<i64, usize>>>,
    /// Per-provider cursor used to rotate upstream API keys across requests.
    pub provider_key_cursor: Arc<Mutex<HashMap<i64, usize>>>,
    /// In-memory cooldowns for provider keys that recently failed.
    pub provider_key_cooldown: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Throttles provider-key usage timestamps on the request hot path.
    pub provider_key_touched: Arc<Mutex<HashMap<i64, Instant>>>,
    pub events: broadcast::Sender<UsageEvent>,
    /// Cached "does the gateway require an API key" flag. `None` means it must
    /// be re-read from SQLite. Invalidated whenever keys are mutated.
    pub auth_required: Arc<RwLock<Option<bool>>>,
    /// Throttles `last_used_at` writes so the hot request path does not take a
    /// SQLite write lock on every single call.
    pub key_touched: Arc<Mutex<HashMap<i64, Instant>>>,
    /// Throttles automatic usage-retention cleanup to once per hour.
    pub retention_last_run: Arc<Mutex<Option<Instant>>>,
    /// Cached models.dev capability catalog plus the instant it was fetched.
    /// Held behind a read-mostly lock because provider sync refreshes it only
    /// once per TTL.
    pub models_dev: Arc<RwLock<Option<CatalogCache>>>,
}

/// A fetched models.dev catalog and the instant it was retrieved.
pub type CatalogCache = (Instant, Arc<crate::models_dev::Catalog>);

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub id: i64,
    pub request_id: String,
    pub success: bool,
    pub streamed: bool,
}

impl AppState {
    pub fn new(pool: SqlitePool, admin_token: Option<String>) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // A total-request timeout would abort long streaming generations
            // (reasoning models routinely run past five minutes). Bound the
            // gap between reads instead, which still catches a stalled upstream
            // without capping total stream duration.
            .read_timeout(Duration::from_secs(300))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build HTTP client");

        let (events, _) = broadcast::channel(128);

        Self {
            pool,
            client,
            admin_token: admin_token.filter(|token| !token.is_empty()),
            round_robin: Arc::new(Mutex::new(HashMap::new())),
            provider_key_cursor: Arc::new(Mutex::new(HashMap::new())),
            provider_key_cooldown: Arc::new(Mutex::new(HashMap::new())),
            provider_key_touched: Arc::new(Mutex::new(HashMap::new())),
            events,
            auth_required: Arc::new(RwLock::new(None)),
            key_touched: Arc::new(Mutex::new(HashMap::new())),
            retention_last_run: Arc::new(Mutex::new(None)),
            models_dev: Arc::new(RwLock::new(None)),
        }
    }
}
