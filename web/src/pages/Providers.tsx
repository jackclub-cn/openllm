import { useEffect, useState } from 'react'
import { App, Form } from 'antd'
import { useSearchParams } from 'react-router-dom'
import {
  api,
  formatError,
  type Provider,
  type ProviderApiKeyInput,
  type ProviderInput,
  type ProviderKeyTestAllResult,
} from '../api'
import { providerPresets } from '../providerPresets'
import ProviderEditorModal from './providers/ProviderEditorModal'
import ProviderKeyTestModal from './providers/ProviderKeyTestModal'
import ProviderLimitsModal from './providers/ProviderLimitsModal'
import ProviderQuotaModal from './providers/ProviderQuotaModal'
import ProviderSyncModal from './providers/ProviderSyncModal'
import ProviderTable from './providers/ProviderTable'
import ProviderTemplateModal from './providers/ProviderTemplateModal'
import ProviderToolbar from './providers/ProviderToolbar'
import type {
  ProviderForm,
  ProviderHealth,
  ProviderStatus,
} from './providers/types'

export default function Providers() {
  const { message } = App.useApp()
  const [searchParams, setSearchParams] = useSearchParams()
  const [items, setItems] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [togglingProviderId, setTogglingProviderId] = useState<number>()
  const [testingAll, setTestingAll] = useState(false)
  const [testingAllKeys, setTestingAllKeys] = useState(false)
  const [testingProviderId, setTestingProviderId] = useState<number>()
  const [providerSearch, setProviderSearch] = useState(() => searchParams.get('q') || '')
  const [providerStatus, setProviderStatus] = useState<ProviderStatus>(() => {
    const status = searchParams.get('status')
    return status === 'enabled' || status === 'disabled' ? status : 'all'
  })
  const [providerHealth, setProviderHealth] = useState<ProviderHealth>(() => {
    const health = searchParams.get('health')
    return health === 'healthy' ||
      health === 'failed' ||
      health === 'untested' ||
      health === 'key_error' ||
      health === 'key_untested'
      ? health
      : 'all'
  })
  const [providerPage, setProviderPage] = useState(1)
  const [syncTarget, setSyncTarget] = useState<Provider>()
  const [keyTestProvider, setKeyTestProvider] = useState<Provider>()
  const [quotaProvider, setQuotaProvider] = useState<Provider>()
  const [limitProvider, setLimitProvider] = useState<Provider>()
  const [editing, setEditing] = useState<Provider>()
  const [open, setOpen] = useState(false)
  const [templateOpen, setTemplateOpen] = useState(false)
  const [presetKey, setPresetKey] = useState<string>()
  const [form] = Form.useForm<ProviderForm>()
  const apiKeyRows = Form.useWatch('api_keys', form) || []

  useEffect(() => {
    const next = new URLSearchParams()
    if (providerSearch.trim()) next.set('q', providerSearch.trim())
    if (providerStatus !== 'all') next.set('status', providerStatus)
    if (providerHealth !== 'all') next.set('health', providerHealth)
    setSearchParams(next, { replace: true })
  }, [providerHealth, providerSearch, providerStatus, setSearchParams])

  const load = async () => {
    setLoading(true)
    try {
      setItems(await api.get<Provider[]>('/api/providers'))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const resetNewProviderForm = () => {
    form.setFieldsValue({
      name: '',
      provider_type: 'openai',
      base_url: '',
      model_prefix: '',
      api_keys: [],
      auto_sync_models: true,
      health_check_interval_minutes: 0,
      health_check_model: '',
      models_sync_interval_minutes: 0,
      timeout_seconds: 0,
      configured_cooldown_seconds: 0,
      headersText: '{}',
      modelsText: '',
    } as never)
  }

  const openEditor = (item?: Provider) => {
    if (!item) {
      setEditing(undefined)
      setPresetKey(undefined)
      resetNewProviderForm()
      setTemplateOpen(true)
      return
    }
    setEditing(item)
    form.setFieldsValue({
      ...item,
      api_keys: item.api_keys.map((key) => ({
        id: key.id,
        name: key.name,
        api_key: '',
        api_key_suffix: key.api_key_suffix,
        enabled: key.enabled,
      })),
      // Never re-sync implicitly while editing: doing so would overwrite a
      // manually curated model list. The user must opt in explicitly.
      auto_sync_models: false,
      health_check_interval_minutes: item.health_check_interval_minutes || 0,
      health_check_model: item.health_check_model || '',
      timeout_seconds: item.timeout_seconds || 0,
      configured_cooldown_seconds: item.configured_cooldown_seconds || 0,
      headersText: JSON.stringify(item.headers || {}, null, 2),
      modelsText: item.models.join('\n'),
    } as never)
    setPresetKey(undefined)
    setOpen(true)
  }

  const selectTemplate = (key: string, initial = false) => {
    setPresetKey(key)
    const preset = providerPresets.find((item) => item.key === key)
    if (!preset) return
    if (initial) resetNewProviderForm()
    form.setFieldsValue(preset.values as never)
    setTemplateOpen(false)
    setOpen(true)
  }

  const save = async () => {
    const values = await form.validateFields()
    let headers: Record<string, string>
    try {
      headers = JSON.parse(values.headersText || '{}')
    } catch {
      message.error('请求头必须是有效的 JSON 对象')
      return
    }
    const apiKeys: ProviderApiKeyInput[] = (values.api_keys || []).map((key) => ({
      id: key.id,
      name: key.name?.trim() || '',
      api_key: key.api_key?.trim() || undefined,
      enabled: key.enabled,
    }))
    const missingSecret = apiKeys.findIndex((key) => !key.id && !key.api_key)
    if (missingSecret >= 0) {
      message.error(`第 ${missingSecret + 1} 个密钥还没有填写内容`)
      return
    }
    const payload: ProviderInput = {
      name: values.name,
      provider_type: values.provider_type,
      base_url: values.base_url,
      model_prefix: values.model_prefix,
      api_keys: apiKeys,
      headers,
      auto_sync_models: values.auto_sync_models,
      models: values.modelsText.split('\n').map((item) => item.trim()).filter(Boolean),
      health_check_interval_minutes: values.health_check_interval_minutes ?? 0,
      health_check_model: values.health_check_model?.trim() || '',
      models_sync_interval_minutes: values.models_sync_interval_minutes ?? 0,
      timeout_seconds: values.timeout_seconds ?? 0,
      configured_cooldown_seconds: values.configured_cooldown_seconds ?? 0,
    }
    setSaving(true)
    try {
      if (editing) await api.put(`/api/providers/${editing.id}`, payload)
      else await api.post('/api/providers', payload)
      message.success(editing ? '提供商已更新' : '提供商已添加')
      setOpen(false)
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  const remove = async (id: number) => {
    try {
      await api.delete(`/api/providers/${id}`)
      message.success('提供商已删除')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const toggleEnabled = async (provider: Provider, enabled: boolean) => {
    setTogglingProviderId(provider.id)
    try {
      const updated = await api.put<Provider>(`/api/providers/${provider.id}`, { enabled })
      setItems((current) =>
        current.map((item) => (item.id === provider.id ? updated : item)),
      )
      message.success(enabled ? '提供商已启用' : '提供商已停用')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setTogglingProviderId(undefined)
    }
  }

  const test = async (id: number) => {
    const key = `provider-test-${id}`
    setTestingProviderId(id)
    message.loading({ content: '正在测试连接...', key })
    try {
      const result = await api.post<{
        ok: boolean
        latency_ms: number
        message: string
        checked: 'inference' | 'models'
      }>(`/api/providers/${id}/test`)
      if (result.ok) {
        // Say whether credentials were actually exercised: a listing-only pass
        // does not prove the key works.
        const scope = result.checked === 'inference' ? '凭证已校验' : '仅验证主机可达'
        message.success({
          content: `连接成功（${scope}），耗时 ${result.latency_ms} ms`,
          key,
        })
      } else {
        message.error({ content: result.message, key, duration: 6 })
      }
      await load()
    } catch (error) {
      message.error({ content: formatError(error), key })
    } finally {
      setTestingProviderId(undefined)
    }
  }

  const testAll = async () => {
    setTestingAll(true)
    try {
      const result = await api.post<{
        total: number
        ok: number
        failed: number
      }>('/api/providers/test-all')
      if (result.failed > 0) {
        message.warning(`检测完成：${result.ok} 正常，${result.failed} 异常`)
      } else {
        message.success(`检测完成：${result.ok} 个提供商全部正常`)
      }
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setTestingAll(false)
    }
  }

  const testAllKeys = async () => {
    setTestingAllKeys(true)
    try {
      const result = await api.post<ProviderKeyTestAllResult>('/api/providers/test-all-keys')
      if (result.failed_keys > 0) {
        message.warning(
          `密钥检测完成：${result.healthy_keys} 正常，${result.failed_keys} 异常`,
        )
      } else {
        message.success(
          `密钥检测完成：${result.tested_providers} 个提供商、${result.healthy_keys} 把密钥全部正常`,
        )
      }
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setTestingAllKeys(false)
    }
  }

  const filteredProviders = items.filter((provider) => {
    const query = providerSearch.trim().toLowerCase()
    if (query) {
      const searchable = [
        provider.name,
        provider.base_url,
        provider.model_prefix,
        ...provider.models,
        ...provider.api_keys.flatMap((key) => [
          key.name,
          key.api_key_suffix,
          key.last_error || '',
        ]),
      ]
        .join(' ')
        .toLowerCase()
      if (!searchable.includes(query)) return false
    }
    if (providerStatus === 'enabled' && !provider.enabled) return false
    if (providerStatus === 'disabled' && provider.enabled) return false
    if (providerHealth === 'healthy' && provider.last_test_ok !== true) return false
    if (providerHealth === 'failed' && provider.last_test_ok !== false) return false
    if (providerHealth === 'untested' && provider.last_test_ok != null) return false
    if (
      providerHealth === 'key_error' &&
      !provider.api_keys.some(
        (key) => key.enabled && (key.last_test_ok === false || Boolean(key.last_error)),
      )
    ) {
      return false
    }
    if (
      providerHealth === 'key_untested' &&
      !provider.api_keys.some((key) => key.enabled && key.last_test_ok == null)
    ) {
      return false
    }
    return true
  })
  const selectedPreset = providerPresets.find((preset) => preset.key === presetKey)

  return (
    <>
      <ProviderToolbar
        providerSearch={providerSearch}
        providerStatus={providerStatus}
        providerHealth={providerHealth}
        providerCount={items.length}
        filteredCount={filteredProviders.length}
        testingAll={testingAll}
        testingAllKeys={testingAllKeys}
        hasTestableKeys={items.some((provider) =>
          provider.api_keys.some((key) => key.enabled),
        )}
        onSearchChange={(value) => {
          setProviderSearch(value)
          setProviderPage(1)
        }}
        onStatusChange={(value) => {
          setProviderStatus(value)
          setProviderPage(1)
        }}
        onHealthChange={(value) => {
          setProviderHealth(value)
          setProviderPage(1)
        }}
        onTestAll={() => void testAll()}
        onTestAllKeys={() => void testAllKeys()}
        onAdd={() => openEditor()}
      />
      <ProviderTable
        providers={filteredProviders}
        providerCount={items.length}
        loading={loading}
        page={providerPage}
        togglingProviderId={togglingProviderId}
        testingProviderId={testingProviderId}
        onPageChange={setProviderPage}
        onToggleEnabled={(provider, enabled) => void toggleEnabled(provider, enabled)}
        onTest={(id) => void test(id)}
        onTestKeys={setKeyTestProvider}
        onOpenQuota={setQuotaProvider}
        onManageModels={setLimitProvider}
        onEdit={openEditor}
        onRemove={(id) => void remove(id)}
        onPreviewSync={setSyncTarget}
      />

      <ProviderTemplateModal
        open={templateOpen}
        onCancel={() => setTemplateOpen(false)}
        onSelect={(key) => selectTemplate(key, true)}
      />
      <ProviderEditorModal
        open={open}
        editing={editing}
        selectedPreset={selectedPreset}
        saving={saving}
        form={form}
        apiKeyRows={apiKeyRows}
        onCancel={() => setOpen(false)}
        onSave={() => void save()}
        onChangeTemplate={() => {
          setOpen(false)
          setTemplateOpen(true)
        }}
      />
      <ProviderSyncModal
        provider={syncTarget}
        onClose={() => setSyncTarget(undefined)}
        onApplied={load}
      />
      <ProviderKeyTestModal
        provider={keyTestProvider}
        onClose={() => setKeyTestProvider(undefined)}
      />
      <ProviderQuotaModal
        provider={quotaProvider}
        onClose={() => setQuotaProvider(undefined)}
      />
      <ProviderLimitsModal
        provider={limitProvider}
        onClose={() => setLimitProvider(undefined)}
      />
    </>
  )
}
