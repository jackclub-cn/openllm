import { useEffect, useEffectEvent, useRef, useState } from 'react'

type Options = {
  enabled?: boolean
  intervalMs?: number
  onRefresh: () => void | Promise<void>
}

export function useAutoRefresh({ enabled = true, intervalMs = 10_000, onRefresh }: Options) {
  const [paused, setPaused] = useState(!enabled)
  const [refreshing, setRefreshing] = useState(false)
  const [lastUpdated, setLastUpdated] = useState<Date>()
  const refresh = useEffectEvent(onRefresh)
  const running = useRef(false)

  useEffect(() => {
    if (paused) return
    const timer = window.setInterval(async () => {
      if (running.current || document.visibilityState !== 'visible') return
      running.current = true
      setRefreshing(true)
      try {
        await refresh()
        setLastUpdated(new Date())
      } finally {
        running.current = false
        setRefreshing(false)
      }
    }, intervalMs)
    return () => window.clearInterval(timer)
  }, [intervalMs, paused])

  const manualRefresh = async () => {
    if (running.current) return
    running.current = true
    setRefreshing(true)
    try {
      await refresh()
      setLastUpdated(new Date())
    } finally {
      running.current = false
      setRefreshing(false)
    }
  }

  return {
    paused,
    setPaused,
    refreshing,
    lastUpdated,
    manualRefresh,
  }
}
