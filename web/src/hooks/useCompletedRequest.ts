import { useEffect, useState } from 'react'
import { api, formatError, type RouteTrace, type UsageLog } from '../api'

export default function useCompletedRequest(requestId?: string, completed = false) {
  const [detail, setDetail] = useState<UsageLog>()
  const [trace, setTrace] = useState<RouteTrace>()
  const [traceError, setTraceError] = useState('')
  const [detailError, setDetailError] = useState('')
  const [loading, setLoading] = useState(false)
  useEffect(() => {
    setDetail(undefined)
    setTrace(undefined)
    setTraceError('')
    setDetailError('')
    if (!requestId || !completed) return
    let cancelled = false
    let timer: number | undefined
    let attempts = 0
    setLoading(true)
    const load = async () => {
      const encoded = encodeURIComponent(requestId)
      const results = await Promise.allSettled([
        api.get<UsageLog>(`/api/usage/${encoded}`),
        api.get<RouteTrace>(`/api/usage/${encoded}/route-trace`),
      ])
      if (cancelled) return
      const [usageResult, traceResult] = results
      if (usageResult.status === 'fulfilled') {
        setDetail(usageResult.value)
        setDetailError('')
      } else setDetailError(formatError(usageResult.reason))
      if (traceResult.status === 'fulfilled') {
        setTrace(traceResult.value)
        setTraceError('')
      } else setTraceError(formatError(traceResult.reason))
      if (attempts < 5 && (results.some((result) => result.status === 'rejected') ||
        (usageResult.status === 'fulfilled' && usageResult.value.in_flight))) {
        timer = window.setTimeout(() => { void load() }, Math.min(500 * 2 ** attempts, 8000))
        attempts += 1
      } else setLoading(false)
    }
    void load()
    return () => { cancelled = true; if (timer !== undefined) window.clearTimeout(timer) }
  }, [requestId, completed])
  return { detail, trace, traceError, detailError, loading }
}
