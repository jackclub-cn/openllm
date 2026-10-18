import type {
  ProviderQuotaOverviewSnapshot,
  ProviderQuotaSnapshot,
} from '../../api'

export const quotaStateLabels: Record<ProviderQuotaSnapshot['state'], string> = {
  unknown: '待更新',
  healthy: '充足',
  low: '偏低',
  exhausted: '已耗尽',
  reset_due: '待重置',
}

export const quotaStateColors: Record<ProviderQuotaSnapshot['state'], string> = {
  unknown: 'default',
  healthy: 'success',
  low: 'warning',
  exhausted: 'error',
  reset_due: 'processing',
}

const quotaStatePriority: Record<ProviderQuotaSnapshot['state'], number> = {
  exhausted: 0,
  low: 1,
  reset_due: 2,
  unknown: 3,
  healthy: 4,
}

const quotaWindowPriority: Record<string, number> = {
  balance: 0,
  '5h': 1,
  daily: 2,
  weekly: 3,
  monthly: 4,
  custom: 5,
}

export function quotaWindowLabel(windowKey: string) {
  switch (windowKey) {
    case '5h':
      return '5 小时'
    case 'daily':
      return '每日'
    case 'weekly':
      return '每周'
    case 'monthly':
      return '每月'
    case 'balance':
      return '余额'
    case 'custom':
      return '当前窗口'
    default:
      return windowKey
  }
}

export function formatQuotaNumber(value: number) {
  return new Intl.NumberFormat('zh-CN', {
    maximumFractionDigits: 2,
  }).format(value)
}

export function formatQuotaAmount(value: number, unit: string) {
  const normalized = unit.trim().toLowerCase()
  const formatted = formatQuotaNumber(value)
  if (normalized === 'usd' || normalized === '$') return `$${formatted}`
  if (normalized === 'cny' || normalized === 'rmb' || normalized === '¥') {
    return `¥${formatted}`
  }
  if (normalized === 'percent' || normalized === '%') return `${formatted}%`
  if (normalized === 'request' || normalized === 'requests') return `${formatted} 次`
  if (normalized === 'token' || normalized === 'tokens') return `${formatted} tokens`
  if (normalized === 'credit' || normalized === 'credits') {
    return `${formatted} credits`
  }
  return unit.trim() ? `${formatted} ${unit}` : formatted
}

export function quotaRemaining(snapshot: ProviderQuotaSnapshot) {
  if (snapshot.remaining != null) return snapshot.remaining
  if (snapshot.used != null && snapshot.limit != null) {
    return Math.max(0, snapshot.limit - snapshot.used)
  }
  if (snapshot.unit.toLowerCase() === 'percent' && snapshot.percent != null) {
    return Math.max(0, 100 - snapshot.percent)
  }
  return undefined
}

export type ProviderQuotaWindowGroup = {
  model: string
  windowKey: string
  unit: string
  used?: number
  limit?: number
  remaining?: number
  percent?: number
  state: ProviderQuotaSnapshot['state']
  resetAt?: string
  observedAt: string
  keyCount: number
}

function quotaStateRank(state: ProviderQuotaSnapshot['state']) {
  return quotaStatePriority[state]
}

function latestTimestamp(values: string[]) {
  return values.reduce((latest, value) => {
    if (!latest) return value
    return Date.parse(value) > Date.parse(latest) ? value : latest
  }, '')
}

function earliestTimestamp(values: Array<string | null | undefined>) {
  return values
    .filter((value): value is string => Boolean(value))
    .reduce<string | undefined>((earliest, value) => {
      if (!earliest) return value
      return Date.parse(value) < Date.parse(earliest) ? value : earliest
    }, undefined)
}

function uniqueSnapshotReadings(snapshots: ProviderQuotaOverviewSnapshot[]) {
  const readings = new Map<string, ProviderQuotaOverviewSnapshot>()
  for (const snapshot of snapshots) {
    const key = [
      snapshot.model,
      snapshot.window_key,
      snapshot.unit,
      snapshot.used ?? '',
      snapshot.limit ?? '',
      snapshot.remaining ?? '',
      snapshot.reset_at ?? '',
    ].join('\u0000')
    if (!readings.has(key)) readings.set(key, snapshot)
  }
  return [...readings.values()]
}

function aggregateWindow(
  snapshots: ProviderQuotaOverviewSnapshot[],
): ProviderQuotaWindowGroup {
  const readings = uniqueSnapshotReadings(snapshots)
  const first = readings[0]
  const state = snapshots.reduce<ProviderQuotaSnapshot['state']>(
    (worst, snapshot) =>
      quotaStateRank(snapshot.state) < quotaStateRank(worst) ? snapshot.state : worst,
    'healthy',
  )
  const keyCount = new Set(
    snapshots.map((snapshot) => snapshot.key_id ?? snapshot.provider_key_id ?? 'default'),
  ).size
  const normalizedUnit = first.unit.trim().toLowerCase()

  let used: number | undefined
  let limit: number | undefined
  let remaining: number | undefined
  let percent: number | undefined

  if (normalizedUnit === 'percent' || normalizedUnit === '%') {
    let usedWeight = 0
    let limitWeight = 0
    let complete = true
    for (const snapshot of readings) {
      const weight = snapshot.limit != null && snapshot.limit > 0 ? snapshot.limit : 100
      const snapshotUsed =
        snapshot.used ?? ((snapshot.percent ?? 0) * weight) / 100
      if (snapshot.used == null && snapshot.percent == null) {
        complete = false
        break
      }
      usedWeight += snapshotUsed
      limitWeight += weight
    }
    if (complete && limitWeight > 0) {
      used = usedWeight
      limit = limitWeight
      percent = Math.min(100, Math.max(0, (usedWeight / limitWeight) * 100))
      remaining = Math.max(0, 100 - percent)
    }
  } else {
    if (readings.every((snapshot) => snapshot.used != null)) {
      used = readings.reduce((total, snapshot) => total + (snapshot.used ?? 0), 0)
    }
    if (readings.every((snapshot) => snapshot.limit != null)) {
      limit = readings.reduce((total, snapshot) => total + (snapshot.limit ?? 0), 0)
    }
    if (readings.every((snapshot) => snapshot.remaining != null)) {
      remaining = readings.reduce(
        (total, snapshot) => total + (snapshot.remaining ?? 0),
        0,
      )
    } else if (used != null && limit != null) {
      remaining = Math.max(0, limit - used)
    }
    if (used != null && limit != null && limit > 0) {
      percent = Math.min(100, Math.max(0, (used / limit) * 100))
    }
  }

  return {
    model: first.model,
    windowKey: first.window_key,
    unit: first.unit,
    used,
    limit,
    remaining,
    percent,
    state,
    resetAt: earliestTimestamp(snapshots.map((snapshot) => snapshot.reset_at)),
    observedAt: latestTimestamp(snapshots.map((snapshot) => snapshot.observed_at)),
    keyCount,
  }
}

export function providerQuotaWindowGroups(
  snapshots: ProviderQuotaOverviewSnapshot[],
) {
  const providerWide = snapshots.filter((snapshot) => !snapshot.model)
  const candidates = providerWide.length > 0 ? providerWide : snapshots
  const grouped = new Map<string, ProviderQuotaOverviewSnapshot[]>()
  for (const snapshot of candidates) {
    const key = `${snapshot.model}\u0000${snapshot.window_key}\u0000${snapshot.unit}`
    const rows = grouped.get(key) ?? []
    rows.push(snapshot)
    grouped.set(key, rows)
  }
  return [...grouped.values()]
    .map(aggregateWindow)
    .sort((left, right) => {
      const window =
        (quotaWindowPriority[left.windowKey] ?? 99) -
        (quotaWindowPriority[right.windowKey] ?? 99)
      if (window !== 0) return window
      return left.model.localeCompare(right.model)
    })
}
