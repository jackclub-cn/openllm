export type ActivityTarget = {
  provider_id: number
  provider_name: string
  provider_api_key_id?: number | null
  upstream_model: string
  context_window_tokens?: number | null
  input_limit?: number | null
}

export type ActivityCandidate = {
  order: number
  provider_id: number
  provider_name?: string | null
  provider_api_key_id?: number | null
  upstream_model: string
  eligible: boolean
  deprioritized: boolean
  reason_code: string
}

export type ActivityAttempt = {
  attempt_index: number
  round: number
  candidate_order: number
  provider_id?: number | null
  provider_name?: string | null
  provider_api_key_id?: number | null
  upstream_model?: string | null
  started_at: string
  latency_ms: number
  upstream_headers_ms?: number | null
  status_code?: number | null
  success: boolean
  reason_code?: string | null
  failure_scope?: string | null
}

export type ActivityTimings = {
  prepare_ms?: number | null
  route_wait_ms?: number | null
  upstream_headers_ms?: number | null
  response_wait_ms?: number | null
  response_read_ms?: number | null
  time_to_first_token_ms?: number | null
  generation_ms?: number | null
  total_ms: number
}

export type ActivityCache = {
  state: string
  read_tokens: number
  write_tokens: number
  hit_rate?: number | null
}

export type ContextCategory = {
  key: string
  tokens: number
  items: number
}

export type ContextBreakdown = {
  window_tokens?: number | null
  input_limit?: number | null
  window_source?: string | null
  input_tokens: number
  estimated: boolean
  input_estimated: boolean
  categories: ContextCategory[]
  unmetered_items: number
  opaque_reasoning_items?: number
}

export type RequestActivity = {
  request_id: string
  revision: number
  started_at: string
  updated_at: string
  finished_at?: string | null
  phase: string
  outcome: string
  endpoint: string
  streamed: boolean
  session_id?: string | null
  api_key_id?: number | null
  api_key_name?: string | null
  route_id?: number | null
  route_name?: string | null
  strategy?: string | null
  requested_model?: string | null
  target?: ActivityTarget | null
  request_tokens_estimated: number
  context?: ContextBreakdown | null
  prompt_tokens?: number | null
  completion_tokens?: number | null
  status_code?: number | null
  error_code?: string | null
  waiting_reason?: string | null
  fallback_count: number
  attempt_count: number
  candidate_count: number
  candidates_truncated: boolean
  attempts_truncated: boolean
  timings: ActivityTimings
  cache: ActivityCache
  candidates: ActivityCandidate[]
  attempts: ActivityAttempt[]
}

export type ActivityEvent = {
  kind: string
  sequence: number
  instance_id: string
  active_count: number
  snapshot: RequestActivity
}

export type ActivityList = {
  enabled: boolean
  generated_at: string
  active_count: number
  instance_id: string
  sequence: number
  items: RequestActivity[]
}
