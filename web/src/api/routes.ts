import type { ModelCapabilities } from './models'

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
    | 'p95_latency_optimized'
    | 'least_used'
    | 'quota_paced'
  enabled: boolean
  context_limit?: number | null
  input_limit?: number | null
  output_limit?: number | null
  context_limit_base?: number | null
  output_limit_base?: number | null
  context_override?: number | null
  output_override?: number | null
  all_targets_busy_wait_seconds: number
  retry_jitter_ms: number
  max_waiting_requests: number
  keepalive_during_wait: boolean
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
  failure_scope?: string | null
  failure_retry?: string | null
  failure_confidence?: string | null
  failure_reason_code?: string | null
  failure_observed_seconds_ago?: number | null
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
