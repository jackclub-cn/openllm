export type DatabaseStats = {
  path?: string | null
  size_bytes: number
  free_bytes: number
  providers: number
  provider_models: number
  provider_api_keys: number
  routes: number
  access_keys: number
  webhooks: number
  webhook_deliveries: number
  audit_logs: number
  usage_logs: number
  in_flight_requests: number
}

export type DatabaseVacuumResult = {
  reclaimed_bytes: number
  database_stats: DatabaseStats
}

export type Settings = {
  admin_auth_enabled: boolean
  database: string
  version: string
  database_stats: DatabaseStats
}

export type RuntimeSettings = {
  usage_retention_days?: number | null
  limits: RuntimeLimits
}

export type RuntimeLimits = {
  upstream_idle_timeout_secs: number
  shutdown_grace_secs: number
  max_request_body_mib: number
  max_upstream_body_mib: number
  sse_keepalive_secs?: number | null
  max_concurrent_requests: number
  max_inflight_request_mib: number
  admission_wait_ms: number
  stream_max_secs?: number | null
  body_read_timeout_secs?: number | null
  request_timeout_secs?: number | null
  provider_open_threshold: number
  resilience_state_persist: boolean
  session_affinity_ttl_secs?: number | null
  session_affinity_capacity: number
  session_affinity_persist: boolean
  memory_limit_mib: number
  memory_limit_source: string
  memory_shed_ratio_pct: number
  db_max_connections: number
  db_busy_timeout_secs: number
  db_acquire_timeout_secs: number
}

export type GuardrailSettings = {
  blocked_terms: string[]
  max_prompt_tokens?: number | null
}

export type InspectorSettings = {
  capture_request_previews: boolean
  request_preview_max_chars: number
}

export type ResilienceSettings = {
  stream_recovery_enabled: boolean
  stream_recovery_max_retries: number
  max_retries: number
  retry_backoff_ms: number
  retry_max_backoff_ms: number
}

export type ProviderCooldown = {
  provider_id: number
  provider_name?: string | null
  remaining_seconds: number
  failure_streak: number
  state: 'closed' | 'open' | 'half_open'
  scope?: string | null
  retry?: string | null
  confidence?: string | null
  reason_code?: string | null
  observed_seconds_ago?: number | null
}

export type ModelCooldown = {
  provider_id: number
  provider_name?: string | null
  model: string
  remaining_seconds: number
  scope?: string | null
  retry?: string | null
  confidence?: string | null
  reason_code?: string | null
  observed_seconds_ago?: number | null
}

export type ProviderKeyCooldown = {
  provider_key_id: number
  provider_name?: string | null
  key_name?: string | null
  remaining_seconds: number
  scope?: string | null
  retry?: string | null
  confidence?: string | null
  reason_code?: string | null
  observed_seconds_ago?: number | null
}

export type CooldownSnapshot = {
  provider_cooldowns: ProviderCooldown[]
  model_cooldowns: ModelCooldown[]
  provider_key_cooldowns: ProviderKeyCooldown[]
  provider_open_threshold: number
  generated_at: string
}

export type ClearCooldownsResult = {
  cleared_provider_cooldowns: number
  cleared_model_cooldowns: number
  cleared_provider_key_cooldowns: number
}

export type RuntimeStatus = {
  version: string
  uptime_seconds: number
  draining: boolean
  runtime_lag_millis: number
  memory: {
    limit_mib: number
    used_mib?: number | null
    used_ratio?: number | null
    pressure: boolean
    source: string
    shed_total: number
  }
  admission: {
    in_flight: number
    max_concurrent_requests: number
    in_flight_request_mib: number
    max_inflight_request_mib: number
    requests_shed_total: number
    request_bytes_shed_total: number
  }
  database: {
    connections: number
    idle: number
    max_connections: number
  }
  maintenance: {
    runs_total: number
    last_run_unix: number
  }
  circuits: {
    open: number
    half_open: number
    threshold: number
  }
}
