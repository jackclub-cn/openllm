use super::*;

impl AppState {
    pub fn new(pool: SqlitePool, admin_token: Option<String>) -> Self {
        let idle_timeout = parse_positive_secs(
            std::env::var("OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS")
                .ok()
                .as_deref(),
            DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS,
        );
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // A total-request timeout would abort long streaming generations
            // (reasoning models routinely run past five minutes). Bound the
            // gap between reads instead, which still catches a stalled upstream
            // without capping total stream duration.
            .read_timeout(Duration::from_secs(idle_timeout))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("failed to build HTTP client");

        let (events, _) = broadcast::channel(128);
        let (activity_events, _) = broadcast::channel(1024);
        let (circuit_events, _) = broadcast::channel(128);
        let request_capacity_limit = parse_concurrency_limit(
            std::env::var("OPENLLM_MAX_CONCURRENT_REQUESTS")
                .ok()
                .as_deref(),
        )
        .unwrap_or(0);
        let request_capacity =
            (request_capacity_limit > 0).then(|| Arc::new(Semaphore::new(request_capacity_limit)));
        let request_bytes_limit = parse_inflight_bytes_mib(
            std::env::var("OPENLLM_MAX_INFLIGHT_REQUEST_MIB")
                .ok()
                .as_deref(),
        )
        .unwrap_or(DEFAULT_MAX_INFLIGHT_REQUEST_MIB * 1024 * 1024);
        let inflight_request_bytes = (request_bytes_limit > 0)
            .then(|| Arc::new(RequestByteBudget::new(request_bytes_limit)));
        let memory_shed_ratio = parse_shed_ratio(
            std::env::var("OPENLLM_MEMORY_SHED_RATIO").ok().as_deref(),
            DEFAULT_MEMORY_SHED_RATIO,
        );
        let memory_limit_env = std::env::var("OPENLLM_MEMORY_LIMIT_MIB").ok();
        let (memory_limit_bytes, memory_limit_source) =
            resolve_memory_limit(memory_limit_env.as_deref(), cgroup_memory_max_bytes());
        let memory_pressure =
            memory_limit_bytes.map(|bytes| Arc::new(MemoryPressure::new(bytes, memory_shed_ratio)));
        if memory_limit_source == "cgroup" {
            tracing::info!(
                limit_mib = memory_limit_bytes.unwrap_or(0) / (1024 * 1024),
                "memory-pressure guard armed from the container cgroup limit"
            );
        }
        if memory_pressure.is_some() && !(cfg!(target_os = "linux") || cfg!(target_os = "windows"))
        {
            tracing::warn!(
                "OPENLLM_MEMORY_LIMIT_MIB is set but memory sampling is only available on Linux \
                 and Windows; the memory-pressure guard will stay inert"
            );
        }

        let affinity_config = SessionAffinityConfig::from_env();
        let session_affinity = affinity_config.ttl.map(|ttl| {
            Arc::new(SessionAffinityStore::new(
                ttl,
                affinity_config.capacity,
                affinity_config.persist,
            ))
        });

        Self {
            pool,
            client,
            release_feed: Arc::new(crate::release_feed::ReleaseFeed::default()),
            admin_token: admin_token.filter(|token| !token.is_empty()),
            round_robin: Arc::new(Mutex::new(HashMap::new())),
            session_affinity,
            provider_key_cursor: Arc::new(Mutex::new(HashMap::new())),
            provider_cooldown: Arc::new(Mutex::new(HashMap::new())),
            provider_failure_streak: Arc::new(Mutex::new(HashMap::new())),
            provider_probe: Arc::new(Mutex::new(HashMap::new())),
            provider_circuit_trips: Arc::new(Mutex::new(HashMap::new())),
            target_cooldown: Arc::new(Mutex::new(HashMap::new())),
            target_slow: Arc::new(Mutex::new(HashMap::new())),
            provider_key_cooldown: Arc::new(Mutex::new(HashMap::new())),
            target_sink: Arc::new(Mutex::new(HashMap::new())),
            busy_waiting_total: Arc::new(AtomicUsize::new(0)),
            busy_waiting_by_route: Arc::new(std::sync::Mutex::new(HashMap::new())),
            provider_key_error_state: Arc::new(Mutex::new(HashSet::new())),
            provider_key_touched: Arc::new(Mutex::new(HashMap::new())),
            provider_key_rpm: Arc::new(Mutex::new(HashMap::new())),
            provider_key_concurrency: Arc::new(Mutex::new(HashMap::new())),
            provider_key_queue: Arc::new(Mutex::new(HashMap::new())),
            provider_concurrency: Arc::new(Mutex::new(HashMap::new())),
            model_concurrency: Arc::new(Mutex::new(HashMap::new())),
            request_capacity,
            request_capacity_limit,
            requests_shed: Arc::new(AtomicU64::new(0)),
            inflight_request_bytes,
            request_bytes_limit,
            request_bytes_shed: Arc::new(AtomicU64::new(0)),
            admission_wait: parse_admission_wait_ms(
                std::env::var("OPENLLM_ADMISSION_WAIT_MS").ok().as_deref(),
            ),
            body_read_timeout: body_read_timeout(),
            request_timeout: request_timeout(),
            memory_pressure,
            memory_limit_source,
            memory_shed_ratio,
            memory_shed: Arc::new(AtomicU64::new(0)),
            runtime_lag_millis: Arc::new(AtomicU64::new(0)),
            maintenance_last_run_unix: Arc::new(AtomicU64::new(0)),
            maintenance_runs: Arc::new(AtomicU64::new(0)),
            started_at: Instant::now(),
            shutting_down: Arc::new(AtomicBool::new(false)),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(0),
            events,
            circuit_events,
            auth_required: Arc::new(RwLock::new(None)),
            guardrails: Arc::new(RwLock::new(None)),
            inspector: Arc::new(RwLock::new(None)),
            resilience: Arc::new(RwLock::new(None)),
            provider_failure_meta: Arc::new(Mutex::new(HashMap::new())),
            target_failure_meta: Arc::new(Mutex::new(HashMap::new())),
            provider_key_failure_meta: Arc::new(Mutex::new(HashMap::new())),
            failure_classifications: Arc::new(Mutex::new(HashMap::new())),
            quota_snapshots: Arc::new(Mutex::new(HashMap::new())),
            capability_profiles: Arc::new(Mutex::new(CapabilityCache::default())),
            sse_keepalive: parse_keepalive_secs(
                std::env::var("OPENLLM_SSE_KEEPALIVE_SECS").ok().as_deref(),
            ),
            key_touched: Arc::new(Mutex::new(HashMap::new())),
            retention_last_run: Arc::new(Mutex::new(None)),
            models_dev: Arc::new(RwLock::new(None)),
            activity: Arc::new(RequestActivityStore::from_env(activity_events.clone())),
            activity_events,
            provider_model_sync: Arc::new(Mutex::new(HashSet::new())),
            provider_health_check: Arc::new(Mutex::new(HashSet::new())),
            resilience_state_persist: crate::resilience_state::resilience_state_persist(),
            resilience_dirty: Arc::new(std::sync::Mutex::new(DirtyResilienceState::default())),
            resilience_flush: Arc::new(Notify::new()),
            pool_catalog: Arc::new(RwLock::new(None)),
            allocation_ledger: Arc::new(Mutex::new(AllocationLedger::new(Instant::now()))),
        }
    }

    /// Returns the cached provider-pool configuration, loading it from SQLite
    /// on first use or after a pool mutation invalidated the cache.
    pub(crate) async fn pool_catalog(&self) -> AppResult<Arc<PoolCatalog>> {
        // Copy the cached Arc out in its own statement so the read guard is
        // released before any write lock is requested (tokio's RwLock is
        // write-preferring, so holding both would deadlock).
        let cached = self.pool_catalog.read().await.clone();
        if let Some(catalog) = cached {
            return Ok(catalog);
        }
        let catalog = Arc::new(PoolCatalog::load(&self.pool).await?);
        *self.pool_catalog.write().await = Some(catalog.clone());
        Ok(catalog)
    }

    /// Drops the cached pool configuration so the next request re-reads it.
    pub(crate) async fn invalidate_pool_catalog(&self) {
        *self.pool_catalog.write().await = None;
    }

    /// Marks a provider's circuit state as needing a persisted update.
    ///
    /// Cheap and synchronous on purpose: it is called from the failure path,
    /// and the actual SQLite write is coalesced by the writer task.
    pub(crate) fn mark_provider_resilience_dirty(&self, provider_id: i64) {
        self.mark_resilience_dirty(|dirty| {
            dirty.providers.insert(provider_id);
        });
    }

    /// Marks one provider model's cooldown as needing a persisted update.
    pub(crate) fn mark_target_resilience_dirty(&self, provider_id: i64, model: &str) {
        self.mark_resilience_dirty(|dirty| {
            dirty.targets.insert((provider_id, model.to_string()));
        });
    }

    /// Marks one short-lived target sink as needing a persisted update.
    pub(crate) fn mark_target_sink_dirty(&self, key: &TargetSinkKey) {
        self.mark_resilience_dirty(|dirty| {
            dirty.sinks.insert(key.clone());
        });
    }

    /// Marks one upstream credential's cooldown as needing a persisted update.
    pub(crate) fn mark_provider_key_resilience_dirty(&self, provider_key_id: i64) {
        self.mark_resilience_dirty(|dirty| {
            dirty.provider_keys.insert(provider_key_id);
        });
    }

    /// Empties every in-memory routing cooldown and marks exactly what was
    /// removed as dirty, so the mirror deletes the rows it still holds.
    ///
    /// The keys are captured *before* the maps are emptied: the dirty mark is a
    /// request to persist "whatever the runtime state is now", so marking the
    /// live keys after the clear would mark nothing and leave the persisted rows
    /// behind. Marking after the clear (rather than before) also closes the race
    /// with the writer task — a writer that picks the mark up always observes the
    /// cleared maps. Returns how many entries were dropped from each map.
    pub(crate) async fn clear_all_resilience_state(&self) -> ClearedCooldowns {
        let mut providers = self
            .provider_failure_streak
            .lock()
            .await
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        providers.extend(self.provider_cooldown.lock().await.keys().copied());
        let targets = self
            .target_cooldown
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let provider_keys = self
            .provider_key_cooldown
            .lock()
            .await
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let sinks = self
            .target_sink
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();

        let cleared = ClearedCooldowns {
            providers: {
                let mut cooldowns = self.provider_cooldown.lock().await;
                let count = cooldowns.len();
                cooldowns.clear();
                count
            },
            models: {
                let mut cooldowns = self.target_cooldown.lock().await;
                let count = cooldowns.len();
                cooldowns.clear();
                count
            },
            provider_keys: {
                let mut cooldowns = self.provider_key_cooldown.lock().await;
                let count = cooldowns.len();
                cooldowns.clear();
                count
            },
            sinks: {
                let mut sinks = self.target_sink.lock().await;
                let count = sinks.len();
                sinks.clear();
                count
            },
        };
        self.provider_failure_streak.lock().await.clear();
        self.provider_probe.lock().await.clear();
        self.provider_failure_meta.lock().await.clear();
        self.target_failure_meta.lock().await.clear();
        self.provider_key_failure_meta.lock().await.clear();

        self.mark_resilience_dirty(|dirty| {
            dirty.providers.extend(providers);
            dirty.targets.extend(targets);
            dirty.provider_keys.extend(provider_keys);
            dirty.sinks.extend(sinks);
        });
        cleared
    }

    /// Attempts to reserve one bounded-wait slot for a route.
    ///
    /// Both the route-specific limit and the process-wide ceiling apply. A
    /// missing slot means the request should fail/shed immediately instead of
    /// adding pressure to the busy route.
    pub(crate) fn try_acquire_busy_wait(
        &self,
        route_id: i64,
        route_limit: usize,
    ) -> Option<BusyWaitLease> {
        let route_limit = route_limit.min(MAX_BUSY_WAITERS);
        if route_limit == 0 {
            return None;
        }
        let mut by_route = self
            .busy_waiting_by_route
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let route_waiters = by_route.get(&route_id).copied().unwrap_or(0);
        if route_waiters >= route_limit
            || self.busy_waiting_total.load(Ordering::Acquire) >= MAX_BUSY_WAITERS
        {
            return None;
        }
        *by_route.entry(route_id).or_default() += 1;
        self.busy_waiting_total.fetch_add(1, Ordering::AcqRel);
        Some(BusyWaitLease {
            total: self.busy_waiting_total.clone(),
            by_route: self.busy_waiting_by_route.clone(),
            route_id,
        })
    }

    pub(super) fn mark_resilience_dirty(&self, mark: impl FnOnce(&mut DirtyResilienceState)) {
        if !self.resilience_state_persist {
            return;
        }
        {
            let mut dirty = self
                .resilience_dirty
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            mark(&mut dirty);
        }
        self.resilience_flush.notify_one();
    }

    pub async fn load_provider_key_error_state(&self) -> Result<(), sqlx::Error> {
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM provider_api_keys WHERE last_error IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut errors = self.provider_key_error_state.lock().await;
        errors.clear();
        errors.extend(ids);
        Ok(())
    }

    /// Returns the cached guardrail policy, loading it from SQLite on first
    /// use or after a settings update invalidates the cache.
    pub async fn guardrail_settings(&self) -> AppResult<GuardrailSettings> {
        if let Some(settings) = self.guardrails.read().await.clone() {
            return Ok(settings);
        }

        let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTING_GUARDRAILS)
            .fetch_optional(&self.pool)
            .await?;
        let settings = raw
            .as_deref()
            .and_then(|raw| match serde_json::from_str::<GuardrailSettings>(raw) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    tracing::warn!(%error, "ignoring invalid guardrail settings");
                    None
                }
            })
            .unwrap_or_default();
        *self.guardrails.write().await = Some(settings.clone());
        Ok(settings)
    }

    /// Returns the cached request-inspector policy, loading it from SQLite on
    /// first use or after a settings update invalidates the cache.
    pub async fn inspector_settings(&self) -> AppResult<InspectorSettings> {
        if let Some(settings) = self.inspector.read().await.clone() {
            return Ok(settings);
        }

        let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTING_INSPECTOR)
            .fetch_optional(&self.pool)
            .await?;
        let settings = raw
            .as_deref()
            .and_then(|raw| match serde_json::from_str::<InspectorSettings>(raw) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    tracing::warn!(%error, "ignoring invalid inspector settings");
                    None
                }
            })
            .unwrap_or_default();
        *self.inspector.write().await = Some(settings.clone());
        Ok(settings)
    }

    /// Returns the cached same-target retry policy, loading it from SQLite on
    /// first use or after a settings update invalidates the cache.
    pub async fn resilience_settings(&self) -> AppResult<ResilienceSettings> {
        if let Some(settings) = self.resilience.read().await.clone() {
            return Ok(settings);
        }

        let raw = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SETTING_RESILIENCE)
            .fetch_optional(&self.pool)
            .await?;
        let settings = raw
            .as_deref()
            .and_then(
                |raw| match serde_json::from_str::<ResilienceSettings>(raw) {
                    Ok(settings) => Some(settings),
                    Err(error) => {
                        tracing::warn!(%error, "ignoring invalid resilience settings");
                        None
                    }
                },
            )
            .unwrap_or_default();
        *self.resilience.write().await = Some(settings.clone());
        Ok(settings)
    }

    /// Wall-clock deadline for the current request's fallback loop, or `None`
    /// when `OPENLLM_REQUEST_TIMEOUT_SECS=0` has disabled the budget.
    pub(crate) fn request_deadline(&self) -> Option<Instant> {
        self.request_timeout.map(|budget| Instant::now() + budget)
    }

    /// Reports the effective environment-derived runtime limits.
    ///
    /// Re-derives the same values the process parsed at startup so the console
    /// and operators can confirm them without reading the environment.
    pub(crate) fn runtime_limits(&self) -> RuntimeLimits {
        RuntimeLimits {
            upstream_idle_timeout_secs: parse_positive_secs(
                std::env::var("OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS")
                    .ok()
                    .as_deref(),
                DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS,
            ),
            shutdown_grace_secs: parse_shutdown_grace_secs(
                std::env::var("OPENLLM_SHUTDOWN_GRACE_SECS").ok().as_deref(),
            ),
            max_request_body_mib: parse_max_body_mib(
                std::env::var("OPENLLM_MAX_BODY_MIB").ok().as_deref(),
            ),
            max_upstream_body_mib: parse_body_mib(
                std::env::var("OPENLLM_MAX_UPSTREAM_BODY_MIB")
                    .ok()
                    .as_deref(),
                DEFAULT_MAX_UPSTREAM_BODY_MIB,
            ),
            sse_keepalive_secs: self.sse_keepalive.map(|interval| interval.as_secs()),
            max_concurrent_requests: self.request_capacity_limit,
            max_inflight_request_mib: self.request_bytes_limit / (1024 * 1024),
            admission_wait_ms: self.admission_wait.as_millis() as u64,
            stream_max_secs: max_stream_lifetime().map(|lifetime| lifetime.as_secs()),
            body_read_timeout_secs: self.body_read_timeout.map(|timeout| timeout.as_secs()),
            request_timeout_secs: self.request_timeout.map(|timeout| timeout.as_secs()),
            provider_open_threshold: provider_open_threshold(),
            resilience_state_persist: self.resilience_state_persist,
            session_affinity_ttl_secs: self.session_affinity.as_ref().map(|store| store.ttl_secs()),
            session_affinity_capacity: self
                .session_affinity
                .as_ref()
                .map(|store| store.capacity())
                .unwrap_or(crate::session_affinity::DEFAULT_SESSION_AFFINITY_CAPACITY),
            session_affinity_persist: self
                .session_affinity
                .as_ref()
                .is_some_and(|store| store.persist()),
            memory_limit_mib: self
                .memory_pressure
                .as_ref()
                .map(|pressure| pressure.limit_bytes() / (1024 * 1024))
                .unwrap_or(0),
            memory_limit_source: self.memory_limit_source.to_string(),
            memory_shed_ratio_pct: (self.memory_shed_ratio * 100.0).round() as u64,
            db_max_connections: crate::db::db_max_connections(),
            db_busy_timeout_secs: crate::db::db_busy_timeout_secs(),
            db_acquire_timeout_secs: crate::db::db_acquire_timeout_secs(),
        }
    }
}
