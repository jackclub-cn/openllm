import { useEffect, useRef, useState } from 'react'
import {
  ApiOutlined,
  AppstoreOutlined,
  CheckCircleOutlined,
  ClearOutlined,
  DeleteOutlined,
  EditOutlined,
  HistoryOutlined,
  KeyOutlined,
  LinkOutlined,
  PlusOutlined,
  SearchOutlined,
  SettingOutlined,
  StopOutlined,
  SyncOutlined,
  ThunderboltOutlined,
  WalletOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  AutoComplete,
  Button,
  Card,
  Form,
  Input,
  InputNumber,
  Modal,
  Popconfirm,
  Progress,
  Segmented,
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import dayjs from 'dayjs'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  api,
  formatError,
  type Provider,
  type ProviderApiKeyInput,
  type ProviderInput,
  type ProviderKeyTestAllResult,
  type ProviderKeyTestResult,
  type ProviderModelLimit,
  type ProviderModelLimitInput,
  type ProviderQuota,
} from '../api'
import PageHeader from '../components/PageHeader'
import { formatCompact, formatExact } from '../format'
import { providerPresets } from '../providerPresets'

const providerLabels = {
  openai: 'OpenAI 兼容',
  anthropic: 'Anthropic',
  ollama: 'Ollama',
  custom: '自定义',
}

const endpointOptions = [
  '/v1/chat/completions',
  '/v1/responses',
  '/v1/completions',
  '/v1/embeddings',
  '/v1/messages',
].map((value) => ({ value, label: value }))

const syncFieldLabels: Record<string, string> = {
  context_limit: '上下文上限',
  input_limit: '输入上限',
  output_limit: '输出上限',
  supported_endpoints: '支持接口',
  cost: '价格',
  display_name: '显示名',
}

function formatUnitPrice(value?: number | null) {
  if (value == null) return '-'
  return `$${value.toLocaleString('en-US', { maximumFractionDigits: 6 })}`
}

type ProviderForm = Omit<ProviderInput, 'api_keys'> & {
  headersText: string
  modelsText: string
  api_keys?: Array<ProviderApiKeyInput & { api_key_suffix?: string }>
}

type ModelSyncPreview = {
  provider_id: number
  added: string[]
  removed: string[]
  changed: Array<{ model_name: string; fields: string[] }>
  retained: number
  disabled_retained: number
}

export default function Providers() {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [items, setItems] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [togglingProviderId, setTogglingProviderId] = useState<number>()
  const [testingAll, setTestingAll] = useState(false)
  const [testingAllKeys, setTestingAllKeys] = useState(false)
  const [providerSearch, setProviderSearch] = useState(() => searchParams.get('q') || '')
  const [providerStatus, setProviderStatus] = useState<'all' | 'enabled' | 'disabled'>(() => {
    const status = searchParams.get('status')
    return status === 'enabled' || status === 'disabled' ? status : 'all'
  })
  const [providerHealth, setProviderHealth] = useState<
    'all' | 'healthy' | 'failed' | 'untested' | 'key_error' | 'key_untested'
  >(() => {
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
  const [keyTestOpen, setKeyTestOpen] = useState(false)
  const [keyTestLoading, setKeyTestLoading] = useState(false)
  const [keyTestProvider, setKeyTestProvider] = useState<Provider>()
  const [keyTestResult, setKeyTestResult] = useState<ProviderKeyTestResult>()
  const [previewingSync, setPreviewingSync] = useState(false)
  const [applyingSync, setApplyingSync] = useState(false)
  const [syncTarget, setSyncTarget] = useState<Provider>()
  const [syncPreview, setSyncPreview] = useState<ModelSyncPreview>()
  const [editing, setEditing] = useState<Provider>()
  const [open, setOpen] = useState(false)
  const [templateOpen, setTemplateOpen] = useState(false)
  const [quotaOpen, setQuotaOpen] = useState(false)
  const [quotaLoading, setQuotaLoading] = useState(false)
  const [quotaProvider, setQuotaProvider] = useState<Provider>()
  const [quota, setQuota] = useState<ProviderQuota>()
  const [quotaKeyId, setQuotaKeyId] = useState<number>()
  const [quotaError, setQuotaError] = useState<string>()
  const quotaRequestSeq = useRef(0)
  const [limitsOpen, setLimitsOpen] = useState(false)
  const [limitsLoading, setLimitsLoading] = useState(false)
  const [limitsSaving, setLimitsSaving] = useState(false)
  const [limitProvider, setLimitProvider] = useState<Provider>()
  const [limitRows, setLimitRows] = useState<ProviderModelLimit[]>([])
  const [limitSearch, setLimitSearch] = useState('')
  const [limitStatus, setLimitStatus] = useState<'all' | 'enabled' | 'disabled'>('all')
  const [limitPage, setLimitPage] = useState(1)
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

  useEffect(() => { void load() }, [])

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
      health_check_model: item.health_check_model || '',
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

  const loadQuota = async (provider: Provider, keyId?: number) => {
    const requestSeq = ++quotaRequestSeq.current
    setQuotaLoading(true)
    setQuotaError(undefined)
    try {
      const query = keyId == null ? '' : `?key_id=${keyId}`
      const next = await api.get<ProviderQuota>(`/api/providers/${provider.id}/quota${query}`)
      if (requestSeq !== quotaRequestSeq.current) return
      setQuota(next)
    } catch (error) {
      if (requestSeq !== quotaRequestSeq.current) return
      setQuota(undefined)
      setQuotaError(formatError(error))
    } finally {
      if (requestSeq === quotaRequestSeq.current) setQuotaLoading(false)
    }
  }

  const openQuota = (provider: Provider) => {
    const defaultKey = provider.api_keys.find((key) => key.enabled) ?? provider.api_keys[0]
    setQuotaProvider(provider)
    setQuota(undefined)
    setQuotaKeyId(defaultKey?.id)
    setQuotaError(undefined)
    setQuotaOpen(true)
    void loadQuota(provider, defaultKey?.id)
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
        message.success({ content: `连接成功（${scope}），耗时 ${result.latency_ms} ms`, key })
      } else {
        message.error({ content: result.message, key, duration: 6 })
      }
      await load()
    } catch (error) {
      message.error({ content: formatError(error), key })
    }
  }

  const testKeys = async (provider: Provider) => {
    setKeyTestProvider(provider)
    setKeyTestResult(undefined)
    setKeyTestOpen(true)
    setKeyTestLoading(true)
    try {
      const result = await api.post<ProviderKeyTestResult>(
        `/api/providers/${provider.id}/keys/test`,
      )
      setKeyTestResult(result)
      if (result.failed > 0) {
        message.warning(`密钥检测完成：${result.ok} 正常，${result.failed} 异常`)
      } else {
        message.success(`密钥检测完成：${result.ok} 把密钥全部正常`)
      }
    } catch (error) {
      message.error(formatError(error))
      setKeyTestOpen(false)
    } finally {
      setKeyTestLoading(false)
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

  const applySync = async (id: number) => {
    const key = `provider-sync-${id}`
    setApplyingSync(true)
    message.loading({ content: '正在应用模型同步...', key })
    try {
      const result = await api.post<{ ok: boolean; count: number; message: string }>(`/api/providers/${id}/models/sync`)
      message.success({ content: `已同步 ${result.count} 个模型`, key })
      setSyncPreview(undefined)
      setSyncTarget(undefined)
      await load()
    } catch (error) {
      message.error({ content: formatError(error), key, duration: 6 })
    } finally {
      setApplyingSync(false)
    }
  }

  const previewSync = async (provider: Provider) => {
    setSyncTarget(provider)
    setPreviewingSync(true)
    try {
      setSyncPreview(
        await api.post<ModelSyncPreview>(`/api/providers/${provider.id}/models/preview`),
      )
    } catch (error) {
      message.error(formatError(error))
      setSyncTarget(undefined)
    } finally {
      setPreviewingSync(false)
    }
  }

  const openLimits = async (provider: Provider) => {
    setLimitProvider(provider)
    setLimitSearch('')
    setLimitStatus('all')
    setLimitPage(1)
    setLimitsOpen(true)
    setLimitsLoading(true)
    try {
      setLimitRows(
        await api.get<ProviderModelLimit[]>(`/api/providers/${provider.id}/model-limits`),
      )
    } catch (error) {
      message.error(formatError(error))
      setLimitsOpen(false)
    } finally {
      setLimitsLoading(false)
    }
  }

  const updateLimitRow = (
    modelName: string,
    field: 'context_override' | 'input_override' | 'output_override',
    value: number | null,
  ) => {
    setLimitRows((rows) =>
      rows.map((row) =>
        row.model_name === modelName ? { ...row, [field]: value ?? undefined } : row,
      ),
    )
  }

  const updateEndpointOverride = (modelName: string, value?: string[]) => {
    setLimitRows((rows) =>
      rows.map((row) =>
        row.model_name === modelName
          ? { ...row, supported_endpoints_override: value?.length ? value : undefined }
          : row,
      ),
    )
  }

  const updateCostOverride = (
    modelName: string,
    field:
      | 'cost_input_override'
      | 'cost_output_override'
      | 'cost_cache_read_override'
      | 'cost_cache_write_override',
    value: number | null,
  ) => {
    setLimitRows((rows) =>
      rows.map((row) =>
        row.model_name === modelName ? { ...row, [field]: value ?? undefined } : row,
      ),
    )
  }

  const toggleLimitRow = (modelName: string, enabled: boolean) => {
    setLimitRows((rows) =>
      rows.map((row) => (row.model_name === modelName ? { ...row, enabled } : row)),
    )
  }

  const filteredLimitRows = limitRows.filter((row) => {
    if (!row.model_name.toLowerCase().includes(limitSearch.trim().toLowerCase())) return false
    if (limitStatus === 'enabled') return row.enabled
    if (limitStatus === 'disabled') return !row.enabled
    return true
  })

  const setFilteredLimitRowsEnabled = (enabled: boolean) => {
    const names = new Set(filteredLimitRows.map((row) => row.model_name))
    setLimitRows((rows) =>
      rows.map((row) => (names.has(row.model_name) ? { ...row, enabled } : row)),
    )
  }

  const clearFilteredLimitOverrides = () => {
    const names = new Set(filteredLimitRows.map((row) => row.model_name))
    setLimitRows((rows) =>
      rows.map((row) =>
        names.has(row.model_name)
          ? {
              ...row,
              context_override: undefined,
              input_override: undefined,
              output_override: undefined,
              supported_endpoints_override: undefined,
              cost_input_override: undefined,
              cost_output_override: undefined,
              cost_cache_read_override: undefined,
              cost_cache_write_override: undefined,
            }
          : row,
      ),
    )
  }

  const saveLimits = async () => {
    if (!limitProvider) return
    const models: ProviderModelLimitInput[] = limitRows.map((row) => ({
      model_name: row.model_name,
      enabled: row.enabled,
      supported_endpoints_override: row.supported_endpoints_override ?? null,
      context_limit: row.context_override ?? null,
      input_limit: row.input_override ?? null,
      output_limit: row.output_override ?? null,
      cost_input_override: row.cost_input_override ?? null,
      cost_output_override: row.cost_output_override ?? null,
      cost_cache_read_override: row.cost_cache_read_override ?? null,
      cost_cache_write_override: row.cost_cache_write_override ?? null,
    }))
    setLimitsSaving(true)
    try {
      setLimitRows(
        await api.put<ProviderModelLimit[]>(
          `/api/providers/${limitProvider.id}/model-limits`,
          { models },
        ),
      )
      message.success('模型设置已保存')
      setLimitsOpen(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLimitsSaving(false)
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
        (key) =>
          key.enabled && (key.last_test_ok === false || Boolean(key.last_error)),
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
      <PageHeader
        title="提供商"
        description="管理 OpenAI、Anthropic、Ollama 及任意兼容 API"
        extra={
          <Space>
            <Input
              allowClear
              prefix={<SearchOutlined />}
              value={providerSearch}
              onChange={(event) => {
                setProviderSearch(event.target.value)
                setProviderPage(1)
              }}
              placeholder="搜索名称、地址、模型或密钥"
              style={{ width: 250 }}
            />
            <Select
              value={providerStatus}
              onChange={(value) => {
                setProviderStatus(value)
                setProviderPage(1)
              }}
              style={{ width: 120 }}
              options={[
                { value: 'all', label: '全部状态' },
                { value: 'enabled', label: '已启用' },
                { value: 'disabled', label: '已停用' },
              ]}
            />
            <Select
              value={providerHealth}
              onChange={(value) => {
                setProviderHealth(value)
                setProviderPage(1)
              }}
              style={{ width: 140 }}
              options={[
                { value: 'all', label: '全部健康' },
                { value: 'healthy', label: '提供商正常' },
                { value: 'failed', label: '提供商异常' },
                { value: 'untested', label: '提供商未检测' },
                { value: 'key_error', label: '密钥异常' },
                { value: 'key_untested', label: '密钥未检测' },
              ]}
            />
            <Tag>
              {filteredProviders.length}/{items.length}
            </Tag>
            <Button
              icon={<ThunderboltOutlined />}
              loading={testingAll}
              disabled={!items.length}
              onClick={() => void testAll()}
            >
              测试全部
            </Button>
            <Popconfirm
              title="逐个探测所有启用密钥？"
              description="每个密钥都会向上游发送一个最小请求。"
              onConfirm={() => void testAllKeys()}
            >
              <Button
                icon={<KeyOutlined />}
                loading={testingAllKeys}
                disabled={
                  !items.some((provider) =>
                    provider.api_keys.some((key) => key.enabled),
                  )
                }
              >
                测试全部密钥
              </Button>
            </Popconfirm>
            <Button type="primary" icon={<PlusOutlined />} onClick={() => openEditor()}>
              添加提供商
            </Button>
          </Space>
        }
      />
      <Card bordered={false}>
        <Table
          rowKey="id"
          loading={loading}
          dataSource={filteredProviders}
          pagination={{
            current: providerPage,
            defaultPageSize: 20,
            pageSizeOptions: [10, 20, 50, 100],
            showSizeChanger: true,
            showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
            onChange: (page) => setProviderPage(page),
          }}
          locale={{
            emptyText: items.length ? '没有符合筛选条件的提供商' : '暂无提供商',
          }}
          scroll={{ x: 1190 }}
          columns={[
            {
              title: '提供商',
              dataIndex: 'name',
              render: (value: string, record) => (
                <Space>
                  <ApiOutlined />
                  <div>
                    <Typography.Text strong>{value}</Typography.Text>
                    <div><Typography.Text type="secondary">{providerLabels[record.provider_type]}</Typography.Text></div>
                  </div>
                </Space>
              ),
            },
            {
              title: 'API 地址',
              dataIndex: 'base_url',
              render: (value: string) => <Typography.Text copyable={{ text: value }}>{value}</Typography.Text>,
            },
            {
              title: '凭证',
              dataIndex: 'api_keys',
              width: 130,
              render: (keys: Provider['api_keys']) => {
                if (!keys?.length) {
                  return <Tag color="default">无需密钥</Tag>
                }
                const enabled = keys.filter((key) => key.enabled).length
                const cooling = keys.filter((key) => key.cooldown_seconds).length
                const failedTests = keys.filter(
                  (key) => key.enabled && key.last_test_ok === false,
                ).length
                const detail = keys.map((key) => (
                  <div key={key.id}>
                    {key.name || `Key ${key.id}`} · {key.api_key_suffix || '****'} ·{' '}
                    {key.enabled ? '启用' : '停用'}
                    {key.cooldown_seconds ? ` · 冷却 ${key.cooldown_seconds}s` : ''}
                    {key.last_test_ok != null
                      ? ` · 检测${key.last_test_ok ? '正常' : '异常'} ${key.last_test_latency_ms ?? 0} ms`
                      : ''}
                    {key.requests > 0
                      ? ` · ${key.requests} 次 · ${key.success_rate.toFixed(1)}% · ${Math.round(key.avg_latency_ms)} ms · 输入 ${formatCompact(key.prompt_tokens)} / 输出 ${formatCompact(key.completion_tokens)}`
                      : ' · 暂无请求'}
                    {key.last_test_ok === false && key.last_test_message
                      ? ` · ${key.last_test_message}`
                      : ''}
                    {key.last_error ? ` · ${key.last_error}` : ''}
                  </div>
                ))
                return (
                  <Tooltip title={<div>{detail}</div>}>
                    <Tag
                      color={
                        failedTests ? 'red' : cooling ? 'orange' : enabled ? 'green' : 'default'
                      }
                    >
                      {enabled}/{keys.length} 个可用
                      {failedTests ? ` · ${failedTests} 检测异常` : ''}
                      {cooling ? ` · ${cooling} 冷却` : ''}
                    </Tag>
                  </Tooltip>
                )
              },
            },
            {
              title: '健康',
              width: 160,
              render: (_, record) => {
                const state =
                  record.last_test_ok == null
                    ? { color: 'default', label: '未检测' }
                    : record.last_test_ok
                      ? { color: 'success', label: '正常' }
                      : { color: 'error', label: '异常' }
                const detail = [
                  record.last_test_message,
                  record.last_test_latency_ms != null ? `${record.last_test_latency_ms} ms` : '',
                  record.health_check_model ? `检测模型 ${record.health_check_model}` : '',
                  record.last_test_checked === 'models' ? '仅验证主机可达' : '',
                  record.health_check_interval_minutes
                    ? `自动每 ${record.health_check_interval_minutes} 分钟`
                    : '',
                ]
                  .filter(Boolean)
                  .join(' · ')
                return (
                  <Tooltip title={detail || undefined}>
                    <Space direction="vertical" size={0}>
                      <Tag color={state.color}>{state.label}</Tag>
                      <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                        {record.last_test_at
                          ? dayjs(record.last_test_at).format('YYYY-MM-DD HH:mm')
                          : '尚未检测'}
                      </Typography.Text>
                    </Space>
                  </Tooltip>
                )
              },
            },
            {
              title: '模型',
              dataIndex: 'models',
              width: 190,
              render: (models: string[], record) => (
                <div>
                  <div>{models.length ? `${models.length} 个` : '-'}</div>
                  <Tooltip
                    title={
                      record.models_sync_error ||
                      (record.models_synced_at
                        ? `同步时间 ${dayjs(record.models_synced_at).format('YYYY-MM-DD HH:mm:ss')}`
                        : undefined)
                    }
                  >
                    <Typography.Text
                      type={record.models_sync_error ? 'danger' : 'secondary'}
                      ellipsis
                      style={{ maxWidth: 170 }}
                    >
                      {record.models_sync_error
                        ? `同步失败：${record.models_sync_error}`
                        : record.models_synced_at
                          ? `同步于 ${dayjs(record.models_synced_at).format('YYYY-MM-DD HH:mm')}`
                          : '尚未同步'}
                    </Typography.Text>
                  </Tooltip>
                </div>
              ),
            },
            {
              title: '前缀',
              dataIndex: 'model_prefix',
              width: 120,
              render: (value: string) => value ? <Typography.Text code>{value}</Typography.Text> : <Typography.Text type="secondary">无</Typography.Text>,
            },
            {
              title: '状态',
              dataIndex: 'enabled',
              width: 190,
              render: (value: boolean, record) => (
                <Space direction="vertical" size={6}>
                  <Switch
                    checked={value}
                    loading={togglingProviderId === record.id}
                    checkedChildren="启用"
                    unCheckedChildren="停用"
                    onChange={(checked) => void toggleEnabled(record, checked)}
                  />
                  <Tooltip
                    title={
                      record.tool_search_supported
                        ? '连接测试时会自动探测；当前直接转发 tool_search'
                        : '连接测试时会自动探测；上游拒绝时会在转发前移除 tool_search'
                    }
                  >
                    <Tag color={record.tool_search_supported ? 'blue' : 'orange'}>
                      工具搜索{record.tool_search_supported ? '原生支持' : '自动兼容'}
                    </Tag>
                  </Tooltip>
                </Space>
              ),
            },
            {
              title: '操作',
              width: 360,
              fixed: 'right',
              render: (_, record) => (
                <Space>
                  <Tooltip title="查看请求日志">
                    <Button
                      type="text"
                      icon={<HistoryOutlined />}
                      onClick={() => navigate(`/usage?provider_id=${record.id}`)}
                    />
                  </Tooltip>
                  <Tooltip title="预览模型同步">
                    <Button
                      type="text"
                      icon={<SyncOutlined />}
                      onClick={() => void previewSync(record)}
                    />
                  </Tooltip>
                  <Tooltip title="测试连接">
                    <Button type="text" icon={<ThunderboltOutlined />} onClick={() => test(record.id)} />
                  </Tooltip>
                  {record.api_keys.length > 0 && (
                    <Tooltip title="逐个测试全部启用密钥">
                      <Button
                        type="text"
                        icon={<KeyOutlined />}
                        disabled={!record.api_keys.some((key) => key.enabled)}
                        onClick={() => void testKeys(record)}
                      />
                    </Tooltip>
                  )}
                  {record.quota_kind && (
                    <Tooltip title="查看额度、余额和价格">
                      <Button
                        type="text"
                        icon={<WalletOutlined />}
                        onClick={() => void openQuota(record)}
                      />
                    </Tooltip>
                  )}
                  <Tooltip title="模型管理">
                    <Button
                      type="text"
                      icon={<SettingOutlined />}
                      onClick={() => void openLimits(record)}
                    />
                  </Tooltip>
                  <Tooltip title="编辑">
                    <Button type="text" icon={<EditOutlined />} onClick={() => openEditor(record)} />
                  </Tooltip>
                  <Popconfirm title="删除此提供商？" onConfirm={() => remove(record.id)}>
                    <Button type="text" danger icon={<DeleteOutlined />} />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Card>

      <Modal
        title="选择提供商模板"
        open={templateOpen}
        onCancel={() => setTemplateOpen(false)}
        footer={null}
        width={980}
        destroyOnHidden
      >
        <Typography.Paragraph type="secondary">
          先选择接近的模板，再填写 API Key。模板只预填协议、地址、前缀和推荐检测模型，所有字段仍可修改。
        </Typography.Paragraph>
        <div className="provider-template-groups">
          {Array.from(new Set(providerPresets.map((item) => item.group))).map((group) => (
            <section key={group}>
              <Typography.Title level={5}>{group}</Typography.Title>
              <div className="provider-template-grid">
                {providerPresets
                  .filter((item) => item.group === group)
                  .map((preset) => (
                    <Card
                      key={preset.key}
                      size="small"
                      hoverable
                      className="provider-template-card"
                      onClick={() => selectTemplate(preset.key, true)}
                    >
                      <Space align="start">
                        <div className="provider-template-icon">
                          <ApiOutlined />
                        </div>
                        <div>
                          <Typography.Text strong>{preset.label}</Typography.Text>
                          <Typography.Paragraph type="secondary" ellipsis={{ rows: 2 }}>
                            {preset.hint}
                          </Typography.Paragraph>
                        </div>
                      </Space>
                    </Card>
                  ))}
              </div>
            </section>
          ))}
        </div>
      </Modal>

      <Modal
        title={editing ? '编辑提供商' : '添加提供商'}
        open={open}
        onCancel={() => setOpen(false)}
        onOk={save}
        confirmLoading={saving}
        width={680}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" className="modal-form">
          {!editing && selectedPreset && (
            <div className="provider-template-selected">
              <Space>
                <AppstoreOutlined />
                <div>
                  <Typography.Text strong>{selectedPreset.label}</Typography.Text>
                  <div>
                    <Typography.Text type="secondary">{selectedPreset.hint}</Typography.Text>
                  </div>
                </div>
              </Space>
              <Button
                onClick={() => {
                  setOpen(false)
                  setTemplateOpen(true)
                }}
              >
                更换模板
              </Button>
            </div>
          )}
          <div className="form-grid">
            <Form.Item name="name" label="名称" rules={[{ required: true, message: '请输入名称' }]}>
              <Input placeholder="例如 OpenAI" />
            </Form.Item>
            <Form.Item name="provider_type" label="协议类型" rules={[{ required: true }]}>
              <Select options={Object.entries(providerLabels).map(([value, label]) => ({ value, label }))} />
            </Form.Item>
          </div>
          <Form.Item name="base_url" label="API 基础地址" rules={[{ required: true, message: '请输入 API 地址' }]}>
            <Input placeholder="https://api.openai.com/v1" />
          </Form.Item>
          <Form.Item
            name="model_prefix"
            label="模型前缀"
            extra="用于命名空间和自动路由，例如 openai/、local/。留空则不添加前缀。"
          >
            <Input placeholder="openai/" />
          </Form.Item>
          <Form.Item
            label="API Keys"
            extra="支持配置多个上游密钥；请求会轮转使用，遇到 401/403 时自动尝试下一把。已有密钥留空即保留原值。"
          >
            <Form.List name="api_keys">
              {(fields, { add, remove }) => (
                <Space direction="vertical" size={8} style={{ width: '100%' }}>
                  {fields.map((field, index) => (
                    <Space key={field.key} align="start" style={{ width: '100%' }}>
                      <Form.Item
                        {...field}
                        name={[field.name, 'id']}
                        hidden
                      >
                        <Input />
                      </Form.Item>
                      <Form.Item
                        {...field}
                        name={[field.name, 'name']}
                        style={{ marginBottom: 0, width: 150 }}
                      >
                        <Input placeholder={`Key ${index + 1}`} />
                      </Form.Item>
                      <Form.Item
                        {...field}
                        name={[field.name, 'api_key']}
                        style={{ marginBottom: 0, width: 260 }}
                      >
                        <Input.Password
                          placeholder={
                            apiKeyRows[index]?.id
                              ? `留空保留${apiKeyRows[index]?.api_key_suffix ? ` (****${apiKeyRows[index].api_key_suffix})` : ''}`
                              : 'sk-...'
                          }
                          autoComplete="new-password"
                        />
                      </Form.Item>
                      <Form.Item
                        {...field}
                        name={[field.name, 'enabled']}
                        valuePropName="checked"
                        style={{ marginBottom: 0 }}
                      >
                        <Switch checkedChildren="启用" unCheckedChildren="停用" />
                      </Form.Item>
                      <Button
                        type="text"
                        danger
                        icon={<DeleteOutlined />}
                        onClick={() => remove(field.name)}
                      />
                    </Space>
                  ))}
                  <Button
                    type="dashed"
                    icon={<PlusOutlined />}
                    onClick={() => add({ name: `Key ${fields.length + 1}`, enabled: true })}
                    block
                  >
                    添加密钥
                  </Button>
                </Space>
              )}
            </Form.List>
          </Form.Item>
          <Form.Item
            name="auto_sync_models"
            label="保存后自动从上游同步模型"
            valuePropName="checked"
          >
            <Switch />
          </Form.Item>
          <Form.Item
            name="health_check_interval_minutes"
            label="自动健康检查间隔（分钟）"
            extra="0 或留空表示关闭；启用后后台会按间隔检测并更新健康状态。"
          >
            <InputNumber min={0} precision={0} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item
            name="health_check_model"
            label="健康检查模型"
            extra="留空时使用同步模型列表中的第一个启用模型；也可手动填写仅供检测使用的模型名。"
          >
            <AutoComplete
              allowClear
              options={(editing?.models || []).map((model) => ({ value: model }))}
              placeholder="例如 gpt-5.4-mini"
            />
          </Form.Item>
          <Form.Item
            name="models_sync_interval_minutes"
            label="自动模型同步间隔（分钟）"
            extra="0 或留空表示关闭；失败也会记录尝试时间，避免每分钟重复请求。"
          >
            <InputNumber min={0} precision={0} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item
            name="modelsText"
            label="支持的模型"
            extra="每行一个模型名。点击列表中的同步按钮可直接覆盖为上游最新模型。"
          >
            <Input.TextArea rows={4} placeholder={'gpt-4.1\ngpt-4.1-mini'} />
          </Form.Item>
          <Form.Item name="headersText" label="附加请求头（JSON）">
            <Input.TextArea rows={4} className="code-input" placeholder={'{"X-Organization": "team-a"}'} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={limitProvider ? `模型管理 · ${limitProvider.name}` : '模型管理'}
        open={limitsOpen}
        onCancel={() => setLimitsOpen(false)}
        onOk={() => void saveLimits()}
        confirmLoading={limitsSaving}
        okButtonProps={{ disabled: limitsLoading }}
        width={960}
        destroyOnHidden
      >
        <Typography.Paragraph type="secondary">
          停用后模型不会出现在 /v1/models 或参与路由。上限、接口和价格留空使用同步值，填写后覆盖同步值。
        </Typography.Paragraph>
        <Space wrap style={{ marginBottom: 12 }}>
          <Input
            allowClear
            prefix={<SearchOutlined />}
            value={limitSearch}
            onChange={(event) => {
              setLimitSearch(event.target.value)
              setLimitPage(1)
            }}
            placeholder="搜索模型"
            style={{ width: 280 }}
          />
          <Segmented
            value={limitStatus}
            onChange={(value) => {
              setLimitStatus(value as 'all' | 'enabled' | 'disabled')
              setLimitPage(1)
            }}
            options={[
              { label: '全部', value: 'all' },
              { label: '已启用', value: 'enabled' },
              { label: '已停用', value: 'disabled' },
            ]}
          />
          <Tag>{filteredLimitRows.length} 个结果</Tag>
          <Button
            icon={<CheckCircleOutlined />}
            disabled={!filteredLimitRows.length}
            onClick={() => setFilteredLimitRowsEnabled(true)}
          >
            批量启用
          </Button>
          <Button
            icon={<StopOutlined />}
            disabled={!filteredLimitRows.length}
            onClick={() => setFilteredLimitRowsEnabled(false)}
          >
            批量停用
          </Button>
          <Button
            icon={<ClearOutlined />}
            disabled={!filteredLimitRows.length}
            onClick={clearFilteredLimitOverrides}
          >
            清除覆盖
          </Button>
        </Space>
        <Table
          rowKey="model_name"
          size="small"
          loading={limitsLoading}
          dataSource={filteredLimitRows}
          pagination={{
            current: limitPage,
            defaultPageSize: 50,
            pageSizeOptions: [20, 50, 100],
            showSizeChanger: true,
            showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
            onChange: setLimitPage,
          }}
          scroll={{ x: 1690, y: 520 }}
          locale={{ emptyText: '暂无模型' }}
          columns={[
            {
              title: '模型',
              dataIndex: 'model_name',
              width: 250,
              render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
            },
            {
              title: '支持接口',
              dataIndex: 'supported_endpoints',
              width: 230,
              render: (value: string[]) =>
                value.length ? (
                  <Space size={4} wrap>
                    {value.map((endpoint) => (
                      <Tag key={endpoint}>
                        {endpoint.replace(/^\/v1/, '') || endpoint}
                      </Tag>
                    ))}
                  </Space>
                ) : (
                  <Typography.Text type="secondary">未声明</Typography.Text>
                ),
            },
            {
              title: '接口覆盖',
              width: 240,
              render: (_, record) => (
                <Select
                  mode="multiple"
                  allowClear
                  maxTagCount="responsive"
                  disabled={!record.enabled}
                  value={record.supported_endpoints_override ?? undefined}
                  placeholder="使用同步值"
                  options={endpointOptions}
                  onChange={(value) => updateEndpointOverride(record.model_name, value)}
                  style={{ width: '100%' }}
                />
              ),
            },
            {
              title: '启用',
              dataIndex: 'enabled',
              width: 70,
              render: (value: boolean, record) => (
                <Switch
                  size="small"
                  checked={value}
                  onChange={(checked) => toggleLimitRow(record.model_name, checked)}
                />
              ),
            },
            {
              title: '当前生效',
              key: 'effective',
              width: 210,
              render: (_, record) => (
                <Space size={4} wrap>
                  {record.context_limit != null && (
                    <Tooltip title={`上下文 ${formatExact(record.context_limit)}`}>
                      <Tag>上下文 {formatCompact(record.context_limit)}</Tag>
                    </Tooltip>
                  )}
                  {record.input_limit != null && (
                    <Tooltip title={`输入 ${formatExact(record.input_limit)}`}>
                      <Tag>输入 {formatCompact(record.input_limit)}</Tag>
                    </Tooltip>
                  )}
                  {record.output_limit != null && (
                    <Tooltip title={`输出 ${formatExact(record.output_limit)}`}>
                      <Tag>输出 {formatCompact(record.output_limit)}</Tag>
                    </Tooltip>
                  )}
                  {record.context_limit == null &&
                    record.input_limit == null &&
                    record.output_limit == null && (
                      <Typography.Text type="secondary">无</Typography.Text>
                    )}
                </Space>
              ),
            },
            {
              title: '上下文覆盖',
              width: 150,
              render: (_, record) => (
                <InputNumber
                  min={1}
                  disabled={!record.enabled}
                  value={record.context_override}
                  placeholder={record.context_limit?.toString()}
                  onChange={(value) =>
                    updateLimitRow(record.model_name, 'context_override', value)
                  }
                  style={{ width: '100%' }}
                />
              ),
            },
            {
              title: '输入覆盖',
              width: 150,
              render: (_, record) => (
                <InputNumber
                  min={1}
                  disabled={!record.enabled}
                  value={record.input_override}
                  placeholder={record.input_limit?.toString()}
                  onChange={(value) =>
                    updateLimitRow(record.model_name, 'input_override', value)
                  }
                  style={{ width: '100%' }}
                />
              ),
            },
            {
              title: '输出覆盖',
              width: 150,
              render: (_, record) => (
                <InputNumber
                  min={1}
                  disabled={!record.enabled}
                  value={record.output_override}
                  placeholder={record.output_limit?.toString()}
                  onChange={(value) =>
                    updateLimitRow(record.model_name, 'output_override', value)
                  }
                  style={{ width: '100%' }}
                />
              ),
            },
            {
              title: '价格覆盖（USD / 1M）',
              width: 320,
              render: (_, record) => (
                <Space direction="vertical" size={4} style={{ width: '100%' }}>
                  <InputNumber
                    min={0}
                    precision={6}
                    disabled={!record.enabled}
                    value={record.cost_input_override}
                    placeholder={record.cost_input?.toString() ?? '输入'}
                    addonBefore="输入"
                    onChange={(value) =>
                      updateCostOverride(record.model_name, 'cost_input_override', value)
                    }
                    style={{ width: '100%' }}
                  />
                  <InputNumber
                    min={0}
                    precision={6}
                    disabled={!record.enabled}
                    value={record.cost_output_override}
                    placeholder={record.cost_output?.toString() ?? '输出'}
                    addonBefore="输出"
                    onChange={(value) =>
                      updateCostOverride(record.model_name, 'cost_output_override', value)
                    }
                    style={{ width: '100%' }}
                  />
                  <InputNumber
                    min={0}
                    precision={6}
                    disabled={!record.enabled}
                    value={record.cost_cache_read_override}
                    placeholder={record.cost_cache_read?.toString() ?? '缓存读'}
                    addonBefore="缓存读"
                    onChange={(value) =>
                      updateCostOverride(record.model_name, 'cost_cache_read_override', value)
                    }
                    style={{ width: '100%' }}
                  />
                  <InputNumber
                    min={0}
                    precision={6}
                    disabled={!record.enabled}
                    value={record.cost_cache_write_override}
                    placeholder={record.cost_cache_write?.toString() ?? '缓存写'}
                    addonBefore="缓存写"
                    onChange={(value) =>
                      updateCostOverride(record.model_name, 'cost_cache_write_override', value)
                    }
                    style={{ width: '100%' }}
                  />
                </Space>
              ),
            },
            {
              title: '状态',
              width: 80,
              render: (_, record) =>
                !record.enabled ? (
                  <Tag color="default">已停用</Tag>
                ) : record.context_override != null ||
                record.input_override != null ||
                record.output_override != null ||
                record.supported_endpoints_override != null ||
                record.cost_input_override != null ||
                record.cost_output_override != null ||
                record.cost_cache_read_override != null ||
                record.cost_cache_write_override != null ? (
                  <Tag color="blue">已覆盖</Tag>
                ) : (
                  <Tag>同步值</Tag>
                ),
            },
          ]}
        />
      </Modal>

      <Modal
        title={syncTarget ? `同步预览 · ${syncTarget.name}` : '同步预览'}
        open={Boolean(syncTarget)}
        onCancel={() => {
          setSyncTarget(undefined)
          setSyncPreview(undefined)
        }}
        onOk={() => syncTarget && void applySync(syncTarget.id)}
        confirmLoading={previewingSync || applyingSync}
        okButtonProps={{ disabled: !syncPreview }}
        okText="应用同步"
        width={640}
        destroyOnHidden
      >
        {syncPreview && (
          <Space direction="vertical" size={12} style={{ width: '100%' }}>
            <Space wrap>
              <Tag color="green">新增 {syncPreview.added.length}</Tag>
              <Tag color="red">移除 {syncPreview.removed.length}</Tag>
              <Tag color="orange">变更 {syncPreview.changed.length}</Tag>
              <Tag color="blue">保留 {syncPreview.retained}</Tag>
              <Tag>停用保留 {syncPreview.disabled_retained}</Tag>
            </Space>
            {[
              ['新增模型', syncPreview.added, 'green'],
              ['移除模型', syncPreview.removed, 'red'],
            ].map(([label, models, color]) =>
              (models as string[]).length ? (
                <div key={label as string}>
                  <Typography.Text strong>{label as string}</Typography.Text>
                  <div
                    style={{
                      maxHeight: 180,
                      overflow: 'auto',
                      marginTop: 6,
                      border: '1px solid #f0f0f0',
                      borderRadius: 6,
                      padding: 8,
                    }}
                  >
                    <Space wrap size={[4, 4]}>
                      {(models as string[]).map((model) => (
                        <Tag key={model} color={color as string}>
                          {model}
                        </Tag>
                      ))}
                    </Space>
                  </div>
                </div>
              ) : null,
            )}
            {syncPreview.changed.length > 0 && (
              <div>
                <Typography.Text strong>元数据变更</Typography.Text>
                <div
                  style={{
                    maxHeight: 180,
                    overflow: 'auto',
                    marginTop: 6,
                    border: '1px solid #f0f0f0',
                    borderRadius: 6,
                    padding: 8,
                  }}
                >
                  <Space direction="vertical" size={6} style={{ width: '100%' }}>
                    {syncPreview.changed.map((change) => (
                      <Space key={change.model_name} wrap>
                        <Typography.Text code>{change.model_name}</Typography.Text>
                        {change.fields.map((field) => (
                          <Tag key={field} color="orange">
                            {syncFieldLabels[field] || field}
                          </Tag>
                        ))}
                      </Space>
                    ))}
                  </Space>
                </div>
              </div>
            )}
            {!syncPreview.added.length &&
              !syncPreview.removed.length &&
              !syncPreview.changed.length && (
              <Typography.Text type="secondary">
                上游模型列表与本地一致，应用后不会改变模型数据。
              </Typography.Text>
            )}
          </Space>
        )}
      </Modal>

      <Modal
        title={keyTestProvider ? `密钥检测 · ${keyTestProvider.name}` : '密钥检测'}
        open={keyTestOpen}
        onCancel={() => setKeyTestOpen(false)}
        footer={null}
        width={780}
        destroyOnHidden
      >
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          {keyTestResult && (
            <Space wrap>
              <Tag color="green">正常 {keyTestResult.ok}</Tag>
              <Tag color={keyTestResult.failed ? 'red' : 'default'}>
                异常 {keyTestResult.failed}
              </Tag>
              <Tag>共 {keyTestResult.total} 把</Tag>
              {keyTestResult.model && (
                <Typography.Text type="secondary">
                  检测模型 {keyTestResult.model}
                </Typography.Text>
              )}
            </Space>
          )}
          <Table
            rowKey={(record) =>
              String(record.key_id ?? `${record.key_name}-${record.api_key_suffix}`)
            }
            size="small"
            loading={keyTestLoading}
            dataSource={keyTestResult?.results || []}
            pagination={false}
            locale={{ emptyText: keyTestLoading ? '正在检测...' : '暂无启用密钥' }}
            columns={[
              {
                title: '密钥',
                dataIndex: 'key_name',
                width: 180,
                render: (value: string, record) => (
                  <div>
                    <Typography.Text strong>{value || `Key ${record.key_id ?? ''}`}</Typography.Text>
                    <div>
                      <Typography.Text type="secondary">
                        尾号 {record.api_key_suffix || '****'}
                      </Typography.Text>
                    </div>
                  </div>
                ),
              },
              {
                title: '结果',
                dataIndex: 'ok',
                width: 80,
                render: (value: boolean) => (
                  <Tag color={value ? 'success' : 'error'}>{value ? '正常' : '异常'}</Tag>
                ),
              },
              {
                title: '检测方式',
                dataIndex: 'checked',
                width: 110,
                render: (value: string) =>
                  value === 'inference' ? '推理凭证' : '仅主机可达',
              },
              {
                title: '耗时',
                dataIndex: 'latency_ms',
                width: 90,
                render: (value: number) => `${value} ms`,
              },
              {
                title: '详情',
                dataIndex: 'message',
                render: (value: string) => (
                  <Tooltip title={value}>
                    <Typography.Text
                      type="secondary"
                      ellipsis
                      style={{ display: 'block', maxWidth: 260 }}
                    >
                      {value}
                    </Typography.Text>
                  </Tooltip>
                ),
              },
            ]}
          />
        </Space>
      </Modal>

      <Modal
        title={quotaProvider ? `额度与价格 · ${quotaProvider.name}` : '额度与价格'}
        open={quotaOpen}
        onCancel={() => {
          quotaRequestSeq.current += 1
          setQuotaOpen(false)
        }}
        footer={null}
        width={860}
        destroyOnHidden
      >
        {quotaProvider && quotaProvider.api_keys.length > 0 && (
          <Space wrap style={{ marginBottom: 16 }}>
            <Typography.Text type="secondary">查询密钥</Typography.Text>
            <Select
              value={quotaKeyId}
              loading={quotaLoading}
              style={{ minWidth: 280 }}
              onChange={(value) => {
                setQuotaKeyId(value)
                void loadQuota(quotaProvider, value)
              }}
              options={quotaProvider.api_keys.map((key) => ({
                value: key.id,
                label: `${key.name} (${key.api_key_suffix})${key.enabled ? '' : ' · 已停用'}`,
              }))}
            />
          </Space>
        )}
        {quotaLoading && (
          <Typography.Text type="secondary">正在向上游查询额度...</Typography.Text>
        )}
        {quotaError && (
          <Alert
            type="error"
            showIcon
            message="额度查询失败"
            description={quotaError}
            style={{ marginBottom: 16 }}
          />
        )}
        {quota && (
          <Space direction="vertical" size={18} style={{ width: '100%' }}>
            <Space wrap>
              <Typography.Text strong>{quota.title}</Typography.Text>
              {quota.plan_name && <Tag color="blue">{quota.plan_name}</Tag>}
              {quota.key_name && (
                <Tag>
                  {quota.key_name} · {quota.key_suffix}
                </Tag>
              )}
              <Typography.Text type="secondary">
                查询于 {dayjs(quota.fetched_at).format('YYYY-MM-DD HH:mm:ss')}
              </Typography.Text>
              {quota.source_url && (
                <Typography.Link href={quota.source_url} target="_blank">
                  <Space size={4}>
                    <LinkOutlined />
                    官方页
                  </Space>
                </Typography.Link>
              )}
            </Space>

            {quota.items.length > 0 && (
              <div className="provider-quota-grid">
                {quota.items.map((item) => (
                  <Card key={item.key} size="small" className="provider-quota-card">
                    <Typography.Text type="secondary">{item.label}</Typography.Text>
                    {item.percent != null ? (
                      <>
                        <Progress
                          percent={Math.round(item.percent)}
                          status={item.percent >= 100 ? 'exception' : 'normal'}
                          strokeColor={item.percent >= 80 ? '#d46b08' : '#1677ff'}
                        />
                        {item.used != null && item.limit != null && (
                          <Typography.Text type="secondary">
                            {item.unit === 'USD' ? '$' : ''}
                            {item.used.toFixed(2)} / {item.unit === 'USD' ? '$' : ''}
                            {item.limit.toFixed(2)} {item.unit !== 'USD' ? item.unit : ''}
                          </Typography.Text>
                        )}
                      </>
                    ) : item.remaining != null ? (
                      <div className="provider-quota-value">
                        <Typography.Title level={3}>
                          {item.unit === 'USD' ? '$' : item.unit === 'CNY' ? '¥' : ''}
                          {item.remaining.toFixed(2)}
                          <span>
                            {item.unit !== 'CNY' && item.unit !== 'USD' ? ` ${item.unit}` : ''}
                          </span>
                        </Typography.Title>
                      </div>
                    ) : (
                      <Typography.Text type="secondary">暂无数据</Typography.Text>
                    )}
                    {item.reset_at && (
                      <Typography.Text type="secondary" className="provider-quota-reset">
                        重置于 {dayjs(item.reset_at).format('MM-DD HH:mm')}
                      </Typography.Text>
                    )}
                  </Card>
                ))}
              </div>
            )}

            {quota.details.length > 0 && (
              <Space wrap>
                {quota.details.map((detail) => (
                  <Tag key={`${detail.label}-${detail.value}`}>
                    {detail.label}：{detail.value}
                  </Tag>
                ))}
              </Space>
            )}

            <div>
              <Typography.Title level={5}>模型价格</Typography.Title>
              <Table
                rowKey="model_name"
                size="small"
                pagination={false}
                dataSource={quota.prices}
                locale={{ emptyText: '暂无已同步价格' }}
                columns={[
                  {
                    title: '模型',
                    dataIndex: 'model_name',
                    render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                  },
                  {
                    title: '输入 / 1M',
                    dataIndex: 'input',
                    width: 120,
                    render: (value?: number | null) => formatUnitPrice(value),
                  },
                  {
                    title: '输出 / 1M',
                    dataIndex: 'output',
                    width: 120,
                    render: (value?: number | null) => formatUnitPrice(value),
                  },
                  {
                    title: '缓存读 / 1M',
                    dataIndex: 'cache_read',
                    width: 130,
                    render: (value?: number | null) => formatUnitPrice(value),
                  },
                  {
                    title: '缓存写 / 1M',
                    dataIndex: 'cache_write',
                    width: 130,
                    render: (value?: number | null) => formatUnitPrice(value),
                  },
                ]}
              />
            </div>
          </Space>
        )}
      </Modal>
    </>
  )
}
