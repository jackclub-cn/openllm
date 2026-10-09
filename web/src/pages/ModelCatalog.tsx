import { useEffect, useMemo, useState } from 'react'
import {
  AppstoreOutlined,
  ExperimentOutlined,
  HistoryOutlined,
  ReloadOutlined,
  SearchOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Input,
  Select,
  Space,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import { useNavigate } from 'react-router-dom'
import { api, formatError, type ModelInventory } from '../api'
import PageHeader from '../components/PageHeader'
import { formatCompact, formatExact } from '../format'

function price(value?: number | null) {
  if (value == null) return '-'
  return `$${value.toLocaleString('en-US', { maximumFractionDigits: 6 })}`
}

export default function ModelCatalog() {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [items, setItems] = useState<ModelInventory[]>([])
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [providerId, setProviderId] = useState<number>()
  const [status, setStatus] = useState<'all' | 'enabled' | 'disabled'>('all')
  const [endpoint, setEndpoint] = useState<string>()

  const load = async () => {
    setLoading(true)
    try {
      setItems(await api.get<ModelInventory[]>('/api/model-inventory'))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const providers = useMemo(
    () =>
      Array.from(
        new Map(items.map((item) => [item.provider_id, item.provider_name])).entries(),
      ).map(([value, label]) => ({ value, label })),
    [items],
  )
  const endpoints = useMemo(
    () =>
      Array.from(new Set(items.flatMap((item) => item.served_endpoints))).sort(),
    [items],
  )
  const filteredItems = items.filter((item) => {
    const query = search.trim().toLowerCase()
    if (query) {
      const publicModel = `${item.model_prefix}${item.model_name}`
      const searchable = [item.model_name, publicModel, item.provider_name]
        .join(' ')
        .toLowerCase()
      if (!searchable.includes(query)) return false
    }
    if (providerId && item.provider_id !== providerId) return false
    if (status === 'enabled' && (!item.enabled || !item.provider_enabled)) return false
    if (status === 'disabled' && item.enabled && item.provider_enabled) return false
    // Filter on what the gateway serves, not just what the upstream declares,
    // so translated endpoints show up in the results.
    if (endpoint && !item.served_endpoints.includes(endpoint)) return false
    return true
  })
  const enabledCount = items.filter((item) => item.enabled && item.provider_enabled).length

  return (
    <>
      <PageHeader
        title="模型目录"
        description="跨提供商查看模型能力、接口和价格，并直接进入日志或路由诊断"
        extra={
          <Space wrap>
            <Input
              allowClear
              prefix={<SearchOutlined />}
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="搜索模型或提供商"
              style={{ width: 240 }}
            />
            <Select
              allowClear
              showSearch
              optionFilterProp="label"
              value={providerId}
              onChange={setProviderId}
              placeholder="提供商"
              options={providers}
              style={{ width: 180 }}
            />
            <Select
              value={status}
              onChange={setStatus}
              style={{ width: 120 }}
              options={[
                { value: 'all', label: '全部状态' },
                { value: 'enabled', label: '启用' },
                { value: 'disabled', label: '停用' },
              ]}
            />
            <Select
              allowClear
              value={endpoint}
              onChange={setEndpoint}
              placeholder="支持接口"
              options={endpoints.map((value) => ({ value, label: value }))}
              style={{ width: 190 }}
            />
            <Tag>{filteredItems.length}/{items.length}</Tag>
            <Button
              icon={<ReloadOutlined spin={loading} />}
              loading={loading}
              onClick={() => void load()}
            >
              刷新
            </Button>
          </Space>
        }
      />
      <Alert
        className="page-alert"
        type="info"
        showIcon
        message={`已收录 ${items.length} 个上游模型，其中 ${enabledCount} 个可用于当前路由。`}
      />
      <Card bordered={false}>
        <Table
          rowKey={(record) => `${record.provider_id}:${record.model_name}`}
          loading={loading}
          dataSource={filteredItems}
          pagination={{
            defaultPageSize: 50,
            pageSizeOptions: [20, 50, 100],
            showSizeChanger: true,
            showTotal: (total, range) => `${range[0]}-${range[1]} / ${total}`,
          }}
          scroll={{ x: 1200 }}
          locale={{ emptyText: items.length ? '没有符合筛选条件的模型' : '暂无上游模型，请先同步提供商模型' }}
          columns={[
            {
              title: '模型',
              dataIndex: 'model_name',
              width: 220,
              render: (value: string, record) => {
                const publicModel = `${record.model_prefix}${value}`
                return (
                  <div>
                    <Typography.Text strong ellipsis={{ tooltip: value }}>
                      {value}
                    </Typography.Text>
                    {publicModel !== value && (
                      <div>
                        <Typography.Text code ellipsis={{ tooltip: publicModel }}>
                          {publicModel}
                        </Typography.Text>
                      </div>
                    )}
                  </div>
                )
              },
            },
            {
              title: '提供商',
              dataIndex: 'provider_name',
              width: 160,
              render: (value: string, record) => (
                <Space size={6}>
                  <AppstoreOutlined />
                  <span>{value}</span>
                  {!record.provider_enabled && <Tag color="default">提供商停用</Tag>}
                </Space>
              ),
            },
            {
              title: '状态',
              dataIndex: 'enabled',
              width: 80,
              render: (value: boolean) => (
                <Tag color={value ? 'success' : 'default'}>{value ? '启用' : '停用'}</Tag>
              ),
            },
            {
              title: '上下文',
              dataIndex: 'context_limit',
              width: 100,
              render: (value: number | null | undefined, record) => (
                <LimitValue value={value} input={record.input_limit} />
              ),
            },
            {
              title: '输出上限',
              dataIndex: 'output_limit',
              width: 90,
              render: (value: number | null | undefined) => <LimitValue value={value} />,
            },
            {
              title: '输入 / 输出价格',
              width: 160,
              render: (_, record) => (
                <div>
                  <Typography.Text>{price(record.cost_input)} / {price(record.cost_output)}</Typography.Text>
                  <div>
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      每 1M tokens
                    </Typography.Text>
                  </div>
                </div>
              ),
            },
            {
              title: '支持接口',
              dataIndex: 'supported_endpoints',
              width: 220,
              render: (values: string[], record) => (
                <Tooltip
                  title={
                    <div>
                      <div>上游声明：{values.length ? values.join('、') : '未声明'}</div>
                      <div>网关可承接：{record.served_endpoints.join('、') || '未验证'}</div>
                    </div>
                  }
                >
                  {values.length ? (
                    <Space wrap size={[4, 4]}>
                      {values.slice(0, 2).map((value) => (
                        <Tag key={value}>{value.replace(/^\/v1/, '')}</Tag>
                      ))}
                      {values.length > 2 && <Tag>+{values.length - 2}</Tag>}
                    </Space>
                  ) : (
                    <Typography.Text type="secondary">未声明</Typography.Text>
                  )}
                </Tooltip>
              ),
            },
            {
              title: '操作',
              width: 100,
              fixed: 'right',
              render: (_, record) => {
                const publicModel = `${record.model_prefix}${record.model_name}`
                const diagnoseEndpoint =
                  record.supported_endpoints.find((value) => value.endsWith('/chat/completions')) ||
                  record.supported_endpoints[0] ||
                  '/v1/chat/completions'
                return (
                  <Space>
                    <Tooltip title="查看该模型的请求日志">
                      <Button
                        type="text"
                        icon={<HistoryOutlined />}
                        onClick={() =>
                          navigate(
                            `/usage?model=${encodeURIComponent(record.model_name)}&provider_id=${record.provider_id}`,
                          )
                        }
                      />
                    </Tooltip>
                    <Tooltip title="诊断该模型的路由">
                      <Button
                        type="text"
                        icon={<ExperimentOutlined />}
                        onClick={() =>
                          navigate(
                            `/routes?diagnose_model=${encodeURIComponent(publicModel)}&diagnose_endpoint=${encodeURIComponent(diagnoseEndpoint)}`,
                          )
                        }
                      />
                    </Tooltip>
                  </Space>
                )
              },
            },
          ]}
        />
      </Card>
    </>
  )
}

function LimitValue({
  value,
  input,
}: {
  value?: number | null
  input?: number | null
}) {
  if (value == null && input == null) return <Typography.Text type="secondary">-</Typography.Text>
  return (
    <Tooltip
      title={[
        value != null ? `上下文 ${formatExact(value)}` : '',
        input != null ? `输入 ${formatExact(input)}` : '',
      ]
        .filter(Boolean)
        .join(' / ')}
    >
      <div>
        {value != null && <div>{formatCompact(value)}</div>}
        {input != null && value !== input && (
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            输入 {formatCompact(input)}
          </Typography.Text>
        )}
      </div>
    </Tooltip>
  )
}
