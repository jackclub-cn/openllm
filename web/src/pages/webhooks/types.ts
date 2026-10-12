export type WebhookFormValues = {
  name: string
  url: string
  secret?: string
  headers?: string
  clear_secret?: boolean
  event_types: string[]
  enabled: boolean
}

export const webhookEventOptions = [
  { label: '请求成功', value: 'request.completed' },
  { label: '请求失败', value: 'request.failed' },
  { label: '提供商熔断', value: 'provider.circuit_open' },
  { label: '提供商恢复', value: 'provider.circuit_closed' },
]
