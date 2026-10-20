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
  max_concurrency?: number | null
  requests_per_minute?: number | null
  queue_limit?: number | null
  queue_wait_ms?: number | null
  cooldown_seconds?: number
  created_at: string
}

export type ProviderApiKeyInput = {
  id?: number
  name: string
  api_key?: string
  enabled: boolean
  max_concurrency?: number | null
  requests_per_minute?: number | null
  queue_limit?: number | null
  queue_wait_ms?: number | null
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
  model_auto_include_rules: string[]
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

export type ProviderQuotaSnapshot = {
  provider_id: number
  provider_key_id?: number | null
  model: string
  window_key: string
  used?: number | null
  limit?: number | null
  remaining?: number | null
  unit: string
  percent?: number | null
  reset_at?: string | null
  source: string
  confidence: string
  observed_at: string
  valid_until: string
  state: 'unknown' | 'healthy' | 'low' | 'exhausted' | 'reset_due'
}

export type ProviderQuotaOverviewSnapshot = ProviderQuotaSnapshot & {
  key_id?: number | null
  key_name?: string | null
  key_suffix?: string | null
}

export type ProviderQuotaOverview = {
  provider_id: number
  provider_name: string
  provider_enabled: boolean
  kind: string
  title: string
  snapshots: ProviderQuotaOverviewSnapshot[]
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
  snapshots: ProviderQuotaSnapshot[]
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
  model_auto_include_rules: string[]
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
