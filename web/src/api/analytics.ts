import type { UsageLog } from './usage'

export type AnalyticsGranularity = 'auto' | 'hour' | 'day' | 'week' | 'month'

export type OverviewComparison = {
  from: string
  to: string
  requests: number
  tokens: number
  prompt_tokens: number
  completion_tokens: number
  cache_read: number
  cache_write: number
  cache_hit_rate: number
  cost_micros: number
  unpriced: number
  success_rate: number
  avg_latency_ms: number
  output_tps: number | null
  gateway_adjusted: number
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
  range_latency_p50_ms: number | null
  range_latency_p95_ms: number | null
  range_latency_p99_ms: number | null
  range_first_token_p50_ms: number | null
  range_first_token_p95_ms: number | null
  range_first_token_p99_ms: number | null
  range_output_tps: number | null
  range_gateway_adjusted: number
  previous_range: OverviewComparison
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
  granularity: Exclude<AnalyticsGranularity, 'auto'>
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
    cache_read: number
    cache_write: number
    cache_hit_rate: number
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
    cache_read: number
    cache_write: number
    cache_hit_rate: number
  }>
  daily_usage: Array<{
    day: string
    requests: number
    tokens: number
    prompt_tokens: number
    completion_tokens: number
    cost_micros: number
    success_rate: number
    cache_read: number
    cache_write: number
    cache_hit_rate: number
  }>
  daily_model_requests: DailyBreakdownUsage[]
  daily_provider_requests: DailyBreakdownUsage[]
  status_breakdown: Array<{ status_code: number; count: number }>
  error_reasons: Array<{ label: string; count: number }>
  gateway_adjustments: Array<{ category: string; count: number }>
}

export type DailyBreakdownUsage = {
  day: string
  label: string
  requests: number
  tokens: number
  prompt_tokens: number
  cost_micros: number
  success_rate: number
  cache_read: number
  cache_write: number
  cache_hit_rate: number
}

export type DimensionUsage = {
  id?: number | null
  label: string
  requests: number
  tokens: number
  prompt_tokens: number
  completion_tokens: number
  cost_micros?: number | null
  success_rate: number
  avg_latency_ms: number
  cache_read: number
  cache_write: number
  cache_hit_rate: number
}

export type SessionUsage = {
  session_id: string
  requests: number
  tokens: number
  prompt_tokens: number
  completion_tokens: number
  cost_micros?: number | null
  success_rate: number
  avg_latency_ms: number
  cache_read: number
  cache_write: number
  cache_hit_rate: number
  last_seen: string
}

export type ContextCategoryUsage = {
  key: string
  tokens: number
  items: number
}

export type ContextAgentUsage = {
  api_key_id?: number | null
  label: string
  requests: number
  sessions: number
  context_samples: number
  measured_samples: number
  estimated_samples: number
  prompt_tokens: number
  avg_prompt_tokens: number
  max_prompt_tokens: number
  cache_hit_rate: number
  category_tokens: ContextCategoryUsage[]
  unmetered_items: number
  opaque_reasoning_items: number
  models: string[]
  last_seen: string
}

export type ContextAnalytics = {
  requests: number
  sessions: number
  context_requests: number
  measured_requests: number
  estimated_requests: number
  agents: ContextAgentUsage[]
}

export type AnalyticsDimensions = {
  api_key_usage: DimensionUsage[]
  provider_api_key_usage: DimensionUsage[]
  route_usage: DimensionUsage[]
  endpoint_usage: DimensionUsage[]
  session_usage: SessionUsage[]
}

export type RouteQualityStrategy = {
  strategy: string
  traces: number
  fallback_rate: number
  avg_attempts: number
  success_rate: number
}

export type RouteQualityRoute = {
  route_id?: number | null
  route_name: string
  traces: number
  fallback_rate: number
  avg_attempts: number
  success_rate: number
}

export type RouteQualityDecision = {
  decision: string
  count: number
}

export type RouteQualityFailure = {
  reason_code: string
  failure_scope: string
  count: number
}

export type RouteQuality = {
  traces: number
  fallback_rate: number
  avg_attempts: number
  session_requests: number
  session_hits: number
  session_hit_rate: number
  success_rate: number
  strategies: RouteQualityStrategy[]
  routes: RouteQualityRoute[]
  session_decisions: RouteQualityDecision[]
  failure_reasons: RouteQualityFailure[]
}
