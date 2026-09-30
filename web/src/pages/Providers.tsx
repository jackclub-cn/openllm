import { useEffect, useState } from 'react'
import {
  ApiOutlined,
  CheckCircleOutlined,
  ClearOutlined,
  DeleteOutlined,
  EditOutlined,
  HistoryOutlined,
  PlusOutlined,
  SearchOutlined,
  SettingOutlined,
  StopOutlined,
  SyncOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons'
import {
  App,
  Button,
  Card,
  Form,
  Input,
  InputNumber,
  Modal,
  Popconfirm,
  Segmented,
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type Provider,
  type ProviderApiKeyInput,
  type ProviderInput,
  type ProviderModelLimit,
  type ProviderModelLimitInput,
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
  const [items, setItems] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [testingAll, setTestingAll] = useState(false)
  const [previewingSync, setPreviewingSync] = useState(false)
  const [applyingSync, setApplyingSync] = useState(false)
  const [syncTarget, setSyncTarget] = useState<Provider>()
  const [syncPreview, setSyncPreview] = useState<ModelSyncPreview>()
  const [editing, setEditing] = useState<Provider>()
  const [open, setOpen] = useState(false)
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

  const openEditor = (item?: Provider) => {
    setEditing(item)
    form.setFieldsValue(item ? {
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
      headersText: JSON.stringify(item.headers || {}, null, 2),
      modelsText: item.models.join('\n'),
    } as never : {
      name: '',
      provider_type: 'openai',
      base_url: 'https://api.openai.com/v1',
      model_prefix: '',
      api_keys: [],
      enabled: true,
      tool_search_supported: true,
      auto_sync_models: true,
      health_check_interval_minutes: 0,
      models_sync_interval_minutes: 0,
      headersText: '{}',
      modelsText: '',
    } as never)
    setPresetKey(undefined)
    setOpen(true)
  }

  /**
   * Fills the form from a common provider preset. Only connection details are
   * written, so anything the user already typed (keys, headers, model list)
   * survives a preset change.
   */
  const applyPreset = (key: string) => {
    setPresetKey(key)
    const preset = providerPresets.find((item) => item.key === key)
    if (!preset) return
    form.setFieldsValue(preset.values as never)
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
      enabled: values.enabled,
      tool_search_supported: values.tool_search_supported,
      auto_sync_models: values.auto_sync_models,
      models: values.modelsText.split('\n').map((item) => item.trim()).filter(Boolean),
      health_check_interval_minutes: values.health_check_interval_minutes ?? 0,
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

  return (
    <>
      <PageHeader
        title="提供商"
        description="管理 OpenAI、Anthropic、Ollama 及任意兼容 API"
        extra={
          <Space>
            <Button
              icon={<ThunderboltOutlined />}
              loading={testingAll}
              disabled={!items.length}
              onClick={() => void testAll()}
            >
              测试全部
            </Button>
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
          dataSource={items}
          pagination={false}
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
                const detail = keys.map((key) => (
                  <div key={key.id}>
                    {key.name || `Key ${key.id}`} · {key.api_key_suffix || '****'} ·{' '}
                    {key.enabled ? '启用' : '停用'}
                    {key.requests > 0
                      ? ` · ${key.requests} 次 · ${key.success_rate.toFixed(1)}% · ${Math.round(key.avg_latency_ms)} ms`
                      : ' · 暂无请求'}
                    {key.last_error ? ` · ${key.last_error}` : ''}
                  </div>
                ))
                return (
                  <Tooltip title={<div>{detail}</div>}>
                    <Tag color={enabled ? 'green' : 'default'}>
                      {enabled}/{keys.length} 个可用
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
                  record.last_test_ok === undefined
                    ? { color: 'default', label: '未检测' }
                    : record.last_test_ok
                      ? { color: 'success', label: '正常' }
                      : { color: 'error', label: '异常' }
                const detail = [
                  record.last_test_message,
                  record.last_test_latency_ms != null ? `${record.last_test_latency_ms} ms` : '',
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
                          ? record.last_test_at.slice(0, 16).replace('T', ' ')
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
                  <Tooltip title={record.models_sync_error || undefined}>
                    <Typography.Text
                      type={record.models_sync_error ? 'danger' : 'secondary'}
                      ellipsis
                      style={{ maxWidth: 170 }}
                    >
                      {record.models_sync_error
                        ? `同步失败：${record.models_sync_error}`
                        : record.models_synced_at
                          ? `同步于 ${record.models_synced_at.slice(0, 16).replace('T', ' ')}`
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
              title: '工具搜索兼容',
              dataIndex: 'tool_search_supported',
              width: 150,
              render: (value: boolean) => (
                <Tooltip
                  title={
                    value
                      ? '直接转发 tool_search'
                      : '转发前自动移除 tool_search，避免不支持该工具的上游返回 400'
                  }
                >
                  <Tag color={value ? 'blue' : 'orange'}>
                    {value ? '原生支持' : '自动兼容'}
                  </Tag>
                </Tooltip>
              ),
            },
            {
              title: '状态',
              dataIndex: 'enabled',
              width: 90,
              render: (value: boolean) => <Tag color={value ? 'success' : 'default'}>{value ? '启用' : '停用'}</Tag>,
            },
            {
              title: '操作',
              width: 280,
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
        title={editing ? '编辑提供商' : '添加提供商'}
        open={open}
        onCancel={() => setOpen(false)}
        onOk={save}
        confirmLoading={saving}
        width={680}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" className="modal-form">
          {!editing && (
            <Form.Item label="常用提供商" extra="选择后自动填入协议、地址和模型前缀，仍可手动修改。">
              <Select
                showSearch
                allowClear
                value={presetKey}
                onChange={(value) => (value ? applyPreset(value) : setPresetKey(undefined))}
                placeholder="选择预设快速填充"
                optionFilterProp="label"
                options={Array.from(new Set(providerPresets.map((item) => item.group))).map((group) => ({
                  label: group,
                  options: providerPresets
                    .filter((item) => item.group === group)
                    .map((item) => ({ value: item.key, label: item.label })),
                }))}
              />
            </Form.Item>
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
            name="tool_search_supported"
            label="上游支持 tool_search"
            valuePropName="checked"
            extra="上游明确拒绝该工具时会自动关闭；上游升级后可在编辑页重新打开。"
          >
            <Switch />
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
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
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
    </>
  )
}
