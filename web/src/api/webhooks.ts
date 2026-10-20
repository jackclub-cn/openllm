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
