import { useEffect, useState } from 'react'
import {
  ApiOutlined,
  DeleteOutlined,
  EditOutlined,
  PlusOutlined,
  SyncOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons'
import {
  App,
  Button,
  Card,
  Form,
  Input,
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
import { api, formatError, type Provider, type ProviderInput } from '../api'
import PageHeader from '../components/PageHeader'

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
      const result = await api.post<{ ok: boolean; latency_ms: number; message: string }>(`/api/providers/${id}/test`)
      if (result.ok) message.success({ content: `连接成功，耗时 ${result.latency_ms} ms`, key })
      else message.error({ content: result.message, key, duration: 6 })
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
              width: 200,
              fixed: 'right',
              render: (_, record) => (
                <Space>
                  <Tooltip title="从上游同步模型">
                    <Button type="text" icon={<SyncOutlined />} onClick={() => sync(record.id)} />
                  </Tooltip>
                  <Tooltip title="测试连接">
                    <Button type="text" icon={<ThunderboltOutlined />} onClick={() => test(record.id)} />
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
    </>
  )
}
