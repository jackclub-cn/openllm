const traceReasonLabels: Record<string, string> = {
  ordered: '按策略排序',
  target_sink: '短期限流',
  slow_first_token: '首字偏慢',
  provider_circuit_open: '提供商熔断',
  provider_health_failed: '健康检查失败',
  concurrency_saturated: '并发已满',
  pool_share: '配额池降权',
  endpoint_unsupported: '端点不支持',
  upstream_success: '上游成功',
  upstream_response: '上游返回错误',
  upstream_transport_error: '上游连接失败',
  upstream_status_error: '上游状态错误',
  upstream_unavailable: '上游不可用',
  upstream_rate_limited: '上游限流',
  rate_limited: '上游限流',
  quota_exhausted: '额度耗尽',
  credential_rejected: '凭证被拒绝',
  model_not_found: '模型不存在',
  endpoint_or_model_not_found: '端点或模型不存在',
  request_shape_incompatible: '请求格式不兼容',
  invalid_request: '上游拒绝请求',
  context_limit_exceeded: '上下文超限',
  content_policy_refusal: '内容策略拒绝',
  request_conflict: '请求冲突',
  upstream_timeout: '上游超时',
  unsupported_upstream_error: '未识别上游错误',
  provider_short_circuit: '提供商短路',
  local_capacity_exhausted: '本地容量已满',
  local_request_rejected: '本地请求被拒绝',
  gateway_rejected: '网关拒绝',
  internal_error: '网关内部错误',
}

const routeStrategyLabels: Record<string, string> = {
  priority: '优先级',
  weighted: '加权随机',
  round_robin: '轮询',
  cost_optimized: '成本优先',
  latency_optimized: '延迟优先',
  p95_latency_optimized: 'P95 延迟优先',
  least_used: '最少使用',
  quota_paced: '额度节流',
}

const sessionDecisionLabels: Record<string, string> = {
  hit: '命中粘性',
  miss: '未命中',
  no_affinity: '未启用',
  session_key_present: '会话已识别',
}

export function traceReasonLabel(code: string) {
  return traceReasonLabels[code] || code
}

export function routeStrategyLabel(strategy: string) {
  return routeStrategyLabels[strategy] || strategy
}

export function sessionDecisionLabel(decision: string) {
  return sessionDecisionLabels[decision] || decision
}
