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
