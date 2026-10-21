import { useCallback, useEffect, useRef, useState } from 'react'
import { api, formatError, type ActivityEvent, type ActivityList } from '../api'
import { useActivityEventSubscription, useRealtime } from '../realtime'
import { mergeActivityEvent, mergeActivitySnapshot } from './activityState'

const empty: ActivityList = {
  enabled: true, generated_at: '', instance_id: '', sequence: 0, active_count: 0, items: [],
}

export default function useRecentActivity(paused: boolean) {
  const { connected } = useRealtime()
  const [state, setState] = useState(empty)
  const current = useRef(empty)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(true)
  const fetching = useRef(false)
  const retry = useRef(false)
  const buffered = useRef<ActivityEvent[]>([])
  const pending = useRef<ActivityEvent[]>([])
  const timer = useRef<number | undefined>(undefined)
  const mounted = useRef(true)
  const update = (value: ActivityList) => {
    current.current = value
    if (mounted.current) setState(value)
  }

  const load = useCallback(async () => {
    if (fetching.current) { retry.current = true; return }
    fetching.current = true
    buffered.current = []
    try {
      const response = await api.get<ActivityList>('/api/activity/recent?limit=50')
      if (!mounted.current) return
      const merged = mergeActivitySnapshot(response, buffered.current)
      update(merged.state)
      retry.current ||= merged.resync
      setError('')
    } catch (reason) {
      if (mounted.current) setError(formatError(reason))
    } finally {
      fetching.current = false
      if (mounted.current) setLoading(false)
      if (retry.current && mounted.current) {
        retry.current = false
        void load()
      }
    }
  }, [])

  // Subscribe before fetching so a snapshot can be reconciled with every newer event.
  useActivityEventSubscription((event) => {
    if (!event) { void load(); return }
    if (fetching.current) {
      buffered.current.push(event)
      if (buffered.current.length > 1024) { buffered.current.shift(); retry.current = true }
    }
    pending.current.push(event)
    if (timer.current !== undefined) return
    timer.current = window.setTimeout(() => {
      timer.current = undefined
      let next = current.current
      let resync = false
      for (const item of pending.current.splice(0)) {
        const merged = mergeActivityEvent(next, item)
        next = merged.state
        resync ||= merged.resync
      }
      update(next)
      if (resync) void load()
    }, 150)
  }, !paused)

  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])

  useEffect(() => {
    if (paused) return
    void load()
    const handle = window.setInterval(() => {
      if (document.visibilityState === 'visible') void load()
    }, connected ? 30_000 : 4_000)
    return () => {
      window.clearInterval(handle)
      if (timer.current !== undefined) window.clearTimeout(timer.current)
      timer.current = undefined
      pending.current = []
    }
  }, [connected, load, paused])

  return { ...state, activeCount: state.active_count, loading, error, reload: load }
}
