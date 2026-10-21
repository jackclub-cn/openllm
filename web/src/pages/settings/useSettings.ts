import { useEffect, useState } from 'react'
import { App } from 'antd'
import {
  api,
  formatError,
  type DatabaseVacuumResult,
  type ClearCooldownsResult,
  type CooldownSnapshot,
  type GuardrailSettings,
  type InspectorSettings,
  type ResilienceSettings,
  type RuntimeLimits,
  type RuntimeSettings,
  type Settings,
} from '../../api'
import { formatBytes, formatFailureClassification } from '../../format'

export type CooldownRow = {
  key: string
  scope: string
  target: string
  detail: string
  remaining: number
  circuit?: 'closed' | 'open' | 'half_open'
  clear: Record<string, unknown>
}
export default function useSettings() {
  const { message } = App.useApp()
  const [settings, setSettings] = useState<Settings>()
  const [backingUp, setBackingUp] = useState(false)
  const [vacuuming, setVacuuming] = useState(false)
  const [runningMaintenance, setRunningMaintenance] = useState(false)
  const [retentionDays, setRetentionDays] = useState<number | null>(null)
  const [savingRetention, setSavingRetention] = useState(false)
  const [limits, setLimits] = useState<RuntimeLimits>()
  const [blockedTerms, setBlockedTerms] = useState('')
  const [maxPromptTokens, setMaxPromptTokens] = useState<number | null>(null)
  const [savingGuardrails, setSavingGuardrails] = useState(false)
  const [captureRequestPreviews, setCaptureRequestPreviews] = useState(false)
  const [requestPreviewMaxChars, setRequestPreviewMaxChars] = useState(4000)
  const [savingInspector, setSavingInspector] = useState(false)
  const [streamRecoveryEnabled, setStreamRecoveryEnabled] = useState(false)
  const [streamRecoveryMaxRetries, setStreamRecoveryMaxRetries] = useState(2)
  const [maxRetries, setMaxRetries] = useState(1)
  const [retryBackoffMs, setRetryBackoffMs] = useState(200)
  const [retryMaxBackoffMs, setRetryMaxBackoffMs] = useState(2000)
  const [savingResilience, setSavingResilience] = useState(false)
  const [cooldowns, setCooldowns] = useState<CooldownSnapshot>()
  const [loadingCooldowns, setLoadingCooldowns] = useState(false)
  const [clearingCooldowns, setClearingCooldowns] = useState(false)

  const loadCooldowns = () => {
    setLoadingCooldowns(true)
    api
      .get<CooldownSnapshot>('/api/resilience/cooldowns')
      .then(setCooldowns)
      .catch(() => undefined)
      .finally(() => setLoadingCooldowns(false))
  }

  useEffect(() => {
    api
      .get<Settings>('/api/settings')
      .then(setSettings)
      .catch(() => undefined)
    api
      .get<RuntimeSettings>('/api/settings/runtime')
      .then((runtime) => {
        setRetentionDays(runtime.usage_retention_days ?? null)
        setLimits(runtime.limits)
      })
      .catch(() => undefined)
    api
      .get<GuardrailSettings>('/api/settings/guardrails')
      .then((guardrails) => {
        setBlockedTerms(guardrails.blocked_terms.join('\n'))
        setMaxPromptTokens(guardrails.max_prompt_tokens ?? null)
      })
      .catch(() => undefined)
    api
      .get<InspectorSettings>('/api/settings/inspector')
      .then((inspector) => {
        setCaptureRequestPreviews(inspector.capture_request_previews)
        setRequestPreviewMaxChars(inspector.request_preview_max_chars)
      })
      .catch(() => undefined)
    api
      .get<ResilienceSettings>('/api/settings/resilience')
      .then((resilience) => {
        setStreamRecoveryEnabled(resilience.stream_recovery_enabled)
        setStreamRecoveryMaxRetries(resilience.stream_recovery_max_retries)
        setMaxRetries(resilience.max_retries)
        setRetryBackoffMs(resilience.retry_backoff_ms)
        setRetryMaxBackoffMs(resilience.retry_max_backoff_ms)
      })
      .catch(() => undefined)
    loadCooldowns()
  }, [])

  const clearCooldowns = async (body: Record<string, unknown>) => {
    setClearingCooldowns(true)
    try {
      const result = await api.post<ClearCooldownsResult>(
        '/api/resilience/cooldowns/clear',
        body
      )
      const total =
        result.cleared_provider_cooldowns +
        result.cleared_model_cooldowns +
        result.cleared_provider_key_cooldowns
      message.success(total ? `已清除 ${total} 条冷却` : '没有匹配的冷却')
      loadCooldowns()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setClearingCooldowns(false)
    }
  }
  const cooldownRows: CooldownRow[] = [
    ...(cooldowns?.provider_cooldowns ?? []).map((item) => ({
      key: `provider-${item.provider_id}`,
      scope: '提供商',
      target: item.provider_name ?? `#${item.provider_id}`,
      detail: [
        item.failure_streak ? `连续失败 ${item.failure_streak} 次` : '',
        item.state === 'open' ? '已熔断' : item.state === 'half_open' ? '探测恢复中' : '',
        formatFailureClassification(item),
      ]
        .filter(Boolean)
        .join(' · '),
      remaining: item.remaining_seconds,
      circuit: item.state,
      clear: { provider_id: item.provider_id },
    })),
    ...(cooldowns?.model_cooldowns ?? []).map((item) => ({
      key: `model-${item.provider_id}-${item.model}`,
      scope: '模型',
      target: `${item.provider_name ?? `#${item.provider_id}`} · ${item.model}`,
      detail: formatFailureClassification(item),
      remaining: item.remaining_seconds,
      clear: { provider_id: item.provider_id, model: item.model },
    })),
    ...(cooldowns?.provider_key_cooldowns ?? []).map((item) => ({
      key: `key-${item.provider_key_id}`,
      scope: '上游密钥',
      target: `${item.provider_name ?? '-'} · ${
        item.key_name ?? `#${item.provider_key_id}`
      }`,
      detail: formatFailureClassification(item),
      remaining: item.remaining_seconds,
      clear: { provider_key_id: item.provider_key_id },
    })),
  ]

  const backup = async () => {
    setBackingUp(true)
    try {
      const blob = await api.download('/api/database/backup')
      const url = URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = `openllm-backup-${new Date()
        .toISOString()
        .slice(0, 19)
        .replaceAll(':', '-')}.db`
      link.click()
      URL.revokeObjectURL(url)
      message.success('数据库备份已下载')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBackingUp(false)
    }
  }

  const saveRetention = async () => {
    setSavingRetention(true)
    try {
      const runtime = await api.put<RuntimeSettings>('/api/settings/runtime', {
        usage_retention_days: retentionDays && retentionDays > 0 ? retentionDays : null,
      })
      setRetentionDays(runtime.usage_retention_days ?? null)
      message.success('日志保留策略已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingRetention(false)
    }
  }

  const runMaintenance = async () => {
    setRunningMaintenance(true)
    try {
      await api.post('/api/maintenance/run')
      message.success('维护任务已运行')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setRunningMaintenance(false)
    }
  }

  const vacuum = async () => {
    setVacuuming(true)
    try {
      const result = await api.post<DatabaseVacuumResult>('/api/database/vacuum')
      setSettings((current) =>
        current ? { ...current, database_stats: result.database_stats } : current
      )
      message.success(
        result.reclaimed_bytes > 0
          ? `数据库已整理，释放 ${formatBytes(result.reclaimed_bytes)}`
          : '数据库已整理，无需释放空间'
      )
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setVacuuming(false)
    }
  }

  const saveGuardrails = async () => {
    setSavingGuardrails(true)
    try {
      const guardrails = await api.put<GuardrailSettings>('/api/settings/guardrails', {
        blocked_terms: blockedTerms
          .split(/\r?\n/)
          .map((term) => term.trim())
          .filter(Boolean),
        max_prompt_tokens:
          maxPromptTokens && maxPromptTokens > 0 ? maxPromptTokens : null,
      })
      setBlockedTerms(guardrails.blocked_terms.join('\n'))
      setMaxPromptTokens(guardrails.max_prompt_tokens ?? null)
      message.success('请求防护策略已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingGuardrails(false)
    }
  }

  const saveInspector = async () => {
    setSavingInspector(true)
    try {
      const inspector = await api.put<InspectorSettings>('/api/settings/inspector', {
        capture_request_previews: captureRequestPreviews,
        request_preview_max_chars: requestPreviewMaxChars,
      })
      setCaptureRequestPreviews(inspector.capture_request_previews)
      setRequestPreviewMaxChars(inspector.request_preview_max_chars)
      message.success('请求检查设置已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingInspector(false)
    }
  }

  const saveResilience = async () => {
    setSavingResilience(true)
    try {
      const resilience = await api.put<ResilienceSettings>('/api/settings/resilience', {
        stream_recovery_enabled: streamRecoveryEnabled,
        stream_recovery_max_retries: streamRecoveryMaxRetries,
        max_retries: maxRetries,
        retry_backoff_ms: retryBackoffMs,
        retry_max_backoff_ms: retryMaxBackoffMs,
      })
      setStreamRecoveryEnabled(resilience.stream_recovery_enabled)
      setStreamRecoveryMaxRetries(resilience.stream_recovery_max_retries)
      setMaxRetries(resilience.max_retries)
      setRetryBackoffMs(resilience.retry_backoff_ms)
      setRetryMaxBackoffMs(resilience.retry_max_backoff_ms)
      message.success('重试策略已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingResilience(false)
    }
  }
  return {
    message,
    settings,
    backingUp,
    vacuuming,
    runningMaintenance,
    retentionDays,
    setRetentionDays,
    savingRetention,
    limits,
    blockedTerms,
    setBlockedTerms,
    maxPromptTokens,
    setMaxPromptTokens,
    savingGuardrails,
    captureRequestPreviews,
    setCaptureRequestPreviews,
    requestPreviewMaxChars,
    setRequestPreviewMaxChars,
    savingInspector,
    streamRecoveryEnabled,
    setStreamRecoveryEnabled,
    streamRecoveryMaxRetries,
    setStreamRecoveryMaxRetries,
    maxRetries,
    setMaxRetries,
    retryBackoffMs,
    setRetryBackoffMs,
    retryMaxBackoffMs,
    setRetryMaxBackoffMs,
    savingResilience,
    loadingCooldowns,
    clearingCooldowns,
    loadCooldowns,
    clearCooldowns,
    cooldownRows,
    backup,
    saveRetention,
    runMaintenance,
    vacuum,
    saveGuardrails,
    saveInspector,
    saveResilience,
  }
}
export type SettingsState = ReturnType<typeof useSettings>
