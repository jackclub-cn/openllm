import { useEffect, useRef, useState } from 'react'
import {
  DeleteOutlined,
  EditOutlined,
  ExperimentOutlined,
  HistoryOutlined,
  PlusOutlined,
  SearchOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  AutoComplete,
  Button,
  Card,
  Descriptions,
  Empty,
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
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  api,
  formatError,
  type GatewayRoute,
  type Provider,
  type RouteDiagnose,
  type RouteTarget,
} from '../api'
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

const matchTypeLabels = {
  explicit_route: '显式路由',
  prefix: '模型前缀',
  direct: '直接模型',
  conflict: '同名冲突',
  none: '未匹配',
}

const diagnosticEndpoints = [
  '/v1/chat/completions',
  '/v1/responses',
  '/v1/completions',
  '/v1/embeddings',
  '/v1/messages',
].map((value) => ({ value, label: value }))

const diagnosisReasonLabels: Record<string, string> = {
  eligible: '可用',
  'route is disabled': '路由已停用',
  'route target is disabled': '目标已停用',
  'provider is disabled': '提供商已停用',
  'model is disabled': '模型已停用',
  'provider does not support this endpoint': '提供商不支持此接口',
  'model does not declare support for this endpoint': '模型未声明支持此接口',
}

export default function RoutesPage() {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [items, setItems] = useState<GatewayRoute[]>([])
  const [providers, setProviders] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [status, setStatus] = useState<'all' | 'enabled' | 'disabled'>('all')
  const [strategy, setStrategy] = useState<'all' | GatewayRoute['strategy']>('all')
  const [page, setPage] = useState(1)
  const [saving, setSaving] = useState(false)
  const [editing, setEditing] = useState<GatewayRoute>()
  const [open, setOpen] = useState(false)
  const [diagnoseOpen, setDiagnoseOpen] = useState(false)
  const [diagnoseModel, setDiagnoseModel] = useState('')
  const [diagnoseEndpoint, setDiagnoseEndpoint] = useState('/v1/chat/completions')
  const [diagnoseSessionId, setDiagnoseSessionId] = useState('')
  const [diagnosing, setDiagnosing] = useState(false)
  const [diagnosis, setDiagnosis] = useState<RouteDiagnose>()
  const [searchParams, setSearchParams] = useSearchParams()
  const handledDiagnosisQuery = useRef('')
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

  const runDiagnosis = async (
    modelValue = diagnoseModel,
    endpointValue = diagnoseEndpoint,
    sessionValue = diagnoseSessionId,
  ) => {
    const model = modelValue.trim()
    const sessionId = sessionValue.trim()
    if (!model) {
      message.warning('请输入要诊断的模型')
      return
    }
    setDiagnosing(true)
    try {
      setDiagnosis(
        await api.post<RouteDiagnose>('/api/routes/diagnose', {
          model,
          endpoint: endpointValue,
          session_id: sessionId || undefined,
        }),
      )
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setDiagnosing(false)
    }
  }

  useEffect(() => {
    const queryKey = searchParams.toString()
    if (!queryKey || handledDiagnosisQuery.current === queryKey) return
    const model = searchParams.get('diagnose_model')?.trim()
    if (!model) return
    handledDiagnosisQuery.current = queryKey
    const endpoint = searchParams.get('diagnose_endpoint') || '/v1/chat/completions'
    const sessionId = searchParams.get('diagnose_session_id')?.trim() || ''
    setDiagnoseModel(model)
    setDiagnoseEndpoint(endpoint)
    setDiagnoseSessionId(sessionId)
    setDiagnosis(undefined)
    setDiagnoseOpen(true)
    void runDiagnosis(model, endpoint, sessionId)
    setSearchParams({}, { replace: true })
  }, [searchParams, setSearchParams])

  const filteredItems = items.filter((item) => {
    const query = search.trim().toLowerCase()
    if (query) {
      const searchable = [
        item.name,
        item.model_pattern,
        ...item.targets.flatMap((target) => [
          providers.find((provider) => provider.id === target.provider_id)?.name || '',
          target.upstream_model,
        ]),
      ]
        .join(' ')
        .toLowerCase()
      if (!searchable.includes(query)) return false
    }
    if (status === 'enabled' && !item.enabled) return false
    if (status === 'disabled' && item.enabled) return false
    if (strategy !== 'all' && item.strategy !== strategy) return false
    return true
  })

  return (
    <>
      <PageHeader
        title="模型路由"
        description="按模型通配符选择上游，并配置故障切换与负载均衡"
        extra={
          <Space>
            <Input
              allowClear
              prefix={<SearchOutlined />}
              value={search}
              onChange={(event) => {
                setSearch(event.target.value)
                setPage(1)
              }}
              placeholder="搜索路由、模型或上游目标"
              style={{ width: 250 }}
            />
            <Select
              value={status}
              onChange={(value) => {
                setStatus(value)
                setPage(1)
              }}
              style={{ width: 120 }}
              options={[
                { value: 'all', label: '全部状态' },
                { value: 'enabled', label: '已启用' },
                { value: 'disabled', label: '已停用' },
              ]}
            />
            <Select
              value={strategy}
              onChange={(value) => {
                setStrategy(value)
                setPage(1)
              }}
              style={{ width: 130 }}
              options={[
                { value: 'all', label: '全部策略' },
                ...Object.entries(strategyLabels).map(([value, label]) => ({
                  value,
                  label,
                })),
              ]}
            />
            <Tag>
              {filteredItems.length}/{items.length}
            </Tag>
            <Button
              icon={<ExperimentOutlined />}
              onClick={() => {
                setDiagnosis(undefined)
                setDiagnoseOpen(true)
              }}
            >
              路由诊断
            </Button>
            <Button type="primary" icon={<PlusOutlined />} onClick={() => openEditor()}>
              创建路由
            </Button>
          </Space>
        }
      />
      <Card bordered={false}>
        <Table
          rowKey="id"
          loading={loading}
          dataSource={filteredItems}
          pagination={{
            current: page,
            defaultPageSize: 20,
            pageSizeOptions: [10, 20, 50, 100],
            showSizeChanger: true,
            showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
            onChange: (nextPage) => setPage(nextPage),
          }}
          locale={{ emptyText: items.length ? '没有符合筛选条件的路由' : '暂无路由' }}
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
                  {targets.map((target, index) => {
                    const endpoints = target.supported_endpoints?.map((endpoint) =>
                      endpoint.replace(/^\/v1/, ''),
                    )
                    return (
                      <Tooltip
                        key={`${target.id ?? index}-${target.provider_id}`}
                        title={
                          endpoints?.length
                            ? `支持接口：${endpoints.join('、')}`
                            : '未声明接口限制，所有兼容接口均可尝试'
                        }
                      >
                        <Tag color={target.enabled ? 'cyan' : 'default'}>
                          {target.provider_name} / {target.model_prefix || ''}
                          {target.upstream_model}
                          {endpoints?.length ? ` · ${endpoints.join(', ')}` : ''}
                        </Tag>
                      </Tooltip>
                    )
                  })}
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
              width: 150,
              fixed: 'right',
              render: (_, record) => (
                <Space>
                  <Tooltip title="查看请求日志">
                    <Button
                      type="text"
                      icon={<HistoryOutlined />}
                      onClick={() => navigate(`/usage?route_id=${record.id}`)}
                    />
                  </Tooltip>
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

      <Modal
        title="路由诊断"
        open={diagnoseOpen}
        onCancel={() => setDiagnoseOpen(false)}
        footer={null}
        width={920}
        destroyOnHidden
      >
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          <Space.Compact block>
            <Input
              autoFocus
              value={diagnoseModel}
              onChange={(event) => setDiagnoseModel(event.target.value)}
              onPressEnter={() => void runDiagnosis()}
              placeholder="输入客户端实际使用的模型名称"
            />
            <Select
              value={diagnoseEndpoint}
              options={diagnosticEndpoints}
              onChange={setDiagnoseEndpoint}
              style={{ width: 220 }}
            />
            <Button type="primary" loading={diagnosing} onClick={() => void runDiagnosis()}>
              诊断
            </Button>
          </Space.Compact>
          <Input
            allowClear
            value={diagnoseSessionId}
            onChange={(event) => setDiagnoseSessionId(event.target.value)}
            onPressEnter={() => void runDiagnosis()}
            placeholder="会话 ID（可选，用于查看粘性路由顺序）"
          />

          {diagnosis && (
            <>
              <Alert
                showIcon
                type={diagnosis.resolved ? 'success' : 'error'}
                message={diagnosis.resolved ? '路由可用' : '路由不可用'}
                description={diagnosis.message}
              />
              <Descriptions size="small" bordered column={2}>
                <Descriptions.Item label="匹配方式">
                  {matchTypeLabels[diagnosis.match_type]}
                </Descriptions.Item>
                <Descriptions.Item label="路由">
                  {diagnosis.route_name || '-'}
                </Descriptions.Item>
                <Descriptions.Item label="策略">
                  {diagnosis.strategy ? strategyLabels[diagnosis.strategy] : '-'}
                </Descriptions.Item>
                <Descriptions.Item label="能力桶">
                  {diagnosis.barrel
                    ? [
                        diagnosis.barrel.context_limit
                          ? `上下文 ${diagnosis.barrel.context_limit}`
                          : '',
                        diagnosis.barrel.output_limit
                          ? `输出 ${diagnosis.barrel.output_limit}`
                          : '',
                      ]
                        .filter(Boolean)
                        .join(' / ') || '-'
                    : '-'}
                  {diagnosis.barrel_incomplete ? '（信息不完整）' : ''}
                </Descriptions.Item>
              </Descriptions>
              {diagnosis.runtime_targets && diagnosis.runtime_targets.length > 0 && (
                <>
                  <Space>
                    <Typography.Text strong>会话实际顺序</Typography.Text>
                    {diagnosis.session_id && (
                      <Button
                        type="link"
                        size="small"
                        icon={<HistoryOutlined />}
                        onClick={() => {
                          const params = new URLSearchParams({ session_id: diagnosis.session_id! })
                          navigate(`/usage?${params}`)
                        }}
                      >
                        查看会话日志
                      </Button>
                    )}
                  </Space>
                  <Table
                    rowKey={(record) =>
                      `${record.order}-${record.provider_id}-${record.upstream_model}-${record.provider_api_key_id ?? 0}`
                    }
                    size="small"
                    pagination={false}
                    dataSource={diagnosis.runtime_targets}
                    scroll={{ x: 720 }}
                    columns={[
                      {
                        title: '顺序',
                        dataIndex: 'order',
                        width: 70,
                      },
                      {
                        title: '提供商',
                        dataIndex: 'provider_name',
                        width: 170,
                        render: (value: string, record) => (
                          <Space size={4}>
                            <Typography.Text strong>{value}</Typography.Text>
                            {record.provider_health === false && <Tag color="error">异常</Tag>}
                          </Space>
                        ),
                      },
                      {
                        title: '上游模型',
                        dataIndex: 'upstream_model',
                        width: 210,
                        render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                      },
                      {
                        title: '上游 Key',
                        dataIndex: 'provider_api_key_name',
                        render: (value?: string | null) =>
                          value ? <Typography.Text>{value}</Typography.Text> : '-',
                      },
                    ]}
                  />
                </>
              )}
              {diagnosis.targets.length ? (
                <Table
                  rowKey={(record) => `${record.provider_id}-${record.upstream_model}`}
                  size="small"
                  pagination={false}
                  dataSource={diagnosis.targets}
                  scroll={{ x: 820 }}
                  columns={[
                    {
                      title: '状态',
                      dataIndex: 'eligible',
                      width: 80,
                      render: (eligible: boolean) => (
                        <Tag color={eligible ? 'success' : 'error'}>
                          {eligible ? '可用' : '跳过'}
                        </Tag>
                      ),
                    },
                    {
                      title: '提供商',
                      dataIndex: 'provider_name',
                      width: 150,
                      render: (value: string, record) => (
                        <Space size={4}>
                          <Typography.Text strong>{value}</Typography.Text>
                          {record.provider_health === false && <Tag color="error">异常</Tag>}
                        </Space>
                      ),
                    },
                    {
                      title: '上游模型',
                      dataIndex: 'upstream_model',
                      width: 180,
                      render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                    },
                    {
                      title: '支持接口',
                      dataIndex: 'supported_endpoints',
                      width: 220,
                      render: (endpoints: string[]) =>
                        endpoints.length
                          ? endpoints.map((endpoint) => (
                              <Tag key={endpoint}>{endpoint.replace(/^\/v1/, '')}</Tag>
                            ))
                          : <Typography.Text type="secondary">未声明</Typography.Text>,
                    },
                    {
                      title: '原因',
                      dataIndex: 'reason',
                      render: (value: string) => (
                        <Typography.Text type={value === 'eligible' ? 'success' : 'secondary'}>
                          {diagnosisReasonLabels[value] || value}
                        </Typography.Text>
                      ),
                    },
                  ]}
                />
              ) : (
                <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有匹配到候选目标" />
              )}
            </>
          )}
        </Space>
      </Modal>
    </>
  )
}

function providerModels(providers: Provider[], providerId?: number) {
  return providers.find((provider) => provider.id === providerId)?.models || []
}
