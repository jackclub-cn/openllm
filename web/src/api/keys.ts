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
