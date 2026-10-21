import type { ActivityTimings, ContextBreakdown } from './activity'

export type RequestPerformance = {
  timings: ActivityTimings
  attempt_count: number
  fallback_count: number
  failed_attempts_ms: number
  attempts_truncated: boolean
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
  context?: ContextBreakdown | null
  cache_usage_reported?: boolean | null
  performance?: RequestPerformance | null
  warning_message?: string
  created_at: string
}

export type RouteTraceCandidate = {
  order: number
  provider_id: number
  provider_api_key_id?: number | null
  upstream_model: string
  eligible: boolean
  deprioritized: boolean
  reason_code: string
}

export type RouteTraceAttempt = {
  attempt_index: number
  round: number
  candidate_order: number
  provider_id?: number | null
  provider_api_key_id?: number | null
  upstream_model?: string | null
  started_at: string
  latency_ms: number
  status_code?: number | null
  success: boolean
  retryable: boolean
  reason_code: string
  failure_scope?: string | null
  failure_retry?: string | null
  failure_confidence?: string | null
}

export type RouteTrace = {
  request_id: string
  created_at: string
  finished_at: string
  route_id?: number | null
  strategy: string
  session_id?: string | null
  session_decision: string
  candidates: RouteTraceCandidate[]
  attempt_count: number
  fallback_count: number
  final_provider_id?: number | null
  final_provider_api_key_id?: number | null
  final_upstream_model?: string | null
  final_status_code?: number | null
  success: boolean
  attempts: RouteTraceAttempt[]
}
