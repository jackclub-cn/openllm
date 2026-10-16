//! Process-local prompt/session affinity for upstream prompt caches.
//!
//! A client can keep sending the same conversation to a route while the
//! selected upstream target changes because of weighted routing, load
//! balancing or a recent latency sample. That destroys the provider-side
//! prompt cache hit even though the prompt prefix is unchanged.
//!
//! The store remembers one target per `(api key, route, session)` scope. It is
//! deliberately only a preference: routing removes failed or saturated
//! candidates before consulting it, and a new target replaces the pin when the
//! old one is no longer healthy. The optional SQLite mirror lets a restart
//! retain the preference, but failures to read or write that mirror never
//! block the data path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::allocation::ANONYMOUS_API_KEY_ID;

pub(crate) const DEFAULT_SESSION_AFFINITY_TTL_SECS: u64 = 30 * 60;
pub(crate) const DEFAULT_SESSION_AFFINITY_CAPACITY: usize = 10_000;
pub(crate) const DEFAULT_SESSION_AFFINITY_PERSIST: bool = false;
pub(crate) const MAX_SESSION_AFFINITY_TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub(crate) const MAX_SESSION_AFFINITY_CAPACITY: usize = 1_000_000;

const WRITE_PRUNE_INTERVAL: u64 = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionAffinityScope {
    api_key_id: i64,
    route_id: i64,
    session_hash: String,
}

impl SessionAffinityScope {
    pub(crate) fn new(api_key_id: Option<i64>, route_id: i64, session_key: &str) -> Self {
        let api_key_id = api_key_id.unwrap_or(ANONYMOUS_API_KEY_ID);
        let mut hasher = Sha256::new();
        hasher.update(b"openllm-session-affinity-scope-v1");
        hasher.update(api_key_id.to_be_bytes());
        hasher.update(route_id.to_be_bytes());
        hasher.update(session_key.as_bytes());
        let session_hash = format!("{:x}", hasher.finalize());
        Self {
            api_key_id,
            route_id,
            session_hash,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionAffinityTarget {
    pub(crate) provider_id: i64,
    pub(crate) upstream_model: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SessionAffinityStats {
    pub(crate) entries: usize,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) rebinds: u64,
    pub(crate) evictions: u64,
    pub(crate) persistence_errors: u64,
}

#[derive(Debug)]
struct SessionAffinityEntry {
    target: SessionAffinityTarget,
    expires_at: Instant,
    last_used: Instant,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionAffinityConfig {
    pub(crate) ttl: Option<Duration>,
    pub(crate) capacity: usize,
    pub(crate) persist: bool,
}

impl SessionAffinityConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            ttl: parse_session_affinity_ttl(
                std::env::var("OPENLLM_SESSION_AFFINITY_TTL_SECS")
                    .ok()
                    .as_deref(),
            ),
            capacity: parse_session_affinity_capacity(
                std::env::var("OPENLLM_SESSION_AFFINITY_CAPACITY")
                    .ok()
                    .as_deref(),
            ),
            persist: parse_session_affinity_persist(
                std::env::var("OPENLLM_SESSION_AFFINITY_PERSIST")
                    .ok()
                    .as_deref(),
            ),
        }
    }
}

pub(crate) struct SessionAffinityStore {
    entries: Mutex<HashMap<SessionAffinityScope, SessionAffinityEntry>>,
    ttl: Duration,
    capacity: usize,
    persist: bool,
    hits: AtomicU64,
    misses: AtomicU64,
    rebinds: AtomicU64,
    evictions: AtomicU64,
    persistence_errors: AtomicU64,
    writes_since_prune: AtomicU64,
}

impl SessionAffinityStore {
    pub(crate) fn new(ttl: Duration, capacity: usize, persist: bool) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            capacity: capacity.max(1),
            persist,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            rebinds: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            persistence_errors: AtomicU64::new(0),
            writes_since_prune: AtomicU64::new(0),
        }
    }

    pub(crate) fn ttl_secs(&self) -> u64 {
        self.ttl.as_secs()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn persist(&self) -> bool {
        self.persist
    }

    pub(crate) async fn lookup(
        &self,
        pool: &SqlitePool,
        scope: &SessionAffinityScope,
    ) -> Option<SessionAffinityTarget> {
        self.lookup_inner(pool, scope, true).await
    }

    pub(crate) async fn peek(
        &self,
        pool: &SqlitePool,
        scope: &SessionAffinityScope,
    ) -> Option<SessionAffinityTarget> {
        self.lookup_inner(pool, scope, false).await
    }

    async fn lookup_inner(
        &self,
        pool: &SqlitePool,
        scope: &SessionAffinityScope,
        record_metrics: bool,
    ) -> Option<SessionAffinityTarget> {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get_mut(scope) {
            if entry.expires_at > now {
                let target = entry.target.clone();
                entry.expires_at = now + self.ttl;
                entry.last_used = now;
                drop(entries);
                if record_metrics {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                }
                return Some(target);
            }
            entries.remove(scope);
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        drop(entries);

        if !self.persist {
            if record_metrics {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            return None;
        }

        let now_unix_ms = now_unix_millis();
        let persisted = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT provider_id, upstream_model, expires_at_unix_ms \
             FROM session_affinity \
             WHERE api_key_id = ? AND route_id = ? AND session_hash = ? \
               AND expires_at_unix_ms > ?",
        )
        .bind(scope.api_key_id)
        .bind(scope.route_id)
        .bind(&scope.session_hash)
        .bind(now_unix_ms)
        .fetch_optional(pool)
        .await;
        let persisted = match persisted {
            Ok(row) => row,
            Err(error) => {
                self.record_persistence_error(error);
                if record_metrics {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                }
                return None;
            }
        };
        let Some((provider_id, upstream_model, expires_at_unix_ms)) = persisted else {
            if record_metrics {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            return None;
        };
        let remaining_ms = expires_at_unix_ms.saturating_sub(now_unix_ms) as u64;
        let remaining = Duration::from_millis(remaining_ms).min(self.ttl);
        let target = SessionAffinityTarget {
            provider_id,
            upstream_model,
        };
        let mut entries = self.entries.lock().await;
        self.insert_locked(&mut entries, scope.clone(), target.clone(), remaining, now);
        drop(entries);
        if record_metrics {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        Some(target)
    }

    pub(crate) async fn bind(
        &self,
        pool: &SqlitePool,
        scope: &SessionAffinityScope,
        target: SessionAffinityTarget,
    ) {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        let replaced = entries
            .get(scope)
            .is_some_and(|entry| entry.target != target);
        if replaced {
            self.rebinds.fetch_add(1, Ordering::Relaxed);
        }
        self.insert_locked(&mut entries, scope.clone(), target.clone(), self.ttl, now);
        drop(entries);

        if !self.persist {
            return;
        }
        let expires_at_unix_ms = now_unix_millis().saturating_add(self.ttl.as_millis() as i64);
        let write = sqlx::query(
            "INSERT INTO session_affinity (
                 api_key_id, route_id, session_hash, provider_id, upstream_model,
                 expires_at_unix_ms, updated_at_unix_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(api_key_id, route_id, session_hash) DO UPDATE SET \
                 provider_id = excluded.provider_id, \
                 upstream_model = excluded.upstream_model, \
                 expires_at_unix_ms = excluded.expires_at_unix_ms, \
                 updated_at_unix_ms = excluded.updated_at_unix_ms",
        )
        .bind(scope.api_key_id)
        .bind(scope.route_id)
        .bind(&scope.session_hash)
        .bind(target.provider_id)
        .bind(&target.upstream_model)
        .bind(expires_at_unix_ms)
        .bind(now_unix_millis())
        .execute(pool)
        .await;
        if let Err(error) = write {
            self.record_persistence_error(error);
            return;
        }

        let write_number = self.writes_since_prune.fetch_add(1, Ordering::Relaxed) + 1;
        if write_number.is_multiple_of(WRITE_PRUNE_INTERVAL) {
            let prune = sqlx::query("DELETE FROM session_affinity WHERE expires_at_unix_ms <= ?")
                .bind(now_unix_millis())
                .execute(pool)
                .await;
            if let Err(error) = prune {
                self.record_persistence_error(error);
            }
        }
    }

    fn insert_locked(
        &self,
        entries: &mut HashMap<SessionAffinityScope, SessionAffinityEntry>,
        scope: SessionAffinityScope,
        target: SessionAffinityTarget,
        ttl: Duration,
        now: Instant,
    ) {
        if !entries.contains_key(&scope) && entries.len() >= self.capacity {
            entries.retain(|_, entry| entry.expires_at > now);
            while entries.len() >= self.capacity {
                let Some(victim) = entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(scope, _)| scope.clone())
                else {
                    break;
                };
                entries.remove(&victim);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
        entries.insert(
            scope,
            SessionAffinityEntry {
                target,
                expires_at: now + ttl,
                last_used: now,
            },
        );
    }

    fn record_persistence_error(&self, error: sqlx::Error) {
        let count = self.persistence_errors.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count.is_power_of_two() {
            tracing::warn!(
                %error,
                failures = count,
                "session-affinity SQLite mirror is unavailable; using process memory only"
            );
        }
    }

    pub(crate) async fn stats(&self) -> SessionAffinityStats {
        let mut entries = self.entries.lock().await;
        let before = entries.len();
        let now = Instant::now();
        entries.retain(|_, entry| entry.expires_at > now);
        if before > entries.len() {
            self.evictions
                .fetch_add((before - entries.len()) as u64, Ordering::Relaxed);
        }
        let entries = entries.len();
        SessionAffinityStats {
            entries,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            rebinds: self.rebinds.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            persistence_errors: self.persistence_errors.load(Ordering::Relaxed),
        }
    }
}

pub(crate) fn parse_session_affinity_ttl(value: Option<&str>) -> Option<Duration> {
    match value.map(str::trim) {
        None | Some("") => Some(Duration::from_secs(DEFAULT_SESSION_AFFINITY_TTL_SECS)),
        Some("0") => None,
        Some(raw) => Some(Duration::from_secs(
            raw.parse::<u64>()
                .unwrap_or(DEFAULT_SESSION_AFFINITY_TTL_SECS)
                .clamp(1, MAX_SESSION_AFFINITY_TTL_SECS),
        )),
    }
}

pub(crate) fn parse_session_affinity_capacity(value: Option<&str>) -> usize {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SESSION_AFFINITY_CAPACITY)
        .clamp(1, MAX_SESSION_AFFINITY_CAPACITY)
}

pub(crate) fn parse_session_affinity_persist(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_SESSION_AFFINITY_PERSIST,
        Some(raw) => matches!(
            raw.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
    }
}

fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
