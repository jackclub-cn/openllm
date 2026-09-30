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

async function requestBlob(path: string): Promise<Blob> {
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
  return response.blob()
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: 'POST', body: body === undefined ? undefined : JSON.stringify(body) }),
  put: <T>(path: string, body: unknown) =>
    request<T>(path, { method: 'PUT', body: JSON.stringify(body) }),
  delete: <T>(path: string) => request<T>(path, { method: 'DELETE' }),
  download: (path: string) => requestBlob(path),
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
  models: string[]
  models_synced_at?: string
  models_sync_error?: string
  last_test_at?: string
  last_test_ok?: boolean
  last_test_latency_ms?: number
  last_test_checked?: 'inference' | 'models'
  last_test_message?: string
  health_check_interval_minutes?: number | null
  models_sync_interval_minutes?: number | null
  models_sync_attempted_at?: string
  created_at: string
  updated_at: string
}

export type ProviderInput = {
  name: string
  provider_type: Provider['provider_type']
  base_url: string
  model_prefix: string
  api_key?: string
  api_keys?: ProviderApiKeyInput[]
  headers: Record<string, string>
  enabled: boolean
  tool_search_supported: boolean
  auto_sync_models: boolean
  models: string[]
  health_check_interval_minutes?: number | null
  models_sync_interval_minutes?: number | null
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
  cost_input_override?: number | null
  cost_output_override?: number | null
  cost_cache_read_override?: number | null
  cost_cache_write_override?: number | null
}

export type RouteTarget = {
  id?: number
  provider_id: number
  provider_name?: string
  provider_type?: string
  upstream_model: string
  supported_endpoints?: string[]
  model_prefix?: string
  weight: number
  priority: number
  enabled: boolean
}

export type GatewayRoute = {
  id: number
  name: string
  model_pattern: string
  strategy: 'priority' | 'weighted' | 'round_robin'
  enabled: boolean
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
  cost_micros?: number | null
  unpriced_requests: number
  daily_token_limit?: number | null
  daily_cost_limit_micros?: number | null
  requests_per_minute?: number | null
  max_concurrency?: number | null
  today_requests: number
  today_tokens: number
  today_cost_micros?: number | null
  requests_this_minute: number
  current_in_flight: number
  allowed_models: string[]
  expires_at?: string | null
}

export type UsageLog = {
  id: number
  request_id: string
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
  first_token_ms?: number
  /** Output tokens per second. For streams this excludes the first-token wait. */
  output_tps?: number
  status_code: number
  in_flight: boolean
  success: boolean
  streamed: boolean
  error_message?: string
  response_preview?: string
  created_at: string
}

export type Overview = {
  requests_today: number
  tokens_today: number
  cache_read_today: number
  cache_write_today: number
  cache_hit_rate: number
  requests_total: number
  tokens_total: number
  cache_read_total: number
  cache_write_total: number
  cost_today_micros: number
  cost_total_micros: number
  unpriced_today: number
  unpriced_total: number
  range_requests: number
  range_tokens: number
  range_cache_read: number
  range_cache_write: number
  range_cache_hit_rate: number
  range_cost_micros: number
  range_unpriced: number
  range_success_rate: number
  range_avg_latency_ms: number
  success_rate: number
  avg_latency_ms: number
  active_providers: number
  active_routes: number
  recent_requests: UsageLog[]
  provider_usage: Array<{
    provider_id: number
    provider_name: string
    requests: number
    tokens: number
    cost_micros?: number | null
    success_rate: number
    avg_latency_ms: number
  }>
  model_usage: Array<{
    model: string
    requests: number
    tokens: number
    cost_micros?: number | null
    success_rate: number
    avg_latency_ms: number
  }>
  daily_usage: Array<{ day: string; requests: number; tokens: number }>
}

export type Settings = {
  admin_auth_enabled: boolean
  database: string
  version: string
}

export type RuntimeSettings = {
  usage_retention_days?: number | null
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

export function formatError(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
