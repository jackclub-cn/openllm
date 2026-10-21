import { useCallback, useEffect, useRef, useState } from 'react'
import { App } from 'antd'
import { api, formatError } from '../../api'

export default function useSettingsSection<T>(
  endpoint: string,
  successMessage: string,
  normalize: (value: T) => T = (value) => value
) {
  const { message } = App.useApp()
  const [value, setValue] = useState<T>()
  const [saved, setSaved] = useState<T>()
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const revision = useRef(0)

  const reload = useCallback(async () => {
    const request = ++revision.current
    setLoading(true)
    setError('')
    try {
      const next = await api.get<T>(endpoint)
      if (revision.current !== request) return
      setValue(next)
      setSaved(next)
    } catch (cause) {
      if (revision.current === request) setError(formatError(cause))
    } finally {
      if (revision.current === request) setLoading(false)
    }
  }, [endpoint])

  useEffect(() => {
    void reload()
    return () => {
      revision.current += 1
    }
  }, [reload])

  const save = async () => {
    if (value === undefined || loading || saving) return
    setSaving(true)
    try {
      const next = await api.put<T>(endpoint, normalize(value))
      setValue(next)
      setSaved(next)
      message.success(successMessage)
    } catch (cause) {
      message.error(formatError(cause))
    } finally {
      setSaving(false)
    }
  }

  return {
    value,
    setValue,
    loading,
    saving,
    error,
    reload,
    save,
    dirty: saved !== undefined && JSON.stringify(value) !== JSON.stringify(saved),
    reset: () => setValue(saved),
  }
}

export type SettingsSectionState<T> = ReturnType<typeof useSettingsSection<T>>
