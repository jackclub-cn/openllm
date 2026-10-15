import type { WebhookKind } from '../../api'

export type WebhookFormValues = {
  name: string
  url: string
  secret?: string
  headers?: string
  kind: WebhookKind
  body_fields?: string
  clear_secret?: boolean
  event_types: string[]
  enabled: boolean
}

export const webhookKindOptions: Array<{
  label: string
  value: WebhookKind
}> = [
  { label: 'generic · 网关事件信封', value: 'generic' },
  { label: 'slack · Slack 消息', value: 'slack' },
  { label: 'discord · Discord 消息', value: 'discord' },
  { label: 'telegram · Telegram 机器人', value: 'telegram' },
  { label: 'feishu · 飞书自定义机器人', value: 'feishu' },
  { label: 'dingtalk · 钉钉自定义机器人', value: 'dingtalk' },
]

export const webhookEventOptions = [
  { label: '请求成功', value: 'request.completed' },
  { label: '请求失败', value: 'request.failed' },
  { label: '提供商熔断', value: 'provider.circuit_open' },
  { label: '提供商恢复', value: 'provider.circuit_closed' },
]
