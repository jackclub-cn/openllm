import type { Dayjs } from 'dayjs'

export type ApiKeyForm = {
  name: string
  daily_token_limit?: number | null
  daily_cost_limit_usd?: number | null
  requests_per_minute?: number | null
  max_concurrency?: number | null
  allowed_models_text?: string
  expires_at?: Dayjs | null
  routing_strategy?: string | null
  routing_provider?: string | null
  routing_exclude_providers_text?: string
}
