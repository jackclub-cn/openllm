import { useEffect, useState } from 'react'
import { CopyOutlined, DeleteOutlined, EditOutlined, KeyOutlined, PlusOutlined } from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Form,
  Input,
  InputNumber,
  Modal,
  Popconfirm,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import dayjs from 'dayjs'
import { api, formatError, type ApiKey } from '../api'
import PageHeader from '../components/PageHeader'
import { formatCompact, formatCostMicros, formatExact } from '../format'

type ApiKeyForm = {
  name: string
  daily_token_limit?: number | null
  daily_cost_limit_usd?: number | null
}

export default function ApiKeys() {
  const { message } = App.useApp()
  const [items, setItems] = useState<ApiKey[]>([])
  const [loading, setLoading] = useState(true)
  const [open, setOpen] = useState(false)
  const [saving, setSaving] = useState(false)
  const [createdKey, setCreatedKey] = useState('')
  const [editing, setEditing] = useState<ApiKey>()
  const [limitsOpen, setLimitsOpen] = useState(false)
  const [limitsSaving, setLimitsSaving] = useState(false)
  const [form] = Form.useForm<ApiKeyForm>()
  const [limitsForm] = Form.useForm<ApiKeyForm>()

  const load = async () => {
    setLoading(true)
    try {
      setItems(await api.get<ApiKey[]>('/api/api-keys'))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { void load() }, [])

  const create = async () => {
    const values = await form.validateFields()
    setSaving(true)
    try {
      const result = await api.post<{ key: string; item: ApiKey }>('/api/api-keys', {
        name: values.name,
        daily_token_limit: values.daily_token_limit ?? 0,
        daily_cost_limit_micros:
          values.daily_cost_limit_usd != null
            ? Math.round(values.daily_cost_limit_usd * 1_000_000)
            : 0,
      })
      setCreatedKey(result.key)
      form.resetFields()
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  const closeModal = () => {
    setOpen(false)
    setCreatedKey('')
    form.resetFields()
  }

  const remove = async (id: number) => {
    try {
      await api.delete(`/api/api-keys/${id}`)
      message.success('密钥已删除')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const toggle = async (id: number, enabled: boolean) => {
    try {
      await api.put(`/api/api-keys/${id}`, { enabled })
      message.success(enabled ? '密钥已启用' : '密钥已停用')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const openLimits = (record: ApiKey) => {
    setEditing(record)
    limitsForm.setFieldsValue({
      daily_token_limit: record.daily_token_limit ?? null,
      daily_cost_limit_usd:
        record.daily_cost_limit_micros != null
          ? record.daily_cost_limit_micros / 1_000_000
          : null,
    })
    setLimitsOpen(true)
  }

  const saveLimits = async () => {
    if (!editing) return
    const values = await limitsForm.validateFields()
    setLimitsSaving(true)
    try {
      await api.put(`/api/api-keys/${editing.id}`, {
        enabled: editing.enabled,
        daily_token_limit: values.daily_token_limit ?? 0,
        daily_cost_limit_micros:
          values.daily_cost_limit_usd != null
            ? Math.round(values.daily_cost_limit_usd * 1_000_000)
            : 0,
      })
      message.success('限额已保存')
      setLimitsOpen(false)
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLimitsSaving(false)
    }
  }

  return (
    <>
      <PageHeader
        title="访问密钥"
        description="为调用方签发网关密钥；密钥仅在创建时显示一次"
        extra={<Button type="primary" icon={<PlusOutlined />} onClick={() => setOpen(true)}>创建密钥</Button>}
      />
      <Alert
        className="page-alert"
        type="info"
        showIcon
        message="未创建任何密钥时，网关默认允许匿名访问。创建首个密钥后，所有 /v1 请求必须携带 Bearer Token。"
      />
      <Card bordered={false}>
        <Table
          rowKey="id"
          loading={loading}
          dataSource={items}
          pagination={false}
          scroll={{ x: 880 }}
          columns={[
            {
              title: '名称',
              dataIndex: 'name',
              render: (value: string) => <Space><KeyOutlined />{value}</Space>,
            },
            {
              title: '密钥',
              render: (_, record) => <Typography.Text code>{record.key_prefix}...{record.key_suffix}</Typography.Text>,
            },
            {
              title: '最后使用',
              dataIndex: 'last_used_at',
              width: 170,
              render: (value?: string) => value ? dayjs(value).format('YYYY-MM-DD HH:mm:ss') : '从未使用',
            },
            {
              title: '请求',
              dataIndex: 'requests',
              width: 80,
            },
            {
              title: '令牌',
              dataIndex: 'tokens',
              width: 110,
              render: (value: number) => (
                <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
              ),
            },
            {
              title: '费用',
              dataIndex: 'cost_micros',
              width: 120,
              render: (value: number | null, record) => (
                <div>
                  <div>{formatCostMicros(value)}</div>
                  {record.unpriced_requests > 0 && (
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      {record.unpriced_requests} 条未定价
                    </Typography.Text>
                  )}
                </div>
              ),
            },
            {
              title: '今日用量 / 限额',
              width: 190,
              render: (_, record) => {
                const limits: string[] = []
                if (record.today_requests > 0) {
                  limits.push(`请求 ${record.today_requests}`)
                }
                if (record.daily_token_limit != null) {
                  limits.push(
                    `token ${formatCompact(record.today_tokens)} / ${formatCompact(record.daily_token_limit)}`,
                  )
                } else if (record.today_tokens > 0) {
                  limits.push(`token ${formatCompact(record.today_tokens)}`)
                }
                if (record.daily_cost_limit_micros != null) {
                  limits.push(
                    `${formatCostMicros(record.today_cost_micros)} / ${formatCostMicros(record.daily_cost_limit_micros)}`,
                  )
                } else if (record.today_cost_micros != null) {
                  limits.push(formatCostMicros(record.today_cost_micros))
                }
                return limits.length ? (
                  <Space direction="vertical" size={0}>
                    {limits.map((limit) => (
                      <Typography.Text key={limit}>{limit}</Typography.Text>
                    ))}
                  </Space>
                ) : (
                  <Typography.Text type="secondary">今日无消耗</Typography.Text>
                )
              },
            },
            {
              title: '创建时间',
              dataIndex: 'created_at',
              width: 150,
              render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm'),
            },
            {
              title: '状态',
              dataIndex: 'enabled',
              width: 130,
              render: (value: boolean, record) => (
                <Space size={8}>
                  <Switch size="small" checked={value} onChange={(checked) => void toggle(record.id, checked)} />
                  <Tag color={value ? 'success' : 'default'}>{value ? '启用' : '停用'}</Tag>
                </Space>
              ),
            },
            {
              title: '操作',
              width: 120,
              render: (_, record) => (
                <Space>
                  <Tooltip title="编辑限额">
                    <Button type="text" icon={<EditOutlined />} onClick={() => openLimits(record)} />
                  </Tooltip>
                  <Popconfirm title="删除此密钥？" onConfirm={() => remove(record.id)}>
                    <Button type="text" danger icon={<DeleteOutlined />} />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Card>

      <Modal
        title={createdKey ? '密钥已创建' : '创建访问密钥'}
        open={open}
        onCancel={closeModal}
        footer={createdKey ? <Button type="primary" onClick={closeModal}>完成</Button> : undefined}
        onOk={createdKey ? closeModal : create}
        confirmLoading={saving}
        width={560}
        destroyOnHidden
      >
        {createdKey ? (
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <Alert type="warning" showIcon message="请立即保存，此密钥之后无法再次查看。" />
            <Input
              value={createdKey}
              readOnly
              addonAfter={
                <Button
                  type="text"
                  icon={<CopyOutlined />}
                  onClick={() => {
                    void navigator.clipboard.writeText(createdKey)
                    message.success('已复制')
                  }}
                />
              }
            />
          </Space>
        ) : (
          <Form form={form} layout="vertical">
            <Form.Item name="name" label="密钥名称" rules={[{ required: true, message: '请输入名称' }]}>
              <Input placeholder="例如 应用服务器" />
            </Form.Item>
            <div className="form-grid">
              <Form.Item
                name="daily_token_limit"
                label="每日 token 上限"
                extra="留空或 0 表示不限制。"
              >
                <InputNumber min={0} precision={0} style={{ width: '100%' }} />
              </Form.Item>
              <Form.Item
                name="daily_cost_limit_usd"
                label="每日费用上限（USD）"
                extra="留空或 0 表示不限制。"
              >
                <InputNumber min={0} precision={4} style={{ width: '100%' }} />
              </Form.Item>
            </div>
          </Form>
        )}
      </Modal>

      <Modal
        title={editing ? `每日限额 · ${editing.name}` : '每日限额'}
        open={limitsOpen}
        onCancel={() => setLimitsOpen(false)}
        onOk={() => void saveLimits()}
        confirmLoading={limitsSaving}
        width={520}
        destroyOnHidden
      >
        <Form form={limitsForm} layout="vertical">
          <Form.Item
            name="daily_token_limit"
            label="每日 token 上限"
            extra="留空或 0 表示不限制。"
          >
            <InputNumber min={0} precision={0} style={{ width: '100%' }} />
          </Form.Item>
          <Form.Item
            name="daily_cost_limit_usd"
            label="每日费用上限（USD）"
            extra="费用为预估值；无定价请求不会计入费用上限。"
          >
            <InputNumber min={0} precision={4} style={{ width: '100%' }} />
          </Form.Item>
        </Form>
      </Modal>
    </>
  )
}
