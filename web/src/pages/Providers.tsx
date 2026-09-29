import { useEffect, useState } from 'react'
import {
  ApiOutlined,
  DeleteOutlined,
  EditOutlined,
  PlusOutlined,
  SettingOutlined,
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
  Select,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  type Provider,
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

type ProviderForm = ProviderInput & {
  headersText: string
  modelsText: string
}

export default function Providers() {
  const { message } = App.useApp()
  const [items, setItems] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [editing, setEditing] = useState<Provider>()
  const [open, setOpen] = useState(false)
  const [limitsOpen, setLimitsOpen] = useState(false)
  const [limitsLoading, setLimitsLoading] = useState(false)
  const [limitsSaving, setLimitsSaving] = useState(false)
  const [limitProvider, setLimitProvider] = useState<Provider>()
  const [limitRows, setLimitRows] = useState<ProviderModelLimit[]>([])
  const [presetKey, setPresetKey] = useState<string>()
  const [form] = Form.useForm<ProviderForm>()

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
      api_key: '',
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
      api_key: '',
      enabled: true,
      auto_sync_models: true,
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
    const payload: ProviderInput = {
      name: values.name,
      provider_type: values.provider_type,
      base_url: values.base_url,
      model_prefix: values.model_prefix,
      api_key: values.api_key,
      headers,
      enabled: values.enabled,
      auto_sync_models: values.auto_sync_models,
      models: values.modelsText.split('\n').map((item) => item.trim()).filter(Boolean),
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
    } catch (error) {
      message.error({ content: formatError(error), key })
    }
  }

  const sync = async (id: number) => {
    const key = `provider-sync-${id}`
    message.loading({ content: '正在从上游同步模型...', key })
    try {
      const result = await api.post<{ ok: boolean; count: number; message: string }>(`/api/providers/${id}/models/sync`)
      message.success({ content: `已同步 ${result.count} 个模型`, key })
      await load()
    } catch (error) {
      message.error({ content: formatError(error), key, duration: 6 })
    }
  }

  const openLimits = async (provider: Provider) => {
    setLimitProvider(provider)
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

  const saveLimits = async () => {
    if (!limitProvider) return
    const models: ProviderModelLimitInput[] = limitRows.map((row) => ({
      model_name: row.model_name,
      context_limit: row.context_override ?? null,
      input_limit: row.input_override ?? null,
      output_limit: row.output_override ?? null,
    }))
    setLimitsSaving(true)
    try {
      setLimitRows(
        await api.put<ProviderModelLimit[]>(
          `/api/providers/${limitProvider.id}/model-limits`,
          { models },
        ),
      )
      message.success('模型上限已保存')
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
        extra={<Button type="primary" icon={<PlusOutlined />} onClick={() => openEditor()}>添加提供商</Button>}
      />
      <Card bordered={false}>
        <Table
          rowKey="id"
          loading={loading}
          dataSource={items}
          pagination={false}
          scroll={{ x: 860 }}
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
              dataIndex: 'api_key_set',
              width: 100,
              render: (value: boolean) => <Tag color={value ? 'green' : 'default'}>{value ? '已配置' : '无需密钥'}</Tag>,
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
              title: '状态',
              dataIndex: 'enabled',
              width: 90,
              render: (value: boolean) => <Tag color={value ? 'success' : 'default'}>{value ? '启用' : '停用'}</Tag>,
            },
            {
              title: '操作',
              width: 240,
              fixed: 'right',
              render: (_, record) => (
                <Space>
                  <Tooltip title="从上游同步模型">
                    <Button type="text" icon={<SyncOutlined />} onClick={() => sync(record.id)} />
                  </Tooltip>
                  <Tooltip title="测试连接">
                    <Button type="text" icon={<ThunderboltOutlined />} onClick={() => test(record.id)} />
                  </Tooltip>
                  <Tooltip title="模型上限">
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
            name="api_key"
            label="API Key"
            extra={editing?.api_key_set ? '留空将保留当前密钥；输入新值会覆盖。' : '本地模型可以留空。'}
          >
            <Input.Password placeholder="sk-..." autoComplete="new-password" />
          </Form.Item>
          <Form.Item
            name="auto_sync_models"
            label="保存后自动从上游同步模型"
            valuePropName="checked"
          >
            <Switch />
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
        title={limitProvider ? `模型上限 · ${limitProvider.name}` : '模型上限'}
        open={limitsOpen}
        onCancel={() => setLimitsOpen(false)}
        onOk={() => void saveLimits()}
        confirmLoading={limitsSaving}
        okButtonProps={{ disabled: limitsLoading }}
        width={900}
        destroyOnHidden
      >
        <Typography.Paragraph type="secondary">
          留空使用同步值，填写后覆盖同步值。
        </Typography.Paragraph>
        <Table
          rowKey="model_name"
          size="small"
          loading={limitsLoading}
          dataSource={limitRows}
          pagination={false}
          scroll={{ x: 820, y: 520 }}
          locale={{ emptyText: '暂无模型' }}
          columns={[
            {
              title: '模型',
              dataIndex: 'model_name',
              width: 260,
              render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
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
              title: '状态',
              width: 80,
              render: (_, record) =>
                record.context_override != null ||
                record.input_override != null ||
                record.output_override != null ? (
                  <Tag color="blue">已覆盖</Tag>
                ) : (
                  <Tag>同步值</Tag>
                ),
            },
          ]}
        />
      </Modal>
    </>
  )
}
