import { useEffect, useState } from 'react'
import type { RequestActivity } from '../api'

const samples = new WeakMap<RequestActivity, number>()

export function activityElapsedMs(item: RequestActivity, now: number) {
  if (item.outcome !== 'running') return item.timings.total_ms
  let sampled = samples.get(item)
  if (sampled === undefined) { sampled = performance.now(); samples.set(item, sampled) }
  return item.timings.total_ms + Math.max(0, now - sampled)
}

export function activityLiveTimings(item: RequestActivity, now: number) {
  const timings = { ...item.timings }
  if (item.outcome !== 'running') return timings
  const phase = item.phase
  const slot = phase === 'admitted' || phase === 'preparing' ? 'prepare_ms'
    : phase === 'waiting' || phase === 'routing' ? 'route_wait_ms'
      : phase === 'attempting' ? 'upstream_headers_ms'
        : phase === 'headers_received' ? item.streamed ? 'response_wait_ms' : 'response_read_ms'
          : phase === 'streaming' ? 'generation_ms' : null
  if (slot) timings[slot] = (timings[slot] || 0) + activityElapsedMs(item, now) - item.timings.total_ms
  return timings
}

export default function useActivityClock(running: boolean) {
  const [now, setNow] = useState(() => performance.now())
  useEffect(() => {
    if (!running) return
    const handle = window.setInterval(() => {
      if (document.visibilityState === 'visible') setNow(performance.now())
    }, 1000)
    return () => window.clearInterval(handle)
  }, [running])
  return now
}
