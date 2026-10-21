import type { ActivityTimings } from '../api/activity'

export function requestTimingStages(timings: ActivityTimings, streamed: boolean) {
  return [
    { key: 'prepare', label: '网关准备', value: timings.prepare_ms, color: '#1677ff' },
    { key: 'routing', label: '路由与等待', value: timings.route_wait_ms, color: '#13a8a8' },
    { key: 'headers', label: '上游响应头', value: timings.upstream_headers_ms, color: '#fa8c16' },
    streamed
      ? { key: 'first_output', label: '等待首输出', value: timings.response_wait_ms, color: '#722ed1' }
      : { key: 'response', label: '读取响应', value: timings.response_read_ms, color: '#722ed1' },
    ...(streamed ? [
      { key: 'generation', label: '生成阶段', value: timings.generation_ms, color: '#52c41a' },
    ] : []),
  ]
}

export function analyzeRequestTimings(timings: ActivityTimings, streamed: boolean) {
  const stages = requestTimingStages(timings, streamed)
  const measured = stages.filter((stage) => stage.value != null && stage.value >= 0)
  const dominant = measured.reduce<(typeof stages)[number] | undefined>(
    (largest, stage) => !largest || stage.value! > largest.value! ? stage : largest,
    undefined,
  )
  const accounted = measured.reduce((sum, stage) => sum + stage.value!, 0)
  return {
    stages,
    dominant: dominant && dominant.value! > 0 ? dominant : undefined,
    unaccounted: Math.max(0, timings.total_ms - accounted),
  }
}

export function formatTimingMs(value?: number | null) {
  if (value == null) return '-'
  return value >= 1000 ? `${(value / 1000).toFixed(2)} s` : `${Math.max(0, Math.round(value))} ms`
}
