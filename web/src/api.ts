const ADMIN_TOKEN_KEY = 'openllm-admin-token'

export class ApiError extends Error {
  status: number

  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

export function getAdminToken() {
  return localStorage.getItem(ADMIN_TOKEN_KEY) || ''
}

export function setAdminToken(value: string) {
  if (value.trim()) localStorage.setItem(ADMIN_TOKEN_KEY, value.trim())
  else localStorage.removeItem(ADMIN_TOKEN_KEY)
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers)
  if (init?.body && !headers.has('Content-Type')) headers.set('Content-Type', 'application/json')
  const adminToken = getAdminToken()
  if (adminToken) headers.set('x-admin-token', adminToken)

  const response = await fetch(path, { ...init, headers })
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`
    try {
      const body = await response.json()
      message = body?.error?.message || message
    } catch {
      // Keep the HTTP status when the body is not JSON.
    }
    throw new ApiError(message, response.status)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

export type DownloadedFile = {
  blob: Blob
  /// The server capped the export at its row limit; the file is incomplete.
  truncated: boolean
}

async function requestBlob(path: string): Promise<DownloadedFile> {
  const headers = new Headers()
  const adminToken = getAdminToken()
  if (adminToken) headers.set('x-admin-token', adminToken)
  const response = await fetch(path, { headers })
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`
    try {
      const body = await response.json()
      message = body?.error?.message || message
    } catch {
      // Keep the HTTP status when the body is not JSON.
    }
    throw new ApiError(message, response.status)
  }
  return {
    blob: await response.blob(),
    truncated: response.headers.get('x-openllm-export-truncated') === 'true',
  }
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: 'POST', body: body === undefined ? undefined : JSON.stringify(body) }),
  put: <T>(path: string, body: unknown) =>
    request<T>(path, { method: 'PUT', body: JSON.stringify(body) }),
  delete: <T>(path: string) => request<T>(path, { method: 'DELETE' }),
  download: async (path: string) => (await requestBlob(path)).blob,
  downloadWithMeta: (path: string) => requestBlob(path),
}

export type ProviderApiKey = {
  id: number
  name: string
  api_key_set: boolean
  api_key_suffix: string
  enabled: boolean
  last_used_at?: string
  last_error_at?: string
  last_error?: string
  last_test_at?: string | null
  last_test_ok?: boolean | null
  last_test_latency_ms?: number | null
  last_test_checked?: 'inference' | 'models' | null
  last_test_message?: string | null
  requests: number
  success_rate: number
  avg_latency_ms: number
  prompt_tokens: number
  completion_tokens: number
  cooldown_seconds?: number
  created_at: string
}

export type ProviderApiKeyInput = {
  id?: number
  name: string
  api_key?: string
  enabled: boolean
}

export type Provider = {
  id: number
  name: string
  provider_type: 'openai' | 'anthropic' | 'ollama' | 'custom'
  base_url: string
  model_prefix: string
  models_dev_id?: string
  headers: Record<string, string>
  enabled: boolean
  api_key_set: boolean
  api_keys: ProviderApiKey[]
  tool_search_supported: boolean
  tool_search_checked_at?: string | null
  models: string[]
  models_synced_at?: string
  models_sync_error?: string
  last_test_at?: string | null
  last_test_ok?: boolean | null
  last_test_latency_ms?: number | null
  last_test_checked?: 'inference' | 'models' | null
  last_test_message?: string | null
  health_check_interval_minutes?: number | null
  health_check_model?: string | null
  cooldown_seconds?: number | null
  circuit_state?: 'closed' | 'open' | 'half_open'
  configured_cooldown_seconds?: number | null
  timeout_seconds?: number | null
  max_concurrency?: number | null
  queue_timeout_seconds?: number | null
  quota_kind?: string | null
  models_sync_interval_minutes?: number | null
  models_sync_attempted_at?: string
  created_at: string
  updated_at: string
}

export type ProviderQuotaItem = {
  key: string
  label: string
  used?: number | null
  limit?: number | null
  remaining?: number | null
  unit: string
  percent?: number | null
  reset_at?: string | null
}

export type ProviderQuotaDetail = {
  label: string
  value: string
}

export type ProviderQuotaModel = {
  model_name: string
  model_id?: string | null
  monthly_credits?: number | null
  promo_credits?: number | null
  note?: string | null
}

export type ProviderPrice = {
  model_name: string
  input?: number | null
  output?: number | null
  cache_read?: number | null
  cache_write?: number | null
}

export type ProviderQuota = {
  kind: string
  title: string
  plan_name?: string | null
  key_id?: number | null
  key_name?: string | null
  key_suffix?: string | null
  source_url?: string | null
  items: ProviderQuotaItem[]
  model_quotas: ProviderQuotaModel[]
  details: ProviderQuotaDetail[]
  prices: ProviderPrice[]
  fetched_at: string
}

export type ProviderKeyTestItem = {
  key_id?: number | null
  key_name: string
  api_key_suffix: string
  ok: boolean
  latency_ms: number
  message: string
  checked: 'inference' | 'models'
}

export type ProviderKeyTestResult = {
  provider_id: number
  provider_name: string
  total: number
  ok: number
  failed: number
  model?: string | null
  results: ProviderKeyTestItem[]
}

export type ProviderKeyTestAllResult = {
  total_providers: number
  tested_providers: number
  healthy_providers: number
  failed_providers: number
  total_keys: number
  healthy_keys: number
  failed_keys: number
}

export type ProviderInput = {
  name: string
  provider_type: Provider['provider_type']
  base_url: string
  model_prefix: string
  api_key?: string
  api_keys?: ProviderApiKeyInput[]
  headers: Record<string, string>
  auto_sync_models: boolean
  models: string[]
  health_check_interval_minutes?: number | null
  health_check_model?: string | null
  models_sync_interval_minutes?: number | null
  timeout_seconds?: number | null
  configured_cooldown_seconds?: number | null
  max_concurrency?: number | null
  queue_timeout_seconds?: number | null
}

export type ProviderModelLimit = {
  model_name: string
  enabled: boolean
  supported_endpoints: string[]
  supported_endpoints_override?: string[] | null
  context_limit?: number | null
  input_limit?: number | null
  output_limit?: number | null
  context_override?: number | null
  input_override?: number | null
  output_override?: number | null
  max_concurrency?: number | null
  queue_timeout_seconds?: number | null
  cost_input?: number | null
  cost_output?: number | null
  cost_cache_read?: number | null
  cost_cache_write?: number | null
  cost_input_override?: number | null
  cost_output_override?: number | null
  cost_cache_read_override?: number | null
  cost_cache_write_override?: number | null
}

export type ProviderModelLimitInput = {
  model_name: string
  enabled: boolean
  supported_endpoints_override?: string[] | null
  context_limit?: number | null
  input_limit?: number | null
  output_limit?: number | null
  max_concurrency?: number | null
  queue_timeout_seconds?: number | null
  cost_input_override?: number | null
  cost_output_override?: number | null
  cost_cache_read_override?: number | null
  cost_cache_write_override?: number | null
}

export type ModelInventory = {
  provider_id: number
  provider_name: string
  provider_enabled: boolean
  model_prefix: string
  model_name: string
  enabled: boolean
  context_limit?: number | null
  input_limit?: number | null
  output_limit?: number | null
  /** Endpoints the upstream declares, after any manual override. */
  supported_endpoints: string[]
  /** Endpoints the gateway accepts, including protocol translation. */
  served_endpoints: string[]
  cost_input?: number | null
  cost_output?: number | null
  cost_cache_read?: number | null
  cost_cache_write?: number | null
}

export type RouteTarget = {
  id?: number
  provider_id: number
  provider_name?: string
  provider_type?: string
  upstream_model: string
  supported_endpoints?: string[]
  context_limit?: number | null
  input_limit?: number | null
  output_limit?: number | null
  provider_enabled?: boolean
  model_enabled?: boolean
  model_prefix?: string
  weight: number
  priority: number
  enabled: boolean
}

export type GatewayRoute = {
  id: number
  name: string
  model_pattern: string
  strategy:
    | 'priority'
    | 'weighted'
    | 'round_robin'
    | 'cost_optimized'
    | 'latency_optimized'
    | 'least_used'
  enabled: boolean
  context_limit?: number | null
  input_limit?: number | null
  output_limit?: number | null
  limits_verified: boolean
  targets: RouteTarget[]
  created_at: string
  updated_at: string
}

export type RouteDiagnoseTarget = {
  provider_id: number
  provider_name: string
  provider_type: string
  upstream_model: string
  eligible: boolean
  reason: string
  supported_endpoints: string[]
  provider_health?: boolean | null
}

export type RouteDiagnoseRuntimeTarget = {
  order: number
  provider_id: number
  provider_name: string
  upstream_model: string
  input_cost_per_million?: number | null
  output_cost_per_million?: number | null
  avg_latency_ms?: number | null
  recent_requests?: number | null
  decision_reason: string
  provider_api_key_id?: number | null
  provider_api_key_name?: string | null
  provider_health?: boolean | null
}

export type RouteDiagnoseFactor = {
  name: string
  score?: number | null
  weight: number
  contribution: number
  detail: string
}

export type RouteDiagnoseCandidate = {
  order: number
  provider_id: number
  provider_name: string
  upstream_model: string
  score: number
  eligible: boolean
  exclusion?: string | null
  deprioritized: boolean
  factors: RouteDiagnoseFactor[]
}

export type RouteDiagnose = {
  model: string
  endpoint: string
  matched: boolean
  resolved: boolean
  match_type: 'explicit_route' | 'prefix' | 'direct' | 'conflict' | 'none'
  route_id?: number
  route_name?: string
  strategy?: GatewayRoute['strategy']
  message: string
  barrel?: ModelCapabilities
  barrel_incomplete: boolean
  session_id?: string
  runtime_targets?: RouteDiagnoseRuntimeTarget[]
  candidates: RouteDiagnoseCandidate[]
  targets: RouteDiagnoseTarget[]
}

export type ApiKey = {
  id: number
  name: string
  key_prefix: string
  key_suffix: string
  enabled: boolean
  last_used_at?: string
  created_at: string
  requests: number
  tokens: number
  prompt_tokens: number
  completion_tokens: number
  cost_micros?: number | null
  unpriced_requests: number
  daily_token_limit?: number | null
  daily_cost_limit_micros?: number | null
  requests_per_minute?: number | null
  max_concurrency?: number | null
  today_requests: number
  today_tokens: number
  today_prompt_tokens: number
  today_completion_tokens: number
  today_cost_micros?: number | null
  requests_this_minute: number
  current_in_flight: number
  allowed_models: string[]
  expires_at?: string | null
  routing_policy: ApiKeyRoutingPolicy
}

export type ApiKeyRoutingPolicy = {
  strategy?: string | null
  provider?: string | null
  exclude_providers: string[]
}

/// Body shape a webhook posts. `generic` is the gateway's own event envelope;
/// the others render a message for that chat platform.
export type WebhookKind =
  | 'generic'
  | 'slack'
  | 'discord'
  | 'telegram'
  | 'feishu'
  | 'dingtalk'

export type Webhook = {
  id: number
  name: string
  url: string
  secret_set: boolean
  headers: Record<string, string>
  kind: WebhookKind
  body_fields: Record<string, unknown>
  event_types: string[]
  enabled: boolean
  created_at: string
  updated_at: string
  last_delivery_at?: string | null
  last_delivery_status?: number | null
  recent_failures: number
}

export type WebhookInput = {
  name: string
  url: string
  secret?: string
  headers?: Record<string, string>
  kind?: WebhookKind
  body_fields?: Record<string, unknown>
  clear_secret?: boolean
  event_types: string[]
  enabled: boolean
}

export type WebhookDelivery = {
  id: number
  webhook_id: number
  event_type: string
  request_id?: string | null
  status_code?: number | null
  attempts: number
  error?: string | null
  duration_ms: number
  created_at: string
}

export type UsageLog = {
  id: number
  request_id: string
  session_id?: string
  api_key_id?: number
  api_key_name?: string
  route_id?: number
  route_name?: string
  provider_id?: number
  provider_name?: string
  provider_api_key_id?: number
  provider_api_key_name?: string
  requested_model: string
  upstream_model?: string
  endpoint: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cache_read_tokens: number
  cache_write_tokens: number
  estimated_cost_micros?: number | null
  latency_ms: number
  /** Streamed requests measure the first output chunk; standard requests use total time. */
  first_token_ms?: number
  /** Output tokens per second. For streams this excludes the first-token wait. */
  output_tps?: number
  status_code: number
  in_flight: boolean
  success: boolean
  streamed: boolean
  error_message?: string
  request_preview?: string
  response_preview?: string
  warning_message?: string
  created_at: string
}

export type Overview = {
  requests_today: number
  tokens_today: number
  prompt_tokens_today: number
  completion_tokens_today: number
  cache_read_today: number
  cache_write_today: number
  cache_hit_rate: number
  requests_total: number
  tokens_total: number
  prompt_tokens_total: number
  completion_tokens_total: number
  cache_read_total: number
  cache_write_total: number
  cost_today_micros: number
  cost_total_micros: number
  unpriced_today: number
  unpriced_total: number
  range_requests: number
  range_tokens: number
  range_prompt_tokens: number
  range_completion_tokens: number
  range_cache_read: number
  range_cache_write: number
  range_cache_hit_rate: number
  range_sessions: number
  range_session_coverage: number
  range_avg_requests_per_session: number
  range_session_cache_hit_rate: number
  range_cost_micros: number
  range_unpriced: number
  range_success_rate: number
  range_avg_latency_ms: number
  range_gateway_adjusted: number
  success_rate: number
  avg_latency_ms: number
  active_providers: number
  active_routes: number
  healthy_providers: number
  failed_providers: number
  untested_providers: number
  provider_keys_total: number
  healthy_provider_keys: number
  failed_provider_keys: number
  untested_provider_keys: number
  runtime_error_provider_keys: number
  cooling_providers: number
  cooling_provider_keys: number
  in_flight_requests: number
  recent_requests: UsageLog[]
  granularity: 'hour' | 'day'
  provider_usage: Array<{
    provider_id: number
    provider_name: string
    requests: number
    tokens: number
    prompt_tokens: number
    completion_tokens: number
    cost_micros?: number | null
    success_rate: number
    avg_latency_ms: number
  }>
  model_usage: Array<{
    model: string
    requests: number
    tokens: number
    prompt_tokens: number
    completion_tokens: number
    cost_micros?: number | null
    success_rate: number
    avg_latency_ms: number
  }>
  daily_usage: Array<{
    day: string
    requests: number
    tokens: number
    prompt_tokens: number
    completion_tokens: number
  }>
  daily_model_requests: DailyBreakdownUsage[]
  daily_provider_requests: DailyBreakdownUsage[]
}

export type DailyBreakdownUsage = {
  day: string
  label: string
  requests: number
}

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
}

export type ModelCooldown = {
  provider_id: number
  provider_name?: string | null
  model: string
  remaining_seconds: number
}

export type ProviderKeyCooldown = {
  provider_key_id: number
  provider_name?: string | null
  key_name?: string | null
  remaining_seconds: number
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

export type ModelCapabilities = {
  context_limit?: number
  output_limit?: number
  input_limit?: number
  attachment?: boolean
  reasoning?: boolean
  tool_call?: boolean
  structured_output?: boolean
  temperature?: boolean
  open_weights?: boolean
  input_modalities?: string[]
  output_modalities?: string[]
  cost?: Record<string, unknown>
  family?: string
  knowledge?: string
  release_date?: string
  last_updated?: string
  canonical_model_id?: string
}

export type ModelInfo = {
  id: string
  object: string
  created: number
  owned_by: string
  provider?: string
  upstream_model?: string
  capabilities?: ModelCapabilities
  target_count?: number
  limits_verified?: boolean
  context_length?: number
  max_input_tokens?: number
  max_output_tokens?: number
  max_completion_tokens?: number
  supported_endpoints?: string[]
}

export type SelfCheckStatus = 'pass' | 'warn' | 'fail'

export type SelfCheckItem = {
  id: string
  category: string
  title: string
  status: SelfCheckStatus
  detail: string
  hint?: string
}

export type SelfCheckReport = {
  generated_at: string
  overall: SelfCheckStatus
  counts: { pass: number; warn: number; fail: number }
  checks: SelfCheckItem[]
}

export type PoolAllocationUsage = {
  api_key_id: number
  weight_pct: number
  policy: 'hard' | 'soft' | 'burst'
  request_share?: number | null
  token_share?: number | null
  key_requests: number
  key_tokens: number
}

export type PoolUsage = {
  pool_id: number
  provider_id: number
  name: string
  window_seconds: number
  request_limit?: number | null
  token_limit?: number | null
  requests: number
  tokens: number
  saturated: boolean
  denials: number
  allocations: PoolAllocationUsage[]
}

/// One gateway key's reserved share of a provider pool.
export type ProviderPoolAllocation = {
  id: number
  api_key_id: number
  api_key_name?: string | null
  weight_pct: number
  policy: 'hard' | 'soft' | 'burst'
  /// Effective capacity reserved for the key, in the pool's own units.
  request_share?: number | null
  token_share?: number | null
}

/// A declared share of one provider's capacity, as returned by the API.
export type ProviderPool = {
  id: number
  provider_id: number
  provider_name?: string | null
  name: string
  enabled: boolean
  window_seconds: number
  request_limit?: number | null
  token_limit?: number | null
  saturation_threshold_pct: number
  allocations: ProviderPoolAllocation[]
  created_at: string
  updated_at: string
}

export type ProviderPoolAllocationInput = {
  api_key_id: number
  weight_pct: number
  policy: 'hard' | 'soft' | 'burst'
}

/// Create/update payload. Create requires `provider_id`; update omits it and
/// leaves any field absent from the body unchanged.
export type ProviderPoolInput = {
  provider_id?: number
  name?: string
  enabled?: boolean
  window_seconds?: number
  request_limit?: number | null
  token_limit?: number | null
  saturation_threshold_pct?: number
  allocations?: ProviderPoolAllocationInput[]
}

export type DiagnosticFailure = {
  request_id: string
  created_at: string
  requested_model: string
  upstream_model?: string | null
  provider_id?: number | null
  status_code: number
  latency_ms: number
  error_message?: string | null
  warning_message?: string | null
}

export type DiagnosticSnapshot = {
  generated_at: string
  version: string
  host: string
  draining: boolean
  uptime_seconds: number
  instance_lease?: {
    id: string
    pid: number
    host: string
    started_at: string
    heartbeat_at: string
    heartbeat_age_seconds: number
    ours: boolean
  } | null
  status: RuntimeStatus
  settings: Settings
  self_check: SelfCheckReport
  resilience: CooldownSnapshot
  pools: PoolUsage[]
  providers: Provider[]
  recent_failures: DiagnosticFailure[]
  requests_24h: {
    total: number
    successful: number
    failed: number
    failures_by_status: { status_code: number; count: number }[]
  }
  secrets_included: boolean
  notes: string[]
}

export type ImportAction = 'create' | 'update' | 'unchanged' | 'skip' | 'error'

export type ImportItem = {
  kind: string
  name: string
  action: ImportAction
  detail: string
}

export type ImportCounts = {
  create: number
  update: number
  unchanged: number
  skip: number
  error: number
}

export type ImportPlan = {
  dry_run: boolean
  applied: boolean
  valid: boolean
  document_version: number
  secrets_included: boolean
  counts: ImportCounts
  items: ImportItem[]
  problems: string[]
  /** Caveats about an applied import (per-key identity, item-by-item writes). */
  notes: string[]
}

/**
 * The portable configuration document. It is intentionally loose on the
 * console side: the page inspects the version and the secret flags, while the
 * server validates the rest.
 */
export type ConfigDocument = {
  openllm_config_version: number
  exported_at: string
  gateway_version: string
  secrets_included: boolean
  notes: string[]
  providers: { name: string }[]
  routes: { name: string }[]
  provider_pools: { name: string }[]
  webhooks: { name: string }[]
  gateway_keys: { name: string }[]
  settings: Record<string, unknown>
}

export type BackupInfo = {
  name: string
  path: string
  size_bytes: number
  created_at: string
  kind: string
}

export type DatabaseFileReport = {
  file: string
  ok: boolean
  size_bytes: number
  integrity: string
  tables: string[]
  missing_tables: string[]
  applied_migrations: number
  schema_version: number
  providers: number
  routes: number
  api_keys: number
  webhooks: number
  provider_pools: number
  problems: string[]
}

export type MigrationEntry = {
  version: number
  description: string
}

export type MigrationReport = {
  database: string
  up_to_date: boolean
  applied: MigrationEntry[]
  pending: MigrationEntry[]
  unknown_applied: number[]
  data_present: boolean
  backup_would_be_written: boolean
  problems: string[]
}

export type UpgradeStatus = {
  current_version: string
  latest_version?: string | null
  update_available: boolean
  status: 'up_to_date' | 'update_available' | 'ahead' | 'unknown'
  source: 'query' | 'feed' | 'none'
  checked_at: string
  message: string
  pending_migrations: number
}

export function formatError(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
