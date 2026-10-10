import type { UsageLog } from '../../api'

export type UsagePageResponse = {
  items: UsageLog[]
  total: number
  page: number
  page_size: number
}

export type UsageStatusFilter = 'all' | 'success' | 'failed' | 'pending'

export function positiveIntegerParam(value: string | null) {
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined
}

export function statusParam(searchParams: URLSearchParams): UsageStatusFilter {
  if (searchParams.get('success') === 'true') return 'success'
  if (searchParams.get('success') === 'false') return 'failed'
  if (searchParams.get('in_flight') === 'true') return 'pending'
  return 'all'
}
