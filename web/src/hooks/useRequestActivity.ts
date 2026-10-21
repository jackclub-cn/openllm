import { useCallback, useEffect, useRef, useState } from 'react'
import { api, ApiError, type RequestActivity } from '../api'
import { useActivityEventSubscription, useRealtime } from '../realtime'

export default function useRequestActivity(requestId?: string, initial?: RequestActivity) {
  const [activity, setActivity] = useState<RequestActivity>()
  const [missing, setMissing] = useState(false)
  const activeRequest = useRef(requestId)
  activeRequest.current = requestId
  const { connected } = useRealtime()
  const load = useCallback(async () => {
    if (!requestId) return
    try {
      const value = await api.get<RequestActivity>(`/api/activity/${encodeURIComponent(requestId)}`)
      if (activeRequest.current !== requestId) return
      setMissing(false)
      setActivity((current) => current?.request_id === value.request_id && current.revision > value.revision ? current : value)
    } catch (reason) {
      if (activeRequest.current === requestId && reason instanceof ApiError && reason.status === 404) setMissing(true)
    }
  }, [requestId])

  useEffect(() => {
    setActivity(initial)
    setMissing(false)
    void load()
  }, [requestId, load]) // An open detail keeps its own snapshot as the list advances.

  useActivityEventSubscription((event) => {
    if (!event) { void load(); return }
    if (event.snapshot.request_id !== requestId) return
    setActivity((current) => current && current.revision >= event.snapshot.revision ? current : event.snapshot)
  }, Boolean(requestId))

  useEffect(() => {
    if (!requestId) return
    void load()
    const handle = window.setInterval(() => { void load() }, connected ? 15_000 : 4_000)
    return () => window.clearInterval(handle)
  }, [requestId, connected, load])

  return { activity: activity?.request_id === requestId ? activity : undefined, missing }
}
