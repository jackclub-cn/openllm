import { useEffect, useState } from 'react'
import { api, formatError, type Settings } from '../../api'

export default function useAccessSettings() {
  const [settings, setSettings] = useState<Settings>()
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [revision, setRevision] = useState(0)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError('')
    api
      .get<Settings>('/api/settings')
      .then((next) => {
        if (!cancelled) setSettings(next)
      })
      .catch((error) => {
        if (!cancelled) setError(formatError(error))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [revision])

  return { settings, loading, error, reload: () => setRevision((current) => current + 1) }
}
