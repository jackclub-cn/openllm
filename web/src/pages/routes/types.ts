import type { GatewayRoute, RouteTarget } from '../../api'

export type RouteFormValues = {
  name: string
  model_pattern: string
  strategy: GatewayRoute['strategy']
  enabled: boolean
  targets: RouteTarget[]
}

export const strategyLabels = {
  priority: '优先级',
  weighted: '加权随机',
  round_robin: '轮询',
  cost_optimized: '成本优先',
  latency_optimized: '延迟优先',
  least_used: '负载最少',
}

export const matchTypeLabels = {
  explicit_route: '显式路由',
  prefix: '模型前缀',
  direct: '直接模型',
  conflict: '同名冲突',
  none: '未匹配',
}

export const diagnosticEndpoints = [
  '/v1/chat/completions',
  '/v1/responses',
  '/v1/completions',
  '/v1/embeddings',
  '/v1/messages',
].map((value) => ({ value, label: value }))

export const diagnosisReasonLabels: Record<string, string> = {
  eligible: '可用',
  'route is disabled': '路由已停用',
  'route target is disabled': '目标已停用',
  'provider is disabled': '提供商已停用',
  'model is disabled': '模型已停用',
  'provider does not support this endpoint': '提供商不支持此接口',
  'model does not declare support for this endpoint': '模型未声明支持此接口',
}
