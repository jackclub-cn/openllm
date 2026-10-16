/**
 * Compact number formatting for large counters.
 *
 * Token totals reach the tens of millions, where a raw figure is hard to read
 * at a glance and overflows narrow cards. Values are shortened with a K/M/B
 * suffix instead.
 */

/** Units in ascending order, with the threshold at which each applies. */
const UNITS: Array<{ limit: number; suffix: string }> = [
  { limit: 1_000_000_000, suffix: 'B' },
  { limit: 1_000_000, suffix: 'M' },
  { limit: 1_000, suffix: 'K' },
]

/**
 * Shortens a number to at most one decimal place, e.g. `60946052` -> `60.9M`.
 *
 * Values below 1000 are returned unchanged, so small counts stay exact. The
 * decimals are trimmed when they add nothing (`2.0M` renders as `2M`).
 */
export function formatCompact(value: number): string {
  if (!Number.isFinite(value)) return '-'
  const negative = value < 0
  const magnitude = Math.abs(value)
  let index = UNITS.findIndex((candidate) => magnitude >= candidate.limit)
  if (index < 0) return String(value)
  let unit = UNITS[index]
  let scaled = magnitude / unit.limit
  // Rounding can push a value up to 1000 of the current unit (999,999,999 ->
  // "1000M"); promote it to the next unit instead, which reads as "1B".
  if (scaled.toFixed(1) === '1000.0' && index > 0) {
    unit = UNITS[index - 1]
    scaled = magnitude / unit.limit
  }
  // One decimal is enough to keep the card readable; drop a trailing `.0`.
  const text = scaled.toFixed(1).replace(/\.0$/, '')
  return `${negative ? '-' : ''}${text}${unit.suffix}`
}

/**
 * Full value with thousands separators, for tooltips where the exact number
 * still matters.
 */
export function formatExact(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return value.toLocaleString('en-US')
}

export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value < 0) return '-'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let scaled = value
  let unit = 0
  while (scaled >= 1024 && unit < units.length - 1) {
    scaled /= 1024
    unit += 1
  }
  const precision = unit === 0 || scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2
  return `${scaled.toFixed(precision).replace(/\.0+$/, '')} ${units[unit]}`
}

/**
 * A compact figure for display, plus the exact value for a tooltip.
 * Returns the pair so callers cannot show one without the other.
 */
export function compactWithExact(value: number): { text: string; exact: string } {
  return { text: formatCompact(value), exact: formatExact(value) }
}

/**
 * Formats a micro-dollar amount (1 USD = 1,000,000) for cost displays.
 * Unknown pricing stays visibly distinct from a real zero-cost request.
 */
export function formatCostMicros(value?: number | null): string {
  if (value == null || !Number.isFinite(value)) return '-'
  const usd = value / 1_000_000
  if (usd === 0) return '$0'
  if (Math.abs(usd) < 0.01) return `$${usd.toFixed(4)}`
  return `$${usd.toFixed(2)}`
}

type FailureClassification = {
  scope?: string | null
  retry?: string | null
  confidence?: string | null
  reason_code?: string | null
}

const FAILURE_SCOPE_LABELS: Record<string, string> = {
  transport: '传输',
  provider: '提供商',
  credential: '上游凭证',
  model: '模型',
  endpoint: '端点',
  request_shape: '请求格式',
  request_content: '请求内容',
  client: '客户端',
}

const FAILURE_RETRY_LABELS: Record<string, string> = {
  do_not_retry: '不重试',
  next_target: '切换目标',
}

const FAILURE_REASON_LABELS: Record<string, string> = {
  upstream_unavailable: '上游不可用',
  upstream_timeout: '上游超时',
  credential_rejected: '凭证被拒绝',
  context_limit_exceeded: '上下文超限',
  model_not_found: '模型不存在',
  quota_exhausted: '额度耗尽',
  rate_limited: '限流',
  content_policy_refusal: '内容策略拒绝',
  request_shape_incompatible: '请求格式不兼容',
  request_conflict: '请求状态冲突',
  invalid_request: '请求无效',
  endpoint_or_model_not_found: '端点或模型不存在',
  unsupported_upstream_error: '未知上游错误',
}

export function formatFailureClassification(value?: FailureClassification | null): string {
  if (!value) return ''
  return [
    value.scope ? FAILURE_SCOPE_LABELS[value.scope] || value.scope : '',
    value.reason_code
      ? FAILURE_REASON_LABELS[value.reason_code] || value.reason_code
      : '',
    value.confidence === 'exact'
      ? '明确'
      : value.confidence === 'inferred'
        ? '推断'
        : value.confidence === 'unknown'
          ? '未知'
          : '',
    value.retry ? FAILURE_RETRY_LABELS[value.retry] || value.retry : '',
  ]
    .filter(Boolean)
    .join(' · ')
}
