import { createContext, useCallback, useContext, useEffect, useEffectEvent, useRef, useState, type ReactNode } from 'react'
import { getAdminToken } from './api'

export type UsageEvent = {
  id: number
  request_id: string
  success: boolean
  streamed: boolean
}

type RealtimeContextValue = {
  connected: boolean
  subscribe: (listener: (event: UsageEvent) => void) => () => void
}

const RealtimeContext = createContext<RealtimeContextValue>({
  connected: false,
  subscribe: () => () => undefined,
})

export function RealtimeProvider({
  revision,
  children,
}: {
  revision: number
  children: ReactNode
}) {
  const [connected, setConnected] = useState(false)
  const [epoch, setEpoch] = useState(0)
  const listeners = useRef(new Set<(event: UsageEvent) => void>())
  // Backoff attempts are kept in a ref, not state: putting them in the effect
  // dependency list would tear down a freshly opened connection the moment the
  // counter resets on success.
  const attempts = useRef(0)

  const subscribe = useCallback((listener: (event: UsageEvent) => void) => {
    listeners.current.add(listener)
    return () => listeners.current.delete(listener)
  }, [])

  const emit = useEffectEvent((event: UsageEvent) => {
    listeners.current.forEach((listener) => listener(event))
  })

  useEffect(() => {
    const query = new URLSearchParams()
    const token = getAdminToken()
    if (token) query.set('admin_token', token)
    const suffix = query.size ? `?${query}` : ''
    const source = new EventSource(`/api/events${suffix}`)
    let retryTimer: number | undefined

    source.addEventListener('open', () => {
      setConnected(true)
      attempts.current = 0
    })
    source.addEventListener('usage', (event) => {
      try {
        emit(JSON.parse((event as MessageEvent).data) as UsageEvent)
      } catch {
        // Ignore malformed frames and keep the stream alive.
      }
    })
    source.addEventListener('error', () => {
      setConnected(false)
      source.close()
      const delay = Math.min(30_000, 2_000 * 2 ** Math.min(attempts.current, 4))
      attempts.current += 1
      retryTimer = window.setTimeout(() => setEpoch((value) => value + 1), delay)
    })

    return () => {
      source.close()
      if (retryTimer) window.clearTimeout(retryTimer)
    }
  }, [epoch, revision])

  return (
    <RealtimeContext.Provider value={{ connected, subscribe }}>
      {children}
    </RealtimeContext.Provider>
  )
}

export function useRealtime() {
  return useContext(RealtimeContext)
}

export function useUsageEventSubscription(listener: (event: UsageEvent) => void, enabled = true) {
  const { subscribe } = useRealtime()
  const callback = useEffectEvent(listener)

  useEffect(() => {
    if (!enabled) return
    return subscribe((event) => callback(event))
  }, [enabled, subscribe])
}

export function useCoalescedUsageEvents(
  listener: (events: UsageEvent[]) => void,
  { enabled = true, delayMs = 350 }: { enabled?: boolean; delayMs?: number } = {},
) {
  const callback = useEffectEvent(listener)
  const pending = useRef<UsageEvent[]>([])
  const timer = useRef<number | undefined>(undefined)

  useUsageEventSubscription((event) => {
    pending.current.push(event)
    // Throttle instead of debounce: a sustained burst must not starve the
    // refresh. The first event schedules a flush; later events join the batch.
    if (timer.current === undefined) {
      timer.current = window.setTimeout(() => {
        timer.current = undefined
        const batch = pending.current
        pending.current = []
        if (batch.length) callback(batch)
      }, delayMs)
    }
  }, enabled)

  useEffect(() => () => {
    if (timer.current !== undefined) {
      window.clearTimeout(timer.current)
      timer.current = undefined
    }
    pending.current = []
  }, [])
}
