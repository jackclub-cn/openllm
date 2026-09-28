import { useEffect, useState } from 'react'
import { DeleteOutlined, EditOutlined, PlusOutlined } from '@ant-design/icons'
import {
  App,
  AutoComplete,
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
import { api, formatError, type GatewayRoute, type Provider, type RouteTarget } from '../api'
import PageHeader from '../components/PageHeader'

type FormValues = {
  name: string
  model_pattern: string
  strategy: GatewayRoute['strategy']
  enabled: boolean
  targets: RouteTarget[]
}

const strategyLabels = {
  priority: '优先级',
  weighted: '加权随机',
  round_robin: '轮询',
}

export default function RoutesPage() {
  const { message } = App.useApp()
  const [items, setItems] = useState<GatewayRoute[]>([])
  const [providers, setProviders] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [editing, setEditing] = useState<GatewayRoute>()
  const [open, setOpen] = useState(false)
  const [form] = Form.useForm<FormValues>()
  const watchedTargets = Form.useWatch('targets', form)

  const load = async () => {
    setLoading(true)
    try {
      const [routes, providerItems] = await Promise.all([
        api.get<GatewayRoute[]>('/api/routes'),
        api.get<Provider[]>('/api/providers'),
      ])
      setItems(routes)
      setProviders(providerItems)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { void load() }, [])

  const openEditor = (item?: GatewayRoute) => {
    setEditing(item)
    form.setFieldsValue(item ? {
      name: item.name,
      model_pattern: item.model_pattern,
      strategy: item.strategy,
      enabled: item.enabled,
      targets: item.targets.map((target) => ({ ...target })),
    } : {
      name: '',
      model_pattern: '',
      strategy: 'priority',
      enabled: true,
      targets: [{ provider_id: providers[0]?.id, upstream_model: '', weight: 100, priority: 0, enabled: true }],
    } as FormValues)
    setOpen(true)
  }

  const save = async () => {
    const values = await form.validateFields()
    setSaving(true)
    try {
      if (editing) await api.put(`/api/routes/${editing.id}`, values)
      else await api.post('/api/routes', values)
      message.success(editing ? '路由已更新' : '路由已创建')
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
      await api.delete(`/api/routes/${id}`)
      message.success('路由已删除')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  return (
    <>
      <PageHeader
        title="模型路由"
        description="按模型通配符选择上游，并配置故障切换与负载均衡"
        extra={<Button type="primary" icon={<PlusOutlined />} onClick={() => openEditor()}>创建路由</Button>}
      />
      <Card bordered={false}>
        <Table
          rowKey="id"
          loading={loading}
          dataSource={items}
          pagination={false}
          scroll={{ x: 900 }}
          columns={[
            {
              title: '路由',
              dataIndex: 'name',
              render: (value: string, record) => (
                <div>
                  <Typography.Text strong>{value}</Typography.Text>
                  <div><Typography.Text code>{record.model_pattern}</Typography.Text></div>
                </div>
              ),
            },
            {
              title: '策略',
              dataIndex: 'strategy',
              width: 120,
              render: (value: GatewayRoute['strategy']) => <Tag color="blue">{strategyLabels[value]}</Tag>,
            },
            {
              title: '上游目标',
              dataIndex: 'targets',
              render: (targets: GatewayRoute['targets']) => (
                <Space wrap>
                  {targets.map((target, index) => (
                    <Tag key={`${target.id ?? index}-${target.provider_id}`} color={target.enabled ? 'cyan' : 'default'}>
                      {target.provider_name} / {target.model_prefix || ''}{target.upstream_model}
                    </Tag>
                  ))}
                </Space>
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
              width: 110,
              fixed: 'right',
              render: (_, record) => (
                <Space>
                  <Tooltip title="编辑">
                    <Button type="text" icon={<EditOutlined />} onClick={() => openEditor(record)} />
                  </Tooltip>
                  <Popconfirm title="删除此路由？" onConfirm={() => remove(record.id)}>
                    <Button type="text" danger icon={<DeleteOutlined />} />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Card>

      <Modal
        title={editing ? '编辑路由' : '创建路由'}
        open={open}
        onCancel={() => setOpen(false)}
        onOk={save}
        confirmLoading={saving}
        width={860}
        destroyOnHidden
      >
        <Form form={form} layout="vertical">
          <div className="form-grid">
            <Form.Item name="name" label="路由名称" rules={[{ required: true, message: '请输入名称' }]}>
              <Input placeholder="默认聊天模型" />
            </Form.Item>
            <Form.Item
              name="model_pattern"
              label="模型匹配"
              extra="支持 * 和 ? 通配符，例如 gpt-* 或 claude-*。"
              rules={[{ required: true, message: '请输入模型匹配规则' }]}
            >
              <Input placeholder="gpt-*" />
            </Form.Item>
            <Form.Item name="strategy" label="调度策略" rules={[{ required: true }]}>
              <Select options={Object.entries(strategyLabels).map(([value, label]) => ({ value, label }))} />
            </Form.Item>
            <Form.Item name="enabled" label="启用" valuePropName="checked">
              <Switch />
            </Form.Item>
          </div>
          <Typography.Title level={5}>上游目标</Typography.Title>
          <Form.List name="targets">
            {(fields, { add, remove: removeTarget }) => (
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                {fields.map((field) => (
                  <div className="target-row" key={field.key}>
                    <Form.Item
                      {...field}
                      name={[field.name, 'provider_id']}
                      rules={[{ required: true, message: '选择提供商' }]}
                    >
                      <Select
                        placeholder="提供商"
                        onChange={(value, previous) => {
                          if (value !== previous) {
                            form.setFieldValue(['targets', field.name, 'upstream_model'], undefined)
                          }
                        }}
                        options={providers.filter((provider) => provider.enabled).map((provider) => ({
                          value: provider.id,
                          label: provider.name,
                        }))}
                      />
                    </Form.Item>
                    <Form.Item
                      {...field}
                      name={[field.name, 'upstream_model']}
                      rules={[{ required: true, message: '填写上游模型' }]}
                    >
                      <AutoComplete
                        placeholder="上游模型名称"
                        options={providerModels(providers, watchedTargets?.[field.name]?.provider_id).map((value) => ({ value }))}
                        filterOption={(input, option) =>
                          String(option?.value || '').toLowerCase().includes(input.toLowerCase())
                        }
                      />
                    </Form.Item>
                    <Form.Item {...field} name={[field.name, 'priority']}>
                      <InputNumber min={0} placeholder="优先级" />
                    </Form.Item>
                    <Form.Item {...field} name={[field.name, 'weight']}>
                      <InputNumber min={1} placeholder="权重" />
                    </Form.Item>
                    <Form.Item {...field} name={[field.name, 'enabled']} valuePropName="checked">
                      <Switch checkedChildren="启用" unCheckedChildren="停用" />
                    </Form.Item>
                    <Button
                      type="text"
                      danger
                      aria-label="删除目标"
                      icon={<DeleteOutlined />}
                      onClick={() => removeTarget(field.name)}
                    />
                  </div>
                ))}
                <Button type="dashed" block icon={<PlusOutlined />} onClick={() => add({ weight: 100, priority: 0, enabled: true })}>
                  添加上游目标
                </Button>
              </Space>
            )}
          </Form.List>
        </Form>
      </Modal>
    </>
  )
}

function providerModels(providers: Provider[], providerId?: number) {
  return providers.find((provider) => provider.id === providerId)?.models || []
}
