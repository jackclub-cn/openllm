import { useCallback, useEffect, useRef, useState } from 'react'
import { api, formatError } from '../api'

export function useLoadableList<Item>(endpoint: string) {
  const [items, setItems] = useState<Item[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const revision = useRef(0)

  const reload = useCallback(async () => {
    const request = ++revision.current
    setLoading(true)
    try {
      const next = await api.get<Item[]>(endpoint)
      if (request !== revision.current) return
      setItems(next)
      setError('')
    } catch (cause) {
      if (request === revision.current) setError(formatError(cause))
    } finally {
      if (request === revision.current) setLoading(false)
    }
  }, [endpoint])

  useEffect(() => {
    void reload()
    return () => {
      revision.current += 1
    }
  }, [reload])

  return { items, setItems, loading, error, reload }
}
